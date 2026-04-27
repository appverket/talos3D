//! BIM quantity set (ADR-026 Phase 6e).
//!
//! Per ADR-026 §3 quantity outputs are a first-class typed output
//! contract produced by the Evaluator alongside geometry
//! representations. Mesh-volume measurement is **explicitly
//! rejected** as a substitute for authored quantity outputs:
//! gross-vs-net distinctions, opening-area deduction, and
//! material-specific quantities require Evaluator-level knowledge
//! that mesh measurement cannot recover.
//!
//! Each quantity value carries [`QuantityProvenance`] so AI agents
//! and export pipelines can trace it back to the authored
//! parameter, evaluator node, or imported source that produced it.
//!
//! `QuantitySet` is a Bevy component attached to an Occurrence
//! entity by the Evaluator. It is independent of the geometry
//! pipeline (`NeedsEvaluation` → `NeedsMesh`); systems consume
//! `QuantitySet` for export, schedules, and cost workflows but the
//! mesh pipeline does not depend on it.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::modeling::bim_material_assignment::BimMaterialRef;

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// Source of a single quantity value. Lets AI agents and export
/// pipelines explain *why* a number is what it is — pointing back
/// to the authored parameter, evaluator node, imported file, or
/// fallback approximation that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuantityProvenance {
    /// Computed from the named authored parameter (e.g. the wall's
    /// `length_m` parameter directly drives the wall's
    /// `length` quantity).
    AuthoredParameter { parameter: String },
    /// Computed by an Evaluator node identified by an opaque
    /// label (the recipe / family that owns the computation).
    EvaluatorNode { node: String },
    /// Carried verbatim from an imported file (e.g. an IFC import
    /// brought this quantity along; the value is foreign-authored).
    Imported { source: String },
    /// Approximated by mesh measurement. **Discouraged**: mesh
    /// measurement loses the gross/net distinction and the opening-
    /// area deduction. Used only as a fallback for imported
    /// geometry without a richer Evaluator. Export pipelines may
    /// flag values with this provenance as approximate.
    MeshApproximation,
    /// Set by the user manually overriding the Evaluator output.
    UserOverride { rationale: Option<String> },
}

impl QuantityProvenance {
    /// `true` when this provenance type is acceptable for a
    /// promotion-critical quantity claim. Mesh approximations are
    /// rejected; authored, evaluator, imported, and user values
    /// are all acceptable.
    pub fn is_grounded(&self) -> bool {
        !matches!(self, Self::MeshApproximation)
    }
}

/// Quantity value paired with its provenance. Quantities in the
/// [`QuantitySet`] are stored as `Option<QuantityValue>` rather
/// than bare numbers so an unset quantity is distinguishable from
/// a quantity that the Evaluator computed and set to zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuantityValue<T> {
    pub value: T,
    pub provenance: QuantityProvenance,
}

impl<T> QuantityValue<T> {
    pub fn new(value: T, provenance: QuantityProvenance) -> Self {
        Self { value, provenance }
    }

    pub fn from_parameter(value: T, parameter: impl Into<String>) -> Self {
        Self {
            value,
            provenance: QuantityProvenance::AuthoredParameter {
                parameter: parameter.into(),
            },
        }
    }

    pub fn from_evaluator(value: T, node: impl Into<String>) -> Self {
        Self {
            value,
            provenance: QuantityProvenance::EvaluatorNode {
                node: node.into(),
            },
        }
    }

