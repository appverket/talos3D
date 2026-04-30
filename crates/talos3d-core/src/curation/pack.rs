//! Pack manifest types, registry, and runtime loading.
//!
//! `PackManifest` is the shipping/integrity/revision/dependency unit per
//! ADR-040. A pack lists which assets and source entries it ships; the
//! actual asset/source bodies live in the shared `AssetRegistry` /
//! `SourceRegistry`. This keeps manifests small and enables content-
//! addressed deduplication across packs.
//!
//! `PackRegistry` is a Bevy `Resource` that holds loaded manifests keyed
//! by `(PackId, PackRevision)`. Loading from disk, cross-kind dependency
//! resolution, compatibility checking, and entitlement enforcement all
//! live here (PP84).

use std::collections::BTreeMap;
use std::path::Path;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::compatibility::CompatibilityRef;
use super::dependencies::DependencyRef;
use super::entitlement::{Actor, AllowAllEntitlements, Entitlement, EntitlementResolver};
use super::identity::{AssetId, PackId, PackRevision, SourceId};
use super::recipes::RecipeArtifactRegistry;
use super::registry::SourceRegistry;

// ---------------------------------------------------------------------------
// Core manifest types (unchanged from PP79 stub)
// ---------------------------------------------------------------------------

/// Reference to a pack at a specific revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PackRef {
    pub pack_id: PackId,
    pub revision: PackRevision,
}

impl PackRef {
    pub fn new(pack_id: PackId, revision: PackRevision) -> Self {
        Self { pack_id, revision }
    }
}

/// Opaque reference to an operator-defined entitlement policy. The
/// substrate carries the reference but never resolves it; an
/// `EntitlementResolver` implementation looks this up.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct EntitlementHook(pub String);

impl EntitlementHook {
    pub fn new(reference: impl Into<String>) -> Self {
        Self(reference.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Pack manifest — a shippable bundle of curated assets + source entries
/// pinned to specific compatibility requirements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PackManifest {
    pub pack_id: PackId,
    pub revision: PackRevision,
    /// Display label. Free-form; used in pack-listing UIs.
    pub label: String,
    /// Assets shipped by this pack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<AssetId>,
    /// Source-registry entries shipped by this pack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceId>,
    pub compatibility: CompatibilityRef,
    /// Orthogonal commercial policy hook. `None` for open packs.
    pub entitlement: Option<EntitlementHook>,
    /// Cross-kind dependencies declared by assets in this pack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DependencyRef>,
}

impl PackManifest {
    pub fn new(pack_id: PackId, revision: PackRevision, label: impl Into<String>) -> Self {
        Self {
            pack_id,
            revision,
            label: label.into(),
            assets: Vec::new(),
            sources: Vec::new(),
            compatibility: CompatibilityRef::unconstrained(),
            entitlement: None,
            dependencies: Vec::new(),
        }
    }

    pub fn as_ref(&self) -> PackRef {
        PackRef::new(self.pack_id.clone(), self.revision.clone())
    }
}

// ---------------------------------------------------------------------------
// Load errors
// ---------------------------------------------------------------------------

/// Errors that can occur when loading or working with packs.
#[derive(Debug, Clone, PartialEq)]
pub enum PackError {
    /// Manifest file could not be read.
    Io { path: String, message: String },
    /// Manifest content could not be parsed.
    Parse { path: String, message: String },
    /// A `DependencyRef` could not be resolved against the registries.
    UnresolvedDependency { pack_id: PackId, dep: DependencyRef },
    /// A cyclic dependency was detected.
    CyclicDependency { cycle: Vec<PackId> },
    /// `core_api` requirement is not satisfied by the running platform.
    CoreApiMismatch {
        pack_id: PackId,
        required: String,
        actual: String,
    },
    /// A `capability_api` entry is not satisfied.
    CapabilityApiMismatch {
        pack_id: PackId,
        kind: String,
        required: String,
        actual: String,
    },
    /// Entitlement check denied load.
    EntitlementDenied { pack_id: PackId, reason: String },
    /// Entitlement resolver returned an error.
    EntitlementError { pack_id: PackId, message: String },
    /// Pack with this id+revision is already registered.
    AlreadyRegistered {
        pack_id: PackId,
        revision: PackRevision,
    },
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, message } => write!(f, "io error reading '{path}': {message}"),
            Self::Parse { path, message } => write!(f, "parse error in '{path}': {message}"),
            Self::UnresolvedDependency { pack_id, dep } => write!(
                f,
                "pack '{}': unresolved {} dep on {}@v{}",
                pack_id.as_str(),
                format!("{:?}", dep.role).to_lowercase(),
                dep.target_id.as_str(),
                dep.revision.version,
            ),
            Self::CyclicDependency { cycle } => write!(
                f,
                "cyclic dependency: {}",
                cycle
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(" → "),
            ),
            Self::CoreApiMismatch {
                pack_id,
                required,
                actual,
            } => write!(
                f,
                "pack '{}': core_api requires '{required}', running '{actual}'",
                pack_id.as_str(),
            ),
            Self::CapabilityApiMismatch {
                pack_id,
                kind,
                required,
                actual,
            } => write!(
                f,
                "pack '{}': capability_api for '{kind}' requires '{required}', running '{actual}'",
                pack_id.as_str(),
            ),
            Self::EntitlementDenied { pack_id, reason } => write!(
                f,
                "pack '{}': entitlement denied — {reason}",
                pack_id.as_str(),
            ),
            Self::EntitlementError { pack_id, message } => write!(
                f,
                "pack '{}': entitlement resolver error — {message}",
                pack_id.as_str(),
            ),
            Self::AlreadyRegistered { pack_id, revision } => write!(
                f,
                "pack '{}' revision '{}' already registered",
                pack_id.as_str(),
                revision.as_str(),
            ),
        }
    }
}

