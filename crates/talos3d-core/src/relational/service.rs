//! Parametric component service (PP-RPS-7, core).
//!
//! The transport-neutral operations behind the parametric inspect/edit UX and
//! MCP surface: set a driver (and get a propagation report), read derived
//! values, lock/unlock a driver, and explain the dependency trace ("why is this
//! part this way?"). The parameter panel, driver-bound handles, and rmcp tools
//! are thin layers over this service (same pattern as the procedural session:
//! a world-side service first, transport second).
//!
//! Derivation itself is supplied by the caller as a pure closure
//! (`Derive`) — in the full system this is the component's declared `ScalarExpr`
//! graph; here it is injected so the service is testable in isolation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::component::{
    ComponentParams, DriverEditError, DriverPolicy, OccurrenceDrivers, ParamRole,
};
use super::graph::{DependencyGraph, NodeId};

/// Recompute closure: derived-value map from the current drivers.
pub type Derive<'a> = dyn Fn(&OccurrenceDrivers) -> BTreeMap<String, Value> + 'a;

/// Report returned to the UI/agent after a driver edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PropagationReport {
    /// Nodes recomputed, in topological order.
    pub recomputed: Vec<NodeId>,
    /// Derived values that changed value, with their new value.
    pub changed_derived: BTreeMap<String, Value>,
}

/// A live parametric component instance: params, driver overrides, the
/// param/derived dependency graph, the last derived values, and lock state.
pub struct ParametricComponent {
    component: u64,
    params: ComponentParams,
    drivers: OccurrenceDrivers,
    graph: DependencyGraph,
    derived: BTreeMap<String, Value>,
}

impl ParametricComponent {
    /// `edges`: for each derived param, the driver/param names it depends on.
    pub fn new(
        component: u64,
        params: ComponentParams,
        edges: &BTreeMap<String, Vec<String>>,
    ) -> Self {
        let mut graph = DependencyGraph::new();
        for (name, _) in &params.roles {
            graph.add_node(NodeId::param(component, name.clone()));
        }
        for (derived, inputs) in edges {
            for inp in inputs {
                // derived depends on inp
                let _ = graph.add_dependency(
                    NodeId::param(component, derived.clone()),
                    NodeId::param(component, inp.clone()),
                );
            }
        }
        Self {
            component,
            params,
            drivers: OccurrenceDrivers::default(),
            graph,
            derived: BTreeMap::new(),
        }
    }

    pub fn derived(&self) -> &BTreeMap<String, Value> {
        &self.derived
    }

    pub fn driver(&self, name: &str) -> Option<&Value> {
        self.drivers.get(name)
    }

    /// Set a driver and propagate. Returns the propagation report or refuses
    /// (derived/read-only/locked).
    pub fn set_driver(
        &mut self,
        name: &str,
        value: Value,
        derive: &Derive<'_>,
    ) -> Result<PropagationReport, DriverEditError> {
        self.drivers.set_driver(&self.params, name, value)?;
        // mark the param dirty; the graph yields the affected (derived) nodes.
        self.graph
            .mark_dirty(NodeId::param(self.component, name.to_string()));
        let recomputed = self
            .graph
            .take_dirty_topological()
            .expect("param graph acyclic");
        // recompute derived values via the supplied derivation
        let before = self.derived.clone();
        self.derived = derive(&self.drivers);
        let mut changed_derived = BTreeMap::new();
        for (k, v) in &self.derived {
            if before.get(k) != Some(v) && self.params.is_derived(k) {
                changed_derived.insert(k.clone(), v.clone());
            }
        }
        Ok(PropagationReport {
            recomputed,
            changed_derived,
        })
    }

    /// Lock a driver (pin it); subsequent direct edits are refused and must use
    /// the lock/inversion workflow (PP-RPS-6).
    pub fn lock(&mut self, name: &str) -> Result<(), DriverEditError> {
        match self.params.roles.get_mut(name) {
            Some(ParamRole::Driver { policy }) => {
                *policy = DriverPolicy::Locked;
                Ok(())
            }
            Some(ParamRole::Derived) => Err(DriverEditError::IsDerived(name.to_string())),
            None => Err(DriverEditError::UnknownParam(name.to_string())),
        }
    }

    pub fn unlock(&mut self, name: &str) -> Result<(), DriverEditError> {
        match self.params.roles.get_mut(name) {
            Some(ParamRole::Driver { policy }) => {
                *policy = DriverPolicy::Editable;
                Ok(())
            }
            Some(ParamRole::Derived) => Err(DriverEditError::IsDerived(name.to_string())),
            None => Err(DriverEditError::UnknownParam(name.to_string())),
        }
    }

