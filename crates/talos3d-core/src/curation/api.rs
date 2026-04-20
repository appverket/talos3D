//! Pure handler functions for the generic curation MCP surface.
//!
//! Each function takes a `&World` or `&mut World` and returns a
//! serialisable DTO (or ApiResult of one). The actual `#[rmcp::tool]`
//! methods in `plugins::model_api` are thin wrappers that dispatch
//! through `ApiClient` into these handlers via a dedicated thread
//! running the Bevy world. Keeping the logic here (rather than in
//! `model_api.rs`) lets us unit-test each handler directly against a
//! bare `World` without the MCP transport.
//!
//! Scope for PP80 slice 6: source-registry inspection + nomination flow
//! + cross-kind corpus-gap reporting. The asset-level tools
//! (`cite_source`, `inspect_provenance`, `explain_asset_lineage`,
//! `publication_status`) require `RecipeArtifact` / `MaterialSpec` to
//! exist; they land alongside those kinds in PP81 / PP83.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::identity::{AssetKindId, SourceId, SourceRevision};
use super::nomination::{Nomination, NominationId, NominationKind, NominationQueue};
use super::provenance::JurisdictionTag;
use super::registry::{SourceFilter, SourceRegistry};
use super::source::{SourceLicense, SourceRegistryEntry, SourceTier};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Short summary of a source entry for list responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SourceInfo {
    pub source_id: String,
    pub revision: String,
    pub title: String,
    pub publisher: String,
    pub tier: String,
    pub license: String,
    pub status: String,
    pub jurisdiction: Option<String>,
    pub canonical_url: Option<String>,
}

impl From<&SourceRegistryEntry> for SourceInfo {
    fn from(e: &SourceRegistryEntry) -> Self {
        Self {
            source_id: e.source_id.0.clone(),
            revision: e.revision.0.clone(),
            title: e.title.clone(),
            publisher: e.publisher.clone(),
            tier: format!("{:?}", e.tier).to_lowercase(),
            license: format!("{:?}", e.license).to_lowercase(),
            status: source_status_tag(&e.status).into(),
            jurisdiction: e.jurisdiction.as_ref().map(|j| j.0.clone()),
            canonical_url: e.canonical_url.clone(),
        }
    }
}

fn source_status_tag(status: &super::source::SourceStatus) -> &'static str {
    match status {
        super::source::SourceStatus::Active => "active",
        super::source::SourceStatus::Superseded { .. } => "superseded",
        super::source::SourceStatus::Sunset { .. } => "sunset",
    }
}

/// Summary of a pending nomination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct NominationInfo {
    pub id: String,
    pub action: String, // "add_source" | "sunset_source"
    pub proposed_by: String,
    pub proposed_at: i64,
    pub justification: Option<String>,
    /// For `add_source`: the proposed entry id + revision. For
    /// `sunset_source`: the target id + revision + replacement.
    pub target_source_id: String,
    pub target_revision: String,
    pub replacement_source_id: Option<String>,
    pub reason: Option<String>,
}

impl From<&Nomination> for NominationInfo {
    fn from(n: &Nomination) -> Self {
        match &n.kind {
            NominationKind::AddSource { entry } => Self {
                id: n.id.0.clone(),
                action: "add_source".into(),
                proposed_by: n.proposed_by.clone(),
                proposed_at: n.proposed_at,
                justification: n.justification.clone(),
                target_source_id: entry.source_id.0.clone(),
                target_revision: entry.revision.0.clone(),
                replacement_source_id: None,
                reason: None,
            },
            NominationKind::SunsetSource {
                source_id,
                revision,
                replacement,
                reason,
            } => Self {
                id: n.id.0.clone(),
                action: "sunset_source".into(),
                proposed_by: n.proposed_by.clone(),
                proposed_at: n.proposed_at,
                justification: n.justification.clone(),
                target_source_id: source_id.0.clone(),
                target_revision: revision.0.clone(),
                replacement_source_id: replacement.as_ref().map(|s| s.0.clone()),
                reason: Some(reason.clone()),
            },
        }
    }
}

