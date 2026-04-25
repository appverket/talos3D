//! PP80 integration test: curation substrate end-to-end.
//!
//! Exercises the full flow an app expects: bootstrap with `CurationPlugin`
//! → seeded Canonical sources are visible → agent nominates a new source
//! → user approves → registry updated → nomination queue drained →
//! publication floor accepts evidence citing the now-registered source →
//! supersession flow marks an old revision and the floor rejects assets
//! still citing it without `Certified` override.
//!
//! Does not depend on the `model-api` feature; drives the handlers
//! directly from `talos3d_core::curation::api`.

use bevy::prelude::*;
use talos3d_core::curation::{
    api::{
        approve_nomination, get_source, list_nominations, list_sources, nominate_source,
        nominate_sunset, reject_nomination, report_corpus_gap, ListSourcesFilter,
    },
    identity::{AssetId, AssetKindId, SourceId, SourceRevision},
    meta::CurationMeta,
    plugin::CurationPlugin,
    policy::PublicationPolicy,
    provenance::{Confidence, EvidenceRef, GroundingKind, JurisdictionTag, Lineage, Provenance},
    registry::SourceRegistry,
    scope_trust::{Scope, Trust},
    source::{SourceLicense, SourceRegistryEntry, SourceTier},
};
use talos3d_core::plugins::refinement::{AgentId, RuleId};

fn bootstrap() -> App {
    let mut app = App::new();
    app.add_plugins(CurationPlugin);
    app.update();
    app
}

fn bbr_entry() -> SourceRegistryEntry {
    SourceRegistryEntry::new(
        SourceId::new("boverket.bbr.8"),
        SourceRevision::new("2011:6"),
        "Boverket BBR 8 — Säkerhet vid användning",
        "Boverket",
        SourceTier::Jurisdictional,
        SourceLicense::OfficialGovernmentPublication,
    )
    .with_jurisdiction(JurisdictionTag::new("SE"))
    .with_canonical_url("https://www.boverket.se/sv/bbr/")
}

fn synthetic_recipe_meta(with_evidence_for: Option<(&str, &str)>) -> CurationMeta {
    let evidence = with_evidence_for
        .map(|(src, rev)| {
            vec![EvidenceRef {
                source_id: SourceId::new(src),
                revision: SourceRevision::new(rev),
                claim_path: None,
                excerpt_ref: None,
                grounding_kind: GroundingKind::ExplicitRule(RuleId("bbr_stair_riser_max".into())),
            }]
        })
        .unwrap_or_default();

    CurationMeta::new(
        AssetId::new("recipe.v1/synthetic_test_stair"),
        AssetKindId::new("recipe.v1"),
        Provenance {
            author: AgentId("test".into()),
            confidence: Confidence::Medium,
            lineage: Lineage::Freeform,
            rationale: None,
            jurisdiction: Some(JurisdictionTag::new("SE")),
            catalog_dependencies: Vec::new(),
            evidence,
        },
    )
    .with_scope(Scope::Project)
    .with_trust(Trust::Published)
}

#[test]
fn bootstrap_has_three_canonical_seeds() {
    let app = bootstrap();
    let sources = list_sources(app.world(), ListSourcesFilter::default());
    assert_eq!(sources.len(), 3);
    let ids: Vec<&str> = sources.iter().map(|s| s.source_id.as_str()).collect();
    assert!(ids.contains(&"iso.129-1"));
    assert!(ids.contains(&"asme.y14.5"));
    assert!(ids.contains(&"iso.80000-1"));
}

#[test]
fn canonical_seeds_are_all_active_and_canonical_tier() {
    let app = bootstrap();
    for s in list_sources(app.world(), ListSourcesFilter::default()) {
        assert_eq!(s.tier, "canonical");
        assert_eq!(s.status, "active");
    }
}

#[test]
fn nominate_then_approve_lands_entry_in_registry() {
    let mut app = bootstrap();
    let nom = nominate_source(
        app.world_mut(),
        bbr_entry(),
        "agent:codex",
        1_700_000_000,
        Some("needed for BBR stair rule authoring".into()),
    )
    .expect("nominate_source must succeed");
    assert_eq!(nom.action, "add_source");

    // Pending.
    assert_eq!(list_nominations(app.world()).len(), 1);

    // Approve.
    let approved = approve_nomination(app.world_mut(), &nom.id).unwrap();
    assert_eq!(approved.target_source_id, "boverket.bbr.8");

    // Registry updated.
    let info = get_source(app.world(), "boverket.bbr.8", "2011:6").unwrap();
    assert_eq!(info.publisher, "Boverket");
    assert_eq!(info.jurisdiction.as_deref(), Some("SE"));

    // Queue drained.
    assert_eq!(list_nominations(app.world()).len(), 0);
}

#[test]
fn reject_keeps_registry_unchanged() {
    let mut app = bootstrap();
    let nom = nominate_source(app.world_mut(), bbr_entry(), "agent:test", 0, None).unwrap();
    reject_nomination(app.world_mut(), &nom.id, Some("not needed".into())).unwrap();
    assert!(list_nominations(app.world()).is_empty());
    let err = get_source(app.world(), "boverket.bbr.8", "2011:6").unwrap_err();
    assert_eq!(err.code, "curation.source_not_found");
}

