//! Admission for reviewed, bundled corpus releases. This is a pack boundary,
//! not an automatic export of user knowledge or a remote trust/signature bypass.
use super::{
    pack::{PackManifest, PackRegistry},
    Scope, Trust,
};
use crate::plugins::{corpus_gap::CorpusPassageRegistry, knowledge_persistence::PersistedPassage};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusReleaseReview {
    pub reviewer: String,
    pub general_knowledge_only: bool,
    pub private_content_removed: bool,
    pub redistribution_rights: String,
    pub verification_refs: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusReleaseManifest {
    pub pack: PackManifest,
    pub body_blake3: String,
    pub review: CorpusReleaseReview,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CorpusReleaseInfo {
    pub pack_id: String,
    pub revision: String,
    pub body_blake3: String,
    pub asset_ids: Vec<String>,
}
#[derive(Resource, Default)]
pub struct InstalledCorpusReleases(pub BTreeMap<String, CorpusReleaseInfo>);

pub fn validate_release(
    manifest: &CorpusReleaseManifest,
    bytes: &[u8],
) -> Result<Vec<PersistedPassage>, String> {
    let r = &manifest.review;
    if !r.general_knowledge_only
        || !r.private_content_removed
        || r.reviewer.trim().is_empty()
        || r.redistribution_rights.trim().is_empty()
        || r.verification_refs.is_empty()
    {
        return Err(
            "Corpus release requires explicit confidentiality, rights and evidence review".into(),
        );
    }
    if bytes.len() > 2 * 1024 * 1024
        || blake3::hash(bytes).to_hex().as_str() != manifest.body_blake3
    {
        return Err("Corpus release body does not match its reviewed content hash".into());
    }
    // v1 passage packs have no executable or unresolved private dependencies.
    if !manifest.pack.dependencies.is_empty() || !manifest.pack.sources.is_empty() {
        return Err(
            "This passage release requires a dependency/source admission implementation".into(),
        );
    }
    if let Some(req) = &manifest.pack.compatibility.core_api {
        let requirement = semver::VersionReq::parse(req.as_str()).map_err(|e| e.to_string())?;
        let version =
            semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
        if !requirement.matches(&version) {
            return Err("Corpus release is incompatible with this core API".into());
        }
    }
    let passages: Vec<PersistedPassage> =
        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if passages.is_empty() || passages.len() > 128 {
        return Err("Invalid corpus release size".into());
    }
    let mut ids = BTreeSet::new();
    for p in &passages {
        let meta = p
            .meta
            .as_ref()
            .ok_or("Legacy passages are private; explicit curation metadata is required")?;
        if meta.scope != Scope::Shipped
            || meta.trust != Trust::Published
            || !meta.validation.is_passing()
            || !meta.dependencies.is_empty()
            || !meta.provenance.catalog_dependencies.is_empty()
            || meta.id.as_str() != p.passage_ref
            || p.citation.trim().is_empty()
            || p.license.as_deref().is_none_or(|s| s.trim().is_empty())
            || p.text.trim().is_empty()
        {
            return Err(format!(
                "Passage {} has private, unreviewed, unresolved or incomplete release metadata",
                p.passage_ref
            ));
        }
        let membership = meta
            .pack_membership
            .as_ref()
            .ok_or("Passage lacks release membership")?;
        if membership.pack_id != manifest.pack.pack_id
            || membership.revision != manifest.pack.revision
            || !ids.insert(p.passage_ref.clone())
        {
            return Err("Corpus pack identity/membership is inconsistent".into());
        }
    }
    let assets: BTreeSet<_> = manifest.pack.assets.iter().map(|id| id.0.clone()).collect();
    if assets.len() != manifest.pack.assets.len() || assets != ids {
        return Err("Corpus manifest assets differ from reviewed body".into());
    }
    Ok(passages)
}

/// Only bundled operator-owned bytes enter this path. Remote packs still require
/// existing signed publication/entitlement checks before becoming trusted bundles.
pub fn install_bundled_release(
    world: &mut World,
    manifest_json: &str,
    body: &str,
) -> Result<CorpusReleaseInfo, String> {
    let manifest: CorpusReleaseManifest =
        serde_json::from_str(manifest_json).map_err(|e| e.to_string())?;
    let passages = validate_release(&manifest, body.as_bytes())?;
    world.init_resource::<InstalledCorpusReleases>();
    let key = format!("{}@{}", manifest.pack.pack_id.0, manifest.pack.revision.0);
    if let Some(old) = world.resource::<InstalledCorpusReleases>().0.get(&key) {
        if old.body_blake3 != manifest.body_blake3 {
            return Err("Released corpus revisions are immutable".into());
        }
        return Ok(old.clone());
    }
    let mut registry = world
        .get_resource::<CorpusPassageRegistry>()
        .cloned()
        .unwrap_or_default();
    for p in passages {
        registry.register_released(p)?;
    }
    world.init_resource::<PackRegistry>();
    world
        .resource_mut::<PackRegistry>()
        .register(manifest.pack.clone())
        .map_err(|e| e.to_string())?;
    let info = CorpusReleaseInfo {
        pack_id: manifest.pack.pack_id.0,
        revision: manifest.pack.revision.0,
        body_blake3: manifest.body_blake3,
        asset_ids: manifest.pack.assets.into_iter().map(|id| id.0).collect(),
    };
    world.insert_resource(registry);
    world
        .resource_mut::<InstalledCorpusReleases>()
        .0
        .insert(key, info.clone());
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::{
        pack::PackRef, AssetId, AssetKindId, CurationMeta, PackId, PackRevision, Provenance,
        ValidationStatus,
    };
    fn fixture() -> (CorpusReleaseManifest, Vec<u8>) {
        let mut p:PersistedPassage=serde_json::from_value(serde_json::json!({"passage_ref":"test.lesson","text":"A general authoring check","citation":"Operator-authored test","license":"cc0"})).unwrap();
        let pack = PackManifest::new(
            PackId::new("test.pack"),
            PackRevision::new("1"),
            "Test corpus",
        );
        let mut meta = CurationMeta::new(
            AssetId::new("test.lesson"),
            AssetKindId::new("corpus_passage.v1"),
            Provenance::freeform(crate::plugins::refinement::AgentId("operator".into())),
        )
        .with_scope(Scope::Shipped)
        .with_trust(Trust::Published)
        .with_pack(PackRef::new(pack.pack_id.clone(), pack.revision.clone()));
        meta.validation = ValidationStatus::Passing;
        p.meta = Some(meta);
        let bytes = serde_json::to_vec(&vec![p]).unwrap();
        let mut manifest = CorpusReleaseManifest {
            pack,
            body_blake3: blake3::hash(&bytes).to_hex().to_string(),
            review: CorpusReleaseReview {
                reviewer: "operator".into(),
                general_knowledge_only: true,
                private_content_removed: true,
                redistribution_rights: "Own test text".into(),
                verification_refs: vec!["regression test".into()],
            },
        };
        manifest.pack.assets = vec![AssetId::new("test.lesson")];
        (manifest, bytes)
    }
    #[test]
    fn release_requires_review_integrity_and_shipped_metadata() {
        let (mut manifest, bytes) = fixture();
        assert!(validate_release(&manifest, &bytes).is_ok());
        let mut changed = bytes.clone();
        changed.push(b' ');
        assert!(validate_release(&manifest, &changed).is_err());
        manifest.review.private_content_removed = false;
        assert!(validate_release(&manifest, &bytes).is_err());
        manifest.review.private_content_removed = true;
        let mut passages = validate_release(&manifest, &bytes).unwrap();
        passages[0].meta.as_mut().unwrap().scope = Scope::Project;
        let private = serde_json::to_vec(&passages).unwrap();
        manifest.body_blake3 = blake3::hash(&private).to_hex().to_string();
        assert!(validate_release(&manifest, &private).is_err());
    }
    #[test]
    fn private_learning_cannot_override_released_content() {
        let (manifest, bytes) = fixture();
        let mut world = World::new();
        install_bundled_release(
            &mut world,
            &serde_json::to_string(&manifest).unwrap(),
            std::str::from_utf8(&bytes).unwrap(),
        )
        .unwrap();
        let mut p = validate_release(&manifest, &bytes).unwrap().remove(0);
        p.meta = None;
        p.text = "Private replacement".into();
        assert!(world
            .resource_mut::<CorpusPassageRegistry>()
            .register_private(p)
            .is_err());
        let entry = world
            .resource::<CorpusPassageRegistry>()
            .get(&crate::capability_registry::PassageRef(
                "test.lesson".into(),
            ))
            .unwrap();
        assert_eq!(entry.scope, Scope::Shipped);
        assert_eq!(entry.text, "A general authoring check");
    }
}
