//! `CuratedManifest` — generic manifest mechanism per ADR-042 §7.
//!
//! A *manifest kind* is a domain contract that lives in a capability crate
//! (architecture, naval, mechanical, …). A *manifest* is a single instance
//! of a kind, authored as data and embedding `CurationMeta`. Core owns
//! only the registration substrate: the `ManifestKindDescriptor`
//! registered by the capability crate, the generic walker that
//! enumerates outbound references from a manifest body, and the registry
//! resources that hold both.
//!
//! Core does not interpret domain semantics. Body shape is opaque
//! `serde_json::Value`; the kind's `body_schema` (JSON Schema) describes
//! the expected layout but runtime validation against the schema is left
//! to later work — this module's responsibilities are:
//!
//! 1. Hold `ManifestKindDescriptor`s indexed by `ManifestKindId`.
//! 2. Hold `CuratedManifest`s indexed by `AssetId`.
//! 3. Enumerate outbound references declared by `walker_hooks`,
//!    grouped by target kind. This is what powers cross-kind
//!    dependency reporting and orphan detection.
//!
//! Per ADR-042 the canonical example is `ConstructionSystemManifest`,
//! registered by `talos3d-architecture-core`. Core itself ships no
//! domain manifests.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::identity::{AssetId, AssetKindId};
use super::meta::CurationMeta;

/// Kind identifier for the curated-manifest asset kind itself. The
/// `manifest_kind` *of a manifest* refers to a `ManifestKindId`
/// registered by a capability crate (e.g. `"construction_system_manifest.v1"`),
/// not to this constant. This constant is what `CurationMeta.kind`
/// carries when the asset *is* a manifest.
pub const CURATED_MANIFEST_ASSET_KIND: &str = "curated_manifest.v1";

/// Identifier of a manifest kind contract. Conventionally
/// `"<domain>_manifest.v<N>"`, e.g. `"construction_system_manifest.v1"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ManifestKindId(pub String);

impl ManifestKindId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ManifestKindId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Declares a field within a manifest body that holds an outbound
/// reference to another curated asset. Used by the walker.
///
/// `path` is a slash-rooted path with `*` as a wildcard for arrays:
///
/// - `/concept_ref` — single string id at the top level
/// - `/variants/*/recipe_refs/*` — array of arrays of string ids
/// - `/pattern_refs/*` — array of string ids
///
/// Path segments other than `*` are matched as JSON object keys.
/// The walker resolves the path against the manifest body and treats
/// every resolved string value as an `AssetId` of `target_kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RefField {
    pub path: String,
    pub target_kind: AssetKindId,
    /// If `true`, missing values at this path are tolerated (the walker
    /// emits no reference). If `false`, a missing required reference is
    /// reported as a structural problem by `CuratedManifestRegistry::
    /// validate_refs`.
    #[serde(default)]
    pub required: bool,
}

