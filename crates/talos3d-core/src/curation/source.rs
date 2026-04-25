//! `SourceRegistryEntry` shape and related enums.
//!
//! This module defines *types only* — the `SourceRegistry` resource,
//! persistence, MCP tools, nomination queue, and publication-floor
//! enforcement land in PP80.
//!
//! The entry type here is the ADR-040 renaming and extension of the
//! shipped `crate::capability_registry::CorpusProvenance`. The shipped
//! type remains available for existing call sites until PP80 migrates
//! them; new code should use `SourceRegistryEntry`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::identity::{ContentHash, SourceId, SourceRevision};
use super::provenance::JurisdictionTag;

/// Five-tier source classification. Narrower tiers cite content that is
/// more specific and more operator- or user-scoped; wider tiers cite
/// content shipped with the platform.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceTier {
    /// Shipped with Talos3D. Universal standards (ISO 129, ASME Y14.5,
    /// IFC vocabulary pointers, metric conventions).
    Canonical,
    /// Country- or region-level jurisdictional content (Eurocodes, IBC,
    /// BBR, NCS, DIN, JIS, NBCC, municipal code supplements). Ships as
    /// separate capability packs per jurisdiction.
    Jurisdictional,
    /// Office-standard content or licensed manufacturer catalogs. User
    /// tier; never ships with the product.
    Organizational,
    /// Documents the user attached to this specific project.
    Project,
    /// LLM training knowledge + ad-hoc web retrieval. Lowest trust;
    /// never promotable to `Published` on its own.
    #[default]
    AdHoc,
}

/// License posture of a source entry. Determines what can be stored
/// alongside the pointer, what can be cited, and whether an entry is
/// promotable across scope boundaries.
///
/// Distinct from the shipped
/// [`crate::capability_registry::LicenseTag`] — this enum is the ADR-040
/// vocabulary and covers all tiers uniformly, while the shipped one is
/// source-family-specific (Cc0 / BoverketPublic / IccCiteOnly /
/// VendorEula / PublicRecord / StandardsBodyCitationOnly). PP80 will
/// provide a mapping.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceLicense {
    /// Public domain or equivalent; content may be redistributed freely.
    PublicDomain,
    /// Official government publication; typically distributable by
    /// default (e.g. Swedish Boverket documents under their public
    /// record policy).
    OfficialGovernmentPublication,
    /// Citation and paraphrase permitted; excerpts generally not.
    PermissiveCite,
    /// Excerpt permitted under a specific license; may require operator-
    /// level licensing to store or display.
    LicensedExcerpt,
    /// User attached this source as private project material. Must not
    /// flow across scope boundaries; never promotable.
    #[default]
    UserAttachedPrivate,
}

/// Lifecycle status of a source entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SourceStatus {
    #[default]
    Active,
    Superseded {
        /// Pointer to the replacement source, if known.
        replacement: Option<SourceId>,
    },
    Sunset {
        reason: String,
    },
}

impl SourceStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// A source entry in the `SourceRegistry`.
///
/// Superset of the shipped
/// [`crate::capability_registry::CorpusProvenance`]. Adds tier,
/// lifecycle status, optional content hash, and a free-form metadata
/// bag. The shipped `CorpusProvenance` fields map as follows:
///
/// | shipped | this struct |
/// | --- | --- |
/// | `source`, `source_version` | `publisher`, `title`, `revision` |
/// | `jurisdiction` | `jurisdiction` |
/// | `license: LicenseTag` | `license: SourceLicense` (mapping in PP80) |
/// | `backlink: Option<PassageRef>` | `canonical_url` (URL) or `metadata["backlink"]` |
/// | `ingested_at`, `ingested_by` | `metadata["ingested_at"]`, `metadata["ingested_by"]` |
/// | `supersedes` | `status: Superseded { replacement: ... }` |
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SourceRegistryEntry {
    pub source_id: SourceId,
    pub revision: SourceRevision,
    pub title: String,
    pub publisher: String,
    pub tier: SourceTier,
    pub jurisdiction: Option<JurisdictionTag>,
    pub license: SourceLicense,
    pub status: SourceStatus,
    pub canonical_url: Option<String>,
    pub content_hash: Option<ContentHash>,
    /// Free-form JSON metadata (ingestion timestamps, original
    /// backlinks, alternative ids, etc.).
    #[serde(default, skip_serializing_if = "is_null_json")]
    pub metadata: Value,
}

