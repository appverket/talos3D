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
use super::transform::{
    map_transform, TransformAxis, TransformBindings, TransformGesture, TransformOutcome,
};

fn default_true_bool() -> bool {
    true
}

fn is_true_bool(v: &bool) -> bool {
    *v
}

// ---------------------------------------------------------------------------
// Declarative geometry representation
// ---------------------------------------------------------------------------

/// Per-member semantic identity declaration, propagated to the spawned entity
/// at materialisation time.
///
/// All fields are optional and serde-default so existing capsules without this
/// field deserialise and materialise with no behaviour change.
///
/// Wire shape is intentionally isomorphic to `SemanticEntityAnnotationRequest`
/// (used by `create_box`) so callers supply the same JSON vocabulary for both
/// surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricMemberSemantic {
    /// Registered element-class term (e.g. `"column"`, `"beam"`, `"panel"`).
    /// Must be present in the capability registry for the annotation to apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_class: Option<String>,
    /// Target refinement state (e.g. `"Conceptual"`, `"Schematic"`,
    /// `"Constructible"`, `"Detailed"`, `"FabricationReady"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refinement_state: Option<String>,
    /// Arbitrary construction-intent parameters (e.g. `{"construction_role":
    /// "chord"}`) stored as a `SemanticIntent` component on the entity.
    #[serde(default)]
    pub parameters: serde_json::Value,
    /// Human-readable rationale for the annotation (stored as
    /// `AuthoringProvenance::rationale`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// One rectangular-extrusion member whose size, translation, and rotation are
/// expressed as `ScalarExpr` values evaluated in the same driver+derived
/// environment as `ParametricTypeDef::derive`.
///
/// Convention:
/// - `size[0]` = X (width), `size[1]` = Y (extrusion height), `size[2]` = Z (depth).
/// - `translate[0..2]` = world-space centre offset from the type's origin.
/// - `rotate_euler_deg[0..2]` = Euler angles in degrees (X, Y, Z order).
///
/// All six `ScalarExpr` arrays may reference any driver or derived name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricMember {
    /// X (width), Y (extrusion/height), Z (depth) — all in mm.
    pub size: [ScalarExpr; 3],
    /// World-space centre translation from the instance origin, in mm.
    #[serde(default = "default_zero_exprs")]
    pub translate: [ScalarExpr; 3],
    /// Euler-angle rotation in degrees (X, Y, Z application order).
    #[serde(default = "default_zero_exprs")]
    pub rotate_euler_deg: [ScalarExpr; 3],
    /// Optional extruded-polygon profile, as ordered `[u, v]` points (mm) in the
    /// profile plane (u → local X, v → local Z), extruded along local Y by
    /// `size[1]`. When non-empty the member is an arbitrary extruded polygon
    /// (e.g. a gable triangle) and `size[0]`/`size[2]` are ignored; when empty
    /// the member is the `rectangle(size[0], size[2])` default. This keeps the
    /// representation fully general — any shape, no per-shape code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_xz: Vec<[ScalarExpr; 2]>,
    /// Optional human-readable label for debugging / MCP inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Optional semantic identity declaration. When present the spawned entity
    /// receives element-class, refinement-state, and parameters components via
    /// the same `apply_semantic_annotation` path used by `create_box`.
    /// Members without this field keep prior behaviour (no regression).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<ParametricMemberSemantic>,
}

fn default_zero_exprs() -> [ScalarExpr; 3] {
    [
        ScalarExpr::lit(Quantity::num(0.0)),
        ScalarExpr::lit(Quantity::num(0.0)),
        ScalarExpr::lit(Quantity::num(0.0)),
    ]
}

/// Declarative geometry attached to a `ParametricTypeDef`. When a type carries
/// a `representation`, `parametric.create` materialises real scene geometry by
/// evaluating each `ParametricMember` against the effective driver+derived env
/// and spawning `ProfileExtrusion` entities — one per member.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricRepresentation {
    pub members: Vec<ParametricMember>,
}

