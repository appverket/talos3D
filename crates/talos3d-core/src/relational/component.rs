//! Component interior model (PP-RPS-3): drivers, derived values, per-driver
//! lock policy, and **stable derived-part identity**.
//!
//! Per the `RELATIONAL_PARAMETRIC_SUBSTRATE_AGREEMENT.md` (Definition And
//! Occurrence Refactor): the interior of a reusable component is rewritten from
//! override slots to **driver values + derived values + dependency edges**.
//! This module owns the generic mechanism; ADR-025's durable identity/lifecycle
//! (`Definition`, `Occurrence`, revision, library identity, instancing,
//! promotion, make-unique) is unchanged and lives in the modeling layer that
//! adopts this model.
//!
//! Two invariants are load-bearing:
//!
//! 1. **Derived values are never overridden directly** — only drivers are set;
//!    a derived value changes only through re-derivation or the lock/inversion
//!    workflow (PP-RPS-6).
//! 2. **Derived parts have stable, deterministic identities** — a pure function
//!    of (occurrence id × script-step identity × instance key/label). This is
//!    what makes re-derivation produce a *diff* instead of delete/recreate
//!    churn, and lets relations (hosting, support) attach reliably.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::graph::NodeId;

/// Per-driver editability / lock policy (replaces ADR-025 `OverridePolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DriverPolicy {
    /// Freely editable on occurrences.
    Editable,
    /// Read-only (set at the Definition, not per-occurrence).
    ReadOnly,
    /// Pinned by the user; edits must go through the lock/inversion workflow.
    Locked,
}

/// A parameter classified as a driver (free, sticky) or derived (computed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ParamRole {
    Driver {
        policy: DriverPolicy,
    },
    /// Derived from a declared expression (the expression itself is a
    /// `relational::ScalarExpr` referenced by parameter name; PP-RPS-2).
    Derived,
}

/// The parameter classification of a component type (Definition-level).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ComponentParams {
    /// name -> role
    pub roles: BTreeMap<String, ParamRole>,
}

impl ComponentParams {
    pub fn driver(mut self, name: impl Into<String>, policy: DriverPolicy) -> Self {
        self.roles.insert(name.into(), ParamRole::Driver { policy });
        self
    }
    pub fn derived(mut self, name: impl Into<String>) -> Self {
        self.roles.insert(name.into(), ParamRole::Derived);
        self
    }
    pub fn is_driver(&self, name: &str) -> bool {
        matches!(self.roles.get(name), Some(ParamRole::Driver { .. }))
    }
    pub fn is_derived(&self, name: &str) -> bool {
        matches!(self.roles.get(name), Some(ParamRole::Derived))
    }
    pub fn policy(&self, name: &str) -> Option<DriverPolicy> {
        match self.roles.get(name) {
            Some(ParamRole::Driver { policy }) => Some(*policy),
            _ => None,
        }
    }
}

/// Why a driver edit was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverEditError {
    UnknownParam(String),
    /// The parameter is derived; it cannot be set directly.
    IsDerived(String),
    /// The driver is read-only.
    ReadOnly(String),
    /// The driver is locked; edits must use the lock/inversion workflow.
    Locked(String),
}

impl std::fmt::Display for DriverEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownParam(n) => write!(f, "unknown parameter '{n}'"),
            Self::IsDerived(n) => write!(
                f,
                "'{n}' is a derived value; it cannot be overridden directly (re-derive or use the lock/inversion workflow)"
            ),
            Self::ReadOnly(n) => write!(f, "driver '{n}' is read-only"),
            Self::Locked(n) => write!(
                f,
                "driver '{n}' is locked; edit via the lock/inversion workflow"
            ),
        }
    }
}

impl std::error::Error for DriverEditError {}

/// Occurrence-level driver overrides (replaces ADR-025 `OverrideMap`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct OccurrenceDrivers {
    /// driver name -> overriding value
    pub overrides: BTreeMap<String, Value>,
}

impl OccurrenceDrivers {
    /// Set a driver value, enforcing the role/policy invariants. Derived
    /// values and read-only/locked drivers are refused.
    pub fn set_driver(
        &mut self,
        params: &ComponentParams,
        name: &str,
        value: Value,
    ) -> Result<(), DriverEditError> {
        match params.roles.get(name) {
            None => Err(DriverEditError::UnknownParam(name.to_string())),
            Some(ParamRole::Derived) => Err(DriverEditError::IsDerived(name.to_string())),
            Some(ParamRole::Driver { policy }) => match policy {
                DriverPolicy::ReadOnly => Err(DriverEditError::ReadOnly(name.to_string())),
                DriverPolicy::Locked => Err(DriverEditError::Locked(name.to_string())),
                DriverPolicy::Editable => {
                    self.overrides.insert(name.to_string(), value);
                    Ok(())
                }
            },
        }
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.overrides.get(name)
    }
}

/// Deterministic, stable identity for a derived part.
///
/// Pure function of `(occurrence/instance id, script-step identity, declared
/// instance key or semantic label)`. Re-derivation with the same logical inputs
/// yields the same `NodeId`, so updates diff instead of delete/recreate.
pub fn derived_part_id(occurrence: u64, step: &str, instance_key: &str) -> NodeId {
    NodeId::part(occurrence, format!("{step}#{instance_key}"))
}