fn is_null_json(v: &Value) -> bool {
    v.is_null()
}

impl SourceRegistryEntry {
    pub fn new(
        source_id: SourceId,
        revision: SourceRevision,
        title: impl Into<String>,
        publisher: impl Into<String>,
        tier: SourceTier,
        license: SourceLicense,
    ) -> Self {
        Self {
            source_id,
            revision,
            title: title.into(),
            publisher: publisher.into(),
            tier,
            jurisdiction: None,
            license,
            status: SourceStatus::Active,
            canonical_url: None,
            content_hash: None,
            metadata: Value::Null,
        }
    }

    pub fn with_jurisdiction(mut self, j: JurisdictionTag) -> Self {
        self.jurisdiction = Some(j);
        self
    }

    pub fn with_canonical_url(mut self, url: impl Into<String>) -> Self {
        self.canonical_url = Some(url.into());
        self
    }

    pub fn with_content_hash(mut self, hash: ContentHash) -> Self {
        self.content_hash = Some(hash);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_ordering_is_narrowing() {
        assert!(SourceTier::Canonical < SourceTier::Jurisdictional);
        assert!(SourceTier::Jurisdictional < SourceTier::Organizational);
        assert!(SourceTier::Organizational < SourceTier::Project);
        assert!(SourceTier::Project < SourceTier::AdHoc);
    }

    #[test]
    fn source_status_variants_round_trip() {
        for s in [
            SourceStatus::Active,
            SourceStatus::Superseded {
                replacement: Some(SourceId::new("boverket.bbr.2025")),
            },
            SourceStatus::Superseded { replacement: None },
            SourceStatus::Sunset {
                reason: "withdrawn by publisher".into(),
            },
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let parsed: SourceStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, s);
        }
    }

    #[test]
    fn source_registry_entry_builder_sets_defaults() {
        let entry = SourceRegistryEntry::new(
            SourceId::new("boverket.bbr.8"),
            SourceRevision::new("2011:6"),
            "Boverket BBR 8 — Säkerhet vid användning",
            "Boverket",
            SourceTier::Jurisdictional,
            SourceLicense::OfficialGovernmentPublication,
        )
        .with_jurisdiction(JurisdictionTag::new("SE"))
        .with_canonical_url("https://www.boverket.se/sv/bbr/");
        assert!(entry.status.is_active());
        assert_eq!(entry.tier, SourceTier::Jurisdictional);
        assert_eq!(entry.license, SourceLicense::OfficialGovernmentPublication);
        assert_eq!(entry.jurisdiction.as_ref().unwrap().as_str(), "SE");
        assert!(entry.canonical_url.is_some());
    }

    #[test]
    fn source_registry_entry_round_trips() {
        let entry = SourceRegistryEntry::new(
            SourceId::new("boverket.bbr.8"),
            SourceRevision::new("2011:6"),
            "BBR 8",
            "Boverket",
            SourceTier::Jurisdictional,
            SourceLicense::OfficialGovernmentPublication,
        )
        .with_jurisdiction(JurisdictionTag::new("SE"))
        .with_content_hash(ContentHash::new("blake3:abc"));
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: SourceRegistryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn license_default_is_strictest() {
        assert_eq!(SourceLicense::default(), SourceLicense::UserAttachedPrivate);
    }

    #[test]
    fn tier_default_is_adhoc() {
        assert_eq!(SourceTier::default(), SourceTier::AdHoc);
    }
}