#[test]
fn publication_floor_passes_when_evidence_resolves() {
    let mut app = bootstrap();
    let nom = nominate_source(app.world_mut(), bbr_entry(), "agent:test", 0, None).unwrap();
    approve_nomination(app.world_mut(), &nom.id).unwrap();

    let meta = synthetic_recipe_meta(Some(("boverket.bbr.8", "2011:6")));
    let registry = app.world().resource::<SourceRegistry>();
    let findings = PublicationPolicy::default().check(&meta, registry);
    assert!(
        findings.is_empty(),
        "expected no findings, got {findings:?}"
    );
    assert!(PublicationPolicy::default().permits(&meta, registry));
}

#[test]
fn publication_floor_rejects_unresolved_evidence() {
    let app = bootstrap();
    let meta = synthetic_recipe_meta(Some(("never.registered", "v0")));
    let registry = app.world().resource::<SourceRegistry>();
    let findings = PublicationPolicy::default().check(&meta, registry);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].code, "curation.publication.evidence_unresolved");
}

#[test]
fn sunset_nomination_approval_flips_status_and_blocks_publication() {
    let mut app = bootstrap();
    let nom = nominate_source(app.world_mut(), bbr_entry(), "agent:test", 0, None).unwrap();
    approve_nomination(app.world_mut(), &nom.id).unwrap();

    let sunset = nominate_sunset(
        app.world_mut(),
        "boverket.bbr.8",
        "2011:6",
        Some("boverket.bbr.8.v2025".into()),
        "superseded by 2025 edition".into(),
        "agent:test",
        0,
        None,
    )
    .unwrap();
    approve_nomination(app.world_mut(), &sunset.id).unwrap();

    // Status changed to superseded.
    let info = get_source(app.world(), "boverket.bbr.8", "2011:6").unwrap();
    assert_eq!(info.status, "superseded");

    // Assets still citing it without Certified override fail the floor.
    let meta = synthetic_recipe_meta(Some(("boverket.bbr.8", "2011:6")));
    let registry = app.world().resource::<SourceRegistry>();
    let findings = PublicationPolicy::default().check(&meta, registry);
    assert!(findings
        .iter()
        .any(|f| f.code == "curation.publication.source_superseded"));

    // Certified override lets it through (the ADR-040 explicit override).
    let mut certified = synthetic_recipe_meta(Some(("boverket.bbr.8", "2011:6")));
    certified.provenance.confidence = Confidence::Certified;
    let findings = PublicationPolicy::default().check(&certified, registry);
    assert!(findings.is_empty());
}

#[test]
fn approve_sunset_of_missing_target_leaves_queue_intact() {
    let mut app = bootstrap();
    let nom = nominate_sunset(
        app.world_mut(),
        "missing.source",
        "v0",
        None,
        "test".into(),
        "agent",
        0,
        None,
    )
    .unwrap();
    let err = approve_nomination(app.world_mut(), &nom.id).unwrap_err();
    assert_eq!(err.code, "curation.sunset_target_missing");
    assert_eq!(list_nominations(app.world()).len(), 1);
}

#[test]
fn report_corpus_gap_cross_kind_survives_and_is_filterable() {
    let mut app = bootstrap();
    let gap_id = report_corpus_gap(
        app.world_mut(),
        AssetKindId::new("material_spec.v1"),
        Some("SE".into()),
        "catalog_row".into(),
        serde_json::json!({"needed_for": "timber_c24_stringer"}),
        "agent:test",
        0,
    );
    assert!(gap_id.starts_with("gap-"));
    let queue = app
        .world()
        .resource::<talos3d_core::plugins::corpus_gap::CorpusGapQueue>();
    let kind = AssetKindId::new("material_spec.v1");
    let gaps: Vec<_> = queue.list_by_kind(&kind).collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].missing_artifact_kind, "catalog_row");
    assert_eq!(gaps[0].jurisdiction.as_deref(), Some("SE"));
}

#[test]
fn list_sources_filter_by_jurisdiction_scopes_to_project_tier_after_approval() {
    let mut app = bootstrap();
    let nom = nominate_source(app.world_mut(), bbr_entry(), "agent:test", 0, None).unwrap();
    approve_nomination(app.world_mut(), &nom.id).unwrap();

    let filter = ListSourcesFilter {
        jurisdiction: Some("SE".into()),
        ..Default::default()
    };
    let results = list_sources(app.world(), filter);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id, "boverket.bbr.8");
}