impl std::error::Error for PackError {}

// ---------------------------------------------------------------------------
// Compatibility: body-schema migration advisory
// ---------------------------------------------------------------------------

/// Advisory flag when a pack's `body_schema` pin differs from the current
/// schema version for that kind. Migration machinery is out of PP84 scope;
/// this flag is returned for callers to handle or surface.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaMigrationAdvisory {
    pub pack_id: PackId,
    pub kind: String,
    pub pinned_version: u32,
    pub current_version: u32,
}

// ---------------------------------------------------------------------------
// Compatibility findings
// ---------------------------------------------------------------------------

/// Severity of a compatibility finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompatFindingSeverity {
    /// Advisory: the pack declares a body_schema pin that differs from
    /// the running version. The pack may still function.
    Advisory,
    /// Error: the pack cannot run on this platform.
    Error,
}

/// One finding from a compatibility check.
#[derive(Debug, Clone, PartialEq)]
pub struct CompatFinding {
    pub severity: CompatFindingSeverity,
    pub code: &'static str,
    pub message: String,
}

impl CompatFinding {
    pub fn is_error(&self) -> bool {
        self.severity == CompatFindingSeverity::Error
    }
}

// ---------------------------------------------------------------------------
// PackRegistry resource
// ---------------------------------------------------------------------------

/// Bevy `Resource` holding all loaded `PackManifest`s keyed by
/// `(PackId, PackRevision)`.
#[derive(Debug, Default, Resource)]
pub struct PackRegistry {
    pub(crate) manifests: BTreeMap<(PackId, PackRevision), PackManifest>,
}

impl PackRegistry {
    /// Register a manifest that has already been validated and passed
    /// entitlement checks. Returns an error if already registered.
    pub fn register(&mut self, manifest: PackManifest) -> Result<PackRef, PackError> {
        let key = (manifest.pack_id.clone(), manifest.revision.clone());
        if self.manifests.contains_key(&key) {
            return Err(PackError::AlreadyRegistered {
                pack_id: manifest.pack_id.clone(),
                revision: manifest.revision.clone(),
            });
        }
        let pack_ref = manifest.as_ref();
        self.manifests.insert(key, manifest);
        Ok(pack_ref)
    }

    /// Get a manifest by id + revision.
    pub fn get(&self, pack_id: &PackId, revision: &PackRevision) -> Option<&PackManifest> {
        self.manifests.get(&(pack_id.clone(), revision.clone()))
    }

    /// Iterate all registered manifests.
    pub fn iter(&self) -> impl Iterator<Item = &PackManifest> {
        self.manifests.values()
    }

