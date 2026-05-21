//! Parametric component registry + live instance store (PP-RPS-7 UX backend).
//!
//! The generic mechanism behind the "inspect / edit / create parametric
//! systems" UX. A capability registers `ParametricTypeDef`s (the *content* —
//! e.g. window, truss — lives in the domain crate per ADR-037); users/agents
//! then **create** instances, **inspect** their drivers/derived values and
//! dependency traces, and **edit** them by setting drivers, applying transform
//! gestures (mapped to drivers), or locking a derived value.
//!
//! Both the MCP tools and the egui parameter panel are thin layers over this
//! store. Derivation is the type's declared `ScalarExpr` graph; nothing here
//! names a discipline noun.

use std::collections::BTreeMap;

use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::component::{ComponentParams, DriverEditError};
use super::graph::NodeId;
use super::param_expr::{Quantity, ScalarExpr, Unit};
use super::service::{ParametricComponent, PropagationReport};
use super::transform::{TransformAxis, TransformBindings, TransformGesture, TransformOutcome, map_transform};

/// A registered parametric component *type*: drivers/derived classification,
/// per-driver units, default driver values, declared `ScalarExpr` derivations,
/// and transform-to-driver bindings. Generic container; domain crates build
/// these for their components.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricTypeDef {
    pub id: String,
    pub label: String,
    pub params: ComponentParams,
    pub driver_units: BTreeMap<String, Unit>,
    pub defaults: BTreeMap<String, f64>,
    pub derivations: BTreeMap<String, ScalarExpr>,
    pub transform: TransformBindings,
}

impl ParametricTypeDef {
    /// Dependency edges: derived -> input param names.
    pub fn edges(&self) -> BTreeMap<String, Vec<String>> {
        self.derivations
            .iter()
            .map(|(d, e)| (d.clone(), e.dependencies().into_iter().collect()))
            .collect()
    }

    fn base_env(&self, drivers: &BTreeMap<String, Value>) -> BTreeMap<String, Quantity> {
        let mut env = BTreeMap::new();
        for (name, unit) in &self.driver_units {
            let v = drivers
                .get(name)
                .and_then(|v| v.as_f64())
                .or_else(|| self.defaults.get(name).copied());
            if let Some(v) = v {
                env.insert(name.clone(), Quantity { value: v, unit: *unit });
            }
        }
        env
    }

    /// Evaluate all derivations (derived-on-derived resolved in dependency
    /// order) -> name -> value.
    pub fn derive(&self, drivers: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        let mut env = self.base_env(drivers);
        let mut out: BTreeMap<String, Value> = BTreeMap::new();
        for _ in 0..=self.derivations.len() {
            let mut progressed = false;
            for (name, expr) in &self.derivations {
                if out.contains_key(name) {
                    continue;
                }
                if let Ok(q) = expr.eval(&env) {
                    env.insert(name.clone(), q);
                    out.insert(name.clone(), Value::from(q.value));
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        out
    }

    /// Effective driver values = defaults overlaid with overrides.
    pub fn effective_drivers(&self, overrides: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        let mut out: BTreeMap<String, Value> = self
            .defaults
            .iter()
            .map(|(k, v)| (k.clone(), Value::from(*v)))
            .collect();
        for (k, v) in overrides {
            out.insert(k.clone(), v.clone());
        }
        out
    }
}

/// Registry of parametric types contributed by capabilities (Bevy resource;
/// domain plugins register their types into it at build time).
#[derive(Debug, Clone, Default, Resource)]
pub struct ParametricRegistry {
    types: BTreeMap<String, ParametricTypeDef>,
}

impl ParametricRegistry {
    pub fn register(&mut self, def: ParametricTypeDef) {
        self.types.insert(def.id.clone(), def);
    }
    pub fn get(&self, id: &str) -> Option<&ParametricTypeDef> {
        self.types.get(id)
    }
    pub fn list(&self) -> Vec<(String, String)> {
        self.types
            .values()
            .map(|t| (t.id.clone(), t.label.clone()))
            .collect()
    }
}

/// A live instance of a parametric type: its type id + driver overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricInstance {
    pub instance_id: u64,
    pub type_id: String,
    pub overrides: BTreeMap<String, Value>,
}

/// Snapshot returned to the inspect UX.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricSnapshot {
    pub instance_id: u64,
    pub type_id: String,
    pub label: String,
    /// driver name -> effective value
    pub drivers: BTreeMap<String, Value>,
    /// derived name -> computed value
    pub derived: BTreeMap<String, Value>,
}

/// Store of live parametric instances (Bevy resource).
#[derive(Debug, Clone, Default, Resource)]
pub struct ParametricStore {
    instances: BTreeMap<u64, ParametricInstance>,
    next_id: u64,
}

impl ParametricStore {
    /// CREATE: instantiate a registered type, returning the new instance id.
    pub fn instantiate(&mut self, type_id: &str) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.instances.insert(
            id,
            ParametricInstance {
                instance_id: id,
                type_id: type_id.to_string(),
                overrides: BTreeMap::new(),
            },
        );
        id
    }

