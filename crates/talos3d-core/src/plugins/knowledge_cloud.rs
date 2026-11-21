//! Governed, typed knowledge data delivered independently of application builds.
//! Network/signature admission occurs off the ECS thread. Only admitted payloads
//! enter the runtime registries; a caller cannot submit raw bytes as a release.
use super::{agent_skill_cloud as cloud, agent_skills::AgentCorpusConnection};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub mod evaluation;

pub const SCHEMA: &str = "talos3d.knowledge-asset.v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct Asset {
    #[schemars(schema_with = "asset_protocol_schema")]
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
fn asset_protocol_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"string","enum":["talos3d.knowledge-asset.v1"]})
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
    #[serde(default)]
    pub evaluation_scope: Option<evaluation::Scope>,
    #[serde(default)]
    pub released: Option<bool>,
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
    scope_sha256: Option<String>,
}
#[derive(Resource, Default)]
pub struct InstalledAssets(BTreeMap<String, InstalledAsset>);
#[derive(Clone)]
struct InstalledAsset {
    revision: String,
    kind: String,
    runtime_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateCheck {
    pub asset: Asset,
    /// Parametric driver cases only. An empty list checks defaults.
    #[serde(default)]
    pub driver_cases: Vec<BTreeMap<String, Value>>,
}

/// Uses the same decoder/validation as admission, in disposable registries.
/// No entity, release, trust decision or project state escapes this check.
pub fn check_candidate(request: CandidateCheck) -> Result<Value, String> {
    let bytes = serde_json::to_vec(&request.asset).map_err(|e| e.to_string())?;
    if bytes.len() > 1_048_576 || request.driver_cases.len() > 16 {
        return Err("Candidate or case limit exceeded".into());
    }
    if request.asset.schema != SCHEMA || request.asset.contains_private_context {
        return Err(format!("Expected schema {SCHEMA} and contains_private_context=false; this is a computational check, not publication"));
    }
    let revision = hex::encode(Sha256::digest(&bytes));
    let mut scratch = World::new();
    install(
        &mut scratch,
        AdmittedAsset {
            asset: request.asset.clone(),
            revision: revision.clone(),
            scope_sha256: None,
        },
    )?;
    let mut cases = Vec::new();
    if request.asset.kind == "parametric_type.v1" {
        let definition: crate::relational::registry::ParametricTypeDef =
            decode(request.asset.payload["definition"].clone())?;
        let inputs = if request.driver_cases.is_empty() {
            vec![BTreeMap::new()]
        } else {
            request.driver_cases
        };
        for drivers in inputs {
            let members = definition.evaluate_representation(&drivers).ok_or(
                "No emitting representation: this candidate cannot close an executable path gap",
            )??;
            cases.push(
                json!({"drivers":drivers,"members":members.iter().map(|m| json!({
                "size":m.size,"translate":m.translate,"rotate_euler_deg":m.rotate_euler_deg,
                "profile_xz":m.profile_xz,"label":m.label
            })).collect::<Vec<_>>()}),
            );
        }
    } else if !request.driver_cases.is_empty() {
        return Err("Driver cases apply only to parametric_type.v1".into());
    }
    Ok(
        json!({"content_hash":revision,"structural_validation":"passed","parametric_cases":cases,
        "installed":false,"released":false,"model_modified":false,
        "limitations":["No independent source, rights or applicability approval.",
            "No rendered model, placement, load-bearing or construction acceptance.",
            "Recipe and Definition bodies are structurally checked; this does not execute an authoring episode."],
        "next_step":"Use independent review and runtime acceptance appropriate to the declared asset scope; never treat this check as publication approval."}),
    )
}

pub async fn index(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
) -> Result<Index, String> {
    index_scoped(c, store, None).await
}
pub async fn index_scoped(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
    scope: Option<&evaluation::Scope>,
) -> Result<Index, String> {
    let bytes = cloud::request(c, "/assets", None).await?;
    let value = cloud::verify_signed_json(&bytes, &c.trusted_public_keys)?;
    let index: Index = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if index.schema != SCHEMA || index.assets.len() > 1000 || index.withdrawn_asset_ids.len() > 1000
    {
        return Err("Unsupported knowledge index".into());
    }
    evaluation::validate_index(&index, scope)?;
    store.observe_knowledge_index(c, index.epoch, &index.withdrawn_asset_ids)?;
    Ok(index)
}
pub async fn retrieve(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
    request: &AssetRequest,
) -> Result<AdmittedAsset, String> {
    retrieve_scoped(c, store, request, None).await
}
pub async fn retrieve_scoped(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
    request: &AssetRequest,
    scope: Option<&evaluation::Scope>,
) -> Result<AdmittedAsset, String> {
    let before = index_scoped(c, store, scope).await?;
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
    let latest = index_scoped(c, store, scope).await?;
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
        scope_sha256: scope.map(evaluation::Scope::digest),
    })
}
pub fn required_tools(admitted: &AdmittedAsset) -> &[String] {
    &admitted.asset.required_tool_ids
}
pub fn inspect(admitted: &AdmittedAsset) -> Value {
    json!({"asset":admitted.asset,"content_hash":admitted.revision,"delivery":"verified_dynamic_corpus","released":admitted.scope_sha256.is_none(),"next_tool":"install_knowledge_asset"})
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
            // Catalog descriptors have no private/shared scope. Only replace a
            // catalog already admitted by this loader, never infer ownership
            // from an identifier supplied by a release.
            let existing = world
                .get_resource::<CapabilityRegistry>()
                .and_then(|r| r.catalog_provider_descriptor(&id));
            let owned = world
                .get_resource::<InstalledAssets>()
                .and_then(|r| r.0.get(&a.id))
                .is_some_and(|r| r.kind == a.kind && r.runtime_id == id.0);
            if existing.is_some() && !owned {
                return Err("Dynamic release cannot overwrite an unowned catalog".into());
            }
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
    evaluation::record(
        world,
        json!({"operation":"install", "asset_id":a.id,
        "content_hash":admitted.revision,"runtime_id":runtime_id,"kind":a.kind,
        "scope_sha256":admitted.scope_sha256}),
    );
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

/// A document load replaces its private registry, not verified session releases.
/// Only copies already admitted in this process are eligible; project bytes are
/// deliberately not a trust source. Private document identities take precedence.
pub fn retain_session_libraries(
    world: &World,
    target: &mut super::modeling::definition::DefinitionLibraryRegistry,
) {
    use super::modeling::definition::{DefinitionLibraryRegistry, DefinitionLibraryScope};
    let (Some(installed), Some(libraries)) = (
        world.get_resource::<InstalledAssets>(),
        world.get_resource::<DefinitionLibraryRegistry>(),
    ) else {
        return;
    };
    for library in libraries.list() {
        if library.scope == DefinitionLibraryScope::SharedRelease
            && installed.0.get(&library.id.0).is_some_and(|asset| {
                asset.kind == "definition_library.v1"
                    && library.source_path.as_deref()
                        == Some(format!("corpus:{}@{}", library.id.0, asset.revision).as_str())
            })
            && target.get(&library.id).is_none()
        {
            target.insert(library.clone());
        }
    }
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
    json!({"assets":items,"coverage":{"portable_kinds":["catalog.v1","corpus_passage.v1","definition_library.v1","parametric_type.v1","recipe.v1"],"excluded_private_content":["document_definitions","occurrences","project_passages","private_parametric_types"],"unsupported_dynamic_kinds":["native_recipe_bodies","generation_priors","native_constraints"],"procedures":"Inspect list_agent_skills and exact resources separately; this inventory is not exhaustive of every compiled registry."},"publication_authorized":false,"notice":"Legacy shared content is a migration candidate, not independently reviewed knowledge. Resolve evidence, rights, applicability and limitations before curator submission. Unsupported native bodies must become explicit gaps."})
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
    let snapshot_sha256 = hex::encode(Sha256::digest(bytes));
    let runtime_inspection = (request.kind == "parametric_type.v1").then(|| json!({
        "next_tool":"evaluate_legacy_knowledge_asset", "arguments":{"id":request.id,"snapshot_sha256":snapshot_sha256},
        "purpose":"Inspection-only runtime observations; do not invent candidate rights or evidence", "rights":"unresolved", "publication_candidate":false
    }));
    Ok(
        json!({"kind":request.kind,"id":request.id,"payload":payload,"snapshot_sha256":snapshot_sha256,
        "review_required":true,"publication_authorized":false,"runtime_inspection":runtime_inspection}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn passage(revision: &str) -> AdmittedAsset {
        AdmittedAsset {
            revision: revision.into(),
            scope_sha256: None,
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
    fn candidate_protocol_is_discoverable_without_source() {
        let schema = serde_json::to_value(schemars::schema_for!(CandidateCheck)).unwrap();
        assert_eq!(
            schema["$defs"]["Asset"]["properties"]["schema"]["enum"],
            json!([SCHEMA])
        );
    }
    #[test]
    fn candidate_check_is_not_admission_and_rejects_private_content() {
        let asset = passage("candidate").asset;
        let checked = check_candidate(CandidateCheck {
            asset: asset.clone(),
            driver_cases: vec![],
        })
        .unwrap();
        assert_eq!(checked["installed"], false);
        assert_eq!(checked["released"], false);
        assert_eq!(checked["model_modified"], false);
        assert_eq!(
            checked["content_hash"],
            hex::encode(Sha256::digest(serde_json::to_vec(&asset).unwrap()))
        );
        let mut private = asset;
        private.contains_private_context = true;
        assert!(check_candidate(CandidateCheck {
            asset: private,
            driver_cases: vec![]
        })
        .is_err());
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
            evaluation_scope: None,
            released: None,
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

    #[test]
    fn catalog_replacement_requires_existing_dynamic_ownership() {
        use crate::capability_registry::{CapabilityRegistry, CatalogProviderId};
        let mut candidate = passage("catalog-one");
        candidate.asset.id = "test.catalog".into();
        candidate.asset.kind = "catalog.v1".into();
        candidate.asset.payload = json!({"id":"test.catalog","label":"Synthetic catalog",
            "description":"Admission unit fixture","category":{"kind":"Other","value":"fixture"},
            "region":null,"license":{"kind":"Cc0"},"source_version":"one","rows":[]});
        let mut world = World::new();
        world.init_resource::<evaluation::Observations>();
        install(&mut world, candidate.clone()).unwrap();
        let rows = crate::plugins::model_api::handle_catalog_query(
            &world,
            "test.catalog".into(),
            json!({}),
        )
        .unwrap();
        assert!(rows.is_empty());
        let observations = evaluation::snapshot(&world);
        assert_eq!(observations["records"][0]["event"]["operation"], "install");
        assert_eq!(
            observations["records"][1]["event"]["content_hash"],
            "catalog-one"
        );
        assert_eq!(
            observations["records"][1]["event"]["operation"],
            "catalog_query"
        );
        assert_eq!(observations["records"][1]["event"]["row_count"], 0);
        candidate.asset.payload["source_version"] = json!("two");
        install(&mut world, candidate.clone()).unwrap();
        world.remove_resource::<InstalledAssets>();
        candidate.asset.payload["source_version"] = json!("unowned-overwrite");
        assert!(install(&mut world, candidate)
            .unwrap_err()
            .contains("unowned catalog"));
        assert_eq!(
            world
                .resource::<CapabilityRegistry>()
                .catalog_provider_descriptor(&CatalogProviderId("test.catalog".into()))
                .unwrap()
                .source_version,
            "two"
        );
        assert!(world.get_resource::<InstalledAssets>().is_none());
    }
}

#[path = "knowledge_candidate.rs"]
pub mod candidate_runtime;
