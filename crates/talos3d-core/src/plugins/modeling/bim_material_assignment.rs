//! BIM material assignment (ADR-026 Phase 6d).
//!
//! ADR-026 §4 introduces a typed, BIM-side authored construct for
//! describing the **physical material composition** of an entity:
//!
//! - `Single(MaterialRef)` — a single uniform material.
//! - `LayerSet(MaterialLayerSet)` — an ordered set of layers with
//!   thickness and function codes (the wall / slab / roof
//!   build-up).
//! - `ConstituentSet(MaterialConstituentSet)` — a set of unordered
//!   constituents with proportional fractions (steel-reinforced
//!   concrete, fibre-reinforced gypsum, …).
//!
//! This is **distinct** from the render-pipeline `MaterialAssignment`
//! in `crate::plugins::materials`. That existing type is the binding
//! between an entity and the visual / PBR material registry. The BIM
//! type here is authored construction metadata: layer thickness,
//! layer function (Structural / Insulation / …), ventilation flag,
//! constituent fractions. It does not drive rendering and the
//! geometry pipeline never observes it.
//!
//! Why a separate module?
//!
//! 1. **Hard structural separation** matches the same architectural
//!    enforcement we use for `PropertySetMap` and
//!    `ExchangeIdentityMap`: BIM authoring data lives in its own
//!    Bevy components and registries; the render pipeline never
//!    touches them.
//! 2. The existing `materials::MaterialAssignment` enum is an open
//!    surface used by the render systems and the materials
//!    browser; extending it with BIM-only variants and per-layer
//!    function codes would force changes across many call sites
//!    for no functional benefit on the render side.
//! 3. ADR-026 §4 explicitly says "the `MaterialLayerSet` and
//!    `MaterialConstituentSet` wrapping concepts are new additions
//!    that live at the authored-model layer, not in the render
//!    pipeline."
//!
//! Definitions that want to declare their type-level default BIM
//! material composition register a `BimMaterialAssignment` in the
//! `BimMaterialAssignmentRegistry` resource (keyed by
//! `DefinitionId`); per-occurrence overrides live as a sibling
//! component on the same entity as `OccurrenceIdentity`.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::modeling::definition::DefinitionId;

// ---------------------------------------------------------------------------
// Material reference (link to ADR-015 material registry by id)
// ---------------------------------------------------------------------------

/// Reference to a Talos material entry. Stored as the same opaque
/// string id used by `materials::MaterialRegistry`. Kept as a thin
/// newtype here (rather than re-exporting the existing render-side
/// type) so the BIM module is decoupled from the render plumbing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct BimMaterialRef(pub String);

impl BimMaterialRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BimMaterialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Layer function codes
// ---------------------------------------------------------------------------

/// Functional role of a single layer in a layered build-up.
/// Maps directly to the values ADR-026 §4 calls out and is the
/// vocabulary export packs translate to format-specific codes
/// (e.g. IFC's `IfcLayerSetUsage` `LayerSetDirection` /
/// `LayerSetUsage` axes use these to tag each layer's function).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LayerFunction {
    /// Load-bearing layer (concrete core, structural framing,
    /// CLT panel, …).
    Structural,
    /// Thermal insulation layer.
    Insulation,
    /// Cladding / paint / finish — visible surface treatment.
    Finish,
    /// Membrane (vapour barrier, weather barrier, fire protection
    /// board acting as a barrier rather than insulation).
    Membrane,
    /// Air gap / ventilated cavity.
    Air,
    /// Default / unclassified. Used when the function is not yet
    /// authored. Export packs may reject this for required
    /// profiles.
    #[default]
    Other,
}

// ---------------------------------------------------------------------------
// Single layer + layer set
// ---------------------------------------------------------------------------