/// Filter parameters for `list_sources`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ListSourcesFilter {
    pub tier: Option<String>,
    pub jurisdiction: Option<String>,
    pub license: Option<String>,
    pub publisher: Option<String>,
    #[serde(default)]
    pub active_only: bool,
}

impl ListSourcesFilter {
    fn into_source_filter(self) -> SourceFilter {
        SourceFilter {
            tier: self.tier.as_deref().and_then(parse_tier),
            jurisdiction: self.jurisdiction.map(JurisdictionTag),
            license: self.license.as_deref().and_then(parse_license),
            active_only: self.active_only,
            publisher: self.publisher,
        }
    }
}

fn parse_tier(s: &str) -> Option<SourceTier> {
    match s {
        "canonical" => Some(SourceTier::Canonical),
        "jurisdictional" => Some(SourceTier::Jurisdictional),
        "organizational" => Some(SourceTier::Organizational),
        "project" => Some(SourceTier::Project),
        "adhoc" | "ad_hoc" => Some(SourceTier::AdHoc),
        _ => None,
    }
}

fn parse_license(s: &str) -> Option<SourceLicense> {
    match s {
        "publicdomain" | "public_domain" => Some(SourceLicense::PublicDomain),
        "officialgovernmentpublication" | "official_government_publication" => {
            Some(SourceLicense::OfficialGovernmentPublication)
        }
        "permissivecite" | "permissive_cite" => Some(SourceLicense::PermissiveCite),
        "licensedexcerpt" | "licensed_excerpt" => Some(SourceLicense::LicensedExcerpt),
        "userattachedprivate" | "user_attached_private" => Some(SourceLicense::UserAttachedPrivate),
        _ => None,
    }
}

/// Plain-english structured error for nomination/source ops.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ApiFailure {
    pub code: String,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiFailure>;

// ---------------------------------------------------------------------------
// Handlers — query
// ---------------------------------------------------------------------------

/// `list_sources { tier?, jurisdiction?, license?, publisher?, active_only }`
pub fn list_sources(world: &World, filter: ListSourcesFilter) -> Vec<SourceInfo> {
    let Some(registry) = world.get_resource::<SourceRegistry>() else {
        return Vec::new();
    };
    let filter = filter.into_source_filter();
    registry.list(&filter).into_iter().map(SourceInfo::from).collect()
}

/// `get_source { source_id, revision }`
pub fn get_source(
    world: &World,
    source_id: &str,
    revision: &str,
) -> ApiResult<SourceInfo> {
    let registry = world
        .get_resource::<SourceRegistry>()
        .ok_or_else(|| ApiFailure {
            code: "curation.source_registry_missing".into(),
            message: "SourceRegistry resource not installed".into(),
        })?;
    let id = SourceId::new(source_id);
    let rev = SourceRevision::new(revision);
    registry
        .get(&id, &rev)
        .map(SourceInfo::from)
        .ok_or_else(|| ApiFailure {
            code: "curation.source_not_found".into(),
            message: format!("no source entry for {source_id}@{revision}"),
        })
}

/// `list_nominations`
pub fn list_nominations(world: &World) -> Vec<NominationInfo> {
    let Some(queue) = world.get_resource::<NominationQueue>() else {
        return Vec::new();
    };
    queue.list().iter().map(NominationInfo::from).collect()
}

// ---------------------------------------------------------------------------
// Handlers — mutation
// ---------------------------------------------------------------------------

/// `nominate_source { entry, justification? }`
pub fn nominate_source(
    world: &mut World,
    entry: SourceRegistryEntry,
    proposed_by: impl Into<String>,
    proposed_at: i64,
    justification: Option<String>,
) -> ApiResult<NominationInfo> {
    let mut queue = world
        .get_resource_mut::<NominationQueue>()
        .ok_or_else(|| ApiFailure {
            code: "curation.nomination_queue_missing".into(),
            message: "NominationQueue resource not installed".into(),
        })?;
    let id = queue.push(
        NominationKind::AddSource { entry },
        proposed_by,
        proposed_at,
        justification,
    );
    let info = queue
        .get(&id)
        .map(NominationInfo::from)
        .expect("just-pushed nomination must be retrievable");
    Ok(info)
}

