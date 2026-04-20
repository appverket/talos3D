//! Pack manifest types and the entitlement hook.
//!
//! `PackManifest` is the shipping/integrity/revision/dependency unit per
//! ADR-040. Entitlement is an orthogonal commercial policy: packs may
//! carry an optional entitlement hook, but resolution lives outside the
//! pack model (operator layer).
//!
//! Runtime behavior — loading manifests from disk, resolving deps,
//! enforcing compatibility — lands in PP84.

use serde::{Deserialize, Serialize};

use super::compatibility::CompatibilityRef;
use super::identity::{AssetId, PackId, PackRevision, SourceId};

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
/// `EntitlementResolver` implementation (landing in PP84) looks this up.
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
///
/// The manifest lists asset and source ids only; the assets and source
/// entries themselves live in the shared `AssetRegistry` / `SourceRegistry`
/// instances at load time. This keeps manifests small and makes content-
/// addressed deduplication across packs straightforward.
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
        }
    }

    pub fn as_ref(&self) -> PackRef {
        PackRef::new(self.pack_id.clone(), self.revision.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::compatibility::VersionReq;

    #[test]
    fn pack_ref_roundtrips() {
        let r = PackRef::new(PackId::new("talos3d_architecture_se"), PackRevision::new("v1"));
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
}
