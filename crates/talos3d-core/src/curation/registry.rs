//! `SourceRegistry` — the runtime resource holding `SourceRegistryEntry`
//! records per source revision, plus the canonical seed set populated on
//! plugin startup.
//!
//! Per ADR-040 the registry is the backing store for every
//! `EvidenceRef.source_id + revision` resolution, the target of
//! nomination/approval flows, and the substrate `publish_*` policies
//! check against.
//!
//! Persistence: the registry splits cleanly by tier. `Canonical` entries
//! are seeded in code on every startup and are NOT persisted (they ride
//! with the binary). `Project`-tier entries persist with the project
//! file. `Organizational` / `Shipped` (via packs) live outside this
//! module and are registered by the relevant capability crate at startup
//! (see PP84 for the pack-load path).

use std::collections::BTreeMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::identity::{SourceId, SourceRevision};
use super::provenance::JurisdictionTag;
use super::source::{SourceLicense, SourceRegistryEntry, SourceStatus, SourceTier};

/// Filter used by `SourceRegistry::list` and the `list_sources` MCP tool.
#[derive(Debug, Clone, Default)]
pub struct SourceFilter {
    pub tier: Option<SourceTier>,
    pub jurisdiction: Option<JurisdictionTag>,
    pub license: Option<SourceLicense>,
    pub active_only: bool,
    pub publisher: Option<String>,
}

impl SourceFilter {
    pub fn matches(&self, entry: &SourceRegistryEntry) -> bool {
        if let Some(t) = self.tier {
            if entry.tier != t {
                return false;
            }
        }
        if let Some(j) = &self.jurisdiction {
            if entry.jurisdiction.as_ref() != Some(j) {
                return false;
            }
        }
        if let Some(l) = self.license {
            if entry.license != l {
                return false;
            }
        }
        if self.active_only && !entry.status.is_active() {
            return false;
        }
        if let Some(pub_name) = &self.publisher {
            if &entry.publisher != pub_name {
                return false;
            }
        }
        true
    }
}

/// Bevy resource holding every registered source, keyed by `SourceId`
/// then `SourceRevision`.
///
/// Multiple revisions per source are supported: `BBR 8` at revision
/// `2011:6` (superseded) and `BBR 8` at revision `2025:1` (active) can
/// coexist. `SourceStatus::Superseded { replacement }` lets callers walk
/// from an old revision to the current one.
#[derive(Resource, Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceRegistry {
    /// `source_id -> revision -> entry`.
    pub entries: BTreeMap<SourceId, BTreeMap<SourceRevision, SourceRegistryEntry>>,
}

impl SourceRegistry {
    /// Insert or replace a single entry. Returns the previous entry at
    /// that (source_id, revision), if any.
    pub fn insert(&mut self, entry: SourceRegistryEntry) -> Option<SourceRegistryEntry> {
        let sid = entry.source_id.clone();
        let rev = entry.revision.clone();
        self.entries.entry(sid).or_default().insert(rev, entry)
    }

    /// Look up a specific revision.
    pub fn get(&self, id: &SourceId, revision: &SourceRevision) -> Option<&SourceRegistryEntry> {
        self.entries.get(id).and_then(|m| m.get(revision))
    }

    /// Return the most-recent revision for a source (BTreeMap's `.last_key_value`
    /// orders lexicographically — callers that need version-sort semantics
    /// should walk explicitly).
    pub fn latest(&self, id: &SourceId) -> Option<&SourceRegistryEntry> {
        self.entries
            .get(id)
            .and_then(|m| m.last_key_value())
            .map(|(_, v)| v)
    }

    /// Every registered source across every revision, flattened.
    pub fn iter(&self) -> impl Iterator<Item = &SourceRegistryEntry> {
        self.entries.values().flat_map(|m| m.values())
    }

    /// Filtered listing; intended to back `list_sources` MCP.
    pub fn list(&self, filter: &SourceFilter) -> Vec<&SourceRegistryEntry> {
        self.iter().filter(|e| filter.matches(e)).collect()
    }

    /// Mark an active entry superseded by another. Returns `true` when
    /// the target existed and was updated. The replacement pointer may
    /// be `None` (i.e. "withdrawn without a direct successor").
    pub fn supersede(
        &mut self,
        id: &SourceId,
        revision: &SourceRevision,
        replacement: Option<SourceId>,
    ) -> bool {
        let Some(map) = self.entries.get_mut(id) else {
            return false;
        };
        let Some(entry) = map.get_mut(revision) else {
            return false;
        };
        entry.status = SourceStatus::Superseded { replacement };
        true
    }

    /// Mark an active entry sunset (withdrawn) with an explicit reason.
    pub fn sunset(&mut self, id: &SourceId, revision: &SourceRevision, reason: String) -> bool {
        let Some(map) = self.entries.get_mut(id) else {
            return false;
        };
        let Some(entry) = map.get_mut(revision) else {
            return false;
        };
        entry.status = SourceStatus::Sunset { reason };
        true
    }

