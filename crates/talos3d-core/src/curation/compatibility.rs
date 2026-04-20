//! Compatibility pinning for curated assets and packs.
//!
//! `CompatibilityRef` answers: "what platform + capability versions does
//! this artifact declare compatibility with?" Three axes, all optional-
//! to-match depending on kind:
//!
//! - `core_api`: pinned against `talos3d-core`'s platform API. PP79
//!   ships this as a `VersionReq` string; PP84 will actually enforce it.
//! - `capability_api`: per-kind version requirement for the capability
//!   crate that defines the asset's body shape.
//! - `body_schema`: optional pin to a specific body-schema version
//!   (`AuthoringScript` will carry versioned schema revisions).
//!
//! All version requirements are stored as opaque `VersionReq` strings in
//! SemVer range syntax (e.g. `"^1.2"`, `"0.1.*"`). Parsing against the
//! `semver` crate happens in PP84 where enforcement lives.

use serde::{Deserialize, Serialize};

use super::identity::AssetKindId;

/// Opaque SemVer range requirement. Parsing and matching live in PP84.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct VersionReq(pub String);

impl VersionReq {
    pub fn new(req: impl Into<String>) -> Self {
        Self(req.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Match-any requirement, used by callers that have no opinion.
    pub fn any() -> Self {
        Self("*".into())
    }
}

/// Per-kind capability-API compatibility clause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CapabilityCompat {
    pub kind: AssetKindId,
    pub version_req: VersionReq,
}

/// Specific body-schema version an artifact targets.
///
/// `kind` is duplicated here (relative to the artifact's own kind id) so
/// that a `CompatibilityRef` is fully self-describing when inspected on
/// its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SchemaVersion {
    pub kind: AssetKindId,
    pub version: u32,
}

/// Full compatibility pin. Optional fields mean "no opinion," not "match
/// anything" — PP84 treats missing fields as unspecified and emits
/// advisory findings when a pack or consumer needs one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CompatibilityRef {
    pub core_api: Option<VersionReq>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_api: Vec<CapabilityCompat>,
    pub body_schema: Option<SchemaVersion>,
}

impl CompatibilityRef {
    pub fn unconstrained() -> Self {
        Self::default()
    }

    pub fn for_core(req: VersionReq) -> Self {
        Self {
            core_api: Some(req),
            ..Self::default()
        }
    }

    pub fn with_capability(mut self, kind: AssetKindId, version_req: VersionReq) -> Self {
        self.capability_api
            .push(CapabilityCompat { kind, version_req });
        self
    }

    pub fn with_body_schema(mut self, kind: AssetKindId, version: u32) -> Self {
        self.body_schema = Some(SchemaVersion { kind, version });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_req_any_serializes_as_star() {
        assert_eq!(serde_json::to_string(&VersionReq::any()).unwrap(), "\"*\"");
    }

    #[test]
    fn unconstrained_is_default() {
        let c = CompatibilityRef::unconstrained();
        assert!(c.core_api.is_none());
        assert!(c.capability_api.is_empty());
        assert!(c.body_schema.is_none());
    }

    #[test]
    fn builder_composes_clauses() {
        let c = CompatibilityRef::for_core(VersionReq::new("^0.1"))
            .with_capability(AssetKindId::new("recipe.v1"), VersionReq::new("^1"))
            .with_capability(
                AssetKindId::new("architecture-core.v1"),
                VersionReq::new("^1"),
            )
            .with_body_schema(AssetKindId::new("recipe.v1"), 1);
        assert_eq!(c.core_api.as_ref().unwrap().as_str(), "^0.1");
        assert_eq!(c.capability_api.len(), 2);
        assert_eq!(c.body_schema.as_ref().unwrap().version, 1);
    }

    #[test]
    fn compatibility_round_trips_with_empty_capability_list_elided() {
        let c = CompatibilityRef::for_core(VersionReq::new("^0.1"));
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("capability_api"));
        let parsed: CompatibilityRef = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn compatibility_round_trips_fully_populated() {
        let c = CompatibilityRef::for_core(VersionReq::new("^0.1"))
            .with_capability(AssetKindId::new("recipe.v1"), VersionReq::new("^1"))
            .with_body_schema(AssetKindId::new("recipe.v1"), 1);
        let json = serde_json::to_string(&c).unwrap();
        let parsed: CompatibilityRef = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, c);
    }
}