impl RefField {
    pub fn new(path: impl Into<String>, target_kind: AssetKindId) -> Self {
        Self {
            path: path.into(),
            target_kind,
            required: false,
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

/// Contract for a manifest kind: identity, expected body shape, and
/// declared reference fields the walker should enumerate.
///
/// Capability crates register one `ManifestKindDescriptor` per kind they
/// own. Core uses the registered descriptor to walk and validate every
/// authored manifest of that kind without needing to understand domain
/// semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ManifestKindDescriptor {
    pub kind_id: ManifestKindId,
    /// JSON Schema describing the expected body shape. Held opaquely
    /// here; runtime body validation against this schema is a future
    /// follow-up — the kernel currently treats it as documentation that
    /// rides with the descriptor for tooling and human inspection.
    pub body_schema: Value,
    /// Reference fields the generic walker should enumerate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub walker_hooks: Vec<RefField>,
    /// Optional human-readable description of the kind. Used by the
    /// curation MCP surface and by tooling. Not load-bearing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl ManifestKindDescriptor {
    pub fn new(kind_id: ManifestKindId, body_schema: Value) -> Self {
        Self {
            kind_id,
            body_schema,
            walker_hooks: Vec::new(),
            description: String::new(),
        }
    }

    pub fn with_walker_hook(mut self, hook: RefField) -> Self {
        self.walker_hooks.push(hook);
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// Registry of `ManifestKindDescriptor`s, indexed by kind id. Capability
/// crates fill this resource at startup; the registry is the only place
/// the curation kernel looks up what a manifest body is allowed to
/// contain.
#[derive(Resource, Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestKindRegistry {
    pub descriptors: BTreeMap<ManifestKindId, ManifestKindDescriptor>,
}

impl ManifestKindRegistry {
    pub fn register(&mut self, descriptor: ManifestKindDescriptor) -> ManifestKindId {
        let id = descriptor.kind_id.clone();
        self.descriptors.insert(id.clone(), descriptor);
        id
    }

    pub fn get(&self, id: &ManifestKindId) -> Option<&ManifestKindDescriptor> {
        self.descriptors.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ManifestKindDescriptor> {
        self.descriptors.values()
    }

    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }
}

/// A curated manifest authored as data. The `manifest_kind` field
/// references a `ManifestKindId` registered in `ManifestKindRegistry`;
/// the `body` is opaque JSON whose shape is governed by the kind's
/// `body_schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CuratedManifest {
    pub meta: CurationMeta,
    pub manifest_kind: ManifestKindId,
    pub body: Value,
}

impl CuratedManifest {
    pub fn asset_kind() -> AssetKindId {
        AssetKindId::new(CURATED_MANIFEST_ASSET_KIND)
    }

    /// Deterministic asset id slug for a manifest:
    /// `curated_manifest.v1/<manifest_kind>/<local_id>`.
    pub fn asset_id_for(manifest_kind: &ManifestKindId, local_id: &str) -> AssetId {
        AssetId::new(format!(
            "{CURATED_MANIFEST_ASSET_KIND}/{}/{}",
            manifest_kind.0, local_id
        ))
    }
}

/// Registry of authored `CuratedManifest`s, indexed by asset id. Plus a
/// `manifest_kind → asset_id` index for kind-scoped iteration.
#[derive(Resource, Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct CuratedManifestRegistry {
    pub entries: BTreeMap<AssetId, CuratedManifest>,
    /// `manifest_kind → asset_ids` index. Stored as a sorted vector
    /// inside the map so iteration order is deterministic.
    pub by_kind: BTreeMap<ManifestKindId, Vec<AssetId>>,
}

impl CuratedManifestRegistry {
    pub fn insert(&mut self, manifest: CuratedManifest) -> AssetId {
        let asset_id = manifest.meta.id.clone();
        let kind = manifest.manifest_kind.clone();
        let bucket = self.by_kind.entry(kind).or_default();
        if !bucket.contains(&asset_id) {
            bucket.push(asset_id.clone());
            bucket.sort();
        }
        self.entries.insert(asset_id.clone(), manifest);
        asset_id
    }

    pub fn get(&self, id: &AssetId) -> Option<&CuratedManifest> {
        self.entries.get(id)
    }

