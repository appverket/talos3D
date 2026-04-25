//! Identity and revisioning newtypes for curated assets, sources, and packs.
//!
//! All ids are `String` newtypes so they serialize as plain strings, round-
//! trip through MCP without wrapping, and remain human-readable. The
//! substrate does not assume any particular generation scheme (uuid, slug,
//! hash, etc.); capability crates pick the scheme that suits their kind.

use serde::{Deserialize, Serialize};

/// Stable opaque identifier for a curated asset (recipe, material spec,
/// product entry, code rule pack, future kinds).
///
/// Capability crates choose how to mint these. For shipped assets derived
/// from a descriptor registered at startup, a deterministic slug
/// (`"<kind>/<family_or_id>"`) is recommended so ids are stable across
/// restarts. For session-scope drafts, any uuid-shaped string is fine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct AssetId(pub String);

impl AssetId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Open string identifier of a curated asset kind.
///
/// By convention, kind ids are `"<domain>.v<N>"` (e.g. `"recipe.v1"`,
/// `"material_spec.v1"`). The `v<N>` suffix carries the body-schema major
/// version — when a kind's body schema gets a breaking change, the kind id
/// is bumped and both old and new assets can coexist.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct AssetKindId(pub String);

impl AssetKindId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetKindId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Revision of a single asset, carrying both a monotonic version number and
/// a content hash.
///
/// `version` increments on every persisted change. `content_hash` is
/// optional because session-scope drafts that have never been persisted do
/// not need one; as soon as an asset is saved beyond `Session` scope the
/// hash is required.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssetRevision {
    pub version: u64,
    pub content_hash: Option<ContentHash>,
}

impl AssetRevision {
    pub fn initial() -> Self {
        Self {
            version: 1,
            content_hash: None,
        }
    }

    pub fn next(&self, hash: Option<ContentHash>) -> Self {
        Self {
            version: self.version.saturating_add(1),
            content_hash: hash,
        }
    }
}

/// Content hash of a serialized asset body. Hex-encoded string so the wire
/// format is stable regardless of the hash algorithm. Algorithm choice is
/// not fixed by this module; callers conventionally use BLAKE3, but a
/// different algorithm with a distinguishing prefix (`sha256:...`) is also
/// acceptable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ContentHash(pub String);

impl ContentHash {
    pub fn new(hash: impl Into<String>) -> Self {
        Self(hash.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifier of a source-registry entry (standard, regulation, manual,
/// manufacturer reference, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct SourceId(pub String);

impl SourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Revision tag of a source (e.g. `"2011:6"` for a BBR edition, or
/// `"2019-11-01"` for a manufacturer PDF dated by release).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct SourceRevision(pub String);

impl SourceRevision {
    pub fn new(revision: impl Into<String>) -> Self {
        Self(revision.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Pack identifier. Packs are the shipping/integrity/revision unit that
/// groups assets and sources; see ADR-040 and PP84.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct PackId(pub String);

impl PackId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Pack revision. Orthogonal to `AssetRevision` because a pack groups many
/// assets and advances on its own cadence.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct PackRevision(pub String);

impl PackRevision {
    pub fn new(revision: impl Into<String>) -> Self {
        Self(revision.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_id_display_matches_inner() {
        let id = AssetId::new("recipe.v1/gable_roof_framing");
        assert_eq!(id.to_string(), "recipe.v1/gable_roof_framing");
        assert_eq!(id.as_str(), "recipe.v1/gable_roof_framing");
    }

    #[test]
    fn ids_serialize_as_plain_strings() {
        let asset_id = AssetId::new("recipe.v1/foo");
        let kind = AssetKindId::new("recipe.v1");
        let source = SourceId::new("boverket.bbr.2011_6");
        let pack = PackId::new("talos3d_architecture_se");
        let hash = ContentHash::new("blake3:abcd");

        assert_eq!(
            serde_json::to_string(&asset_id).unwrap(),
            "\"recipe.v1/foo\""
        );
        assert_eq!(serde_json::to_string(&kind).unwrap(), "\"recipe.v1\"");
        assert_eq!(
            serde_json::to_string(&source).unwrap(),
            "\"boverket.bbr.2011_6\""
        );
        assert_eq!(
            serde_json::to_string(&pack).unwrap(),
            "\"talos3d_architecture_se\""
        );
        assert_eq!(serde_json::to_string(&hash).unwrap(), "\"blake3:abcd\"");
    }

    #[test]
    fn asset_revision_initial_has_no_hash() {
        let rev = AssetRevision::initial();
        assert_eq!(rev.version, 1);
        assert!(rev.content_hash.is_none());
    }

    #[test]
    fn asset_revision_next_increments_version_and_carries_hash() {
        let rev = AssetRevision::initial();
        let hash = Some(ContentHash::new("blake3:deadbeef"));
        let next = rev.next(hash.clone());
        assert_eq!(next.version, 2);
        assert_eq!(next.content_hash, hash);
    }

    #[test]
    fn asset_revision_round_trips_through_json() {
        let rev = AssetRevision {
            version: 42,
            content_hash: Some(ContentHash::new("blake3:cafebabe")),
        };
        let json = serde_json::to_string(&rev).unwrap();
        let parsed: AssetRevision = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, rev);
    }
}
