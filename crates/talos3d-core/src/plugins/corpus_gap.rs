//! PP78 — Corpus Operations: CorpusGap queue and passage registry.
//!
//! `CorpusGapQueue` is a Bevy [`Resource`] that accumulates requests for
//! missing corpus coverage (a jurisdiction's rule pack, a catalog section,
//! etc.). Agents and validators push gaps; a human or CI job reviews and
//! resolves them.
//!
//! `CorpusPassageRegistry` is a companion [`Resource`] that stores plain-text
//! passages keyed by [`PassageRef`].  Domain packs (e.g. `ArchitectureSEPlugin`)
//! call [`CorpusPassageRegistry::register`] during their `Plugin::build` to
//! seed it with hand-authored or ingested passages.  PP78's MCP tools then
//! serve `lookup_source_passage` and `draft_rule_pack` from this registry.
//!
//! Neither resource requires the `model-api` feature flag — they are pure
//! domain state usable in headless test worlds.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::capability_registry::{CorpusProvenance, PassageRef};
use crate::curation::identity::AssetKindId;

// ---------------------------------------------------------------------------
// CorpusGapId
// ---------------------------------------------------------------------------

/// Stable opaque identifier for a single corpus gap entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CorpusGapId(pub String);

// ---------------------------------------------------------------------------
// CorpusGap
// ---------------------------------------------------------------------------

/// A record of missing corpus coverage, pushed by agents or validators.
///
/// `reported_by` is a free-form attribution string — `"agent"` for AI-driven
/// requests, `"validator:<constraint_id>"` for automatic gap detection.
/// `context` carries any extra JSON payload the reporter deems useful (e.g.
/// the failing entity id or the specific parameter that triggered the gap).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusGap {
    /// Stable identifier for this gap entry.
    pub id: CorpusGapId,
    /// Optional element class this gap relates to (e.g. `"stair_straight"`).
    ///
    /// Architecture-specific. Cross-kind code should prefer the newer
    /// `kind` field (ADR-040 / PP80); both are populated when a
    /// recipe-authoring flow emits a gap for a specific element class.
    pub element_class: Option<String>,
    /// Curated asset kind this gap relates to (e.g. `"recipe.v1"`,
    /// `"material_spec.v1"`, `"source"`). Added in PP80 to generalize
    /// the queue beyond architectural element classes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AssetKindId>,
    /// Optional jurisdiction code (e.g. `"SE"`, `"NO"`).
    pub jurisdiction: Option<String>,
    /// What kind of artifact is missing: `"rule_pack"`, `"catalog"`, `"passage"`, …
    pub missing_artifact_kind: String,
    /// Arbitrary JSON payload with reporter-supplied context.
    pub context: serde_json::Value,
    /// Who or what filed this gap.
    pub reported_by: String,
    /// Unix timestamp (seconds) when this gap was reported.
    pub reported_at: i64,
}

// ---------------------------------------------------------------------------
// CorpusGapQueue
// ---------------------------------------------------------------------------

/// A Bevy [`Resource`] that accumulates [`CorpusGap`] entries.
///
/// Agents push gaps via `push`; a human or CI job resolves them via `resolve`.
/// The queue is append-only by design — resolution removes the entry so it
/// does not show up in subsequent `list` calls.
#[derive(Resource, Default, Debug)]
pub struct CorpusGapQueue {
    gaps: Vec<CorpusGap>,
    next_serial: u64,
}

impl CorpusGapQueue {
    /// Push a new gap onto the queue.
    ///
    /// The `id` field is auto-generated as `"gap-{serial}"` to ensure
    /// uniqueness within a session.
    pub fn push(&mut self, mut gap: CorpusGap) -> CorpusGapId {
        let id = CorpusGapId(format!("gap-{}", self.next_serial));
        self.next_serial += 1;
        gap.id = id.clone();
        self.gaps.push(gap);
        id
    }

    /// Return a slice of all unresolved gaps.
    pub fn list(&self) -> &[CorpusGap] {
        &self.gaps
    }

    /// Resolve (remove) a gap by id.  Returns `true` if the gap was found and
    /// removed, `false` if no gap with that id exists.
    pub fn resolve(&mut self, id: &CorpusGapId) -> bool {
        let before = self.gaps.len();
        self.gaps.retain(|g| &g.id != id);
        self.gaps.len() < before
    }

