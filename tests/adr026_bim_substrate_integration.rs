//! ADR-026 BIM substrate integration test.
//!
//! Demonstrates the seven Phase 6 substrates composing end-to-end
//! on a single authored wall + window + storey scenario:
//!
//! - Phase 6a — `PropertySetSchemaRegistry` carries the wall's
//!   `Pset_WallCommon` schema; the wall Occurrence's
//!   `PropertySetMap` carries values.
//! - Phase 6b — `ExchangeIdentityMap` carries the wall's IFC GUID,
//!   assigned-once.
//! - Phase 6c — `RepresentationDecl` declares Body and Annotation
//!   representations with explicit LOD and update policy.
//! - Phase 6d — `BimMaterialAssignment::LayerSet` describes the
//!   wall's three-layer build-up (gypsum / mineral wool / gypsum).
//! - Phase 6e — `QuantitySet` with `QuantityProvenance` on every
//!   value, including `MaterialQuantity` per layer material, and
//!   the gross/net/deduction-consistency invariants check.
//! - Phase 6f — a window placement plans an opening Occurrence
//!   with `OpeningContext` (host + filling) and a `VoidLink` on
//!   the filling.
//! - Phase 6g — the wall's `SpatialMembership` puts it in a
//!   `storey` `SpatialContainer`, validated against the
//!   single-parent / acyclic / kind-registered invariants.
//!
//! Per ADR-026 §1 the geometry pipeline must NOT observe any of
//! the BIM authoring state. This test does not invoke the
//! evaluation pipeline; it asserts the contracts hold at the data
//! level. The architectural separation is proven by the fact
//! that none of the substrates required modifying core types like
//! `Definition`, `OccurrenceIdentity`, or `Interface`.

use bevy::math::DVec3;
use bevy::prelude::*;

use talos3d_core::plugins::identity::ElementId;
use talos3d_core::plugins::modeling::bim_material_assignment::{
    effective_assignment, BimMaterialAssignment, BimMaterialAssignmentRegistry,
    BimMaterialLayer, BimMaterialLayerSet, BimMaterialRef, LayerFunction,
};
use talos3d_core::plugins::modeling::definition::{
    DefinitionId, LevelOfDetail, RepresentationDecl, RepresentationKind, RepresentationRole,
    UpdatePolicy,
};
use talos3d_core::plugins::modeling::exchange_identity::{
    ExchangeId, ExchangeIdentityMap, ExchangeSystem,
};
use talos3d_core::plugins::modeling::property_sets::{
    set_property_validated, ExportProfile, PropertyDef, PropertySetMap, PropertySetSchema,
    PropertySetSchemaRegistry, PropertyValue, PropertyValueType,
};
use talos3d_core::plugins::modeling::quantity_set::{
    MaterialQuantity, QuantitySet, QuantityValue,
};
use talos3d_core::plugins::modeling::spatial_container::{
    validate_assignment, SpatialContainer, SpatialContainerKind, SpatialContainerKindRegistry,
    SpatialContainmentGraph, SpatialMembership,
};
use talos3d_core::plugins::modeling::void_declaration::{
    plan_void_placement, OpeningContext, VoidDeclaration, VoidDeclarationRegistry, VoidLink,
    VoidPlacement,
};

// -- Definition ids ----------------------------------------------------------

fn wall_def_id() -> DefinitionId {
    DefinitionId("wall.light_frame_v1".into())
}

fn window_def_id() -> DefinitionId {
    DefinitionId("window.double_european_v1".into())
}

// -- Material refs -----------------------------------------------------------

fn gypsum() -> BimMaterialRef {
    BimMaterialRef::new("mat.gypsum.standard_12_5mm")
}

fn mineral_wool() -> BimMaterialRef {
    BimMaterialRef::new("mat.insulation.mineral_wool")
}

// -- Schema setup ------------------------------------------------------------

fn pset_wall_common() -> PropertySetSchema {
    PropertySetSchema::new("Pset_WallCommon")
        .with_property(
            PropertyDef::new("FireRating", PropertyValueType::Text)
                .required_for(ExportProfile::new("IFC4")),
        )
        .with_property(
            PropertyDef::new("LoadBearing", PropertyValueType::Boolean)
                .required_for(ExportProfile::new("IFC4")),
        )
        .with_property(
            PropertyDef::new("ThermalTransmittance", PropertyValueType::Number)
                .with_unit("W/(m²·K)"),
        )
}