    pub fn from_mesh_approximation(value: T) -> Self {
        Self {
            value,
            provenance: QuantityProvenance::MeshApproximation,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-material quantities
// ---------------------------------------------------------------------------

/// A material-specific quantity entry. Surfaces in BIM exports as
/// `IfcQuantityVolume` / `IfcQuantityArea` per material, e.g. how
/// much insulation a wall contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialQuantity {
    pub material: BimMaterialRef,
    /// Volume of this material in the entity, in cubic metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_m3: Option<QuantityValue<f64>>,
    /// Area of this material in the entity, in square metres.
    /// Used for sheet materials whose volume is the area times
    /// a thickness (e.g. cladding boards).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_m2: Option<QuantityValue<f64>>,
    /// Length of this material, in metres. Used for linear
    /// materials (rebar, pipes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_m: Option<QuantityValue<f64>>,
    /// Mass in kilograms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mass_kg: Option<QuantityValue<f64>>,
    /// Number of pieces of this material (rebar count, fastener
    /// count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<QuantityValue<u32>>,
}

impl MaterialQuantity {
    pub fn new(material: BimMaterialRef) -> Self {
        Self {
            material,
            volume_m3: None,
            area_m2: None,
            length_m: None,
            mass_kg: None,
            count: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Quantity set component
// ---------------------------------------------------------------------------

/// Bevy component carrying the typed quantity outputs of an
/// Occurrence's Evaluator. ADR-026 §3.
///
/// All numeric quantities are in SI units: metres, square metres,
/// cubic metres, kilograms, integer counts. Export packs translate
/// to format-specific units (millimetres in IFC, etc.).
#[derive(Component, Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuantitySet {
    /// Gross area in m² (face area before opening deductions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_gross_m2: Option<QuantityValue<f64>>,
    /// Net area in m² (after opening deductions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_net_m2: Option<QuantityValue<f64>>,
    /// Gross volume in m³ (before opening / void deductions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_gross_m3: Option<QuantityValue<f64>>,
    /// Net volume in m³ (after opening / void deductions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_net_m3: Option<QuantityValue<f64>>,
    /// Length in metres (linear elements: walls, beams, columns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_m: Option<QuantityValue<f64>>,
    /// Discrete count (assemblies, instances).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<QuantityValue<u32>>,
    /// Total opening area deducted from gross to produce net, m².
    /// Surfaces separately so export packs can audit the deduction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_area_deducted_m2: Option<QuantityValue<f64>>,
    /// Per-material quantities. Used for thermal / cost / quantity-
    /// take-off workflows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub material_quantities: Vec<MaterialQuantity>,
}

impl QuantitySet {
    /// Construct an empty quantity set; the Evaluator fills in
    /// fields as it computes them.
    pub fn empty() -> Self {
        Self::default()
    }

    /// True when none of the quantity fields has been set.
    pub fn is_empty(&self) -> bool {
        self.area_gross_m2.is_none()
            && self.area_net_m2.is_none()
            && self.volume_gross_m3.is_none()
            && self.volume_net_m3.is_none()
            && self.length_m.is_none()
            && self.count.is_none()
            && self.opening_area_deducted_m2.is_none()
            && self.material_quantities.is_empty()
    }

    /// Look up the per-material quantity entry for a given
    /// material, if present.
    pub fn material_quantity(&self, material: &BimMaterialRef) -> Option<&MaterialQuantity> {
        self.material_quantities
            .iter()
            .find(|m| &m.material == material)
    }

    /// Insert or replace the per-material quantity entry. Returns
    /// the prior entry if any.
    pub fn upsert_material_quantity(
        &mut self,
        entry: MaterialQuantity,
    ) -> Option<MaterialQuantity> {
        let mat = entry.material.clone();
        if let Some(idx) = self
            .material_quantities
            .iter()
            .position(|m| m.material == mat)
        {
            let prior = std::mem::replace(&mut self.material_quantities[idx], entry);
            Some(prior)
        } else {
            self.material_quantities.push(entry);
            None
        }
    }

    /// Sanity check: net values, when present alongside gross,
    /// must be `<= gross`. Returns the list of fields that violate
    /// the invariant. Used by export-readiness checks.
    pub fn net_le_gross_violations(&self) -> Vec<&'static str> {
        let mut bad = Vec::new();
        if let (Some(g), Some(n)) = (&self.area_gross_m2, &self.area_net_m2) {
            if n.value > g.value + 1e-9 {
                bad.push("area_net_m2 > area_gross_m2");
            }
        }
        if let (Some(g), Some(n)) = (&self.volume_gross_m3, &self.volume_net_m3) {
            if n.value > g.value + 1e-9 {
                bad.push("volume_net_m3 > volume_gross_m3");
            }
        }
        bad
    }

    /// Sanity check: net values must always equal gross minus
    /// `opening_area_deducted` (within tolerance) when all three
    /// area values are present. Returns true when consistent.
    pub fn area_deduction_consistent(&self, tol: f64) -> bool {
        match (
            &self.area_gross_m2,
            &self.area_net_m2,
            &self.opening_area_deducted_m2,
        ) {
            (Some(g), Some(n), Some(d)) => (g.value - d.value - n.value).abs() <= tol,
            // If any of the three is absent, treat the relationship
            // as not-checkable rather than violated.
            _ => true,
        }
    }

    /// Returns the list of `(field_name, &QuantityProvenance)` for
    /// every set quantity. Used by AI inspection and export-pack
    /// auditing.
    pub fn provenances(&self) -> Vec<(&'static str, &QuantityProvenance)> {
        let mut out: Vec<(&'static str, &QuantityProvenance)> = Vec::new();
        if let Some(v) = &self.area_gross_m2 {
            out.push(("area_gross_m2", &v.provenance));
        }
        if let Some(v) = &self.area_net_m2 {
            out.push(("area_net_m2", &v.provenance));
        }
        if let Some(v) = &self.volume_gross_m3 {
            out.push(("volume_gross_m3", &v.provenance));
        }
        if let Some(v) = &self.volume_net_m3 {
            out.push(("volume_net_m3", &v.provenance));
        }
        if let Some(v) = &self.length_m {
            out.push(("length_m", &v.provenance));
        }
        if let Some(v) = &self.count {
            out.push(("count", &v.provenance));
        }
        if let Some(v) = &self.opening_area_deducted_m2 {
            out.push(("opening_area_deducted_m2", &v.provenance));
        }
        out
    }

    /// Returns true when no set quantity uses
    /// `QuantityProvenance::MeshApproximation`. Discouraging mesh
    /// approximation matches ADR-026 §3 ("relying on mesh volume
    /// measurement as a substitute is explicitly rejected").
    pub fn all_grounded(&self) -> bool {
        self.provenances().iter().all(|(_, p)| p.is_grounded())
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// No-op plugin reserved for symmetry with the other Phase 6
/// substrates. `QuantitySet` is a per-entity component; no resources
/// or systems are needed in core today. The Evaluator pipeline
/// (`NeedsEvaluation` → quantity computation → `QuantitySet` insert)
/// lives in domain-specific recipes.
pub struct QuantitySetPlugin;

impl Plugin for QuantitySetPlugin {
    fn build(&self, _app: &mut App) {
        // Intentionally empty.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rebar() -> BimMaterialRef {
        BimMaterialRef::new("mat.steel.rebar_b500b")
    }

    fn concrete() -> BimMaterialRef {
        BimMaterialRef::new("mat.concrete.c25_30")
    }

    #[test]
    fn empty_set_reports_empty() {
        let qs = QuantitySet::empty();
        assert!(qs.is_empty());
        assert!(qs.provenances().is_empty());
        assert!(qs.all_grounded()); // vacuously true
    }

    #[test]
    fn provenance_is_grounded_excludes_mesh_approximation() {
        assert!(QuantityProvenance::AuthoredParameter {
            parameter: "length_m".into()
        }
        .is_grounded());
        assert!(QuantityProvenance::EvaluatorNode {
            node: "wall_evaluator".into()
        }
        .is_grounded());
        assert!(QuantityProvenance::Imported {
            source: "import-2026-04-01".into()
        }
        .is_grounded());
        assert!(QuantityProvenance::UserOverride { rationale: None }.is_grounded());
        assert!(!QuantityProvenance::MeshApproximation.is_grounded());
    }

    #[test]
    fn quantity_value_constructors_capture_provenance() {
        let v = QuantityValue::from_parameter(2.5, "length_m");
        match v.provenance {
            QuantityProvenance::AuthoredParameter { parameter } => {
                assert_eq!(parameter, "length_m");
            }
            _ => panic!("expected AuthoredParameter"),
        }
        let m = QuantityValue::from_mesh_approximation(0.42);
        assert!(matches!(
            m.provenance,
            QuantityProvenance::MeshApproximation
        ));
    }

    #[test]
    fn upsert_material_quantity_replaces_existing() {
        let mut qs = QuantitySet::empty();
        let mut q1 = MaterialQuantity::new(concrete());
        q1.volume_m3 = Some(QuantityValue::from_parameter(1.0, "wall_volume"));
        qs.upsert_material_quantity(q1.clone());

        let mut q2 = MaterialQuantity::new(concrete());
        q2.volume_m3 = Some(QuantityValue::from_parameter(2.0, "wall_volume"));
        let prior = qs.upsert_material_quantity(q2.clone()).unwrap();
        assert_eq!(prior.volume_m3.as_ref().unwrap().value, 1.0);
        assert_eq!(qs.material_quantities.len(), 1);
        assert_eq!(
            qs.material_quantities[0].volume_m3.as_ref().unwrap().value,
            2.0
        );
    }

    #[test]
    fn upsert_material_quantity_appends_new() {
        let mut qs = QuantitySet::empty();
        qs.upsert_material_quantity(MaterialQuantity::new(concrete()));
        qs.upsert_material_quantity(MaterialQuantity::new(rebar()));
        assert_eq!(qs.material_quantities.len(), 2);
    }

    #[test]
    fn material_quantity_lookup_finds_match() {
        let mut qs = QuantitySet::empty();
        qs.upsert_material_quantity(MaterialQuantity::new(concrete()));
        assert!(qs.material_quantity(&concrete()).is_some());
        assert!(qs.material_quantity(&rebar()).is_none());
    }

    #[test]
    fn net_le_gross_violations_flags_inverted_areas() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_evaluator(10.0, "wall.gross_area"));
        qs.area_net_m2 = Some(QuantityValue::from_evaluator(12.0, "wall.net_area"));
        let v = qs.net_le_gross_violations();
        assert_eq!(v, vec!["area_net_m2 > area_gross_m2"]);
    }

    #[test]
    fn net_le_gross_violations_empty_for_correct_values() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_evaluator(10.0, "g"));
        qs.area_net_m2 = Some(QuantityValue::from_evaluator(8.5, "n"));
        qs.volume_gross_m3 = Some(QuantityValue::from_evaluator(2.0, "g"));
        qs.volume_net_m3 = Some(QuantityValue::from_evaluator(1.7, "n"));
        assert!(qs.net_le_gross_violations().is_empty());
    }

    #[test]
    fn area_deduction_consistent_within_tolerance() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_evaluator(10.0, "g"));
        qs.area_net_m2 = Some(QuantityValue::from_evaluator(8.5, "n"));
        qs.opening_area_deducted_m2 = Some(QuantityValue::from_evaluator(1.5, "d"));
        assert!(qs.area_deduction_consistent(1e-9));
    }

