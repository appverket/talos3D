//! Conversion shims between the shipped `capability_registry` vocabulary
//! (`LicenseTag`, `CorpusProvenance`) and the ADR-040 vocabulary
//! (`SourceLicense`, `SourceRegistryEntry`).
//!
//! These impls let PP80 callers walk a shipped `CorpusProvenance` (still
//! produced by PP70–PP78 corpus plumbing) into a `SourceRegistryEntry`
//! without forcing an all-at-once rename. The shipped types stay
//! authoritative for existing code paths; new code should produce
//! `SourceRegistryEntry` directly.

use serde_json::Value;

use crate::capability_registry::{CorpusProvenance, LicenseTag};

use super::identity::{SourceId, SourceRevision};
use super::provenance::JurisdictionTag;
use super::source::{SourceLicense, SourceRegistryEntry, SourceStatus, SourceTier};

impl From<&LicenseTag> for SourceLicense {
    fn from(value: &LicenseTag) -> Self {
        match value {
            LicenseTag::Cc0 => SourceLicense::PublicDomain,
            LicenseTag::PublicRecord => SourceLicense::OfficialGovernmentPublication,
            LicenseTag::BoverketPublic => SourceLicense::OfficialGovernmentPublication,
            LicenseTag::IccCiteOnly => SourceLicense::PermissiveCite,
            LicenseTag::StandardsBodyCitationOnly => SourceLicense::PermissiveCite,
            LicenseTag::VendorEula(_) => SourceLicense::LicensedExcerpt,
        }
    }
}

impl From<LicenseTag> for SourceLicense {
    fn from(value: LicenseTag) -> Self {
        SourceLicense::from(&value)
    }
}

/// Map a shipped `CorpusProvenance` to a curation `SourceRegistryEntry`.
///
/// The caller chooses the `SourceTier` (the shipped type does not track
/// tier). Reasonable rule of thumb:
///
/// - shipped bundled corpus → `SourceTier::Canonical`
/// - jurisdiction-pack content (e.g. BBR passages from
///   `talos3d-architecture-se`) → `SourceTier::Jurisdictional`
/// - project-attached sources → `SourceTier::Project`
///
/// The original `backlink: Option<PassageRef>` is preserved as
/// `metadata["backlink"]`; `ingested_at` and `supersedes` likewise land
/// in `metadata` to avoid losing information.
pub fn corpus_provenance_to_registry_entry(
    provenance: &CorpusProvenance,
    tier: SourceTier,
) -> SourceRegistryEntry {
    let license = SourceLicense::from(&provenance.license);

    let jurisdiction = provenance
        .jurisdiction
        .as_ref()
        .map(|j| JurisdictionTag::new(j.clone()));

    let status = if provenance.supersedes.is_empty() {
        SourceStatus::Active
    } else {
        // Opposite direction from the shipped `supersedes` list: the
        // shipped list on an *incoming* source names rows it replaces.
        // At the entry level that means the incoming source is Active;
        // the predecessors' own entries (once they exist) should carry
        // `Superseded { replacement = this source }`. We don't mutate
        // predecessors here; the caller does.
        SourceStatus::Active
    };

    let mut metadata = serde_json::Map::new();
    if provenance.ingested_at != 0 {
        metadata.insert("ingested_at".into(), Value::from(provenance.ingested_at));
    }
    if let Some(backlink) = &provenance.backlink {
        metadata.insert("backlink".into(), Value::from(backlink.0.clone()));
    }
    if !provenance.supersedes.is_empty() {
        metadata.insert(
            "supersedes".into(),
            Value::Array(
                provenance
                    .supersedes
                    .iter()
                    .cloned()
                    .map(Value::from)
                    .collect(),
            ),
        );
    }
    let metadata = if metadata.is_empty() {
        Value::Null
    } else {
        Value::Object(metadata)
    };

    SourceRegistryEntry {
        source_id: SourceId::new(slug(&provenance.source)),
        revision: SourceRevision::new(provenance.source_version.clone()),
        title: provenance.source.clone(),
        publisher: derive_publisher(provenance),
        tier,
        jurisdiction,
        license,
        status,
        canonical_url: None,
        content_hash: None,
        metadata,
    }
}

/// Derive a publisher label from a `CorpusProvenance`. The shipped type
/// folds source and publisher into one `source` string, so a best-effort
/// split is used: known prefixes ("Boverket BBR", "ICC", "ISO", "EN")
/// map to their issuing body; otherwise the `source` is used verbatim.
fn derive_publisher(p: &CorpusProvenance) -> String {
    let source = p.source.as_str();
    if source.contains("Boverket") {
        return "Boverket".into();
    }
    if source.contains("ICC") || source.to_ascii_uppercase().contains("IBC") {
        return "International Code Council".into();
    }
    if source.starts_with("ISO ") || source.contains("International Organization for Standardization") {
        return "International Organization for Standardization".into();
    }
    if source.starts_with("EN ") || source.contains("CEN") {
        return "CEN / European Committee for Standardization".into();
    }
    if source.starts_with("SIS ") {
        return "SIS / Swedish Institute for Standards".into();
    }
    if let LicenseTag::VendorEula(name) = &p.license {
        return name.clone();
    }
    source.to_string()
}

