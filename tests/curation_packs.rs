//! PP84 integration tests: PackManifest loader, cross-kind dep resolution,
//! compatibility enforcement, entitlement stub, and MCP-style API handlers.
//!
//! Does NOT edit `tests/curation_substrate.rs` — that file may have
//! uncommitted Codex edits.

use std::io::Write as _;

use talos3d_core::curation::entitlement::Actor;
use talos3d_core::curation::{
    api::{
        check_pack_compatibility_handler, get_pack_manifest, list_packs, resolve_pack_deps_handler,
    },
    compatibility::VersionReq,
    dependencies::{DependencyRef, DependencyRole},
    entitlement::AlwaysDenyEntitlements,
    identity::{AssetId, AssetKindId, AssetRevision, PackId, PackRevision, SourceId},
    load_pack, load_pack_open,
    plugin::CurationPlugin,
    PackError, PackManifest, PackRegistry,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn app() -> bevy::prelude::App {
    let mut app = bevy::prelude::App::new();
    app.add_plugins(CurationPlugin);
    app.update();
    app
}

fn minimal_manifest(pack_id: &str, revision: &str) -> PackManifest {
    PackManifest::new(
        PackId::new(pack_id),
        PackRevision::new(revision),
        "test pack",
    )
}

fn write_manifest_json(manifest: &PackManifest, dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir
        .path()
        .join(format!("{}.json", manifest.pack_id.as_str()));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(serde_json::to_string(manifest).unwrap().as_bytes())
        .unwrap();
    path
}

fn write_manifest_toml(manifest: &PackManifest, dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir
        .path()
        .join(format!("{}.toml", manifest.pack_id.as_str()));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(toml::to_string(manifest).unwrap().as_bytes())
        .unwrap();
    path
}

// ---------------------------------------------------------------------------
// Test: load a hand-authored manifest with assets and sources
// ---------------------------------------------------------------------------

#[test]
fn load_pack_with_assets_and_source_registers_successfully() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = PackManifest {
        assets: vec![
            AssetId::new("recipe.v1/stair_straight"),
            AssetId::new("material_spec.v1/timber_c24"),
        ],
        sources: vec![SourceId::new("boverket.bbr.8")],
        ..minimal_manifest("test_pack", "v1")
    };
    let path = write_manifest_json(&manifest, &dir);
    let mut registry = PackRegistry::default();
    let pack_ref = load_pack_open(&path, "0.1.0", &mut registry).unwrap();

    assert_eq!(pack_ref.pack_id.as_str(), "test_pack");
    let loaded = registry
        .get(&PackId::new("test_pack"), &PackRevision::new("v1"))
        .unwrap();
    assert_eq!(loaded.assets.len(), 2);
    assert_eq!(loaded.sources.len(), 1);
}

// ---------------------------------------------------------------------------
// Test: load from TOML
// ---------------------------------------------------------------------------

#[test]
fn load_pack_from_toml_parses_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = minimal_manifest("toml_pack", "v1");
    let path = write_manifest_toml(&manifest, &dir);
    let mut registry = PackRegistry::default();
    let pack_ref = load_pack_open(&path, "0.1.0", &mut registry).unwrap();
    assert_eq!(pack_ref.pack_id.as_str(), "toml_pack");
}

// ---------------------------------------------------------------------------
// Test: execution-role dep on missing recipe fails resolve_pack_deps
// but passes list_packs
// ---------------------------------------------------------------------------

#[test]
fn execution_dep_on_missing_recipe_fails_resolve_but_pack_is_listed() {
    use talos3d_core::curation::pack::{resolve_pack_deps, DepResolverCtx, PackError};
    use talos3d_core::curation::recipes::RecipeArtifactRegistry;
    use talos3d_core::curation::registry::SourceRegistry;

    let mut manifest = minimal_manifest("pack_with_dep", "v1");
    manifest.dependencies.push(DependencyRef {
        target_kind: AssetKindId::new("recipe.v1"),
        target_id: AssetId::new("recipe.v1/nonexistent"),
        revision: AssetRevision::initial(),
        role: DependencyRole::Execution,
        optional: false,
    });

    let mut registry = PackRegistry::default();
    registry.register(manifest.clone()).unwrap();

    // list_packs works fine (no dep resolution in listing).
    assert_eq!(registry.iter().count(), 1);

    // resolve_pack_deps fails with UnresolvedDependency.
    let recipes = RecipeArtifactRegistry::default();
    let sources = SourceRegistry::default();
    let ctx = DepResolverCtx {
        recipes: &recipes,
        sources: &sources,
    };
    let err = resolve_pack_deps(&manifest, &ctx).unwrap_err();
    assert!(matches!(err, PackError::UnresolvedDependency { .. }));
}