    /// Remove a specific (id, revision). Returns the removed entry.
    pub fn remove(
        &mut self,
        id: &SourceId,
        revision: &SourceRevision,
    ) -> Option<SourceRegistryEntry> {
        let map = self.entries.get_mut(id)?;
        let entry = map.remove(revision);
        if map.is_empty() {
            self.entries.remove(id);
        }
        entry
    }

    /// Iterate only the `Project`-tier entries (the ones that persist
    /// with the project document). Used by `build_project_file`.
    pub fn project_scope_entries(&self) -> impl Iterator<Item = &SourceRegistryEntry> {
        self.iter().filter(|e| e.tier == SourceTier::Project)
    }

    /// Replace all `Project`-tier entries with the given iterator. Used
    /// when loading a project file. Non-`Project` entries (seeded
    /// Canonical, or loaded from packs) are preserved unchanged.
    pub fn replace_project_scope<I>(&mut self, entries: I)
    where
        I: IntoIterator<Item = SourceRegistryEntry>,
    {
        // Drop existing Project-tier entries first.
        let project_keys: Vec<(SourceId, SourceRevision)> = self
            .project_scope_entries()
            .map(|e| (e.source_id.clone(), e.revision.clone()))
            .collect();
        for (id, rev) in project_keys {
            self.remove(&id, &rev);
        }
        for entry in entries {
            debug_assert_eq!(
                entry.tier,
                SourceTier::Project,
                "replace_project_scope only accepts Project-tier entries"
            );
            self.insert(entry);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn source_count(&self) -> usize {
        self.entries.len()
    }

    pub fn revision_count(&self) -> usize {
        self.entries.values().map(|m| m.len()).sum()
    }
}

/// Seed the registry with the Canonical-tier universal sources that ship
/// with every Talos3D build. Intentionally small — jurisdiction-specific
/// content lives in jurisdiction packs (see PP85).
///
/// Safe to call repeatedly: it only inserts entries that are not already
/// registered.
pub fn ensure_canonical_seed(registry: &mut SourceRegistry) {
    for entry in canonical_seed_entries() {
        let id = entry.source_id.clone();
        let rev = entry.revision.clone();
        if registry.get(&id, &rev).is_none() {
            registry.insert(entry);
        }
    }
}

fn canonical_seed_entries() -> Vec<SourceRegistryEntry> {
    let iso_129 = SourceRegistryEntry::new(
        SourceId::new("iso.129-1"),
        SourceRevision::new("2018"),
        "ISO 129-1:2018 — Technical product documentation (TPD) — Presentation of dimensions and tolerances",
        "International Organization for Standardization",
        SourceTier::Canonical,
        SourceLicense::PermissiveCite,
    )
    .with_canonical_url("https://www.iso.org/standard/65241.html");

    let asme_y14_5 = SourceRegistryEntry::new(
        SourceId::new("asme.y14.5"),
        SourceRevision::new("2018"),
        "ASME Y14.5-2018 — Dimensioning and Tolerancing",
        "American Society of Mechanical Engineers",
        SourceTier::Canonical,
        SourceLicense::PermissiveCite,
    )
    .with_canonical_url(
        "https://www.asme.org/codes-standards/find-codes-standards/y14-5-dimensioning-tolerancing",
    );

    let iso_80000 = SourceRegistryEntry::new(
        SourceId::new("iso.80000-1"),
        SourceRevision::new("2022"),
        "ISO 80000-1:2022 — Quantities and units — Part 1: General (SI conventions)",
        "International Organization for Standardization",
        SourceTier::Canonical,
        SourceLicense::PermissiveCite,
    )
    .with_canonical_url("https://www.iso.org/standard/76921.html");

    vec![iso_129, asme_y14_5, iso_80000]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::identity::ContentHash;

    fn entry(id: &str, rev: &str, tier: SourceTier) -> SourceRegistryEntry {
        SourceRegistryEntry::new(
            SourceId::new(id),
            SourceRevision::new(rev),
            format!("{id} {rev}"),
            "Test Publisher",
            tier,
            SourceLicense::PublicDomain,
        )
    }

    #[test]
    fn insert_and_get_roundtrips() {
        let mut reg = SourceRegistry::default();
        reg.insert(entry("a", "v1", SourceTier::Canonical));
        assert!(reg
            .get(&SourceId::new("a"), &SourceRevision::new("v1"))
            .is_some());
        assert!(reg
            .get(&SourceId::new("a"), &SourceRevision::new("v2"))
            .is_none());
    }

    #[test]
    fn multiple_revisions_coexist() {
        let mut reg = SourceRegistry::default();
        reg.insert(entry("bbr.8", "2011:6", SourceTier::Jurisdictional));
        reg.insert(entry("bbr.8", "2025:1", SourceTier::Jurisdictional));
        assert_eq!(reg.revision_count(), 2);
        assert_eq!(reg.source_count(), 1);
    }

    #[test]
    fn latest_returns_highest_revision_lexicographically() {
        let mut reg = SourceRegistry::default();
        reg.insert(entry("bbr.8", "2011:6", SourceTier::Jurisdictional));
        reg.insert(entry("bbr.8", "2025:1", SourceTier::Jurisdictional));
        let latest = reg.latest(&SourceId::new("bbr.8")).unwrap();
        assert_eq!(latest.revision.as_str(), "2025:1");
    }

    #[test]
    fn filter_by_tier_and_jurisdiction() {
        let mut reg = SourceRegistry::default();
        let se = entry("bbr.8", "2011:6", SourceTier::Jurisdictional);
        let se = se.clone().with_jurisdiction(JurisdictionTag::new("SE"));
        let global = entry("iso.129-1", "2018", SourceTier::Canonical);
        reg.insert(se);
        reg.insert(global);

        let filter = SourceFilter {
            tier: Some(SourceTier::Jurisdictional),
            jurisdiction: Some(JurisdictionTag::new("SE")),
            ..Default::default()
        };
        let hits = reg.list(&filter);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source_id.as_str(), "bbr.8");

        let filter = SourceFilter {
            tier: Some(SourceTier::Canonical),
            ..Default::default()
        };
        let hits = reg.list(&filter);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source_id.as_str(), "iso.129-1");
    }