    /// Get the latest revision for a `pack_id` (lexicographically
    /// greatest `PackRevision`). Returns `None` if no manifest with
    /// that id is registered.
    pub fn latest(&self, pack_id: &PackId) -> Option<&PackManifest> {
        self.manifests
            .range((pack_id.clone(), PackRevision::new(""))..)
            .take_while(|((id, _), _)| id == pack_id)
            .last()
            .map(|(_, m)| m)
    }
}

// ---------------------------------------------------------------------------
// Disk loading
// ---------------------------------------------------------------------------

/// Parse a `PackManifest` from a raw byte slice. Supports JSON and TOML —
/// one serde schema, two deserializers, no duplication.
fn parse_manifest(bytes: &[u8], path: &str) -> Result<PackManifest, PackError> {
    // Detect format by extension; fall back to JSON.
    let is_toml = path.ends_with(".toml");
    if is_toml {
        toml::from_str(std::str::from_utf8(bytes).map_err(|e| PackError::Parse {
            path: path.to_owned(),
            message: e.to_string(),
        })?)
        .map_err(|e| PackError::Parse {
            path: path.to_owned(),
            message: e.to_string(),
        })
    } else {
        serde_json::from_slice(bytes).map_err(|e| PackError::Parse {
            path: path.to_owned(),
            message: e.to_string(),
        })
    }
}

/// Load a `PackManifest` from `path`, enforce compatibility and
/// entitlement, register it into `registry`, and return the `PackRef`.
///
/// `platform_version` is the running core API version string (e.g.
/// `"0.1.0"`). `actor` identifies who is loading the pack (used for
/// entitlement checks). `resolver` handles the entitlement hook if
/// present.
pub fn load_pack(
    path: &Path,
    platform_version: &str,
    actor: &Actor,
    resolver: &dyn EntitlementResolver,
    registry: &mut PackRegistry,
) -> Result<PackRef, PackError> {
    let path_str = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|e| PackError::Io {
        path: path_str.clone(),
        message: e.to_string(),
    })?;
    let manifest = parse_manifest(&bytes, &path_str)?;

    // Entitlement check before anything else — a denied pack should not
    // be visible to the registries at all.
    let ent_result = resolver
        .is_entitled(&manifest.pack_id, actor, manifest.entitlement.as_ref())
        .map_err(|e| PackError::EntitlementError {
            pack_id: manifest.pack_id.clone(),
            message: e.to_string(),
        })?;
    if let Entitlement::Denied { reason } = ent_result {
        return Err(PackError::EntitlementDenied {
            pack_id: manifest.pack_id.clone(),
            reason,
        });
    }

    // Compatibility check: core_api.
    check_core_api_compat(&manifest, platform_version)?;

    registry.register(manifest)
}