// ---------------------------------------------------------------------------
// Test: optional dep missing is a warning (in unresolved_optional list), not error
// ---------------------------------------------------------------------------

#[test]
fn optional_dep_missing_goes_to_unresolved_optional_list() {
    use talos3d_core::curation::pack::{resolve_pack_deps, DepResolverCtx};
    use talos3d_core::curation::recipes::RecipeArtifactRegistry;
    use talos3d_core::curation::registry::SourceRegistry;

    let mut manifest = minimal_manifest("pack_optional_dep", "v1");
    manifest.dependencies.push(DependencyRef {
        target_kind: AssetKindId::new("recipe.v1"),
        target_id: AssetId::new("recipe.v1/optional_thing"),
        revision: AssetRevision::initial(),
        role: DependencyRole::Execution,
        optional: true,
    });

    let recipes = RecipeArtifactRegistry::default();
    let sources = SourceRegistry::default();
    let ctx = DepResolverCtx {
        recipes: &recipes,
        sources: &sources,
    };
    let resolved = resolve_pack_deps(&manifest, &ctx).unwrap();
    assert_eq!(resolved.resolved_deps.len(), 0);
    assert_eq!(resolved.unresolved_optional_deps.len(), 1);
}

// ---------------------------------------------------------------------------
// Test: cyclic dependency is rejected at load/register time
// ---------------------------------------------------------------------------

#[test]
fn cyclic_pack_dep_is_detected() {
    use talos3d_core::curation::pack::detect_cycles;

    let mut reg = PackRegistry::default();
    let mut m_a = minimal_manifest("a", "v1");
    m_a.dependencies.push(DependencyRef {
        target_kind: AssetKindId::new("pack"),
        target_id: AssetId::new("b"),
        revision: AssetRevision::initial(),
        role: DependencyRole::Execution,
        optional: false,
    });
    let mut m_b = minimal_manifest("b", "v1");
    m_b.dependencies.push(DependencyRef {
        target_kind: AssetKindId::new("pack"),
        target_id: AssetId::new("a"),
        revision: AssetRevision::initial(),
        role: DependencyRole::Execution,
        optional: false,
    });
    reg.register(m_a).unwrap();
    reg.register(m_b).unwrap();

    let err = detect_cycles(&reg).unwrap_err();
    match err {
        PackError::CyclicDependency { cycle } => {
            assert!(cycle.iter().any(|p| p.as_str() == "a" || p.as_str() == "b"));
        }
        _ => panic!("expected CyclicDependency"),
    }
}

// ---------------------------------------------------------------------------
// Test: core_api ^99.0 fails check_pack_compatibility
// ---------------------------------------------------------------------------

#[test]
fn pack_with_incompatible_core_api_fails_compatibility_check() {
    use talos3d_core::curation::pack::check_pack_compatibility;

    let mut m = minimal_manifest("future_pack", "v1");
    m.compatibility.core_api = Some(VersionReq::new("^99.0"));

    let findings = check_pack_compatibility(&m, "0.1.0");
    assert!(
        findings
            .iter()
            .any(|f| f.is_error() && f.message.contains("core_api")),
        "expected core_api mismatch error in: {:?}",
        findings,
    );
}

// ---------------------------------------------------------------------------
// Test: default entitlement resolver allows all packs
// ---------------------------------------------------------------------------

#[test]
fn default_entitlement_resolver_allows_all() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = minimal_manifest("gated", "v1");
    m.entitlement = Some(talos3d_core::curation::pack::EntitlementHook::new(
        "some/hook",
    ));
    let path = write_manifest_json(&m, &dir);

    let mut registry = PackRegistry::default();
    // load_pack_open uses AllowAllEntitlements.
    let result = load_pack_open(&path, "0.1.0", &mut registry);
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Test: AlwaysDenyEntitlements blocks load
// ---------------------------------------------------------------------------

#[test]
fn always_deny_entitlement_blocks_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = minimal_manifest("denied_pack", "v1");
    m.entitlement = Some(talos3d_core::curation::pack::EntitlementHook::new(
        "some/hook",
    ));
    let path = write_manifest_json(&m, &dir);

    let mut registry = PackRegistry::default();
    let err = load_pack(
        &path,
        "0.1.0",
        &Actor::new("user:test"),
        &AlwaysDenyEntitlements::new("not subscribed"),
        &mut registry,
    )
    .unwrap_err();
    assert!(matches!(err, PackError::EntitlementDenied { .. }));
    // Pack must not be visible in registry.
    assert!(registry
        .get(&PackId::new("denied_pack"), &PackRevision::new("v1"))
        .is_none());
}

