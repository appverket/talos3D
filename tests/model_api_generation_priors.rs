//! PP76 integration test: `GenerationPriorDescriptor` registration and
//! `select_recipe` slope ranking via the prior machinery.
//!
//! Covers:
//! - `handle_list_generation_priors(world, None)` returns both terrain priors.
//! - `handle_select_recipe(world, "foundation_system", {terrain_slope_pct: 2.0})`
//!   returns `slab_on_grade` ranked first (prior mechanism, not hardcoded stub).
//! - `handle_select_recipe(world, "foundation_system", {terrain_slope_pct: 18.0})`
//!   returns `pier_foundation` ranked first.
//! - PP72 regression: no slope context → all recipes at weight 1.0.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architecture_core::{
    priors::terrain::{terrain_slope_pier_prior, terrain_slope_slab_prior},
    recipes::{
        foundation_pier::pier_foundation_recipe,
        foundation_slab_on_grade::{foundation_system_class, slab_on_grade_recipe},
    },
};
use talos3d_core::{
    capability_registry::CapabilityRegistry,
    plugins::model_api::{handle_list_generation_priors, handle_select_recipe},
};

// ---------------------------------------------------------------------------
// Test world setup
// ---------------------------------------------------------------------------

fn init_priors_test_world() -> World {
    let mut world = World::new();

    let mut registry = CapabilityRegistry::default();

    // Element classes and recipe families for foundation_system.
    registry.register_element_class(foundation_system_class());
    registry.register_recipe_family(slab_on_grade_recipe());
    registry.register_recipe_family(pier_foundation_recipe());

    // PP76 generation priors.
    registry.register_generation_prior(terrain_slope_slab_prior());
    registry.register_generation_prior(terrain_slope_pier_prior());

    world.insert_resource(registry);
    world
}

// ---------------------------------------------------------------------------
// list_generation_priors
// ---------------------------------------------------------------------------

#[test]
fn list_generation_priors_returns_both_terrain_priors() {
    let world = init_priors_test_world();
    let priors = handle_list_generation_priors(&world, None);

    assert_eq!(
        priors.len(),
        2,
        "expected 2 terrain priors; got: {:?}",
        priors.iter().map(|p| &p.id).collect::<Vec<_>>()
    );

    let ids: Vec<&str> = priors.iter().map(|p| p.id.as_str()).collect();
    assert!(
        ids.contains(&"terrain_slope_slab_on_grade"),
        "expected terrain_slope_slab_on_grade in {ids:?}"
    );
    assert!(
        ids.contains(&"terrain_slope_pier_foundation"),
        "expected terrain_slope_pier_foundation in {ids:?}"
    );
}

#[test]
fn list_generation_priors_filter_by_element_class() {
    let world = init_priors_test_world();

    let filtered = handle_list_generation_priors(
        &world,
        Some(serde_json::json!({"element_class": "foundation_system"})),
    );
    assert_eq!(
        filtered.len(),
        2,
        "both priors are for foundation_system; got: {:?}",
        filtered.iter().map(|p| &p.id).collect::<Vec<_>>()
    );

    let empty = handle_list_generation_priors(
        &world,
        Some(serde_json::json!({"element_class": "wall_assembly"})),
    );
    assert!(
        empty.is_empty(),
        "no priors for wall_assembly; got: {empty:?}"
    );
}

#[test]
fn list_generation_priors_license_and_version() {
    let world = init_priors_test_world();
    let priors = handle_list_generation_priors(&world, None);

    for p in &priors {
        assert_eq!(p.license, "cc0", "prior {} should be cc0", p.id);
        assert_eq!(
            p.source_version, "2026-Q2",
            "prior {} source_version mismatch",
            p.id
        );
    }
}

// ---------------------------------------------------------------------------
// select_recipe via prior machinery (PP76)
// ---------------------------------------------------------------------------

#[test]
fn select_recipe_flat_terrain_ranks_slab_first_via_prior() {
    let world = init_priors_test_world();
    let ranking = handle_select_recipe(
        &world,
        "foundation_system".into(),
        serde_json::json!({"terrain_slope_pct": 2.0}),
    )
    .expect("select_recipe should succeed");

    assert!(!ranking.is_empty(), "must return at least one recipe");
    assert_eq!(
        ranking[0].id,
        "slab_on_grade",
        "flat terrain (slope 2%) must rank slab_on_grade first; got: {:?}",
        ranking.iter().map(|r| (&r.id, r.weight)).collect::<Vec<_>>()
    );
    assert!(
        ranking[0].weight > ranking[1].weight,
        "slab weight ({}) must exceed pier weight ({})",
        ranking[0].weight,
        ranking[1].weight
    );
}

#[test]
fn select_recipe_steep_terrain_ranks_pier_first_via_prior() {
    let world = init_priors_test_world();
    let ranking = handle_select_recipe(
        &world,
        "foundation_system".into(),
        serde_json::json!({"terrain_slope_pct": 18.0}),
    )
    .expect("select_recipe should succeed");

    assert!(!ranking.is_empty(), "must return at least one recipe");
    assert_eq!(
        ranking[0].id,
        "pier_foundation",
        "steep terrain (slope 18%) must rank pier_foundation first; got: {:?}",
        ranking.iter().map(|r| (&r.id, r.weight)).collect::<Vec<_>>()
    );
    assert!(
        ranking[0].weight > ranking[1].weight,
        "pier weight ({}) must exceed slab weight ({})",
        ranking[0].weight,
        ranking[1].weight
    );
}

/// PP72 regression: without slope context all weights stay at 1.0 (neutral).
#[test]
fn select_recipe_no_slope_all_weight_one_via_prior() {
    let world = init_priors_test_world();
    let ranking = handle_select_recipe(
        &world,
        "foundation_system".into(),
        serde_json::json!({}),
    )
    .expect("select_recipe should succeed");

    assert_eq!(ranking.len(), 2, "both foundation recipes should be returned");
    for entry in &ranking {
        assert!(
            (entry.weight - 1.0).abs() < f32::EPSILON,
            "without slope context, weight must be 1.0; got {} for {}",
            entry.weight,
            entry.id
        );
    }
}
