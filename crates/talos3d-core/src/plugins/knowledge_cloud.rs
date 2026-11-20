//! Governed, typed knowledge data delivered independently of application builds.
//! Network/signature admission occurs off the ECS thread. Only admitted payloads
//! enter the runtime registries; a caller cannot submit raw bytes as a release.
use super::{agent_skill_cloud as cloud, agent_skills::AgentCorpusConnection};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "talos3d.knowledge-asset.v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub element_classes: Vec<String>,
    pub path_kind: String,
    pub required_tool_ids: Vec<String>,
    pub payload: Value,
    pub applicability: String,
    pub limitations: Vec<String>,
    pub evidence: Vec<String>,
    pub redistribution_rights: String,
    pub contains_private_context: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexEntry {
    pub content_hash: String,
    pub asset: Value,
    pub sequence: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Index {
    pub schema: String,
    pub epoch: i64,
    pub assets: Vec<IndexEntry>,
    pub withdrawn_asset_ids: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssetRequest {
    pub asset_id: String,
    pub content_hash: String,
}
/// Created only by verified retrieval, never deserialized from an MCP argument.
#[derive(Clone, Debug)]
pub struct AdmittedAsset {
    asset: Asset,
    revision: String,
}
#[derive(Resource, Default)]
pub struct InstalledAssets(BTreeMap<String, InstalledAsset>);
#[derive(Clone)]
struct InstalledAsset {
    revision: String,
    kind: String,
    runtime_id: String,
}

pub async fn index(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
) -> Result<Index, String> {
    let bytes = cloud::request(c, "/assets", None).await?;
    let value = cloud::verify_signed_json(&bytes, &c.trusted_public_keys)?;
    let index: Index = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if index.schema != SCHEMA || index.assets.len() > 1000 || index.withdrawn_asset_ids.len() > 1000
    {
        return Err("Unsupported knowledge index".into());
    }
    store.observe_knowledge_index(c, index.epoch, &index.withdrawn_asset_ids)?;
    Ok(index)
}
pub async fn retrieve(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
    request: &AssetRequest,
) -> Result<AdmittedAsset, String> {
    let before = index(c, store).await?;
    let expected = before
        .assets
        .iter()
        .find(|e| e.asset["id"] == request.asset_id && e.content_hash == request.content_hash)
        .ok_or("Knowledge revision is not currently released; rediscover")?;
    if request.content_hash.len() != 64
        || !request.content_hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Invalid knowledge revision".into());
    }
    let bytes = cloud::request(c, &format!("/revisions/{}", request.content_hash), None).await?;
    if hex::encode(Sha256::digest(&bytes)) != request.content_hash {
        return Err("Knowledge checksum mismatch".into());
    }
    let asset: Asset = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if asset.schema != SCHEMA
        || asset.id != request.asset_id
        || expected.asset["asset_kind"] != asset.kind
        || asset.contains_private_context
    {
        return Err("Knowledge release identity mismatch".into());
    }
    let latest = index(c, store).await?;
    if !latest
        .assets
        .iter()
        .any(|e| e.asset["id"] == asset.id && e.content_hash == request.content_hash)
    {
        return Err("Knowledge changed or was withdrawn during retrieval; rediscover".into());
    }
    Ok(AdmittedAsset {
        asset,
        revision: request.content_hash.clone(),
    })
}
pub fn required_tools(admitted: &AdmittedAsset) -> &[String] {
    &admitted.asset.required_tool_ids
}
pub fn inspect(admitted: &AdmittedAsset) -> Value {
    json!({"asset":admitted.asset,"content_hash":admitted.revision,"delivery":"verified_dynamic_corpus","next_tool":"install_knowledge_asset"})
}
fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct CatalogData {
    id: String,
    label: String,
    description: String,
    category: crate::capability_registry::CatalogCategory,
    region: Option<String>,
    license: crate::capability_registry::LicenseTag,
    source_version: String,
    rows: Vec<crate::capability_registry::CatalogRow>,
}

pub fn install(world: &mut World, admitted: AdmittedAsset) -> Result<Value, String> {
    use crate::curation::{RecipeArtifact, RecipeArtifactRegistry, Scope, Trust};
    use crate::plugins::modeling::definition::{
        DefinitionLibrary, DefinitionLibraryRegistry, DefinitionLibraryScope,
    };
    use crate::relational::registry::{
        ParametricPlacementContract, ParametricRegistry, ParametricTypeDef,
    };
    let a = admitted.asset;
    // Parse and validate the complete supported payload before any registry mutation.
    let runtime_id = match a.kind.as_str() {
        "parametric_type.v1" => {
            let definition: ParametricTypeDef = decode(a.payload["definition"].clone())?;
            let contract: Option<ParametricPlacementContract> = a
                .payload
                .get("placement_contract")
                .cloned()
                .map(decode)
                .transpose()?
                .flatten();
            if definition.id != a.id {
                return Err("Parametric identity must match asset identity".into());
            }
            // Exercise expression/driver validation through the existing substrate.
            if let Some(result) = definition.evaluate_representation(&BTreeMap::new()) {
                result?;
            }
            if world
                .get_resource::<ParametricRegistry>()
                .and_then(|r| r.get(&a.id))
                .is_some_and(|d| !d.public)
            {
                return Err("Dynamic release cannot overwrite a private/internal type".into());
            }
            world.init_resource::<ParametricRegistry>();
            let mut registry = world.resource_mut::<ParametricRegistry>();
            registry.remove(&definition.id);
            registry.register(definition);
            if let Some(contract) = contract {
                registry.register_placement_contract(&a.id, contract);
            }
            a.id.clone()
        }
        "definition_library.v1" => {
            let mut library: DefinitionLibrary = decode(a.payload.clone())?;
            if library.id.0 != a.id {
                return Err("Library identity must match asset identity".into());
            }
            if world
                .get_resource::<DefinitionLibraryRegistry>()
                .and_then(|r| r.get(&library.id))
                .is_some_and(|l| {
                    !matches!(
                        l.scope,
                        DefinitionLibraryScope::Bundled | DefinitionLibraryScope::SharedRelease
                    )
                })
            {
                return Err("Dynamic release cannot overwrite a private library".into());
            }
            if library.definitions.is_empty() || library.definitions.len() > 1000 {
                return Err("Bounded nonempty definition library required".into());
            }
            for (id, definition) in &library.definitions {
                if id != &definition.id {
                    return Err("Library definition key/identity mismatch".into());
                }
                definition
                    .validate_with(|dependency| library.definitions.contains_key(dependency))?;
            }
            library.scope = DefinitionLibraryScope::SharedRelease;
            library.source_path = Some(format!("corpus:{}@{}", a.id, admitted.revision));
            world.init_resource::<DefinitionLibraryRegistry>();
            world
                .resource_mut::<DefinitionLibraryRegistry>()
                .insert(library);
            a.id.clone()
        }
        "recipe.v1" => {
            let mut recipe: RecipeArtifact = decode(a.payload.clone())?;
            if recipe.meta.id.0 != a.id || !recipe.body.is_authoring_script() {
                return Err("Portable recipe identity/body required".into());
            }
            if world
                .get_resource::<RecipeArtifactRegistry>()
                .and_then(|r| r.get(&recipe.meta.id))
                .is_some_and(|r| r.meta.scope != Scope::Shipped)
            {
                return Err("Dynamic release cannot overwrite a private recipe".into());
            }
            recipe
                .body
                .script()
                .ok_or("Portable script required")?
                .validate_structure()
                .map_err(|e| e.to_string())?;
            recipe.meta.scope = Scope::Shipped;
            recipe.meta.trust = Trust::Published;
            world.init_resource::<RecipeArtifactRegistry>();
            world
                .resource_mut::<RecipeArtifactRegistry>()
                .insert(recipe);
            a.id.clone()
        }
        "catalog.v1" => {
            use crate::capability_registry::{
                CapabilityRegistry, CatalogProviderDescriptor, CatalogProviderId,
            };
            let data: CatalogData = decode(a.payload.clone())?;
            if data.id != a.id || data.rows.len() > 10000 {
                return Err("Catalog identity/size mismatch".into());
            }
            let id = CatalogProviderId(data.id);
            let provider = CatalogProviderDescriptor {
                id: id.clone(),
                label: data.label,
                description: data.description,
                category: data.category,
                region: data.region,
                license: data.license,
                source_version: data.source_version,
                query_fn: std::sync::Arc::new(move |filter| {
                    data.rows
                        .iter()
                        .filter(|row| {
                            filter
                                .as_object()
                                .is_none_or(|f| f.iter().all(|(k, v)| row.data.get(k) == Some(v)))
                        })
                        .cloned()
                        .collect()
                }),
            };
            world.init_resource::<CapabilityRegistry>();
            let mut registry = world.resource_mut::<CapabilityRegistry>();
            registry.remove_catalog_provider(&id);
            registry.register_catalog_provider(provider);
            a.id.clone()
        }
        "corpus_passage.v1" => {
            let mut passage: super::knowledge_persistence::PersistedPassage =
                decode(a.payload.clone())?;
            if passage.passage_ref != a.id
                || passage.text.trim().is_empty()
                || passage.citation.trim().is_empty()
                || passage
                    .license
                    .as_deref()
                    .is_none_or(|v| v.trim().is_empty())
            {
                return Err("Passage identity/text required".into());
            }
            // Published scope comes from signed admission, never candidate labels.
            let mut meta = passage.meta.clone().unwrap_or_else(|| {
                crate::curation::CurationMeta::new(
                    crate::curation::AssetId::new(&a.id),
                    crate::curation::AssetKindId::new("corpus_passage.v1"),
                    crate::curation::Provenance::freeform(crate::plugins::refinement::AgentId(
                        "harness-curator".into(),
                    )),
                )
                .with_scope(Scope::Shipped)
                .with_trust(Trust::Published)
            });
            meta.scope = Scope::Shipped;
            meta.trust = Trust::Published;
            meta.provenance.rationale = Some(format!(
                "{}\nAdmitted dynamic release {}",
                meta.provenance.rationale.as_deref().unwrap_or(""),
                admitted.revision
            ));
            meta.validation = crate::curation::ValidationStatus::Passing;
            passage.meta = Some(meta);
            world.init_resource::<super::corpus_gap::CorpusPassageRegistry>();
            let mut registry = world.resource_mut::<super::corpus_gap::CorpusPassageRegistry>();
            if registry
                .get(&crate::capability_registry::PassageRef(a.id.clone()))
                .is_some_and(|p| p.scope != Scope::Shipped)
            {
                return Err("Dynamic release cannot overwrite a private passage".into());
            }
            registry.remove_released(&a.id);
            registry.register_released(passage)?;
            a.id.clone()
        }
        _ => {
            return Err(
                "Knowledge kind has no compatible dynamic installer; record a runtime gap".into(),
            )
        }
    };
    world.init_resource::<InstalledAssets>();
    world.resource_mut::<InstalledAssets>().0.insert(
        a.id.clone(),
        InstalledAsset {
            revision: admitted.revision.clone(),
            kind: a.kind.clone(),
            runtime_id,
        },
    );
    Ok(
        json!({"asset_id":a.id,"kind":a.kind,"content_hash":admitted.revision,"installed":true,"model_modified":false,"next_step":"Rediscover the registered path and use its advertised authoring tool. Installation is not a verified model outcome."}),
    )
}

pub fn reconcile(world: &mut World, index: &Index) -> Value {
    let removed: Vec<_> = world
        .get_resource::<InstalledAssets>()
        .map(|r| {
            r.0.iter()
                .filter(|(id, a)| {
                    !index
                        .assets
                        .iter()
                        .any(|e| e.asset["id"] == id.as_str() && e.content_hash == a.revision)
                })
                .map(|(id, a)| (id.clone(), a.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (id, a) in &removed {
        match a.kind.as_str() {
            "parametric_type.v1" => {
                if let Some(mut r) =
                    world.get_resource_mut::<crate::relational::registry::ParametricRegistry>()
                {
                    r.remove(&a.runtime_id);
                }
            }
            "definition_library.v1" => {
                if let Some(mut r) = world
                    .get_resource_mut::<super::modeling::definition::DefinitionLibraryRegistry>()
                {
                    r.remove(&super::modeling::definition::DefinitionLibraryId(
                        a.runtime_id.clone(),
                    ));
                }
            }
            "recipe.v1" => {
                if let Some(mut r) =
                    world.get_resource_mut::<crate::curation::RecipeArtifactRegistry>()
                {
                    r.remove(&crate::curation::AssetId::new(&a.runtime_id));
                }
            }
            "catalog.v1" => {
                if let Some(mut r) =
                    world.get_resource_mut::<crate::capability_registry::CapabilityRegistry>()
                {
                    r.remove_catalog_provider(&crate::capability_registry::CatalogProviderId(
                        a.runtime_id.clone(),
                    ));
                }
            }
            "corpus_passage.v1" => {
                if let Some(mut r) =
                    world.get_resource_mut::<super::corpus_gap::CorpusPassageRegistry>()
                {
                    r.remove_released(&a.runtime_id);
                }
            }
            _ => {}
        }
        world.resource_mut::<InstalledAssets>().0.remove(id);
    }
    json!({"epoch":index.epoch,"removed":removed.iter().map(|(id,_)|id).collect::<Vec<_>>(),"assets":index.assets,"model_modified":false})
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct LegacyRequest {
    pub kind: String,
    pub id: String,
}

/// Migration capture includes only shared registries; never document-local
/// Definitions, occurrences, project passages, or ephemeral parametric types.
pub fn legacy_list(world: &World) -> Value {
    let installed = world.get_resource::<InstalledAssets>();
    let mut items = Vec::new();
    let mut add = |kind: &str, id: &str, label: &str, portable: bool| {
        if installed.is_some_and(|r| r.0.contains_key(id)) {
            return;
        }
        items.push(json!({"id":id,"kind":kind,"label":label,"portable":portable,"review_required":true,"next_tool":"get_legacy_knowledge_asset","arguments":{"kind":kind,"id":id}}));
    };
    if let Some(r) = world.get_resource::<crate::relational::registry::ParametricRegistry>() {
        for (id, label) in r.list_public() {
            add("parametric_type.v1", &id, &label, true);
        }
    }
    if let Some(r) = world.get_resource::<super::modeling::definition::DefinitionLibraryRegistry>()
    {
        for l in r.list() {
            if l.scope == super::modeling::definition::DefinitionLibraryScope::Bundled {
                add("definition_library.v1", &l.id.0, &l.name, true);
            }
        }
    }
    if let Some(r) = world.get_resource::<crate::curation::RecipeArtifactRegistry>() {
        for a in r.iter() {
            if a.meta.scope == crate::curation::Scope::Shipped {
                add(
                    "recipe.v1",
                    &a.meta.id.0,
                    &a.meta.id.0,
                    a.body.is_authoring_script(),
                );
            }
        }
    }
    if let Some(r) = world.get_resource::<super::corpus_gap::CorpusPassageRegistry>() {
        for (id, p) in r.iter() {
            if p.scope == crate::curation::Scope::Shipped {
                add("corpus_passage.v1", id, id, true);
            }
        }
    }
    if let Some(r) = world.get_resource::<crate::capability_registry::CapabilityRegistry>() {
        for p in r.catalog_provider_descriptors() {
            add("catalog.v1", &p.id.0, &p.label, true);
        }
    }
    items.sort_by(|a, b| {
        a["kind"]
            .as_str()
            .cmp(&b["kind"].as_str())
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    json!({"assets":items,"publication_authorized":false,"notice":"Legacy shared content is a migration candidate, not independently reviewed knowledge. Resolve evidence, rights, applicability and limitations before curator submission. Unsupported native bodies must become explicit gaps."})
}
pub fn legacy_get(world: &World, request: LegacyRequest) -> Result<Value, String> {
    if !legacy_list(world)["assets"].as_array().is_some_and(|v| {
        v.iter()
            .any(|a| a["id"] == request.id && a["kind"] == request.kind)
    }) {
        return Err("Not an exposed shared legacy asset".into());
    }
    let payload = match request.kind.as_str() {
        "parametric_type.v1" => super::parametric_mcp::world_get_type(
            world,
            super::parametric_mcp::GetParametricTypeRequest {
                type_id: request.id.clone(),
            },
        )?,
        "definition_library.v1" => serde_json::to_value(
            world
                .resource::<super::modeling::definition::DefinitionLibraryRegistry>()
                .get(&super::modeling::definition::DefinitionLibraryId(
                    request.id.clone(),
                ))
                .ok_or("Missing library")?,
        )
        .map_err(|e| e.to_string())?,
        "recipe.v1" => serde_json::to_value(
            world
                .resource::<crate::curation::RecipeArtifactRegistry>()
                .get(&crate::curation::AssetId::new(&request.id))
                .ok_or("Missing recipe")?,
        )
        .map_err(|e| e.to_string())?,
        "corpus_passage.v1" => {
            let p = world
                .resource::<super::corpus_gap::CorpusPassageRegistry>()
                .get(&crate::capability_registry::PassageRef(request.id.clone()))
                .ok_or("Missing passage")?;
            match &p.persisted {
                Some(p) => serde_json::to_value(p).map_err(|e| e.to_string())?,
                None => {
                    json!({"passage_ref":request.id,"text":p.text,"citation":p.provenance.source,"source_version":p.provenance.source_version,"jurisdiction":p.provenance.jurisdiction,"license":p.provenance.license.as_str()})
                }
            }
        }
        "catalog.v1" => {
            let p = world
                .resource::<crate::capability_registry::CapabilityRegistry>()
                .catalog_provider_descriptor(&crate::capability_registry::CatalogProviderId(
                    request.id.clone(),
                ))
                .ok_or("Missing catalog")?;
            json!({"id":p.id.0,"label":p.label,"description":p.description,"category":p.category,"region":p.region,"license":p.license,"source_version":p.source_version,"rows":(p.query_fn)(&json!({}))})
        }
        _ => return Err("Unsupported legacy kind".into()),
    };
    let bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    if bytes.len() > cloud::MAX_BYTES {
        return Err("Legacy asset exceeds capture bound".into());
    }
    Ok(
        json!({"kind":request.kind,"id":request.id,"payload":payload,"snapshot_sha256":hex::encode(Sha256::digest(bytes)),"review_required":true,"publication_authorized":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn passage(revision: &str) -> AdmittedAsset {
        AdmittedAsset {
            revision: revision.into(),
            asset: Asset {
                schema: SCHEMA.into(),
                id: "test.dynamic.passage".into(),
                kind: "corpus_passage.v1".into(),
                title: "Synthetic fixture".into(),
                description: "Registry lifecycle unit fixture".into(),
                element_classes: vec![],
                path_kind: "passage".into(),
                required_tool_ids: vec![],
                payload: json!({"passage_ref":"test.dynamic.passage","text":revision,
                    "citation":"Synthetic unit fixture","license":"cc0"}),
                applicability: "Unit tests".into(),
                limitations: vec![],
                evidence: vec!["Synthetic".into()],
                redistribution_rights: "Original synthetic fixture".into(),
                contains_private_context: false,
            },
        }
    }
    #[test]
    fn replacement_and_withdrawal_update_only_shared_registries() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        install(&mut world, passage("revision-one")).unwrap();
        install(&mut world, passage("revision-two")).unwrap();
        let registry = world.resource::<super::super::corpus_gap::CorpusPassageRegistry>();
        assert_eq!(
            registry
                .get(&crate::capability_registry::PassageRef(
                    "test.dynamic.passage".into()
                ))
                .unwrap()
                .text,
            "revision-two"
        );
        let index = Index {
            schema: SCHEMA.into(),
            epoch: 3,
            assets: vec![],
            withdrawn_asset_ids: vec!["test.dynamic.passage".into()],
        };
        let result = reconcile(&mut world, &index);
        assert_eq!(result["removed"], json!(["test.dynamic.passage"]));
        assert!(world.get_entity(entity).is_ok());
        assert!(world
            .resource::<super::super::corpus_gap::CorpusPassageRegistry>()
            .get(&crate::capability_registry::PassageRef(
                "test.dynamic.passage".into()
            ))
            .is_none());
    }
    #[test]
    fn invalid_update_preserves_previous_revision() {
        let mut world = World::new();
        install(&mut world, passage("revision-one")).unwrap();
        let mut invalid = passage("revision-two");
        invalid.asset.payload["license"] = Value::Null;
        assert!(install(&mut world, invalid).is_err());
        assert_eq!(
            world.resource::<InstalledAssets>().0["test.dynamic.passage"].revision,
            "revision-one"
        );
    }
    #[test]
    fn dynamic_release_cannot_overwrite_private_passage() {
        let mut world = World::new();
        world.init_resource::<super::super::corpus_gap::CorpusPassageRegistry>();
        let private = decode(passage("private").asset.payload).unwrap();
        world
            .resource_mut::<super::super::corpus_gap::CorpusPassageRegistry>()
            .register_private(private)
            .unwrap();
        assert!(install(&mut world, passage("shared")).is_err());
        assert!(world.get_resource::<InstalledAssets>().is_none());
    }
}