/// One layer in a layered build-up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BimMaterialLayer {
    /// Layer material id, referencing the Talos material registry.
    pub material: BimMaterialRef,
    /// Layer thickness in metres. Must be > 0.
    pub thickness_m: f64,
    /// Functional role for export packs and thermal analysis.
    #[serde(default)]
    pub function: LayerFunction,
    /// Whether this layer is a ventilated cavity. Air-cavity layers
    /// without ventilation are treated as still-air for thermal
    /// transmittance calculations; ventilated layers contribute
    /// approximately zero R-value.
    #[serde(default)]
    pub is_ventilated: bool,
    /// Optional human-readable label ("Insulation - mineral wool",
    /// "Gypsum board 12.5mm", …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl BimMaterialLayer {
    /// Build a layer with the given material and thickness, leaving
    /// function = `Other` and is_ventilated = false. Convenience for
    /// the common default-and-then-customize call shape.
    pub fn new(material: BimMaterialRef, thickness_m: f64) -> Self {
        Self {
            material,
            thickness_m,
            function: LayerFunction::Other,
            is_ventilated: false,
            label: None,
        }
    }

    pub fn with_function(mut self, function: LayerFunction) -> Self {
        self.function = function;
        self
    }

    pub fn ventilated(mut self) -> Self {
        self.is_ventilated = true;
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Ordered set of layers describing a physical build-up
/// (wall / slab / roof / cladding stack).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BimMaterialLayerSet {
    /// Ordered list of layers from one face of the build-up to the
    /// other. The orientation (e.g. "interior to exterior" for a
    /// wall) is determined by the host element's evaluator and
    /// captured in the export pack.
    pub layers: Vec<BimMaterialLayer>,
    /// Optional reference to a parameter on the host Definition that
    /// supplies the total thickness. When set, validation can warn
    /// if `sum(layers.thickness_m) != parameter_value`. Stored as
    /// the parameter name (a string), opaque to this module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_thickness_param: Option<String>,
}

impl BimMaterialLayerSet {
    pub fn new(layers: Vec<BimMaterialLayer>) -> Self {
        Self {
            layers,
            total_thickness_param: None,
        }
    }

    pub fn total_thickness_m(&self) -> f64 {
        self.layers.iter().map(|l| l.thickness_m).sum()
    }

    pub fn with_total_thickness_param(mut self, param: impl Into<String>) -> Self {
        self.total_thickness_param = Some(param.into());
        self
    }

    /// Returns true when all layers have a non-default function and
    /// every layer's thickness is strictly positive. Used by
    /// export-completeness checks.
    pub fn is_fully_authored(&self) -> bool {
        !self.layers.is_empty()
            && self
                .layers
                .iter()
                .all(|l| l.thickness_m > 0.0 && !matches!(l.function, LayerFunction::Other))
    }
}

// ---------------------------------------------------------------------------
// Constituents
// ---------------------------------------------------------------------------

/// One constituent in a non-layered composite (e.g. steel rebar in
/// concrete, fibres in gypsum).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BimMaterialConstituent {
    /// Constituent material id.
    pub material: BimMaterialRef,
    /// Volumetric or mass fraction in [0.0, 1.0]. Interpretation
    /// (volume vs mass) is the export pack's responsibility; this
    /// module only enforces the [0, 1] bound.
    pub fraction: f64,
    /// Optional label ("Reinforcement", "Aggregate", …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl BimMaterialConstituent {
    pub fn new(material: BimMaterialRef, fraction: f64) -> Self {
        Self {
            material,
            fraction,
            label: None,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Unordered set of constituents.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BimMaterialConstituentSet {
    pub constituents: Vec<BimMaterialConstituent>,
}

impl BimMaterialConstituentSet {
    pub fn new(constituents: Vec<BimMaterialConstituent>) -> Self {
        Self { constituents }
    }

    pub fn fraction_total(&self) -> f64 {
        self.constituents.iter().map(|c| c.fraction).sum()
    }

    /// Returns true when every constituent's fraction is in [0, 1]
    /// and the total is approximately 1.0 (within `tol`). Used for
    /// export-completeness checks.
    pub fn is_well_formed(&self, tol: f64) -> bool {
        if self.constituents.is_empty() {
            return false;
        }
        if self
            .constituents
            .iter()
            .any(|c| !c.fraction.is_finite() || !(0.0..=1.0).contains(&c.fraction))
        {
            return false;
        }
        (self.fraction_total() - 1.0).abs() <= tol
    }
}

// ---------------------------------------------------------------------------
// Top-level assignment + registry + per-occurrence override
// ---------------------------------------------------------------------------

/// BIM-side material assignment. ADR-026 §4 §"MaterialAssignment".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BimMaterialAssignment {
    Single { material: BimMaterialRef },
    LayerSet(BimMaterialLayerSet),
    ConstituentSet(BimMaterialConstituentSet),
}

impl BimMaterialAssignment {
    /// Build a single-material assignment.
    pub fn single(material: BimMaterialRef) -> Self {
        Self::Single { material }
    }

    /// Build a layered assignment from a layer list.
    pub fn layered(layers: Vec<BimMaterialLayer>) -> Self {
        Self::LayerSet(BimMaterialLayerSet::new(layers))
    }

    /// Build a constituent-based assignment.
    pub fn constituents(constituents: Vec<BimMaterialConstituent>) -> Self {
        Self::ConstituentSet(BimMaterialConstituentSet::new(constituents))
    }
}

/// Bevy resource: type-level (Definition-level) BIM material
/// defaults, keyed by `DefinitionId`. Per ADR-026 §4 the assignment
/// lives on the Definition as the type-level default; per-Occurrence
/// overrides live in `BimMaterialAssignmentOverride` on the
/// Occurrence entity.
#[derive(Resource, Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct BimMaterialAssignmentRegistry {
    pub by_definition: HashMap<DefinitionId, BimMaterialAssignment>,
}