/// Result of comparing a previous derived-part set to a freshly derived one,
/// keyed by stable id. Stable identity is what makes this a diff and not churn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartDiff {
    pub added: Vec<NodeId>,
    pub removed: Vec<NodeId>,
    pub retained: Vec<NodeId>,
}

/// Diff two derived-part sets by stable id.
pub fn diff_parts(before: &[NodeId], after: &[NodeId]) -> PartDiff {
    let before_set: BTreeSet<&NodeId> = before.iter().collect();
    let after_set: BTreeSet<&NodeId> = after.iter().collect();
    PartDiff {
        added: after
            .iter()
            .filter(|n| !before_set.contains(*n))
            .cloned()
            .collect(),
        removed: before
            .iter()
            .filter(|n| !after_set.contains(*n))
            .cloned()
            .collect(),
        retained: after
            .iter()
            .filter(|n| before_set.contains(*n))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window_params() -> ComponentParams {
        ComponentParams::default()
            .driver("width", DriverPolicy::Editable)
            .driver("height", DriverPolicy::Editable)
            .driver("frame_thickness", DriverPolicy::ReadOnly)
            .derived("sash_width")
    }

    #[test]
    fn set_editable_driver_ok() {
        let p = window_params();
        let mut d = OccurrenceDrivers::default();
        d.set_driver(&p, "width", json!(1500.0)).unwrap();
        assert_eq!(d.get("width"), Some(&json!(1500.0)));
    }

    #[test]
    fn derived_cannot_be_overridden() {
        let p = window_params();
        let mut d = OccurrenceDrivers::default();
        let err = d.set_driver(&p, "sash_width", json!(600.0)).unwrap_err();
        assert!(matches!(err, DriverEditError::IsDerived(_)));
    }

    #[test]
    fn readonly_driver_refused() {
        let p = window_params();
        let mut d = OccurrenceDrivers::default();
        let err = d
            .set_driver(&p, "frame_thickness", json!(50.0))
            .unwrap_err();
        assert!(matches!(err, DriverEditError::ReadOnly(_)));
    }

    #[test]
    fn locked_driver_refused() {
        let p = ComponentParams::default().driver("span", DriverPolicy::Locked);
        let mut d = OccurrenceDrivers::default();
        let err = d.set_driver(&p, "span", json!(9000.0)).unwrap_err();
        assert!(matches!(err, DriverEditError::Locked(_)));
    }

    #[test]
    fn unknown_param_refused() {
        let p = window_params();
        let mut d = OccurrenceDrivers::default();
        assert!(matches!(
            d.set_driver(&p, "nope", json!(1)).unwrap_err(),
            DriverEditError::UnknownParam(_)
        ));
    }

    #[test]
    fn derived_part_id_is_deterministic_and_keyed() {
        let a = derived_part_id(7, "place_stud", "0");
        let b = derived_part_id(7, "place_stud", "0");
        let c = derived_part_id(7, "place_stud", "1");
        let d = derived_part_id(8, "place_stud", "0");
        assert_eq!(a, b, "same inputs => same id (idempotent)");
        assert_ne!(a, c, "different instance key => different id");
        assert_ne!(a, d, "different occurrence => different id");
        assert_eq!(a.component(), Some(7));
    }

    #[test]
    fn rederive_diffs_by_stable_id_not_churn() {
        // Before: a truss with 3 webs. After widening: 4 webs (one added),
        // the first 3 retain their stable ids -> diff, not delete/recreate.
        let before: Vec<NodeId> = (0..3)
            .map(|i| derived_part_id(1, "web", &i.to_string()))
            .collect();
        let after: Vec<NodeId> = (0..4)
            .map(|i| derived_part_id(1, "web", &i.to_string()))
            .collect();
        let diff = diff_parts(&before, &after);
        assert_eq!(diff.retained.len(), 3, "existing webs keep their identity");
        assert_eq!(diff.added.len(), 1, "one new web");
        assert!(diff.removed.is_empty(), "nothing deleted/recreated");
    }

    #[test]
    fn rederive_removes_dropped_parts() {
        let before: Vec<NodeId> = (0..4)
            .map(|i| derived_part_id(1, "web", &i.to_string()))
            .collect();
        let after: Vec<NodeId> = (0..2)
            .map(|i| derived_part_id(1, "web", &i.to_string()))
            .collect();
        let diff = diff_parts(&before, &after);
        assert_eq!(diff.retained.len(), 2);
        assert_eq!(diff.removed.len(), 2);
        assert!(diff.added.is_empty());
    }

    #[test]
    fn occurrence_drivers_serde_round_trip() {
        let mut d = OccurrenceDrivers::default();
        d.overrides.insert("width".into(), json!(1500.0));
        let s = serde_json::to_string(&d).unwrap();
        let d2: OccurrenceDrivers = serde_json::from_str(&s).unwrap();
        assert_eq!(d, d2);
    }
}