/// `nominate_sunset { source_id, revision, replacement_source_id?, reason }`
pub fn nominate_sunset(
    world: &mut World,
    source_id: &str,
    revision: &str,
    replacement_source_id: Option<String>,
    reason: String,
    proposed_by: impl Into<String>,
    proposed_at: i64,
    justification: Option<String>,
) -> ApiResult<NominationInfo> {
    let mut queue = world
        .get_resource_mut::<NominationQueue>()
        .ok_or_else(|| ApiFailure {
            code: "curation.nomination_queue_missing".into(),
            message: "NominationQueue resource not installed".into(),
        })?;
    let id = queue.push(
        NominationKind::SunsetSource {
            source_id: SourceId::new(source_id),
            revision: SourceRevision::new(revision),
            replacement: replacement_source_id.map(SourceId::new),
            reason,
        },
        proposed_by,
        proposed_at,
        justification,
    );
    let info = queue
        .get(&id)
        .map(NominationInfo::from)
        .expect("just-pushed nomination must be retrievable");
    Ok(info)
}

/// `approve_nomination { nomination_id }`
pub fn approve_nomination(world: &mut World, nomination_id: &str) -> ApiResult<NominationInfo> {
    let id = NominationId(nomination_id.to_string());
    let mut queue = world
        .remove_resource::<NominationQueue>()
        .ok_or_else(|| ApiFailure {
            code: "curation.nomination_queue_missing".into(),
            message: "NominationQueue resource not installed".into(),
        })?;
    let Some(mut registry) = world.get_resource_mut::<SourceRegistry>() else {
        // Put the queue back before returning the error.
        world.insert_resource(queue);
        return Err(ApiFailure {
            code: "curation.source_registry_missing".into(),
            message: "SourceRegistry resource not installed".into(),
        });
    };
    let result = queue.approve(&id, &mut registry);
    drop(registry);
    world.insert_resource(queue);
    match result {
        Ok(n) => Ok(NominationInfo::from(&n)),
        Err(e) => Err(ApiFailure {
            code: match e {
                super::nomination::NominationError::NotFound(_) => {
                    "curation.nomination_not_found".into()
                }
                super::nomination::NominationError::TargetNotInRegistry { .. } => {
                    "curation.sunset_target_missing".into()
                }
            },
            message: e.to_string(),
        }),
    }
}

/// `reject_nomination { nomination_id, reason? }`
pub fn reject_nomination(
    world: &mut World,
    nomination_id: &str,
    reason: Option<String>,
) -> ApiResult<NominationInfo> {
    let id = NominationId(nomination_id.to_string());
    let mut queue = world
        .get_resource_mut::<NominationQueue>()
        .ok_or_else(|| ApiFailure {
            code: "curation.nomination_queue_missing".into(),
            message: "NominationQueue resource not installed".into(),
        })?;
    match queue.reject(&id, reason) {
        Ok(n) => Ok(NominationInfo::from(&n)),
        Err(e) => Err(ApiFailure {
            code: "curation.nomination_not_found".into(),
            message: e.to_string(),
        }),
    }
}