impl BimMaterialAssignmentRegistry {
    pub fn register(
        &mut self,
        definition_id: DefinitionId,
        assignment: BimMaterialAssignment,
    ) -> Option<BimMaterialAssignment> {
        self.by_definition.insert(definition_id, assignment)
    }

    pub fn get(&self, definition_id: &DefinitionId) -> Option<&BimMaterialAssignment> {
        self.by_definition.get(definition_id)
    }
}

/// Bevy component: per-Occurrence override of the Definition-level
/// BIM material assignment. Sibling to `OccurrenceIdentity` so the
/// geometry pipeline never observes it.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BimMaterialAssignmentOverride(pub BimMaterialAssignment);

/// Resolve the effective BIM material assignment for an Occurrence:
/// returns the override if present on the entity, otherwise the
/// Definition-level default from the registry.
pub fn effective_assignment<'a>(
    registry: &'a BimMaterialAssignmentRegistry,
    definition_id: &DefinitionId,
    override_component: Option<&'a BimMaterialAssignmentOverride>,
) -> Option<&'a BimMaterialAssignment> {
    override_component
        .map(|o| &o.0)
        .or_else(|| registry.get(definition_id))
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin: installs `BimMaterialAssignmentRegistry`. The
/// override component is data attached to entities; no resource is
/// needed for it.
pub struct BimMaterialAssignmentPlugin;