    /// Cross-kind gap constructor (ADR-040 / PP80). Use when the
    /// reporter does not have an element class in mind — e.g. a
    /// publication-floor failure on a `MaterialSpec` or `CodeRulePack`.
    pub fn push_for_kind(
        &mut self,
        kind: AssetKindId,
        jurisdiction: Option<String>,
        missing_artifact_kind: impl Into<String>,
        context: serde_json::Value,
        reported_by: impl Into<String>,
        reported_at: i64,
    ) -> CorpusGapId {
        self.push(CorpusGap {
            id: CorpusGapId(String::new()), // overwritten by push
            element_class: None,
            kind: Some(kind),
            jurisdiction,
            missing_artifact_kind: missing_artifact_kind.into(),
            context,
            reported_by: reported_by.into(),
            reported_at,
        })
    }

    /// List gaps filtered by kind (cross-kind inspection). Returns
    /// references into the queue; callers clone as needed.
    pub fn list_by_kind<'a>(&'a self, kind: &'a AssetKindId) -> impl Iterator<Item = &'a CorpusGap> + 'a {
        self.gaps
            .iter()
            .filter(move |g| g.kind.as_ref() == Some(kind))
    }
}

/// Translate a `PublicationFinding` into a `CorpusGap` entry. Used by
/// `publish_*` paths when policy rejects the attempt because an
/// evidence row cannot be resolved in the `SourceRegistry`.
///
/// Only `evidence_unresolved` findings produce a gap — superseded/
/// sunset/license findings are operator concerns, not corpus gaps.
pub fn gap_from_publication_finding(
    kind: AssetKindId,
    finding: &crate::curation::publication::PublicationFinding,
    evidence_source_id: Option<&str>,
    evidence_revision: Option<&str>,
    jurisdiction: Option<String>,
    reported_by: impl Into<String>,
    reported_at: i64,
) -> Option<CorpusGap> {
    if finding.code != "curation.publication.evidence_unresolved" {
        return None;
    }
    let mut context = serde_json::Map::new();
    context.insert("finding_code".into(), finding.code.into());
    context.insert("finding_message".into(), finding.message.clone().into());
    if let Some(s) = evidence_source_id {
        context.insert("source_id".into(), s.into());
    }
    if let Some(r) = evidence_revision {
        context.insert("revision".into(), r.into());
    }
    Some(CorpusGap {
        id: CorpusGapId(String::new()), // overwritten by push
        element_class: None,
        kind: Some(kind),
        jurisdiction,
        missing_artifact_kind: "source_entry".into(),
        context: serde_json::Value::Object(context),
        reported_by: reported_by.into(),
        reported_at,
    })
}

// ---------------------------------------------------------------------------
// PassageEntry
// ---------------------------------------------------------------------------

/// A single passage stored in the [`CorpusPassageRegistry`].
#[derive(Debug, Clone)]
pub struct PassageEntry {
    /// Full plain-text of the passage (Swedish, English, or mixed depending on
    /// the corpus).
    pub text: String,
    /// Provenance metadata: source, version, license, jurisdiction.
    pub provenance: CorpusProvenance,
}

// ---------------------------------------------------------------------------
// CorpusPassageRegistry
// ---------------------------------------------------------------------------

/// A Bevy [`Resource`] that maps [`PassageRef`]s to their text and provenance.
///
/// Domain packs call [`register`] during `Plugin::build`; PP78's MCP tools
/// read from it via [`get`].  No vector embedding, no disk I/O — plain
/// `HashMap`.
#[derive(Resource, Default, Debug)]
pub struct CorpusPassageRegistry {
    passages: HashMap<String, PassageEntry>,
}

impl CorpusPassageRegistry {
    /// Register a passage.  Overwrites any existing entry with the same ref.
    pub fn register(
        &mut self,
        passage_ref: PassageRef,
        text: impl Into<String>,
        provenance: CorpusProvenance,
    ) {
        self.passages.insert(
            passage_ref.0,
            PassageEntry {
                text: text.into(),
                provenance,
            },
        );
    }

    /// Look up a passage by ref.  Returns `None` if not registered.
    pub fn get(&self, passage_ref: &PassageRef) -> Option<&PassageEntry> {
        self.passages.get(passage_ref.0.as_str())
    }

    /// Iterate over all registered `(PassageRef, PassageEntry)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PassageEntry)> {
        self.passages.iter().map(|(k, v)| (k.as_str(), v))
    }
}

// ---------------------------------------------------------------------------
// CorpusGapPlugin
// ---------------------------------------------------------------------------