// -- The end-to-end test ----------------------------------------------------

#[test]
fn bim_substrate_composes_end_to_end() {
    // ── Phase 6a: register Pset_WallCommon schema for the wall def
    let mut prop_schemas = PropertySetSchemaRegistry::default();
    prop_schemas.register(wall_def_id(), vec![pset_wall_common()]);

    // ── Phase 6d: register the wall's BIM material build-up
    let mut bim_materials = BimMaterialAssignmentRegistry::default();
    let wall_layers = BimMaterialLayerSet::new(vec![
        BimMaterialLayer::new(gypsum(), 0.0125)
            .with_function(LayerFunction::Finish)
            .with_label("Gypsum board (interior face) 12.5mm"),
        BimMaterialLayer::new(mineral_wool(), 0.15)
            .with_function(LayerFunction::Insulation)
            .with_label("Mineral wool 150mm"),
        BimMaterialLayer::new(gypsum(), 0.0125)
            .with_function(LayerFunction::Finish)
            .with_label("Gypsum board (exterior face) 12.5mm"),
    ])
    .with_total_thickness_param("wall_thickness_m");
    bim_materials.register(
        wall_def_id(),
        BimMaterialAssignment::LayerSet(wall_layers.clone()),
    );

    // ── Phase 6f: register the window definition's void declaration
    let mut voids = VoidDeclarationRegistry::default();
    voids.register(
        window_def_id(),
        VoidDeclaration::rectangular("opening_width_m", "opening_height_m")
            .with_placement(VoidPlacement::at(DVec3::new(0.0, 1.0, 0.0))),
    );

    // ── Phase 6g: register the storey kind
    let mut spatial_kinds = SpatialContainerKindRegistry::default();
    spatial_kinds.register(SpatialContainerKind::new("storey"));

    // ── Phase 6c: declare the wall's representations (data only —
    //              the evaluator pipeline isn't invoked in this
    //              test, but RepresentationDecl is what an export
    //              pack would inspect to pick the right output).
    let body_decl = RepresentationDecl::new_detailed(
        RepresentationKind::Body,
        RepresentationRole::PrimaryGeometry,
        LevelOfDetail::Detailed,
        UpdatePolicy::Always,
    );
    let annotation_decl = RepresentationDecl::new_detailed(
        RepresentationKind::Annotation,
        RepresentationRole::Annotation,
        LevelOfDetail::Schematic,
        UpdatePolicy::OnDemand,
    );
    assert_eq!(body_decl.effective_lod(), LevelOfDetail::Detailed);
    assert_eq!(
        annotation_decl.effective_update_policy(),
        UpdatePolicy::OnDemand
    );

    // ── Phase 6a (write): per-occurrence property-set values
    let mut wall_props = PropertySetMap::default();
    set_property_validated(
        &mut wall_props,
        &prop_schemas,
        &wall_def_id(),
        "Pset_WallCommon",
        "FireRating",
        PropertyValue::Text("REI60".into()),
    )
    .expect("FireRating writes");
    set_property_validated(
        &mut wall_props,
        &prop_schemas,
        &wall_def_id(),
        "Pset_WallCommon",
        "LoadBearing",
        PropertyValue::Boolean(true),
    )
    .expect("LoadBearing writes");
    set_property_validated(
        &mut wall_props,
        &prop_schemas,
        &wall_def_id(),
        "Pset_WallCommon",
        "ThermalTransmittance",
        PropertyValue::Number(0.18),
    )
    .expect("U-value writes");

    // Schema rejection: type mismatch must not write through.
    let err = set_property_validated(
        &mut wall_props,
        &prop_schemas,
        &wall_def_id(),
        "Pset_WallCommon",
        "FireRating",
        PropertyValue::Number(60.0),
    )
    .unwrap_err();
    assert!(err.contains("type mismatch"));

    // Profile completeness: IFC4 must have nothing missing now.
    let missing = wall_props.missing_required_for_profile(
        prop_schemas.schemas_for(&wall_def_id()),
        &ExportProfile::new("IFC4"),
    );
    assert!(missing.is_empty(), "missing required IFC4 properties: {missing:?}");

    // ── Phase 6b: assign and lock the wall's IFC GUID
    let mut wall_exchange = ExchangeIdentityMap::empty();
    wall_exchange
        .assign_if_absent(
            ExchangeSystem::Ifc,
            ExchangeId::new("0Lh3Y2nzz3wuRfV4z4xRGn"),
        )
        .expect("first IFC GUID assignment succeeds");
    // Re-assigning must be refused: the ADR-026 §2 "never
    // regenerate" rule.
    let refused = wall_exchange.assign_if_absent(
        ExchangeSystem::Ifc,
        ExchangeId::new("different-guid"),
    );
    assert!(refused.is_err(), "second assignment must be refused");
    assert_eq!(
        wall_exchange.get(&ExchangeSystem::Ifc),
        Some(&ExchangeId::new("0Lh3Y2nzz3wuRfV4z4xRGn"))
    );

    // ── Phase 6e: build a quantity set with provenance + per-material entries
    let mut qs = QuantitySet::empty();
    qs.length_m = Some(QuantityValue::from_parameter(4.0, "wall.length_m"));
    qs.area_gross_m2 = Some(QuantityValue::from_evaluator(11.2, "wall.gross_area"));
    qs.opening_area_deducted_m2 =
        Some(QuantityValue::from_evaluator(1.92, "wall.opening_area"));
    qs.area_net_m2 = Some(QuantityValue::from_evaluator(9.28, "wall.net_area"));
    qs.volume_gross_m3 = Some(QuantityValue::from_evaluator(1.96, "wall.gross_vol"));
    qs.volume_net_m3 = Some(QuantityValue::from_evaluator(1.62, "wall.net_vol"));

    // Per-layer material quantities.
    let mut gypsum_q = MaterialQuantity::new(gypsum());
    gypsum_q.volume_m3 = Some(QuantityValue::from_evaluator(0.232, "wall.gypsum_vol"));
    gypsum_q.area_m2 = Some(QuantityValue::from_evaluator(18.56, "wall.gypsum_area"));
    qs.upsert_material_quantity(gypsum_q);

    let mut wool_q = MaterialQuantity::new(mineral_wool());
    wool_q.volume_m3 = Some(QuantityValue::from_evaluator(1.392, "wall.wool_vol"));
    wool_q.mass_kg = Some(QuantityValue::from_evaluator(48.7, "wall.wool_mass"));
    qs.upsert_material_quantity(wool_q);

    // Quantity invariants per ADR-026 §3.
    assert!(qs.net_le_gross_violations().is_empty());
    assert!(qs.area_deduction_consistent(1e-6));
    assert!(qs.all_grounded(), "no MeshApproximation values allowed");
    assert_eq!(qs.material_quantities.len(), 2);

    // ── Phase 6f: place a window (filling) into the wall (host).
    //              The void substrate produces an opening Occurrence
    //              ElementId and the back-link components.
    let host_eid = ElementId(100); // wall
    let filling_eid = ElementId(200); // window
    let opening_eid = ElementId(201); // newly-minted opening
    let placement = plan_void_placement(
        &voids,
        &window_def_id(),
        host_eid,
        filling_eid,
        opening_eid,
    )
    .expect("void placement plans cleanly");
    assert_eq!(placement.opening_element, opening_eid);
    assert_eq!(placement.filling_link, VoidLink { opening: opening_eid });
    assert_eq!(
        placement.opening_context,
        OpeningContext {
            host: host_eid,
            filling: Some(filling_eid),
        }
    );

    // ── Phase 6g: put the wall in a storey, then attempt invalid
    //              spatial moves to verify the invariants reject them.
    let storey_eid = ElementId(10);
    let storey_kind = SpatialContainerKind::new("storey");
    let _storey_marker = SpatialContainer::new("storey");
    let mut graph = SpatialContainmentGraph::new();
    // First valid assignment: wall in storey.
    validate_assignment(&graph, &spatial_kinds, &storey_kind, host_eid, storey_eid)
        .expect("wall assigns to storey");
    let _wall_membership = SpatialMembership::in_container(storey_eid);
    // Reflect the assignment in the graph for downstream checks.
    graph = graph.with_edge(host_eid, storey_eid);

    // Second-parent rejection (single-parent invariant).
    let other_storey = ElementId(11);
    let err = validate_assignment(
        &graph,
        &spatial_kinds,
        &storey_kind,
        host_eid,
        other_storey,
    )
    .unwrap_err();
    let display = format!("{err}");
    assert!(display.contains("already assigned"));

    // Cycle rejection: trying to put the storey inside the wall
    // would close a cycle.
    let cycle_err = validate_assignment(
        &graph,
        &spatial_kinds,
        &storey_kind,
        storey_eid,
        host_eid,
    )
    .unwrap_err();
    let display = format!("{cycle_err}");
    assert!(display.contains("cycle"));

    // Unregistered-kind rejection.
    let unreg_err = validate_assignment(
        &graph,
        &spatial_kinds,
        &SpatialContainerKind::new("not_in_registry"),
        ElementId(999),
        storey_eid,
    )
    .unwrap_err();
    let display = format!("{unreg_err}");
    assert!(display.contains("not registered"));

    // ── Cross-phase resolution: the wall Occurrence's effective
    //              BIM material assignment falls back to the
    //              Definition-level default since we didn't add an
    //              override on this entity.
    let effective = effective_assignment(&bim_materials, &wall_def_id(), None)
        .expect("wall has a registered BIM material assignment");
    match effective {
        BimMaterialAssignment::LayerSet(set) => {
            assert!((set.total_thickness_m() - 0.175).abs() < 1e-9);
            assert!(set.is_fully_authored());
        }
        _ => panic!("expected LayerSet"),
    }
}