    pub fn get_by_kind<'a>(
        &'a self,
        kind: &ManifestKindId,
    ) -> impl Iterator<Item = &'a CuratedManifest> {
        self.by_kind
            .get(kind)
            .into_iter()
            .flat_map(move |ids| ids.iter())
            .filter_map(move |id| self.entries.get(id))
    }

    pub fn iter(&self) -> impl Iterator<Item = &CuratedManifest> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Walk every authored manifest, return the full set of outbound
    /// references grouped by target asset kind. Reads the
    /// `walker_hooks` of each manifest's kind from `kinds`. Manifests
    /// whose kind is not registered in `kinds` are skipped — the
    /// caller can detect that condition with
    /// [`Self::manifests_with_unknown_kind`].
    pub fn enumerate_outbound_refs(
        &self,
        kinds: &ManifestKindRegistry,
    ) -> BTreeMap<AssetKindId, BTreeSet<AssetId>> {
        let mut out: BTreeMap<AssetKindId, BTreeSet<AssetId>> = BTreeMap::new();
        for manifest in self.entries.values() {
            let Some(descriptor) = kinds.get(&manifest.manifest_kind) else {
                continue;
            };
            for hook in &descriptor.walker_hooks {
                for resolved in resolve_path(&manifest.body, &hook.path) {
                    if let Some(s) = resolved.as_str() {
                        out.entry(hook.target_kind.clone())
                            .or_default()
                            .insert(AssetId::new(s));
                    }
                }
            }
        }
        out
    }

    /// Return the manifest ids whose `manifest_kind` is not registered
    /// in `kinds`. Used by integration tests and orphan detection.
    pub fn manifests_with_unknown_kind(&self, kinds: &ManifestKindRegistry) -> Vec<AssetId> {
        self.entries
            .iter()
            .filter(|(_, m)| kinds.get(&m.manifest_kind).is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Per-manifest reference walk. Returns every outbound `(target_kind,
    /// asset_id)` declared by the manifest body via its kind's walker
    /// hooks, plus a list of `RefField`s that were marked `required` but
    /// resolved to no value. `kinds` provides the descriptor lookup.
    pub fn walk_manifest<'a>(
        &'a self,
        id: &AssetId,
        kinds: &ManifestKindRegistry,
    ) -> Option<ManifestWalkReport> {
        let manifest = self.entries.get(id)?;
        let descriptor = kinds.get(&manifest.manifest_kind)?;
        let mut refs: Vec<(AssetKindId, AssetId)> = Vec::new();
        let mut missing_required: Vec<RefField> = Vec::new();
        for hook in &descriptor.walker_hooks {
            let resolved: Vec<&Value> = resolve_path(&manifest.body, &hook.path).collect();
            let mut emitted_any = false;
            for value in resolved {
                if let Some(s) = value.as_str() {
                    refs.push((hook.target_kind.clone(), AssetId::new(s)));
                    emitted_any = true;
                }
            }
            if hook.required && !emitted_any {
                missing_required.push(hook.clone());
            }
        }
        Some(ManifestWalkReport {
            manifest_id: id.clone(),
            kind: manifest.manifest_kind.clone(),
            refs,
            missing_required,
        })
    }
}

/// Bevy `App` extension methods for registering manifest kinds and
/// authored manifests at plugin build time. Capability crates use these
/// to install their kind descriptors and any shipped manifests in the
/// same shape they already use for `register_recipe_family`,
/// `register_assembly_pattern`, etc.
pub trait CuratedManifestAppExt {
    fn register_manifest_kind(&mut self, descriptor: ManifestKindDescriptor) -> &mut Self;
    fn register_curated_manifest(&mut self, manifest: CuratedManifest) -> &mut Self;
}

impl CuratedManifestAppExt for App {
    fn register_manifest_kind(&mut self, descriptor: ManifestKindDescriptor) -> &mut Self {
        if !self.world().contains_resource::<ManifestKindRegistry>() {
            self.init_resource::<ManifestKindRegistry>();
        }
        let mut registry = self.world_mut().resource_mut::<ManifestKindRegistry>();
        registry.register(descriptor);
        self
    }

    fn register_curated_manifest(&mut self, manifest: CuratedManifest) -> &mut Self {
        if !self.world().contains_resource::<CuratedManifestRegistry>() {
            self.init_resource::<CuratedManifestRegistry>();
        }
        let mut registry = self.world_mut().resource_mut::<CuratedManifestRegistry>();
        registry.insert(manifest);
        self
    }
}

/// Result of walking a single manifest body via its kind's `walker_hooks`.
#[derive(Debug, Clone, PartialEq)]
pub struct ManifestWalkReport {
    pub manifest_id: AssetId,
    pub kind: ManifestKindId,
    /// Every outbound reference declared by the body, as
    /// `(target_kind, target_asset_id)` pairs in walker-declared order.
    pub refs: Vec<(AssetKindId, AssetId)>,
    /// `RefField` entries that were `required: true` but resolved to no
    /// value in the body.
    pub missing_required: Vec<RefField>,
}