/// Concrete member geometry produced by `evaluate_representation`. All values
/// are plain `f64` (mm or degrees); callers convert to `f32`/`Vec3` for Bevy.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedMember {
    /// [width_mm, height_mm, depth_mm]
    pub size: [f64; 3],
    /// [tx_mm, ty_mm, tz_mm]
    pub translate: [f64; 3],
    /// [rx_deg, ry_deg, rz_deg]
    pub rotate_euler_deg: [f64; 3],
    /// Evaluated extruded-polygon profile points `[u_mm, v_mm]` (empty = rectangle).
    pub profile_xz: Vec<[f64; 2]>,
    pub label: Option<String>,
}

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
    /// When `false` this type is an internal evaluator input delegated to by a
    /// `Definition`; it is excluded from `parametric.list_types` MCP discovery
    /// and from `ParametricRegistry::list_public()`.
    ///
    /// Defaults to `true` (backward-compatible; all pre-existing entries remain
    /// publicly discoverable until explicitly set to `false`).
    #[serde(default = "default_true_bool", skip_serializing_if = "is_true_bool")]
    pub public: bool,
    /// Optional declarative geometry. When present, `parametric.create`
    /// synthesises scene geometry by evaluating each member against the
    /// effective driver+derived environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub representation: Option<ParametricRepresentation>,
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
                env.insert(
                    name.clone(),
                    Quantity {
                        value: v,
                        unit: *unit,
                    },
                );
            }
        }
        env
    }

    /// Evaluate all derivations into a unit-carrying environment (drivers +
    /// derived), resolving derived-on-derived in dependency order. Derived
    /// `Quantity`s keep the unit their `ScalarExpr` produced. This is the
    /// authoritative env for every downstream consumer — derivation chains AND
    /// the geometry `representation` — so units stay consistent (e.g. a derived
    /// `half_span` is Mm, not Dimensionless, and composes with Mm drivers).
    pub fn derive_env(&self, drivers: &BTreeMap<String, Value>) -> BTreeMap<String, Quantity> {
        let mut env = self.base_env(drivers);
        for _ in 0..=self.derivations.len() {
            let mut progressed = false;
            for (name, expr) in &self.derivations {
                if env.contains_key(name) {
                    continue;
                }
                if let Ok(q) = expr.eval(&env) {
                    env.insert(name.clone(), q);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        env
    }

    /// Evaluate all derivations (derived-on-derived resolved in dependency
    /// order) -> name -> value. Derived names only (drivers excluded).
    pub fn derive(&self, drivers: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        let env = self.derive_env(drivers);
        self.derivations
            .keys()
            .filter_map(|name| env.get(name).map(|q| (name.clone(), Value::from(q.value))))
            .collect()
    }

    /// Effective driver values = defaults overlaid with overrides.
    pub fn effective_drivers(
        &self,
        overrides: &BTreeMap<String, Value>,
    ) -> BTreeMap<String, Value> {
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

    /// Evaluate the declarative `representation` (if any) against the provided
    /// driver overrides.
    ///
    /// Builds the full evaluation environment (drivers ∪ derived values) and
    /// evaluates each `ParametricMember`'s six `ScalarExpr` fields.
    ///
    /// Returns:
    /// - `None`        — type has no `representation`.
    /// - `Some(Ok(_))` — all members evaluated successfully.
    /// - `Some(Err(_))` — a member expression failed; the error names the
    ///   member index, its label (if any), and which field (`size[i]`,
    ///   `translate[i]`, `rotate[i]`) failed to evaluate.
    pub fn evaluate_representation(
        &self,
        overrides: &BTreeMap<String, Value>,
    ) -> Option<Result<Vec<EvaluatedMember>, String>> {
        let repr = self.representation.as_ref()?;
        let drivers = self.effective_drivers(overrides);
        // Unit-carrying env (drivers + derived). Derived values keep their
        // computed unit so member exprs that mix derived + driver terms (e.g.
        // half_span + overhang_mm) are unit-consistent.
        let env = self.derive_env(&drivers);
        Some(evaluate_members(&repr.members, &env))
    }
}

/// Evaluate a slice of `ParametricMember`s against a pre-built `Quantity`
/// environment. Returns `Ok(members)` on success, or `Err` naming the first
/// member (by index + label) and which expression field failed.
fn evaluate_members(
    members: &[ParametricMember],
    env: &BTreeMap<String, Quantity>,
) -> Result<Vec<EvaluatedMember>, String> {
    let eval_field = |m: &ParametricMember, idx: usize, expr: &ScalarExpr, field: &str| {
        expr.eval(env).map(|q| q.value).map_err(|e| {
            let label_hint = m
                .label
                .as_deref()
                .map(|l| format!(" (label='{l}')"))
                .unwrap_or_default();
            format!("member[{idx}]{label_hint}: failed to evaluate {field}: {e}")
        })
    };

    let mut out = Vec::with_capacity(members.len());
    for (idx, m) in members.iter().enumerate() {
        let size = [
            eval_field(m, idx, &m.size[0], "size[0]")?,
            eval_field(m, idx, &m.size[1], "size[1]")?,
            eval_field(m, idx, &m.size[2], "size[2]")?,
        ];
        let translate = [
            eval_field(m, idx, &m.translate[0], "translate[0]")?,
            eval_field(m, idx, &m.translate[1], "translate[1]")?,
            eval_field(m, idx, &m.translate[2], "translate[2]")?,
        ];
        let rotate_euler_deg = [
            eval_field(m, idx, &m.rotate_euler_deg[0], "rotate[0]")?,
            eval_field(m, idx, &m.rotate_euler_deg[1], "rotate[1]")?,
            eval_field(m, idx, &m.rotate_euler_deg[2], "rotate[2]")?,
        ];
        let mut profile_xz = Vec::with_capacity(m.profile_xz.len());
        for (pi, pt) in m.profile_xz.iter().enumerate() {
            profile_xz.push([
                eval_field(m, idx, &pt[0], &format!("profile_xz[{pi}].u"))?,
                eval_field(m, idx, &pt[1], &format!("profile_xz[{pi}].v"))?,
            ]);
        }
        out.push(EvaluatedMember {
            size,
            translate,
            rotate_euler_deg,
            profile_xz,
            label: m.label.clone(),
        });
    }
    Ok(out)
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

    /// Register an ephemeral (runtime-only, non-public) type and return its
    /// assigned id. Used by the dynamic-authoring path so inline definitions
    /// supplied over MCP are handled identically to curated types.
    pub fn register_ephemeral(&mut self, mut def: ParametricTypeDef) -> String {
        // Assign a unique ephemeral id if the caller did not supply one.
        if def.id.is_empty() || self.types.contains_key(&def.id) {
            def.id = format!("ephemeral.{}", self.types.len());
        }
        def.public = false;
        let id = def.id.clone();
        self.types.insert(id.clone(), def);
        id
    }

    pub fn get(&self, id: &str) -> Option<&ParametricTypeDef> {
        self.types.get(id)
    }
    /// Return all registered types (including internal ones).
    /// Prefer [`list_public`] for MCP and UI discovery surfaces.
    pub fn list(&self) -> Vec<(String, String)> {
        self.types
            .values()
            .map(|t| (t.id.clone(), t.label.clone()))
            .collect()
    }
    /// Return only types whose `public` flag is `true`.
    ///
    /// This is the correct source for `parametric.list_types` MCP discovery
    /// and any UI that shows user-facing component families. Internal evaluator
    /// inputs (types with `public: false`) are excluded so they do not appear
    /// alongside the `Definition`-based components they back.
    pub fn list_public(&self) -> Vec<(String, String)> {
        self.types
            .values()
            .filter(|t| t.public)
            .map(|t| (t.id.clone(), t.label.clone()))
            .collect()
    }
}

/// World-space placement for a parametric instance (metres + Euler degrees).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Placement {
    /// World-space translation in model metres: [tx, ty, tz].
    #[serde(default)]
    pub translate: [f64; 3],
    /// Euler rotation in degrees (X, Y, Z application order): [rx_deg, ry_deg, rz_deg].
    #[serde(default)]
    pub rotate_euler_deg: [f64; 3],
}

/// A live instance of a parametric type: its type id, driver overrides,
/// placement, and the element ids of any spawned geometry entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricInstance {
    pub instance_id: u64,
    pub type_id: String,
    pub overrides: BTreeMap<String, Value>,
    /// World-space placement applied to every synthesised geometry member.
    #[serde(default)]
    pub placement: Placement,
    /// Element IDs of spawned geometry entities (updated by synthesis / re-synthesis).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geometry: Vec<u64>,
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
    /// CREATE: instantiate a registered type with empty overrides and default placement.
    pub fn instantiate(&mut self, type_id: &str) -> u64 {
        self.instantiate_with(type_id, BTreeMap::new(), Placement::default())
    }

    /// CREATE: instantiate with explicit overrides and placement.
    pub fn instantiate_with(
        &mut self,
        type_id: &str,
        overrides: BTreeMap<String, Value>,
        placement: Placement,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.instances.insert(
            id,
            ParametricInstance {
                instance_id: id,
                type_id: type_id.to_string(),
                overrides,
                placement,
                geometry: Vec::new(),
            },
        );
        id
    }

    /// Record the element ids of synthesised geometry entities for the instance.
    pub fn set_geometry(&mut self, id: u64, element_ids: Vec<u64>) {
        if let Some(inst) = self.instances.get_mut(&id) {
            inst.geometry = element_ids;
        }
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
            .and_then(|d| {
                def.effective_drivers(&inst.overrides)
                    .get(d)
                    .and_then(|v| v.as_f64())
            })
            .unwrap_or(0.0);
        let outcome = map_transform(&def.transform, axis, gesture, current);
        // if it resolved to a driver edit, apply it
        if let TransformOutcome::DriverEdit { driver, new_value } = &outcome {
            let _ = self.set_driver(registry, id, driver, Value::from(*new_value));
        }
        Some(outcome)
    }

    /// INSPECT: "why is this param the way it is" — its controlling inputs.
    pub fn explain(
        &self,
        registry: &ParametricRegistry,
        id: u64,
        param: &str,
    ) -> Option<Vec<NodeId>> {
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
                    rhs: Box::new(ScalarExpr::Tan {
                        expr: Box::new(ScalarExpr::param("pitch")),
                    }),
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
            public: true,
            representation: None,
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
            .transform(
                &reg,
                id,
                TransformAxis::X,
                TransformGesture::SetExtent { value: 9000.0 },
            )
            .unwrap();
        assert!(matches!(out, TransformOutcome::DriverEdit { .. }));
        assert_eq!(
            store.snapshot(&reg, id).unwrap().drivers["span"],
            json!(9000.0)
        );
        // unmapped axis refuses
        let z = store
            .transform(
                &reg,
                id,
                TransformAxis::Z,
                TransformGesture::Scale { factor: 2.0 },
            )
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

    // -----------------------------------------------------------------------
    // ParametricRepresentation tests
    // -----------------------------------------------------------------------

    /// Build a simple type with a two-member representation: a box whose width
    /// and height are driven by `w` and `h`, and a second box offset in X by `w`.
    fn repr_type() -> ParametricTypeDef {
        use crate::relational::component::DriverPolicy;

        let params = ComponentParams::default()
            .driver("w", DriverPolicy::Editable)
            .driver("h", DriverPolicy::Editable);
        let mut driver_units = BTreeMap::new();
        driver_units.insert("w".into(), Unit::Mm);
        driver_units.insert("h".into(), Unit::Mm);
        let mut defaults = BTreeMap::new();
        defaults.insert("w".into(), 1000.0);
        defaults.insert("h".into(), 500.0);

        // Member 0: box centred at origin, size [w, h, 100]
        let m0 = ParametricMember {
            size: [
                ScalarExpr::param("w"),
                ScalarExpr::param("h"),
                ScalarExpr::lit(Quantity::mm(100.0)),
            ],
            translate: default_zero_exprs(),
            rotate_euler_deg: default_zero_exprs(),
            profile_xz: Vec::new(),
            label: Some("box_a".into()),
            semantic: None,
        };
        // Member 1: same size but translated X = w (placed next to box_a).
        let m1 = ParametricMember {
            size: [
                ScalarExpr::param("w"),
                ScalarExpr::param("h"),
                ScalarExpr::lit(Quantity::mm(100.0)),
            ],
            translate: [
                ScalarExpr::param("w"),
                ScalarExpr::lit(Quantity::mm(0.0)),
                ScalarExpr::lit(Quantity::mm(0.0)),
            ],
            rotate_euler_deg: default_zero_exprs(),
            profile_xz: Vec::new(),
            label: Some("box_b".into()),
            semantic: None,
        };

        ParametricTypeDef {
            id: "test.repr".into(),
            label: "Test Repr".into(),
            params,
            driver_units,
            defaults,
            derivations: BTreeMap::new(),
            transform: TransformBindings::default(),
            public: true,
            representation: Some(ParametricRepresentation {
                members: vec![m0, m1],
            }),
        }
    }

    #[test]
    fn representation_evaluates_correct_member_count() {
        let ty = repr_type();
        let overrides = BTreeMap::new();
        let members = ty
            .evaluate_representation(&overrides)
            .expect("type has a representation")
            .expect("all members should evaluate");
        assert_eq!(members.len(), 2, "expected 2 members");
    }

    #[test]
    fn representation_evaluates_sizes_from_default_drivers() {
        let ty = repr_type();
        let overrides = BTreeMap::new();
        let members = ty.evaluate_representation(&overrides).unwrap().unwrap();

        // box_a: size = [1000, 500, 100], translate = [0, 0, 0]
        let a = &members[0];
        assert_eq!(a.label.as_deref(), Some("box_a"));
        assert!((a.size[0] - 1000.0).abs() < 1e-6, "width from default w");
        assert!((a.size[1] - 500.0).abs() < 1e-6, "height from default h");
        assert!((a.size[2] - 100.0).abs() < 1e-6, "depth literal");
        assert!(a.translate.iter().all(|&v| v.abs() < 1e-6));
        assert!(a.rotate_euler_deg.iter().all(|&v| v.abs() < 1e-6));
    }

    #[test]
    fn representation_evaluates_translation_from_driver() {
        let ty = repr_type();
        let overrides = BTreeMap::new();
        let members = ty.evaluate_representation(&overrides).unwrap().unwrap();

        // box_b: translated X = w = 1000 mm
        let b = &members[1];
        assert_eq!(b.label.as_deref(), Some("box_b"));
        assert!((b.translate[0] - 1000.0).abs() < 1e-6, "tx = w");
        assert!(b.translate[1].abs() < 1e-6);
    }

    #[test]
    fn representation_respects_driver_overrides() {
        let ty = repr_type();
        let mut overrides = BTreeMap::new();
        overrides.insert("w".to_string(), Value::from(2000.0));
        let members = ty.evaluate_representation(&overrides).unwrap().unwrap();
        assert!(
            (members[0].size[0] - 2000.0).abs() < 1e-6,
            "width = override w"
        );
        assert!(
            (members[1].translate[0] - 2000.0).abs() < 1e-6,
            "tx = override w"
        );
    }

    #[test]
    fn representation_none_returns_none() {
        let ty = truss_type(); // no representation
        assert!(ty.evaluate_representation(&BTreeMap::new()).is_none());
    }

    #[test]
    fn representation_err_on_unresolvable_expr() {
        use crate::relational::component::DriverPolicy;

        // A type with a member that references a non-existent param in size.
        let params = ComponentParams::default().driver("w", DriverPolicy::Editable);
        let mut driver_units = BTreeMap::new();
        driver_units.insert("w".into(), Unit::Mm);
        let mut defaults = BTreeMap::new();
        defaults.insert("w".into(), 1000.0);

        let bad_member = ParametricMember {
            size: [
                ScalarExpr::param("w"),
                ScalarExpr::param("missing_param"), // does not exist
                ScalarExpr::lit(Quantity::mm(100.0)),
            ],
            translate: default_zero_exprs(),
            rotate_euler_deg: default_zero_exprs(),
            profile_xz: Vec::new(),
            label: Some("bad_box".into()),
            semantic: None,
        };
        let ty = ParametricTypeDef {
            id: "test.bad".into(),
            label: "Bad".into(),
            params,
            driver_units,
            defaults,
            derivations: BTreeMap::new(),
            transform: TransformBindings::default(),
            public: false,
            representation: Some(ParametricRepresentation {
                members: vec![bad_member],
            }),
        };

        let result = ty
            .evaluate_representation(&BTreeMap::new())
            .expect("type has a representation");
        assert!(result.is_err(), "expected Err on unresolvable expr");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("bad_box"),
            "error should name the member label: {msg}"
        );
        assert!(
            msg.contains("size[1]"),
            "error should name the failing field: {msg}"
        );
    }

    #[test]
    fn representation_evaluates_polygon_profile_points() {
        use crate::relational::component::DriverPolicy;

        // A member whose profile is an extruded triangle expressed via
        // profile_xz: [(-w,0),(w,0),(0,h)] — a gable-end-shaped polygon.
        let params = ComponentParams::default()
            .driver("w", DriverPolicy::Editable)
            .driver("h", DriverPolicy::Editable);
        let mut driver_units = BTreeMap::new();
        driver_units.insert("w".into(), Unit::Mm);
        driver_units.insert("h".into(), Unit::Mm);
        let mut defaults = BTreeMap::new();
        defaults.insert("w".into(), 5000.0);
        defaults.insert("h".into(), 3000.0);

        let tri = ParametricMember {
            size: [
                ScalarExpr::lit(Quantity::num(0.0)),
                ScalarExpr::lit(Quantity::mm(18.0)),
                ScalarExpr::lit(Quantity::num(0.0)),
            ],
            translate: default_zero_exprs(),
            rotate_euler_deg: default_zero_exprs(),
            profile_xz: vec![
                [
                    ScalarExpr::Neg {
                        expr: Box::new(ScalarExpr::param("w")),
                    },
                    ScalarExpr::lit(Quantity::mm(0.0)),
                ],
                [ScalarExpr::param("w"), ScalarExpr::lit(Quantity::mm(0.0))],
                [ScalarExpr::lit(Quantity::mm(0.0)), ScalarExpr::param("h")],
            ],
            label: Some("gable".into()),
            semantic: None,
        };
        let ty = ParametricTypeDef {
            id: "test.tri".into(),
            label: "Tri".into(),
            params,
            driver_units,
            defaults,
            derivations: BTreeMap::new(),
            transform: TransformBindings::default(),
            public: false,
            representation: Some(ParametricRepresentation { members: vec![tri] }),
        };

        let members = ty
            .evaluate_representation(&BTreeMap::new())
            .expect("has representation")
            .expect("evaluates");
        assert_eq!(members.len(), 1);
        let pts = &members[0].profile_xz;
        assert_eq!(pts.len(), 3, "triangle has 3 points");
        assert!((pts[0][0] - (-5000.0)).abs() < 1e-6);
        assert!((pts[1][0] - 5000.0).abs() < 1e-6);
        assert!((pts[2][1] - 3000.0).abs() < 1e-6, "apex v = h");
    }

    #[test]
    fn instantiate_with_overrides_and_placement() {
        let (reg, mut store) = setup();
        let mut overrides = BTreeMap::new();
        overrides.insert("span".to_string(), Value::from(9000.0));
        let pl = Placement {
            translate: [100.0, 0.0, 0.0],
            rotate_euler_deg: [0.0, 45.0, 0.0],
        };
        let id = store.instantiate_with("test.truss", overrides.clone(), pl.clone());
        let inst = store.get(id).unwrap();
        assert_eq!(inst.type_id, "test.truss");
        assert_eq!(inst.overrides["span"], Value::from(9000.0));
        assert_eq!(inst.placement, pl);
        // Snapshot reflects the override.
        let snap = store.snapshot(&reg, id).unwrap();
        assert_eq!(snap.drivers["span"], Value::from(9000.0));
        assert_eq!(snap.derived["half"], Value::from(4500.0));
    }

    #[test]
    fn set_geometry_records_ids() {
        let (_reg, mut store) = setup();
        let id = store.instantiate("test.truss");
        store.set_geometry(id, vec![101, 102, 103]);
        assert_eq!(store.get(id).unwrap().geometry, vec![101u64, 102, 103]);
    }

    #[test]
    fn register_ephemeral_assigns_non_public_id() {
        let mut reg = ParametricRegistry::default();
        let ty = truss_type();
        let ephemeral_id = reg.register_ephemeral(ty);
        let def = reg.get(&ephemeral_id).unwrap();
        assert!(!def.public, "ephemeral types must be non-public");
        // Not listed in public types.
        assert!(reg.list_public().is_empty());
        // But accessible by id.
        assert!(reg.get(&ephemeral_id).is_some());
    }
}