/// Plugin that registers `CorpusGapQueue` and `CorpusPassageRegistry` as
/// default Bevy resources.
///
/// Domain pack plugins that want to populate the passage registry at startup
/// should call [`CorpusPassageRegistry::register`] in their own `build`
/// implementation *after* adding this plugin (or simply rely on the world
/// auto-initialising the default if `CorpusGapPlugin` ran first).
pub struct CorpusGapPlugin;

impl Plugin for CorpusGapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CorpusGapQueue>();
        app.init_resource::<CorpusPassageRegistry>();
    }
}

// ---------------------------------------------------------------------------
// BacklinkCheckReport — PP78 CI helper
// ---------------------------------------------------------------------------

/// A backlink that could not be resolved against the [`CorpusPassageRegistry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokenBacklink {
    /// The constraint id whose `source_backlink` is unresolvable.
    pub constraint_id: String,
    /// The passage ref that is missing from the registry.
    pub passage_ref: String,
}

/// Summary produced by [`resolve_all_rule_pack_backlinks`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklinkCheckReport {
    /// Total number of constraints that have a non-`None` `source_backlink`.
    pub total: usize,
    /// How many of those backlinks resolved successfully.
    pub resolved: usize,
    /// Entries for backlinks that could not be resolved.
    pub broken: Vec<BrokenBacklink>,
}