/// Check that `manifest.compatibility.core_api` is satisfied by
/// `platform_version`. Uses the `semver` crate.
fn check_core_api_compat(manifest: &PackManifest, platform_version: &str) -> Result<(), PackError> {
    let Some(req_str) = &manifest.compatibility.core_api else {
        return Ok(()); // no constraint
    };
    let req =
        semver::VersionReq::parse(req_str.as_str()).map_err(|_| PackError::CoreApiMismatch {
            pack_id: manifest.pack_id.clone(),
            required: req_str.as_str().to_owned(),
            actual: platform_version.to_owned(),
        })?;
    let ver = semver::Version::parse(platform_version).map_err(|_| PackError::CoreApiMismatch {
        pack_id: manifest.pack_id.clone(),
        required: req_str.as_str().to_owned(),
        actual: platform_version.to_owned(),
    })?;
    if !req.matches(&ver) {
        return Err(PackError::CoreApiMismatch {
            pack_id: manifest.pack_id.clone(),
            required: req_str.as_str().to_owned(),
            actual: platform_version.to_owned(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Dependency resolution
// ---------------------------------------------------------------------------

/// Resolved report for a pack: all assets + sources transitively reachable
/// from its `dependencies`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPack {
    pub pack_id: PackId,
    pub revision: PackRevision,
    /// All asset ids (direct).
    pub assets: Vec<AssetId>,
    /// All source ids (direct).
    pub sources: Vec<SourceId>,
    /// Deps that resolved successfully.
    pub resolved_deps: Vec<DependencyRef>,
    /// Deps that could not be resolved, paired with the roles. Optional
    /// deps (role == any, optional == true) end up here as warnings but
    /// don't cause an error return.
    pub unresolved_optional_deps: Vec<DependencyRef>,
}

/// Resolution context: the registries to check.
pub struct DepResolverCtx<'a> {
    pub recipes: &'a RecipeArtifactRegistry,
    pub sources: &'a SourceRegistry,
}

impl<'a> DepResolverCtx<'a> {
    /// Check whether a `DependencyRef` resolves in the available registries.
    fn resolves(&self, dep: &DependencyRef) -> bool {
        let kind = dep.target_kind.as_str();
        let id = &dep.target_id;
        if kind.starts_with("recipe") {
            return self.recipes.get(id).is_some();
        }
        if kind == "source" {
            // Source deps carry the source id in target_id.as_str().
            // Accept any registered revision for that source_id; the pin
            // is documentation — actual source validation is at publication.
            let source_id = super::identity::SourceId::new(id.as_str());
            return self.sources.entries.contains_key(&source_id);
        }
        // Unknown kinds: pessimistically unresolvable so future kinds
        // get an explicit error rather than silent success.
        false
    }
}

/// Resolve the direct dependencies of `manifest` against `ctx`.
///
/// Returns `Ok(ResolvedPack)` if all required deps are resolvable.
/// Optional deps that are missing are recorded in
/// `unresolved_optional_deps`.
pub fn resolve_pack_deps(
    manifest: &PackManifest,
    ctx: &DepResolverCtx<'_>,
) -> Result<ResolvedPack, PackError> {
    let mut resolved_deps = Vec::new();
    let mut unresolved_optional = Vec::new();

    for dep in &manifest.dependencies {
        if ctx.resolves(dep) {
            resolved_deps.push(dep.clone());
        } else if dep.optional {
            unresolved_optional.push(dep.clone());
        } else {
            return Err(PackError::UnresolvedDependency {
                pack_id: manifest.pack_id.clone(),
                dep: dep.clone(),
            });
        }
    }

    Ok(ResolvedPack {
        pack_id: manifest.pack_id.clone(),
        revision: manifest.revision.clone(),
        assets: manifest.assets.clone(),
        sources: manifest.sources.clone(),
        resolved_deps,
        unresolved_optional_deps: unresolved_optional,
    })
}

// ---------------------------------------------------------------------------
// Cycle detection
// ---------------------------------------------------------------------------

/// Detect cyclic dependencies among the registered packs. Cycles are
/// expressed via `PackManifest.dependencies` that target other packs by
/// `target_kind == "pack"`. Returns the first cycle found, or `Ok(())`.
pub fn detect_cycles(registry: &PackRegistry) -> Result<(), PackError> {
    // Build adjacency as `&str → Vec<&str>` — both `PackId` and `AssetId`
    // are newtype Strings so we unify by borrowing their inner strs.
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for manifest in registry.iter() {
        let children: Vec<&str> = manifest
            .dependencies
            .iter()
            .filter(|d| d.target_kind.as_str() == "pack")
            .map(|d| d.target_id.as_str())
            .collect();
        adj.insert(manifest.pack_id.as_str(), children);
    }

    // DFS-based cycle detection using three-colour marking.
    #[derive(PartialEq)]
    enum Mark {
        White,
        Gray,
        Black,
    }
    let mut marks: BTreeMap<&str, Mark> = BTreeMap::new();
    let mut path: Vec<&str> = Vec::new();

    fn dfs<'a>(
        node: &'a str,
        adj: &'a BTreeMap<&'a str, Vec<&'a str>>,
        marks: &mut BTreeMap<&'a str, Mark>,
        path: &mut Vec<&'a str>,
    ) -> Option<Vec<PackId>> {
        marks.insert(node, Mark::Gray);
        path.push(node);

        if let Some(children) = adj.get(node) {
            for &child in children {
                match marks.get(child).unwrap_or(&Mark::White) {
                    Mark::Gray => {
                        let start = path.iter().position(|&p| p == child).unwrap_or(0);
                        let mut cycle: Vec<PackId> =
                            path[start..].iter().map(|&p| PackId::new(p)).collect();
                        cycle.push(PackId::new(child));
                        return Some(cycle);
                    }
                    Mark::Black => {}
                    Mark::White => {
                        if let Some(cycle) = dfs(child, adj, marks, path) {
                            return Some(cycle);
                        }
                    }
                }
            }
        }
        path.pop();
        marks.insert(node, Mark::Black);
        None
    }

    let nodes: Vec<&str> = adj.keys().copied().collect();
    for node in nodes {
        if marks.get(node).unwrap_or(&Mark::White) == &Mark::White {
            if let Some(cycle) = dfs(node, &adj, &mut marks, &mut path) {
                return Err(PackError::CyclicDependency { cycle });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Compatibility report
// ---------------------------------------------------------------------------

/// Check the compatibility declarations of a manifest against the running
/// platform. Returns all findings (green = empty vec, yellow/red have
/// Advisory/Error entries).
pub fn check_pack_compatibility(
    manifest: &PackManifest,
    platform_version: &str,
) -> Vec<CompatFinding> {
    let mut findings = Vec::new();

    // core_api check.
    if let Some(req_str) = &manifest.compatibility.core_api {
        match semver::VersionReq::parse(req_str.as_str()) {
            Err(_) => findings.push(CompatFinding {
                severity: CompatFindingSeverity::Error,
                code: "curation.pack.core_api_unparseable",
                message: format!(
                    "pack '{}': core_api requirement '{}' is not valid semver",
                    manifest.pack_id.as_str(),
                    req_str.as_str(),
                ),
            }),
            Ok(req) => match semver::Version::parse(platform_version) {
                Err(_) => findings.push(CompatFinding {
                    severity: CompatFindingSeverity::Error,
                    code: "curation.pack.platform_version_unparseable",
                    message: format!(
                        "running platform version '{platform_version}' is not valid semver",
                    ),
                }),
                Ok(ver) => {
                    if !req.matches(&ver) {
                        findings.push(CompatFinding {
                            severity: CompatFindingSeverity::Error,
                            code: "curation.pack.core_api_mismatch",
                            message: format!(
                                "pack '{}': core_api requires '{}', running '{}'",
                                manifest.pack_id.as_str(),
                                req_str.as_str(),
                                platform_version,
                            ),
                        });
                    }
                }
            },
        }
    }

    // body_schema check (advisory — migration out of scope).
    if let Some(schema) = &manifest.compatibility.body_schema {
        // We don't have a live schema-version registry here (that's
        // capability-crate territory), so we emit a static advisory
        // noting the pin for callers to act on.
        findings.push(CompatFinding {
            severity: CompatFindingSeverity::Advisory,
            code: "curation.pack.body_schema_pinned",
            message: format!(
                "pack '{}': body_schema pinned to kind '{}' v{}; verify migration status",
                manifest.pack_id.as_str(),
                schema.kind.as_str(),
                schema.version,
            ),
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Convenience: load_pack with default allow-all entitlement
// ---------------------------------------------------------------------------

/// Load with the default `AllowAllEntitlements` resolver. Useful for
/// tests and non-commercial open-pack flows.
pub fn load_pack_open(
    path: &Path,
    platform_version: &str,
    registry: &mut PackRegistry,
) -> Result<PackRef, PackError> {
    load_pack(
        path,
        platform_version,
        &Actor::new("open"),
        &AllowAllEntitlements,
        registry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::compatibility::VersionReq;
    use crate::curation::entitlement::AlwaysDenyEntitlements;
    use crate::curation::identity::{
        AssetId, AssetKindId, AssetRevision, PackId, PackRevision, SourceId,
    };

    fn minimal_manifest(pack_id: &str, revision: &str) -> PackManifest {
        PackManifest::new(
            PackId::new(pack_id),
            PackRevision::new(revision),
            "test pack",
        )
    }

    #[test]
    fn pack_ref_roundtrips() {
        let r = PackRef::new(
            PackId::new("talos3d_architecture_se"),
            PackRevision::new("v1"),
        );
        let json = serde_json::to_string(&r).unwrap();
        let parsed: PackRef = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn pack_manifest_new_has_empty_collections() {
        let m = PackManifest::new(
            PackId::new("open_pack"),
            PackRevision::new("v0"),
            "Open Pack",
        );
        assert!(m.assets.is_empty());
        assert!(m.sources.is_empty());
        assert!(m.entitlement.is_none());
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn pack_manifest_roundtrips_with_content() {
        let m = PackManifest {
            pack_id: PackId::new("talos3d_architecture_se"),
            revision: PackRevision::new("v1"),
            label: "Sweden jurisdiction pack".into(),
            assets: vec![AssetId::new("recipe.v1/stair_straight_residential")],
            sources: vec![SourceId::new("boverket.bbr.8")],
            compatibility: CompatibilityRef::for_core(VersionReq::new("^0.1")),
            entitlement: Some(EntitlementHook::new("appverket/paddle/SKU-SE-BBR-01")),
            dependencies: vec![],
        };
        let json = serde_json::to_string(&m).unwrap();
        let parsed: PackManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn as_ref_constructs_packref() {
        let m = PackManifest::new(PackId::new("p"), PackRevision::new("r"), "");
        assert_eq!(
            m.as_ref(),
            PackRef::new(PackId::new("p"), PackRevision::new("r"))
        );
    }

    // --- PackRegistry ---

    #[test]
    fn registry_register_and_get() {
        let mut reg = PackRegistry::default();
        let m = minimal_manifest("pack_a", "v1");
        let pack_ref = reg.register(m).unwrap();
        assert_eq!(pack_ref.pack_id.as_str(), "pack_a");
        assert!(reg
            .get(&PackId::new("pack_a"), &PackRevision::new("v1"))
            .is_some());
    }

    #[test]
    fn registry_register_duplicate_returns_error() {
        let mut reg = PackRegistry::default();
        reg.register(minimal_manifest("pack_a", "v1")).unwrap();
        let err = reg.register(minimal_manifest("pack_a", "v1")).unwrap_err();
        assert!(matches!(err, PackError::AlreadyRegistered { .. }));
    }

    #[test]
    fn registry_latest_returns_lexicographically_last_revision() {
        let mut reg = PackRegistry::default();
        reg.register(minimal_manifest("pack_a", "v1")).unwrap();
        reg.register(minimal_manifest("pack_a", "v2")).unwrap();
        let latest = reg.latest(&PackId::new("pack_a")).unwrap();
        assert_eq!(latest.revision.as_str(), "v2");
    }

    // --- parse_manifest ---

    #[test]
    fn parse_json_manifest() {
        let json = serde_json::to_string(&minimal_manifest("p", "v1")).unwrap();
        let parsed = parse_manifest(json.as_bytes(), "test.json").unwrap();
        assert_eq!(parsed.pack_id.as_str(), "p");
    }

    #[test]
    fn parse_toml_manifest() {
        let toml_src = r#"
pack_id = "toml_pack"
revision = "v1"
label = "A TOML pack"

[compatibility]
"#;
        let parsed = parse_manifest(toml_src.as_bytes(), "test.toml").unwrap();
        assert_eq!(parsed.pack_id.as_str(), "toml_pack");
    }

    #[test]
    fn parse_bad_json_returns_parse_error() {
        let err = parse_manifest(b"not json", "bad.json").unwrap_err();
        assert!(matches!(err, PackError::Parse { .. }));
    }

    // --- check_core_api_compat ---

    #[test]
    fn core_api_compat_passes_when_unconstrained() {
        let m = minimal_manifest("p", "v1");
        assert!(check_core_api_compat(&m, "0.1.0").is_ok());
    }

    #[test]
    fn core_api_compat_passes_matching_requirement() {
        let mut m = minimal_manifest("p", "v1");
        m.compatibility.core_api = Some(VersionReq::new("^0.1"));
        assert!(check_core_api_compat(&m, "0.1.0").is_ok());
    }

    #[test]
    fn core_api_compat_fails_mismatched_requirement() {
        let mut m = minimal_manifest("p", "v1");
        m.compatibility.core_api = Some(VersionReq::new("^99.0"));
        let err = check_core_api_compat(&m, "0.1.0").unwrap_err();
        assert!(matches!(err, PackError::CoreApiMismatch { .. }));
        assert!(err.to_string().contains("core_api"));
    }

    // --- check_pack_compatibility ---

    #[test]
    fn check_compat_empty_for_unconstrained() {
        let m = minimal_manifest("p", "v1");
        assert!(check_pack_compatibility(&m, "0.1.0").is_empty());
    }

    #[test]
    fn check_compat_error_for_mismatched_core_api() {
        let mut m = minimal_manifest("p", "v1");
        m.compatibility.core_api = Some(VersionReq::new("^99.0"));
        let findings = check_pack_compatibility(&m, "0.1.0");
        assert!(findings.iter().any(|f| f.is_error()));
        assert!(findings
            .iter()
            .any(|f| f.code == "curation.pack.core_api_mismatch"));
    }

    #[test]
    fn check_compat_advisory_for_body_schema_pin() {
        use crate::curation::compatibility::SchemaVersion;
        let mut m = minimal_manifest("p", "v1");
        m.compatibility.body_schema = Some(SchemaVersion {
            kind: AssetKindId::new("recipe.v1"),
            version: 1,
        });
        let findings = check_pack_compatibility(&m, "0.1.0");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, CompatFindingSeverity::Advisory);
        assert_eq!(findings[0].code, "curation.pack.body_schema_pinned");
    }

    // --- cycle detection ---

    #[test]
    fn detect_cycles_passes_on_empty_registry() {
        let reg = PackRegistry::default();
        assert!(detect_cycles(&reg).is_ok());
    }

    #[test]
    fn detect_cycles_passes_with_acyclic_deps() {
        let mut reg = PackRegistry::default();
        let mut m_a = minimal_manifest("a", "v1");
        m_a.dependencies.push(DependencyRef {
            target_kind: AssetKindId::new("pack"),
            target_id: AssetId::new("b"),
            revision: AssetRevision::initial(),
            role: crate::curation::dependencies::DependencyRole::Execution,
            optional: false,
        });
        reg.register(m_a).unwrap();
        reg.register(minimal_manifest("b", "v1")).unwrap();
        assert!(detect_cycles(&reg).is_ok());
    }

    #[test]
    fn detect_cycles_detects_direct_cycle() {
        let mut reg = PackRegistry::default();
        let mut m_a = minimal_manifest("a", "v1");
        m_a.dependencies.push(DependencyRef {
            target_kind: AssetKindId::new("pack"),
            target_id: AssetId::new("b"),
            revision: AssetRevision::initial(),
            role: crate::curation::dependencies::DependencyRole::Execution,
            optional: false,
        });
        let mut m_b = minimal_manifest("b", "v1");
        m_b.dependencies.push(DependencyRef {
            target_kind: AssetKindId::new("pack"),
            target_id: AssetId::new("a"),
            revision: AssetRevision::initial(),
            role: crate::curation::dependencies::DependencyRole::Execution,
            optional: false,
        });
        reg.register(m_a).unwrap();
        reg.register(m_b).unwrap();
        let err = detect_cycles(&reg).unwrap_err();
        assert!(matches!(err, PackError::CyclicDependency { .. }));
    }

    // --- entitlement ---

    #[test]
    fn load_pack_denied_entitlement_returns_error() {
        // Use a temp file for the manifest bytes.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pack.json");
        let mut f = std::fs::File::create(&path).unwrap();
        let m = PackManifest {
            entitlement: Some(EntitlementHook::new("gated")),
            ..minimal_manifest("gated_pack", "v1")
        };
        f.write_all(serde_json::to_string(&m).unwrap().as_bytes())
            .unwrap();

        let mut reg = PackRegistry::default();
        let resolver = AlwaysDenyEntitlements::new("test denial");
        let err =
            load_pack(&path, "0.1.0", &Actor::new("user:x"), &resolver, &mut reg).unwrap_err();
        assert!(matches!(err, PackError::EntitlementDenied { .. }));
        assert!(reg
            .get(&PackId::new("gated_pack"), &PackRevision::new("v1"))
            .is_none());
    }

    #[test]
    fn load_pack_open_succeeds_for_unconstrained_manifest() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pack.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            serde_json::to_string(&minimal_manifest("open", "v1"))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();

        let mut reg = PackRegistry::default();
        let pack_ref = load_pack_open(&path, "0.1.0", &mut reg).unwrap();
        assert_eq!(pack_ref.pack_id.as_str(), "open");
    }
}