/// Resolve a slash-rooted path with `*` wildcards into a borrowing
/// iterator over matching JSON values inside `root`. Path segments
/// other than `*` are matched as JSON object keys; a `*` segment
/// expands to all values when the current node is an array, or to all
/// object values when it is an object.
///
/// Examples:
///
/// - `"/concept_ref"` → `[ root["concept_ref"] ]`
/// - `"/variants/*/recipe_refs/*"` → every recipe_ref string in every variant
fn resolve_path<'a>(root: &'a Value, path: &str) -> impl Iterator<Item = &'a Value> + 'a {
    let segments: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut current: Vec<&'a Value> = vec![root];
    for segment in segments {
        let mut next: Vec<&'a Value> = Vec::new();
        for node in current {
            match (segment.as_str(), node) {
                ("*", Value::Array(arr)) => next.extend(arr.iter()),
                ("*", Value::Object(map)) => next.extend(map.values()),
                ("*", _) => {}
                (key, Value::Object(map)) => {
                    if let Some(v) = map.get(key) {
                        next.push(v);
                    }
                }
                _ => {}
            }
        }
        current = next;
    }
    current.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::provenance::{Confidence, Lineage, Provenance};
    use crate::curation::scope_trust::{Scope, Trust};
    use crate::plugins::refinement::AgentId;

    fn sample_meta(asset_id: AssetId) -> CurationMeta {
        CurationMeta::new(
            asset_id,
            CuratedManifest::asset_kind(),
            Provenance {
                author: AgentId("test".into()),
                confidence: Confidence::High,
                lineage: Lineage::Freeform,
                rationale: None,
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        )
        .with_scope(Scope::Project)
        .with_trust(Trust::Draft)
    }

    fn sample_manifest(kind: &str, local_id: &str, body: Value) -> CuratedManifest {
        let kind = ManifestKindId::new(kind);
        let id = CuratedManifest::asset_id_for(&kind, local_id);
        CuratedManifest {
            meta: sample_meta(id),
            manifest_kind: kind,
            body,
        }
    }

    #[test]
    fn asset_kind_constant_matches() {
        assert_eq!(CuratedManifest::asset_kind().as_str(), CURATED_MANIFEST_ASSET_KIND);
    }

    #[test]
    fn asset_id_for_uses_deterministic_slug() {
        let id = CuratedManifest::asset_id_for(
            &ManifestKindId::new("construction_system_manifest.v1"),
            "roof.truss.attic",
        );
        assert_eq!(
            id.as_str(),
            "curated_manifest.v1/construction_system_manifest.v1/roof.truss.attic"
        );
    }

    #[test]
    fn manifest_kind_descriptor_round_trips() {
        let descriptor = ManifestKindDescriptor::new(
            ManifestKindId::new("construction_system_manifest.v1"),
            serde_json::json!({"type": "object"}),
        )
        .with_walker_hook(
            RefField::new("/concept_ref", AssetKindId::new("vocabulary_concept.v1")).required(),
        )
        .with_walker_hook(RefField::new(
            "/variants/*/recipe_refs/*",
            AssetKindId::new("recipe.v1"),
        ))
        .with_description("Architecture-owned manifest kind");
        let json = serde_json::to_string(&descriptor).unwrap();
        let parsed: ManifestKindDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, descriptor);
    }

    #[test]
    fn registry_register_and_lookup() {
        let mut registry = ManifestKindRegistry::default();
        let id = registry.register(ManifestKindDescriptor::new(
            ManifestKindId::new("construction_system_manifest.v1"),
            serde_json::json!({}),
        ));
        assert_eq!(registry.len(), 1);
        assert!(registry.get(&id).is_some());
    }

    #[test]
    fn curated_manifest_registry_groups_by_kind() {
        let mut reg = CuratedManifestRegistry::default();
        let m1 = sample_manifest(
            "construction_system_manifest.v1",
            "roof.truss.attic",
            serde_json::json!({}),
        );
        let m2 = sample_manifest(
            "construction_system_manifest.v1",
            "roof.truss.scissor",
            serde_json::json!({}),
        );
        let m3 = sample_manifest(
            "naval_hull_manifest.v1",
            "round_bilge",
            serde_json::json!({}),
        );
        reg.insert(m1);
        reg.insert(m2);
        reg.insert(m3);
        assert_eq!(reg.len(), 3);
        let arch_kind = ManifestKindId::new("construction_system_manifest.v1");
        let arch: Vec<&CuratedManifest> = reg.get_by_kind(&arch_kind).collect();
        assert_eq!(arch.len(), 2);
    }

    #[test]
    fn enumerate_outbound_refs_walks_required_and_array_paths() {
        let mut kinds = ManifestKindRegistry::default();
        kinds.register(
            ManifestKindDescriptor::new(
                ManifestKindId::new("construction_system_manifest.v1"),
                serde_json::json!({}),
            )
            .with_walker_hook(
                RefField::new("/concept_ref", AssetKindId::new("vocabulary_concept.v1")).required(),
            )
            .with_walker_hook(RefField::new(
                "/variants/*/recipe_refs/*",
                AssetKindId::new("recipe.v1"),
            )),
        );
        let mut manifests = CuratedManifestRegistry::default();
        manifests.insert(sample_manifest(
            "construction_system_manifest.v1",
            "roof.truss.attic",
            serde_json::json!({
                "concept_ref": "vocabulary_concept.v1/roof.truss.attic",
                "variants": {
                    "storage": {
                        "recipe_refs": ["recipe.v1/attic_truss_storage_schematic"]
                    },
                    "room": {
                        "recipe_refs": ["recipe.v1/attic_truss_room_schematic"]
                    }
                }
            }),
        ));
        let refs = manifests.enumerate_outbound_refs(&kinds);
        let concept_kind = AssetKindId::new("vocabulary_concept.v1");
        let recipe_kind = AssetKindId::new("recipe.v1");
        let concept_refs = refs.get(&concept_kind).unwrap();
        assert!(concept_refs.contains(&AssetId::new("vocabulary_concept.v1/roof.truss.attic")));
        let recipe_refs = refs.get(&recipe_kind).unwrap();
        assert_eq!(recipe_refs.len(), 2);
        assert!(recipe_refs.contains(&AssetId::new("recipe.v1/attic_truss_storage_schematic")));
        assert!(recipe_refs.contains(&AssetId::new("recipe.v1/attic_truss_room_schematic")));
    }

    #[test]
    fn walk_manifest_reports_missing_required_refs() {
        let mut kinds = ManifestKindRegistry::default();
        kinds.register(
            ManifestKindDescriptor::new(
                ManifestKindId::new("construction_system_manifest.v1"),
                serde_json::json!({}),
            )
            .with_walker_hook(
                RefField::new("/concept_ref", AssetKindId::new("vocabulary_concept.v1")).required(),
            ),
        );
        let mut manifests = CuratedManifestRegistry::default();
        let id = manifests.insert(sample_manifest(
            "construction_system_manifest.v1",
            "no_concept",
            serde_json::json!({}),
        ));
        let report = manifests.walk_manifest(&id, &kinds).expect("manifest exists");
        assert!(report.refs.is_empty());
        assert_eq!(report.missing_required.len(), 1);
        assert_eq!(report.missing_required[0].path, "/concept_ref");
    }

    #[test]
    fn manifests_with_unknown_kind_is_detected() {
        let kinds = ManifestKindRegistry::default(); // empty — no kinds registered
        let mut manifests = CuratedManifestRegistry::default();
        let id = manifests.insert(sample_manifest(
            "construction_system_manifest.v1",
            "orphan",
            serde_json::json!({}),
        ));
        let unknown = manifests.manifests_with_unknown_kind(&kinds);
        assert_eq!(unknown, vec![id]);
    }

    #[test]
    fn resolve_path_handles_root_object_keys() {
        let body = serde_json::json!({"a": {"b": "c"}});
        let resolved: Vec<&Value> = resolve_path(&body, "/a/b").collect();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], &Value::String("c".into()));
    }

    #[test]
    fn resolve_path_handles_array_wildcard() {
        let body = serde_json::json!({"items": ["a", "b", "c"]});
        let resolved: Vec<&Value> = resolve_path(&body, "/items/*").collect();
        assert_eq!(resolved.len(), 3);
    }

    #[test]
    fn resolve_path_handles_object_wildcard() {
        let body = serde_json::json!({"variants": {"storage": "x", "room": "y"}});
        let resolved: Vec<&Value> = resolve_path(&body, "/variants/*").collect();
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn resolve_path_returns_empty_for_missing_segments() {
        let body = serde_json::json!({"a": 1});
        assert_eq!(resolve_path(&body, "/missing").count(), 0);
        assert_eq!(resolve_path(&body, "/a/missing").count(), 0);
    }

    #[test]
    fn curated_manifest_round_trips() {
        let manifest = sample_manifest(
            "construction_system_manifest.v1",
            "round_trip",
            serde_json::json!({
                "concept_ref": "vocabulary_concept.v1/x",
                "variants": [{"id": "default", "recipe_refs": ["recipe.v1/foo"]}]
            }),
        );
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: CuratedManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, manifest);
    }
}