/// Walk all registered [`ConstraintDescriptor`]s and check whether each
/// `source_backlink` resolves against the [`CorpusPassageRegistry`].
///
/// Intended as a headless CI helper:
///
/// ```no_run
/// let report = resolve_all_rule_pack_backlinks(&world);
/// assert!(report.broken.is_empty(), "broken backlinks: {:?}", report.broken);
/// ```
pub fn resolve_all_rule_pack_backlinks(world: &World) -> BacklinkCheckReport {
    use crate::capability_registry::CapabilityRegistry;

    let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
        return BacklinkCheckReport {
            total: 0,
            resolved: 0,
            broken: Vec::new(),
        };
    };
    let passage_registry = world.get_resource::<CorpusPassageRegistry>();

    let mut total = 0usize;
    let mut resolved = 0usize;
    let mut broken = Vec::new();

    for descriptor in registry.constraint_descriptors() {
        let Some(ref backlink) = descriptor.source_backlink else {
            continue;
        };
        total += 1;
        let found = passage_registry
            .and_then(|pr| pr.get(backlink))
            .is_some();
        if found {
            resolved += 1;
        } else {
            broken.push(BrokenBacklink {
                constraint_id: descriptor.id.0.clone(),
                passage_ref: backlink.0.clone(),
            });
        }
    }

    BacklinkCheckReport { total, resolved, broken }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::LicenseTag;

    fn sample_provenance() -> CorpusProvenance {
        CorpusProvenance {
            source: "test".into(),
            source_version: "v1".into(),
            jurisdiction: Some("SE".into()),
            ingested_at: 0,
            license: LicenseTag::BoverketPublic,
            backlink: None,
            supersedes: Vec::new(),
        }
    }

    fn make_gap(kind: &str) -> CorpusGap {
        CorpusGap {
            id: CorpusGapId(String::new()), // overwritten by push()
            element_class: Some("stair_straight".into()),
            kind: None,
            jurisdiction: Some("SE".into()),
            missing_artifact_kind: kind.into(),
            context: serde_json::json!({}),
            reported_by: "agent".into(),
            reported_at: 0,
        }
    }

    // --- CorpusGapQueue ---

    #[test]
    fn push_assigns_unique_ids() {
        let mut queue = CorpusGapQueue::default();
        let id1 = queue.push(make_gap("rule_pack"));
        let id2 = queue.push(make_gap("catalog"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn list_returns_all_pushed_gaps() {
        let mut queue = CorpusGapQueue::default();
        queue.push(make_gap("rule_pack"));
        queue.push(make_gap("catalog"));
        assert_eq!(queue.list().len(), 2);
    }

    #[test]
    fn resolve_removes_gap_by_id() {
        let mut queue = CorpusGapQueue::default();
        let id = queue.push(make_gap("rule_pack"));
        assert!(queue.resolve(&id));
        assert!(queue.list().is_empty());
    }

    #[test]
    fn resolve_returns_false_for_unknown_id() {
        let mut queue = CorpusGapQueue::default();
        let unknown = CorpusGapId("gap-9999".into());
        assert!(!queue.resolve(&unknown));
    }

    #[test]
    fn resolve_only_removes_matching_gap() {
        let mut queue = CorpusGapQueue::default();
        let id1 = queue.push(make_gap("rule_pack"));
        queue.push(make_gap("catalog"));
        assert!(queue.resolve(&id1));
        assert_eq!(queue.list().len(), 1);
        assert_eq!(queue.list()[0].missing_artifact_kind, "catalog");
    }

    // --- CorpusPassageRegistry ---

    #[test]
    fn register_and_get_roundtrip() {
        let mut registry = CorpusPassageRegistry::default();
        let pref = PassageRef("BBR_8:22_riser_max".into());
        registry.register(pref.clone(), "Stigningen ska vara högst 200 mm.", sample_provenance());
        let entry = registry.get(&pref).expect("passage should be present");
        assert!(entry.text.contains("200 mm"));
    }

    #[test]
    fn get_unknown_passage_returns_none() {
        let registry = CorpusPassageRegistry::default();
        let pref = PassageRef("does_not_exist".into());
        assert!(registry.get(&pref).is_none());
    }

    #[test]
    fn register_overwrites_existing_passage() {
        let mut registry = CorpusPassageRegistry::default();
        let pref = PassageRef("BBR_8:22_riser_max".into());
        registry.register(pref.clone(), "old text", sample_provenance());
        registry.register(pref.clone(), "new text", sample_provenance());
        let entry = registry.get(&pref).unwrap();
        assert_eq!(entry.text, "new text");
    }

    // --- PP80 cross-kind generalization ---

    #[test]
    fn push_for_kind_sets_kind_and_leaves_element_class_none() {
        let mut queue = CorpusGapQueue::default();
        let id = queue.push_for_kind(
            AssetKindId::new("material_spec.v1"),
            Some("SE".into()),
            "catalog_row",
            serde_json::json!({"needed_for": "C24 timber stringer"}),
            "agent:claude",
            1_700_000_000,
        );
        let gap = queue.list().iter().find(|g| g.id == id).unwrap();
        assert_eq!(gap.kind.as_ref().map(|k| k.as_str()), Some("material_spec.v1"));
        assert!(gap.element_class.is_none());
    }

    #[test]
    fn list_by_kind_filters_by_asset_kind() {
        let mut queue = CorpusGapQueue::default();
        queue.push_for_kind(
            AssetKindId::new("material_spec.v1"),
            None,
            "catalog_row",
            serde_json::json!({}),
            "agent",
            0,
        );
        queue.push_for_kind(
            AssetKindId::new("recipe.v1"),
            None,
            "body",
            serde_json::json!({}),
            "agent",
            0,
        );
        let kind = AssetKindId::new("material_spec.v1");
        let spec_gaps: Vec<_> = queue.list_by_kind(&kind).collect();
        assert_eq!(spec_gaps.len(), 1);
        assert_eq!(spec_gaps[0].missing_artifact_kind, "catalog_row");
    }

    #[test]
    fn gap_from_publication_finding_produces_entry_for_unresolved_evidence() {
        use crate::curation::publication::{PublicationFinding, PublicationFindingSeverity};
        let finding = PublicationFinding {
            code: "curation.publication.evidence_unresolved",
            severity: PublicationFindingSeverity::Error,
            message: "evidence #0 unresolved".into(),
            evidence_index: Some(0),
        };
        let gap = gap_from_publication_finding(
            AssetKindId::new("recipe.v1"),
            &finding,
            Some("boverket.bbr.8"),
            Some("2011:6"),
            Some("SE".into()),
            "agent:test",
            0,
        )
        .expect("unresolved finding should produce a gap");
        assert_eq!(gap.missing_artifact_kind, "source_entry");
        assert_eq!(gap.kind.as_ref().map(|k| k.as_str()), Some("recipe.v1"));
        assert_eq!(gap.jurisdiction.as_deref(), Some("SE"));
        let ctx = gap.context.as_object().unwrap();
        assert_eq!(ctx.get("source_id").and_then(|v| v.as_str()), Some("boverket.bbr.8"));
        assert_eq!(ctx.get("revision").and_then(|v| v.as_str()), Some("2011:6"));
    }

    #[test]
    fn gap_from_publication_finding_returns_none_for_non_unresolved() {
        use crate::curation::publication::{PublicationFinding, PublicationFindingSeverity};
        let finding = PublicationFinding {
            code: "curation.publication.source_sunset",
            severity: PublicationFindingSeverity::Error,
            message: "sunset".into(),
            evidence_index: Some(0),
        };
        assert!(gap_from_publication_finding(
            AssetKindId::new("recipe.v1"),
            &finding,
            None,
            None,
            None,
            "agent",
            0,
        )
        .is_none());
    }
}