    pub fn get(&self, id: u64) -> Option<&ParametricInstance> {
        self.instances.get(&id)
    }

    /// INSPECT: snapshot of an instance's drivers + derived values.
    pub fn snapshot(&self, registry: &ParametricRegistry, id: u64) -> Option<ParametricSnapshot> {
        let inst = self.instances.get(&id)?;
        let def = registry.get(&inst.type_id)?;
        let drivers = def.effective_drivers(&inst.overrides);
        let derived = def.derive(&drivers);
        Some(ParametricSnapshot {
            instance_id: id,
            type_id: inst.type_id.clone(),
            label: def.label.clone(),
            drivers,
            derived,
        })
    }

    /// EDIT: set a driver (policy-checked), returning the propagation report.
    pub fn set_driver(
        &mut self,
        registry: &ParametricRegistry,
        id: u64,
        name: &str,
        value: Value,
    ) -> Result<PropagationReport, DriverEditError> {
        let inst = self
            .instances
            .get_mut(&id)
            .ok_or_else(|| DriverEditError::UnknownParam(format!("instance {id}")))?;
        let def = registry
            .get(&inst.type_id)
            .ok_or_else(|| DriverEditError::UnknownParam(inst.type_id.clone()))?;
        // Build a service component to enforce policy + report propagation.
        let mut comp = ParametricComponent::new(id, def.params.clone(), &def.edges());
        // replay existing overrides into the component so propagation is correct
        let derive = |d: &super::component::OccurrenceDrivers| {
            let mut drivers: BTreeMap<String, Value> = def
                .defaults
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(*v)))
                .collect();
            for (k, v) in &d.overrides {
                drivers.insert(k.clone(), v.clone());
            }
            def.derive(&drivers)
        };
        for (k, v) in inst.overrides.clone() {
            // ignore policy errors when replaying already-accepted overrides
            let _ = comp.set_driver(&k, v, &derive);
        }
        let report = comp.set_driver(name, value.clone(), &derive)?;
        inst.overrides.insert(name.to_string(), value);
        Ok(report)
    }

    /// EDIT via gesture: map a transform onto a driver edit (or refuse).
    pub fn transform(
        &mut self,
        registry: &ParametricRegistry,
        id: u64,
        axis: TransformAxis,
        gesture: TransformGesture,
    ) -> Option<TransformOutcome> {
        let inst = self.instances.get(&id)?;
        let def = registry.get(&inst.type_id)?;
        let current = def
            .transform
            .driver_for(axis)
            .and_then(|d| def.effective_drivers(&inst.overrides).get(d).and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        let outcome = map_transform(&def.transform, axis, gesture, current);
        // if it resolved to a driver edit, apply it
        if let TransformOutcome::DriverEdit { driver, new_value } = &outcome {
            let _ = self.set_driver(registry, id, driver, Value::from(*new_value));
        }
        Some(outcome)
    }

    /// INSPECT: "why is this param the way it is" — its controlling inputs.
    pub fn explain(&self, registry: &ParametricRegistry, id: u64, param: &str) -> Option<Vec<NodeId>> {
        let inst = self.instances.get(&id)?;
        let def = registry.get(&inst.type_id)?;
        let comp = ParametricComponent::new(id, def.params.clone(), &def.edges());
        Some(comp.explain(param))
    }
}

/// Installs the parametric registry + instance store resources. Domain plugins
/// register their `ParametricTypeDef`s into `ParametricRegistry` during build.
pub struct ParametricPlugin;

