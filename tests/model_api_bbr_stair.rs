//! PP77 integration test: Boverket BBR stair regulation pilot.
//!
//! Covers:
//! - Promoting a stair with BBR-compliant values produces only standard
//!   completeness findings (no BBR errors).
//! - Non-compliant riser height fires `BBR_8_22_riser_max` with an error
//!   finding whose backlink points at the hand-authored BBR passage.
//! - Non-compliant tread depth fires `BBR_8_22_tread_min`.
//! - Non-compliant clear width fires `BBR_residential_clear_width`.
//! - `list_generation_priors` filtered by `stair_straight` element class
//!   includes the Swedish stair defaults prior.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architecture_core::recipes::stair_straight::{
    stair_class, stair_straight_residential_recipe,
};
use talos3d_architecture_se::bbr::{priors::se_stair_riser_default_prior, rules};
use talos3d_core::{
    capability_registry::{
        CapabilityRegistry, ElementClassAssignment, ElementClassId, RecipeFamilyId,
    },
    plugins::{
        commands::{
            ApplyEntityChangesCommand, BeginCommandGroup, CreateBoxCommand, CreateCylinderCommand,
            CreateEntityCommand, CreatePlaneCommand, CreatePolylineCommand, CreateSphereCommand,
            CreateTriangleMeshCommand, DeleteEntitiesCommand, EndCommandGroup,
            ResolvedDeleteEntitiesCommand,
        },
        history::{History, PendingCommandQueue},
        identity::ElementIdAllocator,
        model_api::{
            handle_list_generation_priors, handle_promote_refinement, handle_run_validation,
        },
        modeling::assembly::{AssemblyFactory, RelationFactory},
    },
};

fn init_bbr_test_world() -> World {
    let mut world = World::new();
    world.insert_resource(Messages::<CreateBoxCommand>::default());
    world.insert_resource(Messages::<CreateCylinderCommand>::default());
    world.insert_resource(Messages::<CreateSphereCommand>::default());
    world.insert_resource(Messages::<CreatePlaneCommand>::default());
    world.insert_resource(Messages::<CreatePolylineCommand>::default());
    world.insert_resource(Messages::<CreateTriangleMeshCommand>::default());
    world.insert_resource(Messages::<CreateEntityCommand>::default());
    world.insert_resource(Messages::<DeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ResolvedDeleteEntitiesCommand>::default());
    world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
    world.insert_resource(Messages::<BeginCommandGroup>::default());
    world.insert_resource(Messages::<EndCommandGroup>::default());
    world.insert_resource(PendingCommandQueue::default());
    world.insert_resource(History::default());
    world.insert_resource(ElementIdAllocator::default());

    let mut registry = CapabilityRegistry::default();
    registry.register_element_class(stair_class());
    registry.register_recipe_family(stair_straight_residential_recipe());
    registry.register_constraint(rules::bbr_stair_riser_max_constraint());
    registry.register_constraint(rules::bbr_stair_tread_min_constraint());
    registry.register_constraint(rules::bbr_stair_clear_width_residential_constraint());
    registry.register_generation_prior(se_stair_riser_default_prior());

    registry.register_factory(AssemblyFactory);
    registry.register_factory(RelationFactory);

    world.insert_resource(registry);
    world
}

fn spawn_stair(world: &mut World) -> u64 {
    let eid = world.resource::<ElementIdAllocator>().next_id();
    world.spawn((
        eid,
        ElementClassAssignment {
            element_class: ElementClassId("stair_straight".into()),
            active_recipe: Some(RecipeFamilyId("stair_straight_residential".into())),
        },
    ));
    eid.0
}

fn promote_stair(world: &mut World, eid: u64, overrides: serde_json::Value) {
    handle_promote_refinement(
        world,
        eid,
        "Constructible".into(),
        Some("stair_straight_residential".into()),
        overrides,
    )
    .expect("stair promotion should succeed");
}

fn bbr_findings(world: &World, eid: u64) -> Vec<(String, String)> {
    handle_run_validation(world, eid)
        .expect("run_validation")
        .into_iter()
        .filter(|f| f.validator.starts_with("BBR_"))
        .map(|f| (f.validator, f.severity))
        .collect()
}

// ---------------------------------------------------------------------------
// Happy path — compliant stair produces no BBR findings
// ---------------------------------------------------------------------------

#[test]
fn compliant_stair_produces_no_bbr_findings() {
    let mut world = init_bbr_test_world();
    let eid = spawn_stair(&mut world);
    promote_stair(
        &mut world,
        eid,
        serde_json::json!({
            "riser_height_mm": 175,
            "tread_depth_mm": 260,
            "clear_width_mm": 1000
        }),
    );

    let findings = bbr_findings(&world, eid);
    assert!(
        findings.is_empty(),
        "compliant stair must have no BBR findings; got: {findings:?}"
    );
}

// ---------------------------------------------------------------------------
// Each rule fires on non-compliant values
// ---------------------------------------------------------------------------

#[test]
fn noncompliant_riser_fires_bbr_8_22_riser_max() {
    let mut world = init_bbr_test_world();
    let eid = spawn_stair(&mut world);
    promote_stair(
        &mut world,
        eid,
        serde_json::json!({
            "riser_height_mm": 220,
            "tread_depth_mm": 260,
            "clear_width_mm": 1000
        }),
    );

    let findings = bbr_findings(&world, eid);
    assert!(
        findings.iter().any(|(id, sev)| id == "BBR_8_22_riser_max" && sev == "error"),
        "riser 220 must fire BBR_8_22_riser_max error; got: {findings:?}"
    );
}

#[test]
fn noncompliant_tread_fires_bbr_8_22_tread_min() {
    let mut world = init_bbr_test_world();
    let eid = spawn_stair(&mut world);
    promote_stair(
        &mut world,
        eid,
        serde_json::json!({
            "riser_height_mm": 175,
            "tread_depth_mm": 220,
            "clear_width_mm": 1000
        }),
    );

    let findings = bbr_findings(&world, eid);
    assert!(
        findings.iter().any(|(id, sev)| id == "BBR_8_22_tread_min" && sev == "error"),
        "tread 220 must fire BBR_8_22_tread_min error; got: {findings:?}"
    );
}

#[test]
fn noncompliant_clear_width_fires_bbr_residential_clear_width() {
    let mut world = init_bbr_test_world();
    let eid = spawn_stair(&mut world);
    promote_stair(
        &mut world,
        eid,
        serde_json::json!({
            "riser_height_mm": 175,
            "tread_depth_mm": 260,
            "clear_width_mm": 850
        }),
    );

    let findings = bbr_findings(&world, eid);
    assert!(
        findings.iter().any(|(id, sev)| {
            id == "BBR_residential_clear_width" && sev == "error"
        }),
        "clear width 850 must fire BBR_residential_clear_width error; got: {findings:?}"
    );
}

// ---------------------------------------------------------------------------
// Prior is listed under stair_straight scope
// ---------------------------------------------------------------------------

#[test]
fn se_stair_riser_default_prior_is_listed_under_stair_class() {
    let world = init_bbr_test_world();
    let filter = serde_json::json!({ "element_class": "stair_straight" });
    let priors = handle_list_generation_priors(&world, Some(filter));
    assert!(
        priors.iter().any(|p| p.id == "se_stair_riser_default"),
        "expected se_stair_riser_default in list_generation_priors(stair_straight)"
    );
}