// ---------------------------------------------------------------------------
// Test: MCP-style handler round-trip through Bevy World
// ---------------------------------------------------------------------------

#[test]
fn api_list_packs_empty_without_loaded_packs() {
    let app = app();
    let packs = list_packs(app.world());
    assert!(packs.is_empty());
}

#[test]
fn api_get_pack_manifest_round_trip() {
    let mut app = app();
    let m = minimal_manifest("my_pack", "v2");
    app.world_mut()
        .resource_mut::<PackRegistry>()
        .register(m.clone())
        .unwrap();

    // list_packs returns it.
    let packs = list_packs(app.world());
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].pack_id, "my_pack");

    // get_pack_manifest with exact revision.
    let manifest = get_pack_manifest(app.world(), "my_pack", Some("v2")).unwrap();
    assert_eq!(manifest.label, "test pack");

    // get_pack_manifest without revision (latest).
    let manifest2 = get_pack_manifest(app.world(), "my_pack", None).unwrap();
    assert_eq!(manifest2.revision.as_str(), "v2");

    // get_pack_manifest on missing id.
    let err = get_pack_manifest(app.world(), "missing", None).unwrap_err();
    assert_eq!(err.code, "curation.pack_not_found");
}

#[test]
fn api_resolve_pack_deps_returns_correct_counts() {
    use talos3d_core::curation::identity::SourceRevision;
    use talos3d_core::curation::registry::SourceRegistry;
    use talos3d_core::curation::source::{SourceLicense, SourceRegistryEntry, SourceTier};

    let mut app = app();

    // Seed a source in SourceRegistry.
    app.world_mut()
        .resource_mut::<SourceRegistry>()
        .insert(SourceRegistryEntry::new(
            SourceId::new("boverket.bbr.8"),
            SourceRevision::new("2011:6"),
            "BBR 8",
            "Boverket",
            SourceTier::Jurisdictional,
            SourceLicense::OfficialGovernmentPublication,
        ));

    // Pack with one resolved source dep + one optional unresolved dep.
    let mut m = minimal_manifest("dep_pack", "v1");
    m.dependencies.push(DependencyRef {
        target_kind: AssetKindId::new("source"),
        target_id: AssetId::new("boverket.bbr.8"),
        revision: AssetRevision::initial(),
        role: DependencyRole::Citation,
        optional: false,
    });
    m.dependencies.push(DependencyRef {
        target_kind: AssetKindId::new("recipe.v1"),
        target_id: AssetId::new("recipe.v1/nonexistent"),
        revision: AssetRevision::initial(),
        role: DependencyRole::Validation,
        optional: true,
    });

    app.world_mut()
        .resource_mut::<PackRegistry>()
        .register(m)
        .unwrap();

    let resolved = resolve_pack_deps_handler(app.world(), "dep_pack", None).unwrap();
    assert_eq!(resolved.resolved_deps.len(), 1);
    assert_eq!(resolved.unresolved_optional_deps.len(), 1);
}

#[test]
fn api_check_pack_compatibility_returns_error_for_future_core_api() {
    let mut app = app();
    let mut m = minimal_manifest("future", "v1");
    m.compatibility.core_api = Some(VersionReq::new("^99.0"));
    app.world_mut()
        .resource_mut::<PackRegistry>()
        .register(m)
        .unwrap();

    let findings = check_pack_compatibility_handler(app.world(), "future", None, "0.1.0").unwrap();
    assert!(findings.iter().any(|f| f.severity == "error"));
    assert!(findings
        .iter()
        .any(|f| f.code == "curation.pack.core_api_mismatch"));
}

#[test]
fn api_check_pack_compatibility_green_for_compatible_pack() {
    let mut app = app();
    let mut m = minimal_manifest("compat_pack", "v1");
    m.compatibility.core_api = Some(VersionReq::new("^0.1"));
    app.world_mut()
        .resource_mut::<PackRegistry>()
        .register(m)
        .unwrap();

    let findings =
        check_pack_compatibility_handler(app.world(), "compat_pack", None, "0.1.0").unwrap();
    // Compatible — no error findings.
    assert!(!findings.iter().any(|f| f.severity == "error"));
}