impl bevy::prelude::Plugin for ParametricPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.init_resource::<ParametricRegistry>()
            .init_resource::<ParametricStore>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relational::component::DriverPolicy;
    use serde_json::json;

    // a tiny "truss-ish" parametric type for the registry tests
    fn truss_type() -> ParametricTypeDef {
        let params = ComponentParams::default()
            .driver("span", DriverPolicy::Editable)
            .driver("pitch", DriverPolicy::Editable)
            .driver("heel", DriverPolicy::Editable)
            .derived("half")
            .derived("apex");
        let mut driver_units = BTreeMap::new();
        driver_units.insert("span".into(), Unit::Mm);
        driver_units.insert("heel".into(), Unit::Mm);
        driver_units.insert("pitch".into(), Unit::Deg);
        let mut defaults = BTreeMap::new();
        defaults.insert("span".into(), 6000.0);
        defaults.insert("pitch".into(), 30.0);
        defaults.insert("heel".into(), 90.0);
        let mut d = BTreeMap::new();
        d.insert(
            "half".into(),
            ScalarExpr::Div {
                lhs: Box::new(ScalarExpr::param("span")),
                rhs: Box::new(ScalarExpr::lit(Quantity::num(2.0))),
            },
        );
        d.insert(
            "apex".into(),
            ScalarExpr::Add {
                lhs: Box::new(ScalarExpr::param("heel")),
                rhs: Box::new(ScalarExpr::Mul {
                    lhs: Box::new(ScalarExpr::param("half")),
                    rhs: Box::new(ScalarExpr::Tan { expr: Box::new(ScalarExpr::param("pitch")) }),
                }),
            },
        );
        ParametricTypeDef {
            id: "test.truss".into(),
            label: "Test Truss".into(),
            params,
            driver_units,
            defaults,
            derivations: d,
            transform: TransformBindings::default().bind(TransformAxis::X, "span"),
        }
    }

    fn setup() -> (ParametricRegistry, ParametricStore) {
        let mut reg = ParametricRegistry::default();
        reg.register(truss_type());
        (reg, ParametricStore::default())
    }

    #[test]
    fn create_and_inspect() {
        let (reg, mut store) = setup();
        let id = store.instantiate("test.truss");
        let snap = store.snapshot(&reg, id).unwrap();
        assert_eq!(snap.label, "Test Truss");
        assert_eq!(snap.drivers["span"], json!(6000.0));
        assert_eq!(snap.derived["half"], json!(3000.0));
        // apex = 90 + 3000*tan(30) ~ 1822
        assert!((snap.derived["apex"].as_f64().unwrap() - 1822.0).abs() < 3.0);
    }

    #[test]
    fn edit_driver_rederives() {
        let (reg, mut store) = setup();
        let id = store.instantiate("test.truss");
        let report = store.set_driver(&reg, id, "span", json!(9000.0)).unwrap();
        assert!(report.changed_derived.contains_key("half"));
        let snap = store.snapshot(&reg, id).unwrap();
        assert_eq!(snap.derived["half"], json!(4500.0));
    }

    #[test]
    fn transform_maps_to_driver() {
        let (reg, mut store) = setup();
        let id = store.instantiate("test.truss");
        let out = store
            .transform(&reg, id, TransformAxis::X, TransformGesture::SetExtent { value: 9000.0 })
            .unwrap();
        assert!(matches!(out, TransformOutcome::DriverEdit { .. }));
        assert_eq!(store.snapshot(&reg, id).unwrap().drivers["span"], json!(9000.0));
        // unmapped axis refuses
        let z = store
            .transform(&reg, id, TransformAxis::Z, TransformGesture::Scale { factor: 2.0 })
            .unwrap();
        assert!(matches!(z, TransformOutcome::Refused { .. }));
    }

    #[test]
    fn explain_lists_controlling_inputs() {
        let (reg, store) = {
            let (reg, mut store) = setup();
            store.instantiate("test.truss");
            (reg, store)
        };
        let trace: std::collections::BTreeSet<String> = store
            .explain(&reg, 1, "apex")
            .unwrap()
            .iter()
            .filter_map(|n| match n {
                NodeId::ComponentParam { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(trace.contains("half") && trace.contains("pitch") && trace.contains("heel"));
        assert!(trace.contains("span"));
    }

    #[test]
    fn registry_lists_types() {
        let (reg, _s) = setup();
        let ids: Vec<String> = reg.list().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec!["test.truss".to_string()]);
    }
}