/// Stable slug of a free-form title, used as the source_id when we
/// don't have a better identifier. Lowercase, alphanumerics and dots
/// only; runs of other characters collapse to a single `_`.
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_sep = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep {
            out.push('_');
            prev_sep = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::PassageRef;

    #[test]
    fn license_tag_maps_to_source_license() {
        assert_eq!(
            SourceLicense::from(&LicenseTag::Cc0),
            SourceLicense::PublicDomain
        );
        assert_eq!(
            SourceLicense::from(&LicenseTag::BoverketPublic),
            SourceLicense::OfficialGovernmentPublication
        );
        assert_eq!(
            SourceLicense::from(&LicenseTag::PublicRecord),
            SourceLicense::OfficialGovernmentPublication
        );
        assert_eq!(
            SourceLicense::from(&LicenseTag::IccCiteOnly),
            SourceLicense::PermissiveCite
        );
        assert_eq!(
            SourceLicense::from(&LicenseTag::StandardsBodyCitationOnly),
            SourceLicense::PermissiveCite
        );
        assert_eq!(
            SourceLicense::from(&LicenseTag::VendorEula("Acme Windows".into())),
            SourceLicense::LicensedExcerpt
        );
    }

    #[test]
    fn slug_collapses_punctuation_and_lowercases() {
        assert_eq!(slug("BBR 8"), "bbr_8");
        assert_eq!(slug("Boverket BBR 8 — Säkerhet"), "boverket_bbr_8_s_kerhet");
        assert_eq!(slug("!!!"), "unknown");
        assert_eq!(slug("iso.129-1"), "iso.129_1");
    }

    #[test]
    fn corpus_provenance_to_registry_entry_bbr_case() {
        let prov = CorpusProvenance {
            source: "Boverket BBR 8 — Säkerhet vid användning".into(),
            source_version: "2011:6".into(),
            jurisdiction: Some("SE".into()),
            ingested_at: 1_700_000_000,
            license: LicenseTag::BoverketPublic,
            backlink: Some(PassageRef("bbr.8/§8:22".into())),
            supersedes: vec![],
        };
        let entry = corpus_provenance_to_registry_entry(&prov, SourceTier::Jurisdictional);
        assert_eq!(entry.publisher, "Boverket");
        assert_eq!(entry.tier, SourceTier::Jurisdictional);
        assert_eq!(entry.license, SourceLicense::OfficialGovernmentPublication);
        assert_eq!(
            entry.jurisdiction.as_ref().map(|j| j.as_str()),
            Some("SE")
        );
        // Ingested-at and backlink land in metadata, not lost.
        let metadata = entry
            .metadata
            .as_object()
            .expect("metadata should be an object");
        assert_eq!(
            metadata.get("ingested_at").and_then(|v| v.as_i64()),
            Some(1_700_000_000)
        );
        assert_eq!(
            metadata.get("backlink").and_then(|v| v.as_str()),
            Some("bbr.8/§8:22")
        );
    }

    #[test]
    fn corpus_provenance_vendor_eula_becomes_licensed_excerpt() {
        let prov = CorpusProvenance {
            source: "Acme Widgets Installation Manual".into(),
            source_version: "v3".into(),
            jurisdiction: None,
            ingested_at: 0,
            license: LicenseTag::VendorEula("Acme Widgets Inc.".into()),
            backlink: None,
            supersedes: vec![],
        };
        let entry = corpus_provenance_to_registry_entry(&prov, SourceTier::Organizational);
        assert_eq!(entry.publisher, "Acme Widgets Inc.");
        assert_eq!(entry.license, SourceLicense::LicensedExcerpt);
        // metadata is null because ingested_at == 0 and no backlink.
        assert!(entry.metadata.is_null());
    }

    #[test]
    fn iso_source_publisher_is_iso() {
        let prov = CorpusProvenance {
            source: "ISO 80000-1:2022".into(),
            source_version: "2022".into(),
            jurisdiction: None,
            ingested_at: 0,
            license: LicenseTag::StandardsBodyCitationOnly,
            backlink: None,
            supersedes: vec![],
        };
        let entry = corpus_provenance_to_registry_entry(&prov, SourceTier::Canonical);
        assert_eq!(
            entry.publisher,
            "International Organization for Standardization"
        );
        assert_eq!(entry.license, SourceLicense::PermissiveCite);
    }
}