    #[test]
    fn supersede_updates_status() {
        let mut reg = SourceRegistry::default();
        reg.insert(entry("bbr.8", "2011:6", SourceTier::Jurisdictional));
        reg.insert(entry("bbr.8", "2025:1", SourceTier::Jurisdictional));
        assert!(reg.supersede(
            &SourceId::new("bbr.8"),
            &SourceRevision::new("2011:6"),
            Some(SourceId::new("bbr.8")),
        ));
        let old = reg
            .get(&SourceId::new("bbr.8"), &SourceRevision::new("2011:6"))
            .unwrap();
        assert!(matches!(old.status, SourceStatus::Superseded { .. }));

        let filter = SourceFilter {
            active_only: true,
            ..Default::default()
        };
        let active = reg.list(&filter);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].revision.as_str(), "2025:1");
    }

    #[test]
    fn sunset_marks_with_reason() {
        let mut reg = SourceRegistry::default();
        reg.insert(entry("vendor.x", "2020", SourceTier::Organizational));
        assert!(reg.sunset(
            &SourceId::new("vendor.x"),
            &SourceRevision::new("2020"),
            "vendor discontinued the product line".into(),
        ));
        let e = reg
            .get(&SourceId::new("vendor.x"), &SourceRevision::new("2020"))
            .unwrap();
        assert!(matches!(e.status, SourceStatus::Sunset { .. }));
    }

    #[test]
    fn remove_drops_entry_and_empty_outer_key() {
        let mut reg = SourceRegistry::default();
        reg.insert(entry("a", "v1", SourceTier::Canonical));
        assert!(reg
            .remove(&SourceId::new("a"), &SourceRevision::new("v1"))
            .is_some());
        assert!(reg.is_empty());
    }

    #[test]
    fn canonical_seed_inserts_known_entries() {
        let mut reg = SourceRegistry::default();
        ensure_canonical_seed(&mut reg);
        assert!(reg
            .get(&SourceId::new("iso.129-1"), &SourceRevision::new("2018"))
            .is_some());
        assert!(reg
            .get(&SourceId::new("asme.y14.5"), &SourceRevision::new("2018"))
            .is_some());
        assert!(reg
            .get(&SourceId::new("iso.80000-1"), &SourceRevision::new("2022"))
            .is_some());

        // Idempotent: second call does not duplicate or overwrite.
        let before = reg.revision_count();
        ensure_canonical_seed(&mut reg);
        assert_eq!(reg.revision_count(), before);
    }

    #[test]
    fn project_scope_partition_only_touches_project_entries() {
        let mut reg = SourceRegistry::default();
        ensure_canonical_seed(&mut reg);
        reg.insert(entry("proj.doc", "draft", SourceTier::Project));
        reg.insert(entry("proj.spec", "v2", SourceTier::Project));
        assert_eq!(reg.project_scope_entries().count(), 2);

        // Replace with a different set.
        reg.replace_project_scope(vec![entry("proj.other", "v1", SourceTier::Project)]);
        assert_eq!(reg.project_scope_entries().count(), 1);
        // Canonicals intact.
        assert!(reg
            .get(&SourceId::new("iso.129-1"), &SourceRevision::new("2018"))
            .is_some());
    }

    #[test]
    fn registry_roundtrips_through_json() {
        let mut reg = SourceRegistry::default();
        ensure_canonical_seed(&mut reg);
        let mut proj = entry("proj.doc", "draft", SourceTier::Project);
        proj.content_hash = Some(ContentHash::new("blake3:proj"));
        reg.insert(proj);

        let json = serde_json::to_string(&reg).unwrap();
        let parsed: SourceRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, reg);
    }
}