impl Plugin for BimMaterialAssignmentPlugin {
    fn build(&self, app: &mut App) {
        if !app
            .world()
            .contains_resource::<BimMaterialAssignmentRegistry>()
        {
            app.init_resource::<BimMaterialAssignmentRegistry>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concrete() -> BimMaterialRef {
        BimMaterialRef::new("mat.concrete.c25_30")
    }

    fn rebar() -> BimMaterialRef {
        BimMaterialRef::new("mat.steel.rebar_b500b")
    }

    fn mineral_wool() -> BimMaterialRef {
        BimMaterialRef::new("mat.insulation.mineral_wool")
    }

    fn gypsum() -> BimMaterialRef {
        BimMaterialRef::new("mat.gypsum.standard_12_5mm")
    }

    #[test]
    fn material_layer_new_uses_other_function_and_unventilated() {
        let layer = BimMaterialLayer::new(concrete(), 0.2);
        assert_eq!(layer.function, LayerFunction::Other);
        assert!(!layer.is_ventilated);
        assert!(layer.label.is_none());
    }

    #[test]
    fn material_layer_builders_set_fields() {
        let layer = BimMaterialLayer::new(mineral_wool(), 0.15)
            .with_function(LayerFunction::Insulation)
            .ventilated()
            .with_label("Mineral wool 150mm");
        assert_eq!(layer.function, LayerFunction::Insulation);
        assert!(layer.is_ventilated);
        assert_eq!(layer.label.as_deref(), Some("Mineral wool 150mm"));
    }

    #[test]
    fn layer_set_total_thickness_is_sum() {
        let set = BimMaterialLayerSet::new(vec![
            BimMaterialLayer::new(gypsum(), 0.0125),
            BimMaterialLayer::new(mineral_wool(), 0.15)
                .with_function(LayerFunction::Insulation),
            BimMaterialLayer::new(gypsum(), 0.0125),
        ]);
        assert!((set.total_thickness_m() - 0.175).abs() < 1e-9);
    }

    #[test]
    fn layer_set_is_fully_authored_requires_function_and_thickness() {
        let unauthored = BimMaterialLayerSet::new(vec![
            BimMaterialLayer::new(gypsum(), 0.0125), // function = Other
        ]);
        assert!(!unauthored.is_fully_authored());

        let authored = BimMaterialLayerSet::new(vec![
            BimMaterialLayer::new(gypsum(), 0.0125).with_function(LayerFunction::Finish),
            BimMaterialLayer::new(mineral_wool(), 0.15)
                .with_function(LayerFunction::Insulation),
        ]);
        assert!(authored.is_fully_authored());

        let zero_thickness = BimMaterialLayerSet::new(vec![
            BimMaterialLayer::new(gypsum(), 0.0).with_function(LayerFunction::Finish),
        ]);
        assert!(!zero_thickness.is_fully_authored());
    }

    #[test]
    fn layer_set_total_thickness_param_round_trip() {
        let set = BimMaterialLayerSet::new(vec![BimMaterialLayer::new(concrete(), 0.2)])
            .with_total_thickness_param("wall_thickness_m");
        assert_eq!(set.total_thickness_param.as_deref(), Some("wall_thickness_m"));
    }

    #[test]
    fn constituent_set_is_well_formed_requires_total_one() {
        let good = BimMaterialConstituentSet::new(vec![
            BimMaterialConstituent::new(concrete(), 0.95),
            BimMaterialConstituent::new(rebar(), 0.05),
        ]);
        assert!(good.is_well_formed(1e-6));

        let bad_total = BimMaterialConstituentSet::new(vec![
            BimMaterialConstituent::new(concrete(), 0.5),
            BimMaterialConstituent::new(rebar(), 0.3),
        ]);
        assert!(!bad_total.is_well_formed(1e-6));
    }

    #[test]
    fn constituent_set_rejects_out_of_bounds_fraction() {
        let bad = BimMaterialConstituentSet::new(vec![
            BimMaterialConstituent::new(concrete(), 1.5),
        ]);
        assert!(!bad.is_well_formed(1e-6));
    }

    #[test]
    fn constituent_set_rejects_empty() {
        let empty = BimMaterialConstituentSet::default();
        assert!(!empty.is_well_formed(1e-6));
    }

    #[test]
    fn material_assignment_round_trips_through_json() {
        for assignment in [
            BimMaterialAssignment::single(concrete()),
            BimMaterialAssignment::layered(vec![
                BimMaterialLayer::new(gypsum(), 0.0125)
                    .with_function(LayerFunction::Finish),
                BimMaterialLayer::new(mineral_wool(), 0.15)
                    .with_function(LayerFunction::Insulation),
                BimMaterialLayer::new(gypsum(), 0.0125)
                    .with_function(LayerFunction::Finish),
            ]),
            BimMaterialAssignment::constituents(vec![
                BimMaterialConstituent::new(concrete(), 0.95),
                BimMaterialConstituent::new(rebar(), 0.05).with_label("Rebar B500B"),
            ]),
        ] {
            let json = serde_json::to_string(&assignment).unwrap();
            let parsed: BimMaterialAssignment = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, assignment);
        }
    }

    #[test]
    fn registry_register_and_get() {
        let mut reg = BimMaterialAssignmentRegistry::default();
        let def = DefinitionId("wall.lf_v1".into());
        let prior = reg.register(def.clone(), BimMaterialAssignment::single(concrete()));
        assert!(prior.is_none());
        assert_eq!(
            reg.get(&def),
            Some(&BimMaterialAssignment::single(concrete()))
        );
    }

    #[test]
    fn effective_assignment_prefers_override_over_registry() {
        let mut reg = BimMaterialAssignmentRegistry::default();
        let def = DefinitionId("wall.lf_v1".into());
        reg.register(def.clone(), BimMaterialAssignment::single(concrete()));
        let override_comp = BimMaterialAssignmentOverride(BimMaterialAssignment::single(gypsum()));
        let effective = effective_assignment(&reg, &def, Some(&override_comp));
        assert_eq!(effective, Some(&BimMaterialAssignment::single(gypsum())));
    }

    #[test]
    fn effective_assignment_falls_back_to_registry() {
        let mut reg = BimMaterialAssignmentRegistry::default();
        let def = DefinitionId("wall.lf_v1".into());
        reg.register(def.clone(), BimMaterialAssignment::single(concrete()));
        let effective = effective_assignment(&reg, &def, None);
        assert_eq!(effective, Some(&BimMaterialAssignment::single(concrete())));
    }

    #[test]
    fn effective_assignment_returns_none_when_neither_present() {
        let reg = BimMaterialAssignmentRegistry::default();
        let def = DefinitionId("never_registered".into());
        let effective = effective_assignment(&reg, &def, None);
        assert!(effective.is_none());
    }

    #[test]
    fn plugin_installs_registry_resource() {
        let mut app = App::new();
        app.add_plugins(BimMaterialAssignmentPlugin);
        assert!(app
            .world()
            .contains_resource::<BimMaterialAssignmentRegistry>());
    }

    #[test]
    fn layer_function_default_is_other() {
        let f: LayerFunction = Default::default();
        assert_eq!(f, LayerFunction::Other);
    }
}