// ---------------------------------------------------------------------------
// PP81: RecipeArtifact — verify all shipped recipes appear via the mirror,
// and exercise the kind-specific handlers end-to-end.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod recipe_artifacts {
    use super::*;
    use talos3d_architecture_core::ArchitectureCorePlugin;
    use talos3d_core::curation::api::{
        get_recipe, list_recipes, publish_recipe, save_recipe, ListRecipesFilter,
    };
    use talos3d_core::curation::recipes::RecipeArtifactRegistry;

    fn app_with_architecture() -> App {
        let mut app = App::new();
        // CapabilityRegistry must exist before ArchitectureCorePlugin registers
        // descriptors.
        app.init_resource::<talos3d_core::capability_registry::CapabilityRegistry>();
        app.add_plugins(CurationPlugin);
        app.add_plugins(ArchitectureCorePlugin);
        app.update(); // runs Startup — mirror populates RecipeArtifactRegistry
        app
    }

    #[test]
    fn shipped_recipes_are_mirrored_as_shipped_published_artifacts() {
        let app = app_with_architecture();
        let recipes = app.world().resource::<RecipeArtifactRegistry>();
        // ArchitectureCorePlugin ships at least 6 recipe families.
        assert!(
            recipes.len() >= 5,
            "expected >= 5 shipped recipes, got {}",
            recipes.len()
        );
        for r in recipes.iter() {
            assert_eq!(r.meta.scope, Scope::Shipped);
            assert_eq!(r.meta.trust, Trust::Published);
            assert!(r.body.is_native());
        }
    }

    #[test]
    fn list_recipes_filters_by_target_class() {
        let app = app_with_architecture();
        let filter = ListRecipesFilter {
            target_class: Some("foundation_system".into()),
            ..Default::default()
        };
        let recipes = list_recipes(app.world(), filter);
        assert!(!recipes.is_empty());
        assert!(recipes
            .iter()
            .all(|r| r.target_class == "foundation_system"));
        // All shipped recipes are scope=shipped trust=published.
        for r in &recipes {
            assert_eq!(r.scope, "shipped");
            assert_eq!(r.trust, "published");
            assert_eq!(r.body_kind, "native_fn_ref");
        }
    }

    #[test]
    fn get_recipe_by_asset_id_returns_info() {
        let app = app_with_architecture();
        let info = get_recipe(app.world(), "recipe.v1/pier_foundation").unwrap();
        assert_eq!(info.family_id.as_deref(), Some("pier_foundation"));
        assert_eq!(info.target_class, "foundation_system");
    }

    #[test]
    fn get_recipe_not_found_returns_structured_error() {
        let app = app_with_architecture();
        let err = get_recipe(app.world(), "recipe.v1/does_not_exist").unwrap_err();
        assert_eq!(err.code, "curation.recipe_not_found");
    }

    #[test]
    fn save_recipe_refuses_to_demote_shipped_artifacts() {
        let mut app = app_with_architecture();
        let err = save_recipe(app.world_mut(), "recipe.v1/pier_foundation", "project").unwrap_err();
        assert_eq!(err.code, "curation.shipped_scope_immutable");
    }

    #[test]
    fn publish_recipe_rejects_draft_without_tests_or_certified() {
        let mut app = app_with_architecture();
        // Synthesize a session-scope Draft artifact into the registry.
        let mut synthetic = talos3d_core::curation::recipes::RecipeArtifact {
            meta: CurationMeta::new(
                AssetId::new("recipe.v1/test_draft"),
                AssetKindId::new("recipe.v1"),
                Provenance {
                    author: AgentId("test".into()),
                    confidence: Confidence::Medium,
                    lineage: Lineage::Freeform,
                    rationale: None,
                    jurisdiction: None,
                    catalog_dependencies: Vec::new(),
                    evidence: Vec::new(),
                },
            )
            .with_scope(Scope::Session)
            .with_trust(Trust::Draft),
            body: talos3d_core::curation::recipes::RecipeBody::native(
                talos3d_core::capability_registry::RecipeFamilyId("test_draft".into()),
            ),
            parameter_schema: serde_json::json!({"type":"object"}),
            target_class: "test_class".into(),
            supported_refinement_states: vec![
                talos3d_core::plugins::refinement::RefinementState::Constructible,
            ],
            tests: Vec::new(),
        };
        {
            let mut reg = app.world_mut().resource_mut::<RecipeArtifactRegistry>();
            reg.insert(synthetic.clone());
        }
        let err = publish_recipe(app.world_mut(), "recipe.v1/test_draft").unwrap_err();
        assert_eq!(err.code, "curation.recipe_missing_review");

        // Now re-install with Certified override.
        synthetic.meta.provenance.confidence = Confidence::Certified;
        {
            let mut reg = app.world_mut().resource_mut::<RecipeArtifactRegistry>();
            reg.insert(synthetic);
        }
        let info = publish_recipe(app.world_mut(), "recipe.v1/test_draft").unwrap();
        assert_eq!(info.trust, "published");
    }
}

#[test]
fn nomination_ids_increment_without_collision() {
    let mut app = bootstrap();
    let n1 = nominate_source(app.world_mut(), bbr_entry(), "agent:a", 0, None).unwrap();
    let entry_b = SourceRegistryEntry::new(
        SourceId::new("iso.80000-2"),
        SourceRevision::new("2019"),
        "ISO 80000-2",
        "ISO",
        SourceTier::Canonical,
        SourceLicense::PermissiveCite,
    );
    let n2 = nominate_source(app.world_mut(), entry_b, "agent:b", 0, None).unwrap();
    assert_ne!(n1.id, n2.id);
}
