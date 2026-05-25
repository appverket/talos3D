//! PP77 integration test: Boverket BBR stair regulation pilot.
//!
//! Recipe families were retired (architecture-core ships no Rust recipe
//! families; executable authoring comes from curated assets), so this test
//! authors the stair's claim values directly and exercises the validation
//! dispatch + applicability gating in `handle_run_validation` rather than a
//! recipe `generate` step. Covers:
//! - A BBR-compliant stair produces no BBR findings.
//! - Non-compliant riser height fires `BBR_8_22_riser_max` (error) with a
//!   backlink to the hand-authored BBR passage.
//! - Non-compliant tread depth fires `BBR_8_22_tread_min`.
//! - Non-compliant clear width fires `BBR_residential_clear_width`.
//! - `list_generation_priors` filtered by `stair_straight` element class
//!   includes the Swedish stair defaults prior.

#![cfg(feature = "model-api")]

use bevy::prelude::*;
use talos3d_architecture_core::element_classes::stair_straight_class;
use talos3d_architecture_se::bbr::{priors::se_stair_riser_default_prior, rules};
use talos3d_core::{
    capability_registry::{CapabilityRegistry, ElementClassAssignment, ElementClassId},
    plugins::{
        identity::ElementId,
        model_api::{handle_list_generation_priors, handle_run_validation},
        refinement::{
            ClaimGrounding, ClaimPath, ClaimRecord, Grounding, HeuristicTag, RefinementState,
            RefinementStateComponent,
        },
    },
};

fn init_bbr_test_world() -> World {
    let mut world = World::new();

    let mut registry = CapabilityRegistry::default();
    registry.register_element_class(stair_straight_class());
    registry.register_constraint(rules::bbr_stair_riser_max_constraint());
    registry.register_constraint(rules::bbr_stair_tread_min_constraint());
    registry.register_constraint(rules::bbr_stair_clear_width_residential_constraint());
    registry.register_generation_prior(se_stair_riser_default_prior());
    world.insert_resource(registry);

    world
}

/// Author a `Constructible` `stair_straight` whose riser/tread/clear-width
/// claims carry the given millimetre values, in the `value = ...)` grounding
/// form the BBR validators read. Returns the element id.
fn author_stair(world: &mut World, riser_mm: f64, tread_mm: f64, clear_mm: f64) -> u64 {
    const EID: u64 = 1;

    let mut grounding = ClaimGrounding::default();
    for (path, value) in [
        ("riser_height_mm", riser_mm),
        ("tread_depth_mm", tread_mm),
        ("clear_width_mm", clear_mm),
    ] {
        grounding.claims.insert(
            ClaimPath(path.into()),
            ClaimRecord {
                grounding: Grounding::LLMHeuristic {
                    rationale: format!("authored stair parameter (value = {value})"),
                    heuristic_tag: HeuristicTag("test".into()),
                },
                set_at: 0,
                set_by: None,
            },
        );
    }

    world.spawn((
        ElementId(EID),
        ElementClassAssignment {
            element_class: ElementClassId("stair_straight".into()),
            active_recipe: None,
        },
        RefinementStateComponent {
            state: RefinementState::Constructible,
        },
        grounding,
    ));
    EID
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
    let eid = author_stair(&mut world, 175.0, 260.0, 1000.0);

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
    let eid = author_stair(&mut world, 220.0, 260.0, 1000.0);

    let findings = bbr_findings(&world, eid);
    assert!(
        findings
            .iter()
            .any(|(id, sev)| id == "BBR_8_22_riser_max" && sev == "error"),
        "riser 220 must fire BBR_8_22_riser_max error; got: {findings:?}"
    );
}

#[test]
fn noncompliant_tread_fires_bbr_8_22_tread_min() {
    let mut world = init_bbr_test_world();
    let eid = author_stair(&mut world, 175.0, 220.0, 1000.0);

    let findings = bbr_findings(&world, eid);
    assert!(
        findings
            .iter()
            .any(|(id, sev)| id == "BBR_8_22_tread_min" && sev == "error"),
        "tread 220 must fire BBR_8_22_tread_min error; got: {findings:?}"
    );
}

#[test]
fn noncompliant_clear_width_fires_bbr_residential_clear_width() {
    let mut world = init_bbr_test_world();
    let eid = author_stair(&mut world, 175.0, 260.0, 850.0);

    let findings = bbr_findings(&world, eid);
    assert!(
        findings
            .iter()
            .any(|(id, sev)| { id == "BBR_residential_clear_width" && sev == "error" }),
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