#[test]
fn property_set_changes_do_not_imply_geometry_changes() {
    // ADR-026 §1's hard invariant: changing PropertySetMap must
    // never imply a mesh-cache invalidation. The architectural
    // enforcement is structural (PropertySetMap is a separate
    // component, not a field inside OccurrenceIdentity); this
    // test makes the invariant explicit at the API level by
    // confirming PropertySetMap mutations do not touch any
    // geometry-related types.

    let mut prop_schemas = PropertySetSchemaRegistry::default();
    prop_schemas.register(wall_def_id(), vec![pset_wall_common()]);

    let mut props = PropertySetMap::default();
    set_property_validated(
        &mut props,
        &prop_schemas,
        &wall_def_id(),
        "Pset_WallCommon",
        "FireRating",
        PropertyValue::Text("EI30".into()),
    )
    .unwrap();

    // Updating a property must succeed and return the prior value;
    // the act of updating does not produce or require any
    // mesh-related side effects.
    let prior = set_property_validated(
        &mut props,
        &prop_schemas,
        &wall_def_id(),
        "Pset_WallCommon",
        "FireRating",
        PropertyValue::Text("REI60".into()),
    )
    .unwrap();
    assert_eq!(prior, Some(PropertyValue::Text("EI30".into())));

    // Removing a property updates the map without touching
    // anything else.
    let removed = props.remove("Pset_WallCommon", "FireRating").unwrap();
    assert_eq!(removed, PropertyValue::Text("REI60".into()));
}

#[test]
fn quantity_set_rejects_mesh_only_provenance_in_grounded_mode() {
    // The "mesh measurement is rejected as a substitute for
    // authored quantities" rule from ADR-026 §3 surfaces as
    // QuantitySet::all_grounded() returning false.
    let mut qs = QuantitySet::empty();
    qs.area_gross_m2 = Some(QuantityValue::from_evaluator(10.0, "evaluator_node"));
    qs.volume_gross_m3 = Some(QuantityValue::from_mesh_approximation(2.5));
    assert!(!qs.all_grounded());

    // Replacing the mesh-approximated value with an evaluator
    // value flips the gating.
    qs.volume_gross_m3 = Some(QuantityValue::from_evaluator(2.5, "wall.vol"));
    assert!(qs.all_grounded());
}