/// `report_corpus_gap { kind, jurisdiction?, missing_artifact_kind, context, rationale? }`
///
/// Cross-kind variant of the shipped `request_corpus_expansion`. Emits
/// a `CorpusGap` with `kind` populated and `element_class` empty.
pub fn report_corpus_gap(
    world: &mut World,
    kind: AssetKindId,
    jurisdiction: Option<String>,
    missing_artifact_kind: String,
    context: serde_json::Value,
    reported_by: impl Into<String>,
    reported_at: i64,
) -> String {
    use crate::plugins::corpus_gap::CorpusGapQueue;
    if !world.contains_resource::<CorpusGapQueue>() {
        world.insert_resource(CorpusGapQueue::default());
    }
    world
        .resource_mut::<CorpusGapQueue>()
        .push_for_kind(
            kind,
            jurisdiction,
            missing_artifact_kind,
            context,
            reported_by,
            reported_at,
        )
        .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::{
        identity::ContentHash,
        plugin::CurationPlugin,
        source::{SourceLicense, SourceRegistryEntry, SourceTier},
    };

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(CurationPlugin);
        app.update();
        app
    }

    fn entry(id: &str, rev: &str, tier: SourceTier) -> SourceRegistryEntry {
        SourceRegistryEntry::new(
            SourceId::new(id),
            SourceRevision::new(rev),
            format!("{id} {rev}"),
            "Test",
            tier,
            SourceLicense::PublicDomain,
        )
    }

    #[test]
    fn list_sources_returns_canonical_seeds_by_default() {
        let app = app();
        let sources = list_sources(app.world(), ListSourcesFilter::default());
        // The seed set includes ISO 129-1, ASME Y14.5, ISO 80000-1.
        assert_eq!(sources.len(), 3);
        assert!(sources.iter().any(|s| s.source_id == "iso.129-1"));
    }

    #[test]
    fn list_sources_filters_by_tier() {
        let mut app = app();
        app.world_mut()
            .resource_mut::<SourceRegistry>()
            .insert(entry("proj.doc", "v1", SourceTier::Project));
        let filter = ListSourcesFilter {
            tier: Some("canonical".into()),
            ..Default::default()
        };
        let sources = list_sources(app.world(), filter);
        assert_eq!(sources.len(), 3);
        assert!(sources.iter().all(|s| s.tier == "canonical"));
    }

    #[test]
    fn list_sources_filters_by_publisher_and_active_only() {
        let mut app = app();
        app.world_mut()
            .resource_mut::<SourceRegistry>()
            .insert(entry("proj.doc", "v1", SourceTier::Project));
        let filter = ListSourcesFilter {
            publisher: Some("Test".into()),
            active_only: true,
            ..Default::default()
        };
        let sources = list_sources(app.world(), filter);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_id, "proj.doc");
    }

    #[test]
    fn get_source_returns_specific_revision_or_error() {
        let app = app();
        let ok = get_source(app.world(), "iso.129-1", "2018").unwrap();
        assert_eq!(ok.source_id, "iso.129-1");
        let err = get_source(app.world(), "iso.129-1", "9999").unwrap_err();
        assert_eq!(err.code, "curation.source_not_found");
    }

    #[test]
    fn nominate_source_adds_to_queue_without_touching_registry() {
        let mut app = app();
        let e = entry("new.source", "v1", SourceTier::Project);
        let info = nominate_source(app.world_mut(), e.clone(), "agent:test", 0, None).unwrap();
        assert_eq!(info.action, "add_source");
        assert_eq!(info.target_source_id, "new.source");

        // Registry not yet mutated.
        let reg = app.world().resource::<SourceRegistry>();
        assert!(reg
            .get(&SourceId::new("new.source"), &SourceRevision::new("v1"))
            .is_none());

        // Queue has one entry.
        let noms = list_nominations(app.world());
        assert_eq!(noms.len(), 1);
    }

    #[test]
    fn approve_nomination_applies_add_source_to_registry() {
        let mut app = app();
        let e = entry("new.source", "v1", SourceTier::Project);
        let info = nominate_source(app.world_mut(), e, "agent:test", 0, None).unwrap();
        let approved = approve_nomination(app.world_mut(), &info.id).unwrap();
        assert_eq!(approved.action, "add_source");

        let reg = app.world().resource::<SourceRegistry>();
        assert!(reg
            .get(&SourceId::new("new.source"), &SourceRevision::new("v1"))
            .is_some());

        // Queue is drained.
        assert!(list_nominations(app.world()).is_empty());
    }

    #[test]
    fn approve_sunset_nomination_updates_status() {
        let mut app = app();
        // Seed the target.
        app.world_mut()
            .resource_mut::<SourceRegistry>()
            .insert(entry("proj.old", "v1", SourceTier::Project));
        let info = nominate_sunset(
            app.world_mut(),
            "proj.old",
            "v1",
            Some("proj.new".into()),
            "replaced".into(),
            "agent:test",
            0,
            None,
        )
        .unwrap();
        approve_nomination(app.world_mut(), &info.id).unwrap();
        let e = app
            .world()
            .resource::<SourceRegistry>()
            .get(&SourceId::new("proj.old"), &SourceRevision::new("v1"))
            .unwrap();
        assert_eq!(e.status, crate::curation::SourceStatus::Superseded {
            replacement: Some(SourceId::new("proj.new")),
        });
    }

    #[test]
    fn approve_sunset_of_missing_target_surfaces_structured_error() {
        let mut app = app();
        let info = nominate_sunset(
            app.world_mut(),
            "unknown",
            "?",
            None,
            "test".into(),
            "agent:test",
            0,
            None,
        )
        .unwrap();
        let err = approve_nomination(app.world_mut(), &info.id).unwrap_err();
        assert_eq!(err.code, "curation.sunset_target_missing");
        // Nomination still in queue (failed approval must not drop it).
        assert_eq!(list_nominations(app.world()).len(), 1);
    }

    #[test]
    fn reject_nomination_drops_without_registry_change() {
        let mut app = app();
        let e = entry("x", "v1", SourceTier::Project);
        let info = nominate_source(app.world_mut(), e, "agent:test", 0, None).unwrap();
        reject_nomination(app.world_mut(), &info.id, Some("out of scope".into())).unwrap();
        assert!(list_nominations(app.world()).is_empty());
        let reg = app.world().resource::<SourceRegistry>();
        assert!(reg.get(&SourceId::new("x"), &SourceRevision::new("v1")).is_none());
    }

    #[test]
    fn report_corpus_gap_cross_kind_ends_up_in_queue() {
        let mut app = app();
        let id = report_corpus_gap(
            app.world_mut(),
            AssetKindId::new("material_spec.v1"),
            Some("SE".into()),
            "catalog_row".into(),
            serde_json::json!({"needed_for": "C24 timber"}),
            "agent:test",
            0,
        );
        assert!(id.starts_with("gap-"));
        let queue = app.world().resource::<crate::plugins::corpus_gap::CorpusGapQueue>();
        let gap = queue.list().iter().find(|g| g.id.0 == id).unwrap();
        assert_eq!(gap.kind.as_ref().map(|k| k.as_str()), Some("material_spec.v1"));
    }

    #[test]
    fn list_sources_infos_include_status_and_tier_lowercase() {
        let app = app();
        let sources = list_sources(app.world(), ListSourcesFilter::default());
        for s in &sources {
            assert_eq!(s.status, "active");
            assert_eq!(s.tier, "canonical");
        }
    }

    #[test]
    fn source_info_round_trips_through_json() {
        let e = entry("a", "v1", SourceTier::Project)
            .with_content_hash(ContentHash::new("blake3:x"));
        let info = SourceInfo::from(&e);
        let json = serde_json::to_string(&info).unwrap();
        let parsed: SourceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, info);
    }

    #[test]
    fn nomination_info_round_trips_for_both_kinds() {
        let add = Nomination {
            id: NominationId("nom-1".into()),
            kind: NominationKind::AddSource {
                entry: entry("a", "v1", SourceTier::Project),
            },
            proposed_by: "agent".into(),
            proposed_at: 0,
            justification: None,
        };
        let sunset = Nomination {
            id: NominationId("nom-2".into()),
            kind: NominationKind::SunsetSource {
                source_id: SourceId::new("a"),
                revision: SourceRevision::new("v1"),
                replacement: Some(SourceId::new("b")),
                reason: "tests".into(),
            },
            proposed_by: "agent".into(),
            proposed_at: 0,
            justification: Some("because".into()),
        };
        for n in [add, sunset] {
            let info = NominationInfo::from(&n);
            let json = serde_json::to_string(&info).unwrap();
            let parsed: NominationInfo = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, info);
        }
    }
}