    /// "Why is this the way it is?" — the transitive inputs that control `param`
    /// (its dependency closure), nearest-first.
    pub fn explain(&self, param: &str) -> Vec<NodeId> {
        let start = NodeId::param(self.component, param.to_string());
        let mut seen: BTreeSet<NodeId> = BTreeSet::new();
        let mut out = Vec::new();
        let mut frontier = self.graph.dependencies_of(&start);
        while let Some(n) = frontier.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            out.push(n.clone());
            for d in self.graph.dependencies_of(&n) {
                frontier.push(d);
            }
        }
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // truss-ish: drivers span, pitch, heel; derived half, apex.
    // half <- span ; apex <- half, pitch, heel
    fn truss() -> (ComponentParams, BTreeMap<String, Vec<String>>) {
        let params = ComponentParams::default()
            .driver("span", DriverPolicy::Editable)
            .driver("pitch", DriverPolicy::Editable)
            .driver("heel", DriverPolicy::Editable)
            .derived("half")
            .derived("apex");
        let mut edges = BTreeMap::new();
        edges.insert("half".to_string(), vec!["span".to_string()]);
        edges.insert(
            "apex".to_string(),
            vec!["half".to_string(), "pitch".to_string(), "heel".to_string()],
        );
        (params, edges)
    }

    fn derive_fn(d: &OccurrenceDrivers) -> BTreeMap<String, Value> {
        let g = |k: &str, def: f64| d.get(k).and_then(|v| v.as_f64()).unwrap_or(def);
        let span = g("span", 6000.0);
        let pitch = g("pitch", 30.0);
        let heel = g("heel", 90.0);
        let half = span / 2.0;
        let apex = heel + half * (pitch.to_radians()).tan();
        let mut m = BTreeMap::new();
        m.insert("half".into(), json!(half));
        m.insert("apex".into(), json!(apex));
        m
    }

    #[test]
    fn set_driver_propagates_and_reports() {
        let (p, e) = truss();
        let mut c = ParametricComponent::new(1, p, &e);
        // establish baseline
        c.set_driver("pitch", json!(30.0), &derive_fn).unwrap();
        let r = c.set_driver("span", json!(9000.0), &derive_fn).unwrap();
        // half + apex recompute when span changes
        assert!(r.changed_derived.contains_key("half"));
        assert!(r.changed_derived.contains_key("apex"));
        assert_eq!(c.derived()["half"], json!(4500.0));
    }

    #[test]
    fn changing_heel_does_not_recompute_half() {
        let (p, e) = truss();
        let mut c = ParametricComponent::new(1, p, &e);
        c.set_driver("span", json!(6000.0), &derive_fn).unwrap();
        let r = c.set_driver("heel", json!(120.0), &derive_fn).unwrap();
        // heel feeds apex but not half
        let names: BTreeSet<String> = r
            .recomputed
            .iter()
            .filter_map(|n| match n {
                NodeId::ComponentParam { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(names.contains("apex"));
        assert!(!names.contains("half"));
    }

    #[test]
    fn derived_cannot_be_set() {
        let (p, e) = truss();
        let mut c = ParametricComponent::new(1, p, &e);
        assert!(matches!(
            c.set_driver("apex", json!(1.0), &derive_fn).unwrap_err(),
            DriverEditError::IsDerived(_)
        ));
    }

    #[test]
    fn lock_then_edit_is_refused() {
        let (p, e) = truss();
        let mut c = ParametricComponent::new(1, p, &e);
        c.lock("apex").err(); // apex is derived -> err, ignore
        c.lock("span").unwrap();
        assert!(matches!(
            c.set_driver("span", json!(9000.0), &derive_fn).unwrap_err(),
            DriverEditError::Locked(_)
        ));
        c.unlock("span").unwrap();
        assert!(c.set_driver("span", json!(9000.0), &derive_fn).is_ok());
    }

    #[test]
    fn explain_traces_controlling_drivers() {
        let (p, e) = truss();
        let c = ParametricComponent::new(1, p, &e);
        // apex is controlled by half, pitch, heel, and (via half) span
        let trace: BTreeSet<String> = c
            .explain("apex")
            .iter()
            .filter_map(|n| match n {
                NodeId::ComponentParam { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(trace.contains(&"half".to_string()));
        assert!(trace.contains(&"pitch".to_string()));
        assert!(trace.contains(&"heel".to_string()));
        assert!(trace.contains(&"span".to_string()), "transitive via half");
    }
}
