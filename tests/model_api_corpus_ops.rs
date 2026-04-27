//! PP78 integration test: Corpus Operations.
//!
//! Covers:
//! - `handle_request_corpus_expansion` → creates a gap and returns `CorpusGapInfo`.
//! - `handle_list_corpus_gaps` → returns the pushed gap.
//! - `handle_lookup_source_passage` with a known passage ref → returns text.
//! - `handle_lookup_source_passage` with an unknown ref → returns `Err`.
//! - `handle_draft_rule_pack` → returns a non-empty skeleton referencing the passage.
//! - `handle_check_rule_pack_backlinks` → resolved=3, broken=[] with SE pack + registry.
//! - Broken-backlink detection: an extra constraint with a missing passage is flagged.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architecture_se::bbr::{priors::se_stair_riser_default_prior, rules};
use talos3d_core::{
    capability_registry::CapabilityRegistry,
    plugins::{
        corpus_gap::{CorpusGapQueue, CorpusPassageRegistry},
        model_api::{
            handle_check_rule_pack_backlinks, handle_draft_rule_pack, handle_list_corpus_gaps,
            handle_lookup_source_passage, handle_request_corpus_expansion,
        },
    },
};

// ---------------------------------------------------------------------------
// World setup — follows the same manual pattern as model_api_bbr_stair.rs
// ---------------------------------------------------------------------------

fn init_world() -> World {
    let mut world = World::new();

    // CapabilityRegistry with all three BBR constraints and the SE prior.
    let mut registry = CapabilityRegistry::default();
    registry.register_constraint(rules::bbr_stair_riser_max_constraint());
    registry.register_constraint(rules::bbr_stair_tread_min_constraint());
    registry.register_constraint(rules::bbr_stair_clear_width_residential_constraint());
    registry.register_generation_prior(se_stair_riser_default_prior());
    world.insert_resource(registry);

    // CorpusGapQueue (empty at start).
    world.insert_resource(CorpusGapQueue::default());

    // CorpusPassageRegistry seeded from the hand-authored BBR fixtures (PP78 wiring).
    let mut passage_registry = CorpusPassageRegistry::default();
    for fixture in talos3d_architecture_se::bbr::corpus::all_fixtures() {
        passage_registry.register(fixture.passage_ref, fixture.text, fixture.provenance);
    }
    world.insert_resource(passage_registry);

    world
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn request_corpus_expansion_returns_gap_info() {
    let mut world = init_world();
    let info = handle_request_corpus_expansion(
        &mut world,
        Some("stair_straight".into()),
        Some("SE".into()),
        "rule_pack".into(),
        "need egress rule".into(),
    );

    assert!(!info.id.is_empty(), "gap id must not be empty");
    assert_eq!(info.element_class.as_deref(), Some("stair_straight"));
    assert_eq!(info.jurisdiction.as_deref(), Some("SE"));
    assert_eq!(info.missing_artifact_kind, "rule_pack");
    assert_eq!(info.reported_by, "agent");
}

#[test]
fn list_corpus_gaps_returns_pushed_gap() {
    let mut world = init_world();
    handle_request_corpus_expansion(
        &mut world,
        Some("stair_straight".into()),
        Some("SE".into()),
        "rule_pack".into(),
        "need egress rule".into(),
    );

    let gaps = handle_list_corpus_gaps(&world);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].missing_artifact_kind, "rule_pack");
    assert_eq!(gaps[0].element_class.as_deref(), Some("stair_straight"));
}

#[test]
fn lookup_source_passage_known_ref_returns_text() {
    let world = init_world();
    let info = handle_lookup_source_passage(&world, "BBR_8:22_riser_max".into())
        .expect("BBR_8:22_riser_max should be found");
    assert!(
        info.text.contains("Stigningen"),
        "passage text should contain 'Stigningen'; got: {}",
        info.text
    );
    assert_eq!(info.passage_ref, "BBR_8:22_riser_max");
    assert_eq!(info.jurisdiction.as_deref(), Some("SE"));
    assert_eq!(info.license, "boverket_public");
}

#[test]
fn lookup_source_passage_unknown_ref_returns_error() {
    let world = init_world();
    let result = handle_lookup_source_passage(&world, "nonexistent_passage".into());
    assert!(result.is_err(), "unknown passage ref must return Err");
}

#[test]
fn draft_rule_pack_returns_non_empty_skeleton_with_backlink() {
    let world = init_world();
    let draft =
        handle_draft_rule_pack(&world, "BBR_8:22_riser_max".into(), "stair_straight".into())
            .expect("draft_rule_pack should succeed for known passage");

    assert!(
        !draft.rust_skeleton.is_empty(),
        "rust_skeleton must not be empty"
    );
    assert!(
        draft.rust_skeleton.contains("BBR_8:22_riser_max"),
        "skeleton must reference the passage ref; got: {}",
        draft.rust_skeleton
    );
    assert_eq!(draft.backlink, "BBR_8:22_riser_max");
    assert!(!draft.notes.is_empty(), "notes must not be empty");
}

#[test]
fn draft_rule_pack_unknown_chunk_returns_error() {
    let world = init_world();
    let result =
        handle_draft_rule_pack(&world, "nonexistent_chunk".into(), "stair_straight".into());
    assert!(result.is_err(), "unknown chunk_id must return Err");
}

#[test]
fn check_rule_pack_backlinks_all_resolved_with_se_pack() {
    let world = init_world();
    let report = handle_check_rule_pack_backlinks(&world);

    // The 3 BBR constraints all have source_backlinks that are in the registry.
    assert_eq!(report.total, 3, "expected 3 constraints with backlinks");
    assert_eq!(report.resolved, 3, "all 3 should be resolved");
    assert!(
        report.broken.is_empty(),
        "no broken backlinks expected; got: {:?}",
        report.broken
    );
}

#[test]
fn check_rule_pack_backlinks_detects_missing_passage() {
    use std::sync::Arc;
    use talos3d_core::capability_registry::{
        Applicability, ConstraintDescriptor, ConstraintId, PassageRef, Severity,
    };

    let mut world = init_world();

    // Register an extra constraint whose source_backlink does NOT exist in the registry.
    world
        .resource_mut::<CapabilityRegistry>()
        .register_constraint(ConstraintDescriptor {
            id: ConstraintId("phantom_constraint".into()),
            label: "Phantom".into(),
            description: "Test only".into(),
            applicability: Applicability::any(),
            default_severity: Severity::Warning,
            rationale: "Test".into(),
            source_backlink: Some(PassageRef("nonexistent_passage_ref".into())),
            role: ConstraintRole::Validation,
            validator: Arc::new(|_, _| vec![]),
        });

    let report = handle_check_rule_pack_backlinks(&world);
    assert_eq!(report.total, 4, "4 constraints with backlinks total");
    assert_eq!(report.resolved, 3, "3 resolved");
    assert_eq!(report.broken.len(), 1, "1 broken");
    assert_eq!(report.broken[0].constraint_id, "phantom_constraint");
    assert_eq!(report.broken[0].passage_ref, "nonexistent_passage_ref");
}
