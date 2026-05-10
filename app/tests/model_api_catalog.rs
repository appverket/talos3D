//! PP75 integration test: catalog provider registration and query.
//!
//! Covers:
//! - `handle_list_catalog_providers`: returns the single metric lumber provider.
//! - `handle_catalog_query`: returns all 6 C24 rows for an empty filter.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architecture_core::catalogs::dimensional_lumber_metric::dimensional_lumber_metric;
use talos3d_core::{
    capability_registry::CapabilityRegistry,
    plugins::model_api::{handle_catalog_query, handle_list_catalog_providers},
};

// ---------------------------------------------------------------------------
// Test world setup
// ---------------------------------------------------------------------------

fn init_catalog_test_world() -> World {
    let mut world = World::new();

    let mut registry = CapabilityRegistry::default();
    registry.register_catalog_provider(dimensional_lumber_metric());
    world.insert_resource(registry);

    world
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn list_catalog_providers_returns_one_provider() {
    let world = init_catalog_test_world();
    let providers = handle_list_catalog_providers(&world);

    assert_eq!(providers.len(), 1);
    let p = &providers[0];
    assert_eq!(p.id, "dimensional_lumber_metric");
    assert_eq!(p.category, "dimensional_lumber");
    assert_eq!(p.license, "cc0");
    assert_eq!(p.source_version, "2026-Q1");
    assert_eq!(p.region.as_deref(), Some("SE_EU"));
}

#[test]
fn catalog_query_returns_six_c24_rows() {
    let world = init_catalog_test_world();
    let rows = handle_catalog_query(
        &world,
        "dimensional_lumber_metric".into(),
        serde_json::json!({}),
    )
    .expect("catalog_query should succeed");

    assert_eq!(rows.len(), 6, "expected 6 C24 rows, got {}", rows.len());

    let expected_ids = [
        "C24_45x95",
        "C24_45x120",
        "C24_45x145",
        "C24_45x170",
        "C24_45x195",
        "C24_45x220",
    ];

    for expected_id in &expected_ids {
        assert!(
            rows.iter().any(|r| r.row_id == *expected_id),
            "missing row_id '{expected_id}'"
        );
    }

    // Verify every row carries the expected metadata.
    for row in &rows {
        assert_eq!(row.category, "dimensional_lumber");
        assert_eq!(row.license, "cc0");
        assert_eq!(row.source_version, "2026-Q1");
        let grade = row.data.get("grade").and_then(|v| v.as_str());
        assert_eq!(grade, Some("C24"), "row {} missing grade C24", row.row_id);
        let actual_mm = row.data.get("actual_mm").and_then(|v| v.as_array());
        assert!(
            actual_mm.is_some_and(|a| a.len() == 2),
            "row {} must have actual_mm: [w, h]",
            row.row_id,
        );
    }
}

#[test]
fn catalog_query_unknown_provider_returns_error() {
    let world = init_catalog_test_world();
    let result = handle_catalog_query(&world, "nonexistent_provider".into(), serde_json::json!({}));
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .contains("Unknown catalog provider 'nonexistent_provider'"));
}