    #[test]
    fn area_deduction_consistent_rejects_inconsistent() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_evaluator(10.0, "g"));
        qs.area_net_m2 = Some(QuantityValue::from_evaluator(8.0, "n"));
        qs.opening_area_deducted_m2 = Some(QuantityValue::from_evaluator(1.5, "d"));
        // 10 - 1.5 - 8 = 0.5, not consistent.
        assert!(!qs.area_deduction_consistent(1e-9));
    }

    #[test]
    fn area_deduction_consistent_returns_true_when_partial() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_evaluator(10.0, "g"));
        // No net or deduction set → not checkable, treat as ok.
        assert!(qs.area_deduction_consistent(1e-9));
    }

    #[test]
    fn provenances_lists_all_set_fields() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_evaluator(10.0, "g"));
        qs.length_m = Some(QuantityValue::from_parameter(2.5, "length"));
        let provenances = qs.provenances();
        assert_eq!(provenances.len(), 2);
        let names: Vec<&str> = provenances.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"area_gross_m2"));
        assert!(names.contains(&"length_m"));
    }

    #[test]
    fn all_grounded_false_when_mesh_approximation_present() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_mesh_approximation(10.0));
        assert!(!qs.all_grounded());
    }

    #[test]
    fn all_grounded_true_for_authored_only() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_parameter(10.0, "g"));
        qs.length_m = Some(QuantityValue::from_evaluator(2.5, "n"));
        assert!(qs.all_grounded());
    }

    #[test]
    fn quantity_set_round_trips_through_json() {
        let mut qs = QuantitySet::empty();
        qs.area_gross_m2 = Some(QuantityValue::from_parameter(10.0, "wall.gross_area"));
        qs.area_net_m2 = Some(QuantityValue::from_evaluator(8.5, "wall.net_area"));
        qs.opening_area_deducted_m2 =
            Some(QuantityValue::from_evaluator(1.5, "wall.opening_area"));
        let mut concrete_q = MaterialQuantity::new(concrete());
        concrete_q.volume_m3 = Some(QuantityValue::from_parameter(0.6, "wall.concrete_vol"));
        qs.upsert_material_quantity(concrete_q);

        let json = serde_json::to_string(&qs).unwrap();
        let parsed: QuantitySet = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, qs);
    }

    #[test]
    fn material_quantity_round_trips_through_json() {
        let mut q = MaterialQuantity::new(rebar());
        q.mass_kg = Some(QuantityValue::from_evaluator(120.5, "rebar.mass"));
        q.length_m = Some(QuantityValue::from_evaluator(48.0, "rebar.length"));
        q.count = Some(QuantityValue::from_parameter(24, "rebar_count"));
        let json = serde_json::to_string(&q).unwrap();
        let parsed: MaterialQuantity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, q);
    }

    #[test]
    fn plugin_can_be_added_without_panic() {
        let mut app = App::new();
        app.add_plugins(QuantitySetPlugin);
        app.update();
    }
}
