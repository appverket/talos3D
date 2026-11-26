//! Governed, typed knowledge data delivered independently of application builds.
//! Network/signature admission occurs off the ECS thread. Only admitted payloads
//! enter the runtime registries; a caller cannot submit raw bytes as a release.
use super::{agent_skill_cloud as cloud, agent_skills::AgentCorpusConnection};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub mod declarative_prior;
pub mod evaluation;
pub mod registry_kinds;

pub use talos3d_knowledge_protocol::{Asset, SCHEMA};

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
    catalog_contract: Option<Value>,
    /// Exact payload of a registry-kind asset, so withdrawal and replacement
    /// remove precisely what this revision contributed.
    owned_payload: Option<Value>,
}

impl InstalledAssets {
    /// The installed asset whose runtime registration (recipe family, catalog
    /// provider, parametric type) is `runtime_id`.
    fn by_runtime(&self, kind: &str, runtime_id: &str) -> Option<(&String, &InstalledAsset)> {
        self.0
            .iter()
            .find(|(_, a)| a.kind == kind && a.runtime_id == runtime_id)
    }
}

/// For each refinement state a recipe supports, the obligations in force on its
/// target class (class minimum plus the recipe's own specializations) and which
/// of them the recipe declares a satisfier for through `obligation_satisfied`
/// postconditions. Undeclared obligations stay unresolved after execution, so
/// an agent can tell before authoring whether a recipe can reach a state.
fn recipe_obligation_coverage(
    world: &World,
    recipe: &crate::curation::RecipeArtifact,
    refinement: &crate::capability_registry::RefinementSpecializations,
) -> Value {
    use crate::capability_registry::{effective_obligations, CapabilityRegistry, ElementClassId};
    use crate::curation::authoring_script::{ArgExpr, Postcondition};
    let Some(class) = world
        .get_resource::<CapabilityRegistry>()
        .and_then(|registry| {
            registry.element_class_descriptor(&ElementClassId(recipe.target_class.clone()))
        })
    else {
        return json!({"status":"unknown_target_class"});
    };
    let mut declared = std::collections::BTreeSet::new();
    let mut computed_declarations = 0;
    for condition in recipe
        .body
        .script()
        .map_or(&[][..], |s| &s.postconditions[..])
    {
        if let Postcondition::ObligationSatisfied {
            obligation_id_expr, ..
        } = condition
        {
            match obligation_id_expr {
                ArgExpr::Literal {
                    value: Value::String(id),
                } => {
                    declared.insert(id.clone());
                }
                _ => computed_declarations += 1,
            }
        }
    }
    let states: serde_json::Map<String, Value> = recipe
        .supported_refinement_states
        .iter()
        .map(|state| {
            let required: Vec<String> = effective_obligations(class, Some(refinement), *state)
                .into_iter()
                .map(|o| o.id.0)
                .collect();
            let undeclared: Vec<&String> = required
                .iter()
                .filter(|id| !declared.contains(*id))
                .collect();
            (
                state.as_str().to_string(),
                json!({"in_force":required,"undeclared":undeclared,
                    "covered":undeclared.is_empty()}),
            )
        })
        .collect();
    json!({"by_state":states,"computed_declarations":computed_declarations,
        "notice":"Undeclared obligations remain unresolved after execution and block promotion unless resolved, deferred or waived explicitly. Prefer a variant that covers the requested state."})
}

/// Exact shared-knowledge revision behind a runtime registration, when it was
/// installed from the governed corpus rather than compiled in.
pub fn installed_revision(
    world: &World,
    kind: &str,
    runtime_id: &str,
) -> Option<crate::plugins::refinement::KnowledgeRevisionRef> {
    let (id, asset) = world
        .get_resource::<InstalledAssets>()?
        .by_runtime(kind, runtime_id)?;
    Some(crate::plugins::refinement::KnowledgeRevisionRef {
        asset_id: id.clone(),
        asset_kind: asset.kind.clone(),
        content_hash: asset.revision.clone(),
    })
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateCheck {
    pub asset: Asset,
    /// Parametric driver cases only. An empty list checks defaults.
    #[serde(default)]
    pub driver_cases: Vec<BTreeMap<String, Value>>,
    /// Catalog-only filters executed by the ordinary runtime query handler in disposable registries.
    /// At most 16 object filters. Returns actual counts, bounded rows and result hashes; not acceptance.
    #[serde(default)]
    pub catalog_queries: Vec<BTreeMap<String, Value>>,
}

/// Uses the same decoder/validation as admission, in disposable registries.
/// No entity, release, trust decision or project state escapes this check.
pub fn check_candidate(world: &World, request: CandidateCheck) -> Result<Value, String> {
    request
        .asset
        .validate_envelope()
        .map_err(|error| error.to_string())?;
    let missing = super::model_api::missing_knowledge_tools(&request.asset.required_tool_ids);
    if !missing.is_empty() {
        return Err(json!({"code":"MissingRequiredRuntimeTools","missing_required_tool_ids":missing,
            "route":"CapabilityGap","notice":"Candidate requirements must name actual MCP tools. Inspect live schemas; never invent aliases. No installation or publication."}).to_string());
    }
    let mut checked = check_payload(world, request)?;
    checked["envelope_validation"] = json!("passed");
    Ok(checked)
}

// Payload inspection is also used for exact server-owned legacy snapshots whose
// provenance/rights remain unresolved. It never establishes candidate eligibility.
fn check_payload(world: &World, request: CandidateCheck) -> Result<Value, String> {
    let bytes = serde_json::to_vec(&request.asset).map_err(|e| e.to_string())?;
    if request.driver_cases.len() > 16 || request.catalog_queries.len() > 16 {
        return Err("Candidate or case limit exceeded".into());
    }
    let revision = hex::encode(Sha256::digest(&bytes));
    let mut scratch = World::new();
    // Disposable registries route the same kinds as the live world.
    if let Some(kinds) = world.get_resource::<registry_kinds::KnowledgeKindRegistry>() {
        scratch.insert_resource(kinds.clone());
    }
    let installation = install_payload(
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
    let mut catalog_cases = Vec::new();
    if !request.catalog_queries.is_empty() && request.asset.kind != "catalog.v1" {
        return Err("Catalog query cases apply only to catalog.v1".into());
    }
    for filter in request.catalog_queries {
        let filter = serde_json::to_value(filter).map_err(|e| e.to_string())?;
        let rows = super::model_api::handle_catalog_query(
            &scratch,
            request.asset.id.clone(),
            filter.clone(),
        )?;
        let result = serde_json::to_value(&rows).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(&result).map_err(|e| e.to_string())?;
        catalog_cases.push(json!({"filter":filter,"row_count":rows.len(),
            "rows":rows.iter().take(100).collect::<Vec<_>>(),"rows_truncated":rows.len()>100,
            "response_sha256":hex::encode(Sha256::digest(bytes))}));
    }
    Ok(
        json!({"catalog_cases":catalog_cases,"content_hash":revision,"payload_validation":"passed", "structural_validation":"passed","parametric_cases":cases,"execution":installation["execution"],
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
/// Record the exact admitted read response without installing the asset.
pub fn inspect_observed(world: &mut World, admitted: AdmittedAsset) -> Value {
    world.init_resource::<evaluation::Observations>();
    let value = inspect(&admitted);
    evaluation::record(
        world,
        json!({"operation":"get_knowledge_asset","asset_id":admitted.asset.id,
        "asset_kind":admitted.asset.kind,"content_hash":admitted.revision,
        "response_sha256":hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap())),
        "released":admitted.scope_sha256.is_none(),"installed":false}),
    );
    value
}

/// Resolve only an admitted, currently installed typed passage revision.
/// A metadata read is reference use, never executable or geometric acceptance.
pub fn read_installed_passage(world: &World, admitted: AdmittedAsset) -> Result<Value, String> {
    let a = &admitted.asset;
    if a.kind != "corpus_passage.v1" {
        return Err("Pinned source lookup requires corpus_passage.v1".into());
    }
    let installed = world
        .get_resource::<InstalledAssets>()
        .and_then(|assets| assets.0.get(&a.id));
    if !installed.is_some_and(|v| {
        v.kind == a.kind && v.revision == admitted.revision && v.runtime_id == a.id
    }) {
        return Err("Install the exact current passage revision before lookup".into());
    }
    let info = super::model_api::handle_lookup_source_passage(world, a.id.clone())?;
    let mut value = serde_json::to_value(info).map_err(|e| e.to_string())?;
    value["content_hash"] = json!(admitted.revision);
    value["delivery"] = json!("verified_dynamic_corpus");
    value["released"] = json!(admitted.scope_sha256.is_none());
    evaluation::asset_execution(
        world,
        &a.kind,
        &a.id,
        "lookup_source_passage",
        &json!({"passage_ref":a.id,"content_hash":admitted.revision}),
        &value,
    );
    Ok(value)
}

/// Strict payload decoder shared by candidate checks and installation. A field
/// the runtime would ignore (unknown or misplaced, e.g. a nested copy of a
/// top-level option) is refused with its path instead of silently dropping a
/// declared constraint.
/// Recipe payload fields that carry the construction system's refinement
/// specialisation beside the executable recipe artifact.
const RECIPE_REFINEMENT_FIELDS: [&str; 2] = [
    "obligation_specializations",
    "promotion_critical_path_specializations",
];

/// Strictly decode a portable recipe payload and its optional specialisation.
fn decode_recipe(
    payload: &Value,
) -> Result<
    (
        crate::curation::RecipeArtifact,
        crate::capability_registry::RefinementSpecializations,
    ),
    String,
> {
    let mut artifact = payload.clone();
    let mut refinement = serde_json::Map::new();
    if let Some(map) = artifact.as_object_mut() {
        for key in RECIPE_REFINEMENT_FIELDS {
            if let Some(value) = map.remove(key) {
                refinement.insert(key.into(), value);
            }
        }
    }
    let refinement: crate::capability_registry::RefinementSpecializations =
        decode(Value::Object(refinement))?;
    refinement.validate()?;
    Ok((decode(artifact)?, refinement))
}

fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
    let mut ignored = Vec::new();
    let decoded = serde_ignored::deserialize(v, |path| ignored.push(path.to_string()))
        .map_err(|e| e.to_string())?;
    if !ignored.is_empty() {
        return Err(json!({"code":"UnknownPayloadFields","ignored_paths":ignored,
            "notice":"The runtime decoder does not read these fields, so they would have no effect. Move or remove them; get_knowledge_payload_schema returns the exact accepted shape."}).to_string());
    }
    Ok(decoded)
}

#[derive(Deserialize, schemars::JsonSchema)]
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

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n)
            if n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|x| x.fract() == 0.0) =>
        {
            "integer"
        }
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
// Compare integer/float encodings without rounding a large integer through f64.
fn catalog_value_equal(left: &Value, right: &Value) -> bool {
    fn integer_float(integer: &serde_json::Number, float: f64) -> bool {
        if !float.is_finite() || float.fract() != 0.0 {
            return false;
        }
        if let Some(value) = integer.as_u64() {
            return float >= 0.0 && float < 18446744073709551616.0 && float as u64 == value;
        }
        integer.as_i64().is_some_and(|value| {
            float >= -9223372036854775808.0 && float < 0.0 && float as i64 == value
        })
    }
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => {
            if a == b {
                return true;
            }
            if a.is_f64() && !b.is_f64() {
                return integer_float(b, a.as_f64().unwrap());
            }
            if b.is_f64() && !a.is_f64() {
                return integer_float(a, b.as_f64().unwrap());
            }
            false
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| catalog_value_equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|other| catalog_value_equal(v, other)))
        }
        _ => left == right,
    }
}
fn build_catalog_query_contract(data: &CatalogData) -> Result<Value, String> {
    let mut fields: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    for row in &data.rows {
        let object = row
            .data
            .as_object()
            .ok_or("Catalog row data must be an object")?;
        for (key, value) in object {
            fields
                .entry(key.clone())
                .or_default()
                .insert(json_type(value).into());
        }
    }
    let properties: serde_json::Map<String, Value> = fields
        .into_iter()
        .map(|(key, types)| (key, json!({"type":types})))
        .collect();
    Ok(
        json!({"filter_mode":"exact_row_field_equality","filter_schema":{"type":"object","properties":properties,"additionalProperties":false},
        "scope_probe_tool":"probe_catalog_scope","scope_semantics":"Compare opaque scope declarations literally; no geographic equivalence or suitability is inferred. Provider region is metadata, not an implicit row field."}),
    )
}
pub fn catalog_query_contract(world: &World, provider: &str) -> Value {
    world.get_resource::<InstalledAssets>().and_then(|assets|assets.by_runtime("catalog.v1", provider))
        .and_then(|(_, a)|a.catalog_contract.clone()).unwrap_or_else(||json!({"filter_mode":"provider_defined","filter_schema":null,
            "scope_probe_tool":"probe_catalog_scope","scope_semantics":"Opaque provider declaration only; unknown provider filtering behavior is not scope evidence."}))
}
pub fn validate_catalog_filter(
    world: &World,
    provider: &str,
    filter: &Value,
) -> Result<(), String> {
    let contract = catalog_query_contract(world, provider);
    if contract["filter_mode"] != "exact_row_field_equality" {
        return Ok(());
    }
    let object=filter.as_object().ok_or_else(||json!({"code":"InvalidCatalogFilter","expected":"object","next_tool":"list_catalog_providers"}).to_string())?;
    for (field, value) in object {
        let types = contract["filter_schema"]["properties"][field]["type"].as_array();
        if !types.is_some_and(|types| {
            types.iter().any(|t| {
                t.as_str() == Some(json_type(value)) || (t == "number" && value.is_number())
            })
        }) {
            return Err(json!({"code":"UnsupportedCatalogFilter","provider_id":provider,"field":field,"value_type":json_type(value),
                "next_tool":"list_catalog_providers","scope_probe_tool":"probe_catalog_scope","notice":"Unsupported fields/types are not empty query results or scope decisions."}).to_string());
        }
    }
    Ok(())
}
pub fn probe_catalog_scope(
    world: &World,
    provider: &str,
    requested: &str,
) -> Result<Value, String> {
    use crate::capability_registry::{CapabilityRegistry, CatalogProviderId};
    if requested.trim().is_empty() || requested.len() > 256 {
        return Err("Bounded nonempty scope identifier required".into());
    }
    let descriptor = world
        .get_resource::<CapabilityRegistry>()
        .and_then(|r| r.catalog_provider_descriptor(&CatalogProviderId(provider.into())))
        .ok_or_else(|| format!("Unknown catalog provider '{provider}'"))?;
    let installed = world
        .get_resource::<InstalledAssets>()
        .and_then(|r| r.by_runtime("catalog.v1", provider));
    let declared = descriptor
        .region
        .as_deref()
        .filter(|v| !v.trim().is_empty());
    let decision = match declared {
        None => "unspecified",
        Some(scope) if scope == requested => "declared",
        Some(_) => "not_declared",
    };
    let result = json!({"provider_id":provider,"requested_scope":requested,"declared_scope":declared,"decision":decision,
        "comparison":"literal_provider_declaration","compatibility_established":false,
        "notice":"This compares the curator/provider declaration only. Not declared is not a geographic exclusion or a structural, market or regulatory suitability verdict. No scope aliases or equivalences are inferred.",
        "source_version":descriptor.source_version,"asset_id":installed.map(|(id,_)|id),"content_hash":installed.map(|(_,a)|&a.revision)});
    evaluation::catalog_scope_probe(world, provider, requested, &result);
    Ok(result)
}

/// Runtime wire shapes, not domain facts or publication approval.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PayloadSchemaRequest {
    pub kind: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ParametricPayload {
    definition: crate::relational::registry::ParametricTypeDef,
    #[serde(default)]
    placement_contract: Option<crate::relational::registry::ParametricPlacementContract>,
    /// Optional applicability range per driver (driver unit). parametric.create
    /// and driver edits outside a range are refused before geometry is emitted.
    #[serde(default)]
    driver_bounds: BTreeMap<String, crate::relational::registry::DriverBounds>,
    /// Optional construction-system obligations per member element class: what
    /// members of that class must resolve at each refinement state beyond the
    /// element class minimum (for example every material layer at Constructible).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    refinement_specializations:
        BTreeMap<String, crate::capability_registry::RefinementSpecializations>,
}

pub fn payload_schema(world: &World, kind: &str) -> Result<Value, String> {
    if kind == "candidate_runtime.v1" {
        return Ok(json!({"kind":kind,
            "request_schema":schemars::schema_for!(candidate_runtime::Request),
            "execution":super::model_api::document_dispatch::contract(),
            "candidate_kinds":["parametric_type.v1","recipe.v1"],
            "vocabulary_tool":"list_vocabulary",
            "post_reopen_observations":{"read_only":true,"maximum_calls":32,"result_path":"round_trip.observations","state_path":"round_trip.after_reopen"},
            "observation_contract":{"schema":"talos3d.candidate-scenario-observation.v2","states":"Each state_ref resolves in observed_states in the same response; identical complete native snapshots are transmitted once.","empty_rejection":"An actual failed operation may yield an empty rendered scene, with explicit execution_error and no geometry checks."},
            "geometry_types":["box","profile_extrusion"],
            "knowledge_observation_operations":evaluation::LIFECYCLE_OPERATIONS,"candidate_checks":candidate_checks(),"next_tool":"evaluate_knowledge_candidate"}));
    }
    let (payload, constraints) = match kind {
        registry if registry_kinds::handler(world, registry).is_some() => {
            let handler = registry_kinds::handler(world, registry).expect("checked above");
            ((handler.payload_schema)(), handler.constraints.to_vec())
        }
        "catalog.v1" => (
            serde_json::to_value(schemars::schema_for!(CatalogData)).unwrap(),
            vec![
                "payload.id must equal asset.id",
                "At most 10000 catalog rows",
                "A release cannot replace an unowned runtime catalog",
            ],
        ),
        "parametric_type.v1" => (
            serde_json::to_value(schemars::schema_for!(ParametricPayload)).unwrap(),
            vec![
                "payload.definition.id must equal asset.id",
                "An executable path requires a valid emitting representation",
                "A release cannot overwrite a private/internal type",
                "Optional refinement_specializations map a member element_class to the obligations and promotion-critical paths that construction system must resolve per refinement state beyond the class minimum; each class must be declared by a member semantic, and required_by_state must equal the state key",
            ],
        ),
        "recipe.v1" => {
            let mut schema =
                serde_json::to_value(schemars::schema_for!(crate::curation::RecipeArtifact))
                    .unwrap();
            schema["allOf"] = json!([{"properties":{"body":{"properties":{"kind":{"const":"authoring_script"}}}}}]);
            let refinement = serde_json::to_value(schemars::schema_for!(
                crate::capability_registry::RefinementSpecializations
            ))
            .unwrap();
            for key in RECIPE_REFINEMENT_FIELDS {
                schema["properties"][key] = refinement["properties"][key].clone();
            }
            if let Some(defs) = refinement.get("$defs").and_then(Value::as_object) {
                for (name, def) in defs {
                    schema["$defs"][name] = def.clone();
                }
            }
            (schema,vec!["Optional obligation_specializations / promotion_critical_path_specializations declare what this construction system must resolve at each refinement state beyond the element class minimum (for example every material layer of the system at Constructible). required_by_state must equal the state key", "payload.meta.id must equal asset.id; opaque IDs are executable family IDs, legacy kind/family IDs use the family suffix. check_knowledge_candidate returns the exact execution arguments", "Only authoring_script recipe bodies are portable; native function references are not executable releases", "Script structure must pass the native validator", "Declare which created element resolves each obligation in force at the target state with an obligation_satisfied postcondition: obligation_id_expr names the obligation and by_step names a step that binds the element it created as element_id. Instantiation and promotion then record those obligations as satisfied; undeclared obligations stay unresolved and block promotion. A postcondition that depends on a step skipped by its precondition asserts nothing, so optional steps can carry conditional declarations. coarse_envelope and setting_out_reservation, when declared, must name the same step: that element's thickness sets out the assembly", "Relations between elements (for example bears_on to a foundation or hosted_on to an existing wall) are emitted with create_assembly's relations list, each {source, target, type}; source and target may come from script parameters (ids of existing project elements the user supplies) or earlier step outputs, composed with array and object argument expressions. The reserved parameter target_element_id names the refinement root the recipe runs for (the element whose obligations, such as bears_on, check relations it is the source of); it is supplied at execution and must not be declared", "Members can carry a material identity with assign_material (material_id, or name with base_color, perceptual_roughness and metallic); texture files are not available to recipes", "Give every member a role: create the semantic assembly once, then call add_assembly_members (assembly_id, members [{target, role}]) for members created later, including once per iteration of a repeat loop"])
        }
        "definition_library.v1" => (
            serde_json::to_value(schemars::schema_for!(
                super::modeling::definition::DefinitionLibrary
            ))
            .unwrap(),
            vec![
                "payload.id must equal asset.id",
                "Between 1 and 1000 definitions, with map keys matching definition ids",
                "Dependencies must resolve within the library",
                "Use canonical Definition body; legacy migration fields are not authoring inputs",
                "Scope and source_path are assigned by signed admission, never candidate authority",
            ],
        ),
        "corpus_passage.v1" => (
            serde_json::to_value(schemars::schema_for!(
                super::knowledge_persistence::PersistedPassage
            ))
            .unwrap(),
            vec![
                "passage_ref must equal asset.id",
                "Nonempty text, citation and explicit license are required",
                "Published scope is assigned by signed admission, never candidate metadata",
            ],
        ),
        _ => {
            return Err(
                "Unsupported portable knowledge kind; discover a supported kind through MCP".into(),
            )
        }
    };
    Ok(json!({
        "kind": kind,
        "envelope_contract": talos3d_knowledge_protocol::contract_manifest(),
        "asset_schema": talos3d_knowledge_protocol::asset_schema(),
        "payload_schema": payload,
        "constraints": constraints,
        "next_tool": "check_knowledge_candidate",
        "candidate_checks":candidate_checks(),
        "notice": "This is the runtime decoder contract. Supply independently grounded data and rights; schema validity is not evidence or release approval."
    }))
}

fn candidate_checks() -> Value {
    let schema = serde_json::to_value(schemars::schema_for!(CandidateCheck)).unwrap();
    json!({"catalog.v1":{"tool":"check_knowledge_candidate","query_field":"catalog_queries",
        "consumer_tool":"catalog_query","required_tools_checked":true,"maximum_queries":16,"query_schema":schema["properties"]["catalog_queries"],
        "result_path":"catalog_cases","scope":"Disposable native catalog queries; not independent acceptance or publication"}})
}

pub fn install(world: &mut World, admitted: AdmittedAsset) -> Result<Value, String> {
    admitted
        .asset
        .validate_envelope()
        .map_err(|error| error.to_string())?;
    install_payload(world, admitted)
}

// Internal decoder shared by admitted assets and disposable legacy inspection.
// Never expose this as an admission, publication or model-edit API.
fn install_payload(world: &mut World, admitted: AdmittedAsset) -> Result<Value, String> {
    use crate::capability_registry::DynamicSpecializations;
    use crate::curation::{RecipeArtifact, RecipeArtifactRegistry, Scope, Trust};
    use crate::plugins::modeling::definition::{
        DefinitionLibrary, DefinitionLibraryRegistry, DefinitionLibraryScope,
    };
    use crate::relational::registry::ParametricRegistry;
    let a = admitted.asset;
    let mut catalog_contract = None;
    let mut owned_payload = None;
    // Parse and validate the complete supported payload before any registry mutation.
    let mut execution = Value::Null;
    let runtime_id = match a.kind.as_str() {
        "parametric_type.v1" => {
            let payload: ParametricPayload = decode(a.payload.clone())?;
            let definition = payload.definition;
            let contract = payload.placement_contract;
            let bounds = payload.driver_bounds;
            let refinement = payload.refinement_specializations;
            if definition.id != a.id {
                return Err("Parametric identity must match asset identity".into());
            }
            let member_classes: std::collections::BTreeSet<&str> = definition
                .representation
                .iter()
                .flat_map(|r| &r.members)
                .filter_map(|m| m.semantic.as_ref()?.element_class.as_deref())
                .collect();
            for (class, spec) in &refinement {
                if !member_classes.contains(class.as_str()) {
                    return Err(format!(
                        "refinement_specializations names element class '{class}', which no member declares"
                    ));
                }
                spec.validate()
                    .map_err(|e| format!("refinement_specializations.{class}: {e}"))?;
            }
            crate::relational::registry::DriverBounds::validate_all(&definition, &bounds)?;
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
            registry.register_driver_bounds(&a.id, bounds)?;
            registry.register_knowledge_revision(&a.id, &admitted.revision);
            world.init_resource::<DynamicSpecializations>();
            let mut installed = world.resource_mut::<DynamicSpecializations>();
            installed.parametric.remove(&a.id);
            if !refinement.is_empty() {
                installed.parametric.insert(a.id.clone(), refinement);
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
            let (mut recipe, refinement): (RecipeArtifact, _) = decode_recipe(&a.payload)?;
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
            let family = recipe.execution_family_id();
            if family.0.trim().is_empty() {
                return Err("Recipe executable family identity is empty".into());
            }
            if world
                .get_resource::<RecipeArtifactRegistry>()
                .and_then(|r| r.get_by_family(&family))
                .is_some_and(|other| other.meta.id.0 != a.id)
            {
                return Err("Recipe executable identity collides with another asset; use a distinct identity".into());
            }
            execution = json!({"tool":"instantiate_recipe","arguments":{"family_id":family.0,"target_class":recipe.target_class},"required_parameters":recipe.body.script().map(|s|s.parameter_schema["required"].clone()),"supported_refinement_states":recipe.supported_refinement_states,"notice":"Supply parameters and choose a supported target_state. Use this exact executable identity. Candidate checks install only in scratch registries; geometry and applicability still require execution and review."});
            execution["obligation_coverage"] =
                recipe_obligation_coverage(world, &recipe, &refinement);
            recipe
                .body
                .script()
                .ok_or("Portable script required")?
                .validate_structure()
                .map_err(|e| e.to_string())?;
            recipe.meta.scope = Scope::Shipped;
            recipe.meta.trust = Trust::Published;
            world.init_resource::<DynamicSpecializations>();
            {
                let mut installed = world.resource_mut::<DynamicSpecializations>();
                installed.recipes.remove(&family);
                if !refinement.is_empty() {
                    installed.recipes.insert(family.clone(), refinement);
                }
            }
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
            catalog_contract = Some(build_catalog_query_contract(&data)?);
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
                            filter.as_object().is_none_or(|f| {
                                f.iter().all(|(k, v)| {
                                    row.data
                                        .get(k)
                                        .is_some_and(|value| catalog_value_equal(value, v))
                                })
                            })
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
        kind => {
            let handler = registry_kinds::handler(world, kind).ok_or(
                "Knowledge kind has no compatible dynamic installer; record a runtime gap",
            )?;
            let previous = world
                .get_resource::<InstalledAssets>()
                .and_then(|r| r.0.get(&a.id))
                .filter(|r| r.kind == a.kind)
                .and_then(|r| r.owned_payload.clone());
            (handler.install)(world, &a.id, &a.payload, previous.as_ref())?;
            owned_payload = Some(a.payload.clone());
            a.id.clone()
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
            catalog_contract,
            owned_payload,
        },
    );
    Ok(
        json!({"asset_id":a.id,"kind":a.kind,"content_hash":admitted.revision,"installed":true,"model_modified":false,"execution":execution,"next_step":"Rediscover the registered path and use its advertised authoring tool. Installation is not a verified model outcome."}),
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
                if let Some(mut installed) =
                    world.get_resource_mut::<crate::capability_registry::DynamicSpecializations>()
                {
                    installed.parametric.remove(&a.runtime_id);
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
                let removed = world
                    .get_resource_mut::<crate::curation::RecipeArtifactRegistry>()
                    .and_then(|mut r| r.remove(&crate::curation::AssetId::new(&a.runtime_id)));
                if let (Some(recipe), Some(mut installed)) = (
                    removed,
                    world.get_resource_mut::<crate::capability_registry::DynamicSpecializations>(),
                ) {
                    installed.recipes.remove(&recipe.execution_family_id());
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
            kind => {
                if let (Some(handler), Some(payload)) =
                    (registry_kinds::handler(world, kind), &a.owned_payload)
                {
                    (handler.withdraw)(world, payload);
                }
            }
        }
        world.resource_mut::<InstalledAssets>().0.remove(id);
    }
    let result = json!({"epoch":index.epoch,"removed":removed.iter().map(|(id,_)|id).collect::<Vec<_>>(),
        "assets":index.assets,"model_modified":false,"delivery":"verified_dynamic_corpus",
        "released":index.evaluation_scope.is_none(),"complete":true});
    world.init_resource::<evaluation::Observations>();
    evaluation::record(
        world,
        json!({"operation":"list_knowledge_assets","epoch":index.epoch,
        "asset_count":index.assets.len(),"complete":true,"released":index.evaluation_scope.is_none(),
        "response_sha256":hex::encode(Sha256::digest(serde_json::to_vec(&result).unwrap()))}),
    );
    result
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
    let mut add = |kind: &str, id: &str, label: &str, portable: bool, gap: Option<&str>| {
        if installed.is_some_and(|r| r.0.get(id).is_some_and(|a| a.kind == kind)) {
            return;
        }
        let mut item = json!({"id":id,"kind":kind,"label":label,"portable":portable,"review_required":true,"next_tool":"get_legacy_knowledge_asset","arguments":{"kind":kind,"id":id}});
        if let Some(gap) = gap {
            item["gap"] = json!(gap);
        }
        items.push(item);
    };
    if let Some(r) = world.get_resource::<crate::relational::registry::ParametricRegistry>() {
        for (id, label) in r.list_public() {
            add("parametric_type.v1", &id, &label, true, None);
        }
    }
    if let Some(r) = world.get_resource::<super::modeling::definition::DefinitionLibraryRegistry>()
    {
        for l in r.list() {
            if l.scope == super::modeling::definition::DefinitionLibraryScope::Bundled {
                add("definition_library.v1", &l.id.0, &l.name, true, None);
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
                    None,
                );
            }
        }
    }
    if let Some(r) = world.get_resource::<super::corpus_gap::CorpusPassageRegistry>() {
        for (id, p) in r.iter() {
            if p.scope == crate::curation::Scope::Shipped {
                add("corpus_passage.v1", id, id, true, None);
            }
        }
    }
    if let Some(r) = world.get_resource::<crate::capability_registry::CapabilityRegistry>() {
        for p in r.catalog_provider_descriptors() {
            add("catalog.v1", &p.id.0, &p.label, true, None);
        }
    }
    let handlers = registry_kinds::handlers(world);
    for handler in &handlers {
        for item in (handler.legacy_list)(world) {
            add(
                handler.kind,
                &item.id,
                &item.label,
                item.gap.is_none(),
                item.gap,
            );
        }
    }
    items.sort_by(|a, b| {
        a["kind"]
            .as_str()
            .cmp(&b["kind"].as_str())
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let mut portable_kinds: Vec<&str> = talos3d_knowledge_protocol::KINDS
        .iter()
        .map(|(kind, _)| *kind)
        .chain(handlers.iter().map(|h| h.kind))
        .collect();
    portable_kinds.sort_unstable();
    portable_kinds.dedup();
    json!({"assets":items,"coverage":{"portable_kinds":portable_kinds,"excluded_private_content":["document_definitions","occurrences","project_passages","private_parametric_types"],"inspection_only_kinds":[],"unsupported_dynamic_kinds":registry_kinds::UNSUPPORTED_NATIVE,"procedures":"Inspect list_agent_skills and exact resources separately; this inventory is not exhaustive of every compiled registry."},"publication_authorized":false,"notice":"Legacy shared content is a migration candidate, not independently reviewed knowledge. Resolve evidence, rights, applicability and limitations before curator submission. Unsupported native bodies must become explicit gaps."})
}
pub fn legacy_get(world: &World, request: LegacyRequest) -> Result<Value, String> {
    if !legacy_list(world)["assets"].as_array().is_some_and(|v| {
        v.iter()
            .any(|a| a["id"] == request.id && a["kind"] == request.kind)
    }) {
        return Err("Not an exposed shared legacy asset".into());
    }
    let payload = match request.kind.as_str() {
        "parametric_type.v1" => {
            // Snapshot the decodable payload shape, not the inspection wrapper.
            let inspection = super::parametric_mcp::world_get_type(
                world,
                super::parametric_mcp::GetParametricTypeRequest {
                    type_id: request.id.clone(),
                },
            )?;
            let bounds = world
                .get_resource::<crate::relational::registry::ParametricRegistry>()
                .and_then(|r| r.driver_bounds(&request.id).cloned())
                .unwrap_or_default();
            json!({"definition":inspection["definition"],
                "placement_contract":inspection["placement_contract"],"driver_bounds":bounds})
        }
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
        kind => (registry_kinds::handler(world, kind)
            .ok_or("Unsupported legacy kind")?
            .legacy_get)(world, &request.id)?,
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
    fn misplaced_or_unknown_payload_fields_are_refused_not_dropped() {
        use crate::relational::component::{ComponentParams, DriverPolicy};
        use crate::relational::{
            param_expr::Unit, registry::ParametricTypeDef, transform::TransformBindings,
        };
        let definition = ParametricTypeDef {
            id: "test.strict".into(),
            label: "Strict".into(),
            params: ComponentParams::default().driver("w", DriverPolicy::Editable),
            driver_units: BTreeMap::from([("w".into(), Unit::Mm)]),
            defaults: BTreeMap::from([("w".into(), 100.0)]),
            derivations: BTreeMap::new(),
            transform: TransformBindings::default(),
            public: true,
            representation: None,
        };
        let bounds = json!({"w":{"min":100,"max":100}});
        let mut nested = serde_json::to_value(&definition).unwrap();
        nested["driver_bounds"] = bounds.clone();
        let error = decode::<ParametricPayload>(json!({"definition":nested}))
            .err()
            .expect("a bound nested inside definition must not be silently dropped");
        let error: Value = serde_json::from_str(&error).unwrap();
        assert_eq!(error["code"], "UnknownPayloadFields");
        assert_eq!(error["ignored_paths"], json!(["definition.driver_bounds"]));
        let placed =
            decode::<ParametricPayload>(json!({"definition":definition,"driver_bounds":bounds}))
                .unwrap_or_else(|e| panic!("top-level bounds decode: {e}"));
        assert_eq!(placed.driver_bounds.len(), 1);

        // Installation records the exact admitted revision for later grounding.
        let mut admitted = passage("strict-rev");
        admitted.asset.id = "test.strict".into();
        admitted.asset.kind = "parametric_type.v1".into();
        admitted.asset.path_kind = "parametric".into();
        admitted.asset.payload = json!({"definition":definition,"driver_bounds":bounds});
        let mut world = World::new();
        world.init_resource::<evaluation::Observations>();
        install(&mut world, admitted).unwrap();
        assert_eq!(
            world
                .resource::<crate::relational::registry::ParametricRegistry>()
                .knowledge_revision("test.strict"),
            Some("strict-rev")
        );

        let mut asset = passage("r").asset;
        asset.payload["unread"] = json!(true);
        let refused = check_candidate(
            &World::new(),
            CandidateCheck {
                asset,
                driver_cases: vec![],
                catalog_queries: vec![],
            },
        )
        .err()
        .expect("candidate checks share the strict installer decoder");
        assert!(
            refused.contains("UnknownPayloadFields") && refused.contains("unread"),
            "{refused}"
        );
    }

    #[test]
    fn portable_recipes_carry_construction_system_obligations() {
        use crate::capability_registry::{
            effective_obligations, recipe_refinement, ElementClassDescriptor, ObligationTemplate,
            RecipeFamilyId,
        };
        use crate::curation::provenance::{Confidence, Lineage, Provenance};
        use crate::curation::{AssetId, AssetKindId, CurationMeta, RecipeArtifact, RecipeBody};
        use crate::plugins::refinement::{AgentId, ObligationId, RefinementState, SemanticRole};
        use std::collections::HashMap;
        let script = serde_json::from_value(json!({"parameter_schema":{"type":"object"},
            "allowed_tools":["create_box"],"mutation_scope":{"kind":"project_root"},
            "steps":[{"id":"frame","tool":"create_box","args":{}}],
            "postconditions":[{"postcondition":"obligation_satisfied",
                "obligation_id_expr":{"expr":"literal","value":"structure"},"by_step":"frame"}]}))
        .unwrap();
        let recipe = RecipeArtifact {
            meta: CurationMeta::new(
                AssetId("system".into()),
                AssetKindId("recipe.v1".into()),
                Provenance {
                    author: AgentId("unit-test".into()),
                    confidence: Confidence::High,
                    lineage: Lineage::Freeform,
                    rationale: None,
                    jurisdiction: None,
                    catalog_dependencies: vec![],
                    evidence: vec![],
                },
            ),
            body: RecipeBody::AuthoringScript { script },
            parameter_schema: json!({}),
            target_class: "fixture".into(),
            supported_refinement_states: vec![RefinementState::Constructible],
            tests: vec![],
        };
        let mut admitted = passage("system-r1");
        admitted.asset.id = "system".into();
        admitted.asset.kind = "recipe.v1".into();
        admitted.asset.path_kind = "recipe".into();
        admitted.asset.payload = serde_json::to_value(&recipe).unwrap();
        admitted.asset.payload["obligation_specializations"] = json!({"Constructible":[
            {"id":"member_section","role":"primary_structure","required_by_state":"Constructible"}]});
        admitted.asset.payload["promotion_critical_path_specializations"] =
            json!({"Constructible":["member_section"]});
        let mut world = World::new();
        world.init_resource::<evaluation::Observations>();
        world.insert_resource(crate::capability_registry::CapabilityRegistry::default());
        world
            .resource_mut::<crate::capability_registry::CapabilityRegistry>()
            .register_element_class(ElementClassDescriptor {
                class_min_obligations: HashMap::from([(
                    RefinementState::Constructible,
                    vec![ObligationTemplate {
                        id: ObligationId("structure".into()),
                        role: SemanticRole("primary_structure".into()),
                        required_by_state: RefinementState::Constructible,
                    }],
                )]),
                id: crate::capability_registry::ElementClassId("fixture".into()),
                label: "Fixture".into(),
                description: String::new(),
                semantic_roles: vec![],
                class_min_promotion_critical_paths: HashMap::new(),
                parameter_schema: json!({}),
            });
        let installed = install(&mut world, admitted.clone()).unwrap();
        // The install response says which in-force obligations stay undeclared.
        let coverage = &installed["execution"]["obligation_coverage"]["by_state"]["Constructible"];
        assert_eq!(coverage["in_force"], json!(["structure", "member_section"]));
        assert_eq!(coverage["undeclared"], json!(["member_section"]));
        assert_eq!(coverage["covered"], json!(false));
        let family = recipe.execution_family_id();
        // Recipe outputs are stamped with the exact installed revision.
        let revision = installed_revision(&world, "recipe.v1", &family.0).unwrap();
        assert_eq!(
            (revision.asset_id.as_str(), revision.content_hash.as_str()),
            ("system", "system-r1")
        );
        assert!(installed_revision(&world, "catalog.v1", &family.0).is_none());
        let class = ElementClassDescriptor {
            class_min_obligations: HashMap::from([(
                RefinementState::Constructible,
                vec![ObligationTemplate {
                    id: ObligationId("structure".into()),
                    role: SemanticRole("primary_structure".into()),
                    required_by_state: RefinementState::Constructible,
                }],
            )]),
            id: crate::capability_registry::ElementClassId("fixture".into()),
            label: "Fixture".into(),
            description: String::new(),
            semantic_roles: vec![],
            class_min_promotion_critical_paths: HashMap::new(),
            parameter_schema: json!({}),
        };
        let ids = |world: &World| {
            effective_obligations(
                &class,
                recipe_refinement(world, &family),
                RefinementState::Constructible,
            )
            .into_iter()
            .map(|o| o.id.0)
            .collect::<Vec<_>>()
        };
        assert_eq!(ids(&world), ["structure", "member_section"]);
        assert_eq!(
            recipe_refinement(&world, &family)
                .unwrap()
                .critical_paths_at(RefinementState::Constructible)
                .len(),
            1
        );
        // Keys and declared states must agree; unknown fields are refused.
        for (key, value) in [
            (
                "obligation_specializations",
                json!({"Constructible":[
                {"id":"x","role":"r","required_by_state":"Detailed"}]}),
            ),
            (
                "obligation_specializations",
                json!({"Constructible":[
                {"id":"x","role":"r","required_by_state":"Constructible","extra":1}]}),
            ),
        ] {
            let mut bad = admitted.clone();
            bad.asset.payload[key] = value;
            assert!(install(&mut World::new(), bad).is_err());
        }
        // A revision without specialisation clears the previous one.
        let mut plain = admitted.clone();
        plain.revision = "system-r2".into();
        let map = plain.asset.payload.as_object_mut().unwrap();
        map.remove("obligation_specializations");
        map.remove("promotion_critical_path_specializations");
        install(&mut world, plain).unwrap();
        assert_eq!(ids(&world), ["structure"]);
        assert!(recipe_refinement(&world, &RecipeFamilyId("missing".into())).is_none());
    }

    #[test]
    fn released_parametric_members_carry_system_obligations() {
        use crate::capability_registry::{
            effective_obligations, element_refinement, CapabilityRegistry, ElementClassDescriptor,
            ElementClassId,
        };
        use crate::plugins::parametric_mcp::{world_create, CreateParametricRequest};
        use crate::plugins::refinement::RefinementState;
        use crate::relational::component::{ComponentParams, DriverPolicy};
        use crate::relational::param_expr::{Quantity, ScalarExpr, Unit};
        use crate::relational::registry::{
            ParametricMember, ParametricMemberSemantic, ParametricRepresentation, ParametricTypeDef,
        };
        let mut app = bevy::app::App::new();
        app.add_plugins(crate::plugins::parametric_mcp::ParametricMcpPlugin);
        app.init_resource::<crate::plugins::identity::ElementIdAllocator>();
        app.init_resource::<CapabilityRegistry>();
        app.init_resource::<evaluation::Observations>();
        let w = app.world_mut();
        let class = ElementClassDescriptor {
            id: ElementClassId("test_panel".into()),
            label: "Panel".into(),
            description: String::new(),
            semantic_roles: vec![],
            class_min_obligations: std::collections::HashMap::new(),
            class_min_promotion_critical_paths: std::collections::HashMap::new(),
            parameter_schema: Value::Null,
        };
        w.resource_mut::<CapabilityRegistry>()
            .register_element_class(class.clone());
        let lit = |mm| ScalarExpr::lit(Quantity::mm(mm));
        let definition = ParametricTypeDef {
            id: "test.panel".into(),
            label: "Panel".into(),
            params: ComponentParams::default().driver("w", DriverPolicy::Editable),
            driver_units: BTreeMap::from([("w".into(), Unit::Mm)]),
            defaults: BTreeMap::from([("w".into(), 1000.0)]),
            derivations: BTreeMap::new(),
            transform: Default::default(),
            public: true,
            representation: Some(ParametricRepresentation {
                members: vec![ParametricMember {
                    size: [ScalarExpr::param("w"), lit(200.0), lit(100.0)],
                    translate: [lit(0.0), lit(0.0), lit(0.0)],
                    rotate_euler_deg: [lit(0.0), lit(0.0), lit(0.0)],
                    profile_xz: Vec::new(),
                    label: Some("panel".into()),
                    semantic: Some(ParametricMemberSemantic {
                        element_class: Some("test_panel".into()),
                        refinement_state: Some("Schematic".into()),
                        parameters: json!({}),
                        rationale: None,
                        concept: None,
                    }),
                }],
            }),
        };
        let spec = json!({"obligation_specializations":{"Constructible":[
            {"id":"panel_layers","role":"layer_composition","required_by_state":"Constructible"}]}});
        let admitted = |classes: Value| {
            let mut a = passage("panel-r1");
            a.asset.id = "test.panel".into();
            a.asset.kind = "parametric_type.v1".into();
            a.asset.path_kind = "parametric".into();
            a.asset.payload = json!({"definition":definition,"refinement_specializations":classes});
            a
        };
        assert!(install(w, admitted(json!({"other_class":spec})))
            .unwrap_err()
            .contains("no member declares"));
        install(w, admitted(json!({"test_panel":spec}))).unwrap();
        let created = world_create(
            w,
            CreateParametricRequest {
                type_id: "test.panel".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let entity = w
            .query::<(Entity, &crate::plugins::identity::ElementId)>()
            .iter(w)
            .find_map(|(e, id)| (id.0 == created.element_ids[0]).then_some(e))
            .unwrap();
        let ids = |w: &World| {
            effective_obligations(
                &class,
                element_refinement(w, entity, None),
                RefinementState::Constructible,
            )
            .into_iter()
            .map(|o| o.id.0)
            .collect::<Vec<_>>()
        };
        assert_eq!(ids(w), ["panel_layers"]);
        // A revision without specialisation clears it for existing members too.
        install(w, admitted(json!({}))).unwrap();
        assert!(ids(w).is_empty());
    }

    #[test]
    fn legacy_pattern_capture_is_exact_and_portable() {
        use crate::capability_registry::{AssemblyPatternDescriptor, CapabilityRegistry};
        let pattern = AssemblyPatternDescriptor {
            pattern_id: "synthetic.pattern".into(),
            label: "Synthetic".into(),
            description: "Test-only shared descriptor".into(),
            target_types: vec!["synthetic".into()],
            axis: "test".into(),
            layers: vec![],
            relation_rules: vec![],
            root_layer_ids: vec![],
            requires_support_path: false,
            tags: vec![],
            parameter_schema: json!({"type":"object"}),
        };
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_assembly_pattern(pattern.clone());
        world.insert_resource(registry);
        let listing = legacy_list(&world);
        let item = &listing["assets"][0];
        assert_eq!(item["portable"], true);
        let capture = legacy_get(
            &world,
            LegacyRequest {
                kind: "assembly_pattern.v1".into(),
                id: pattern.pattern_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(capture["payload"], serde_json::to_value(pattern).unwrap());
        assert_eq!(capture["publication_authorized"], false);
        assert!(capture["runtime_inspection"].is_null());
        let bytes = serde_json::to_vec(&capture["payload"]).unwrap();
        assert_eq!(
            capture["snapshot_sha256"],
            hex::encode(Sha256::digest(bytes))
        );
        assert!(payload_schema(&World::new(), "assembly_pattern.v1").is_ok());
        assert!(payload_schema(&World::new(), "native_fn_ref").is_err());
        assert!(legacy_get(
            &world,
            LegacyRequest {
                kind: "assembly_pattern.v1".into(),
                id: "private.draft".into()
            }
        )
        .is_err());
    }

    #[test]
    fn catalog_payload_schema_uses_actual_decoder_types() {
        let response = payload_schema(&World::new(), "catalog.v1").unwrap();
        let schema = &response["payload_schema"];
        assert_eq!(
            schema,
            &serde_json::to_value(schemars::schema_for!(CatalogData)).unwrap()
        );
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("rows")));
        assert_eq!(
            schema["properties"]["rows"]["items"]["$ref"],
            "#/$defs/CatalogRow"
        );
        for name in [
            "CatalogRow",
            "CorpusProvenance",
            "LicenseTag",
            "CatalogCategory",
        ] {
            assert!(schema["$defs"][name].is_object(), "{name}");
        }
        assert!(schema["$defs"]["CorpusProvenance"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("supersedes")));
        assert_eq!(response["next_tool"], "check_knowledge_candidate");
        assert!(payload_schema(&World::new(), "unknown.v1").is_err());
        assert!(payload_schema(&World::new(), "recipe.v1").is_ok());
    }

    #[test]
    fn portable_payload_schemas_resolve_every_reference_and_stay_bounded() {
        fn check_refs(value: &Value, root: &Value) {
            match value {
                Value::Object(fields) => {
                    if let Some(reference) = fields.get("$ref").and_then(Value::as_str) {
                        let pointer = reference.strip_prefix('#').expect("local schema reference");
                        assert!(root.pointer(pointer).is_some(), "unresolved {reference}");
                    }
                    for child in fields.values() {
                        check_refs(child, root);
                    }
                }
                Value::Array(children) => {
                    for child in children {
                        check_refs(child, root);
                    }
                }
                _ => (),
            }
        }
        for kind in [
            "catalog.v1",
            "parametric_type.v1",
            "recipe.v1",
            "definition_library.v1",
            "corpus_passage.v1",
        ] {
            let response = payload_schema(&World::new(), kind).unwrap();
            for field in ["asset_schema", "payload_schema"] {
                check_refs(&response[field], &response[field]);
            }
            let bytes = serde_json::to_vec(&response).unwrap().len();
            eprintln!("{kind}: {bytes} schema bytes");
            assert!(bytes < 200_000, "{kind}: {bytes}");
            assert_eq!(response["next_tool"], "check_knowledge_candidate");
        }
        let recipe = payload_schema(&World::new(), "recipe.v1").unwrap();
        assert_eq!(
            recipe["payload_schema"]["allOf"][0]["properties"]["body"]["properties"]["kind"]
                ["const"],
            "authoring_script"
        );
        let parametric = payload_schema(&World::new(), "parametric_type.v1").unwrap();
        assert_eq!(
            parametric["payload_schema"]["required"],
            json!(["definition"])
        );
        // Curators can declare an enforced applicability envelope per driver.
        assert!(parametric["payload_schema"]["properties"]["driver_bounds"].is_object());
        let library = payload_schema(&World::new(), "definition_library.v1").unwrap();
        let properties = &library["payload_schema"]["$defs"]["Definition"]["properties"];
        assert!(properties["body"].is_object());
        assert!(properties.get("evaluators").is_none());
        let unit = &library["payload_schema"]["$defs"]["ParameterUnit"];
        let variants = unit["anyOf"].as_array().expect("unit decoder is untagged");
        assert!(variants.iter().any(|v| v["type"] == "string"));
        assert!(variants.iter().any(
            |v| v["properties"]["dimension"].is_object() && v["properties"]["unit"].is_object()
        ));
        assert!(variants
            .iter()
            .all(|v| v["properties"].get("Typed").is_none()));
        let required = library["payload_schema"]["$defs"]["Definition"]["required"]
            .as_array()
            .unwrap();
        assert!(required.contains(&json!("body")));
        assert!(!required.contains(&json!("visibility")));
        let passage = payload_schema(&World::new(), "corpus_passage.v1").unwrap();
        assert!(passage["payload_schema"]["properties"]["proactive_guidance"].is_object());
    }

    #[test]
    fn candidate_check_rejects_envelope_mismatch_before_payload_admission() {
        let mut asset = passage("synthetic").asset;
        asset.path_kind = "recipe_or_executable_asset".into();
        let error = check_candidate(
            &World::new(),
            CandidateCheck {
                catalog_queries: vec![],
                asset,
                driver_cases: vec![],
            },
        )
        .unwrap_err();
        let detail: Value = serde_json::from_str(&error).unwrap();
        assert_eq!(detail["error"], "invalid_knowledge_envelope");
        assert_eq!(detail["violations"][0]["pointer"], "/path_kind");
        assert_eq!(detail["violations"][0]["expected"], "passage");
    }

    #[test]
    fn candidate_and_installation_share_exact_runtime_tool_requirements() {
        let mut asset = passage("candidate").asset;
        asset.required_tool_ids = vec!["lookup_source_passage".into()];
        assert!(
            super::super::model_api::missing_knowledge_tools(&asset.required_tool_ids).is_empty()
        );
        assert!(check_candidate(
            &World::new(),
            CandidateCheck {
                asset: asset.clone(),
                driver_cases: vec![],
                catalog_queries: vec![]
            }
        )
        .is_ok());
        asset.required_tool_ids = vec!["invented_test_alias".into()];
        let expected = super::super::model_api::missing_knowledge_tools(&asset.required_tool_ids);
        let error = check_candidate(
            &World::new(),
            CandidateCheck {
                asset,
                driver_cases: vec![],
                catalog_queries: vec![],
            },
        )
        .unwrap_err();
        let error: Value = serde_json::from_str(&error).unwrap();
        assert_eq!(error["missing_required_tool_ids"], json!(expected));
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
        let checked = check_candidate(
            &World::new(),
            CandidateCheck {
                catalog_queries: vec![],
                asset: asset.clone(),
                driver_cases: vec![],
            },
        )
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
        assert!(check_candidate(
            &World::new(),
            CandidateCheck {
                catalog_queries: vec![],
                asset: private,
                driver_cases: vec![]
            }
        )
        .is_err());
    }

    #[test]
    fn discovery_and_exact_reads_have_native_evidence_even_when_empty() {
        let mut world = World::new();
        let mut index = Index {
            schema: SCHEMA.into(),
            epoch: 1,
            assets: vec![],
            withdrawn_asset_ids: vec![],
            evaluation_scope: None,
            released: Some(true),
        };
        let empty = reconcile(&mut world, &index);
        assert_eq!(empty["assets"], json!([]));
        assert_eq!(empty["complete"], true);
        let log = evaluation::snapshot(&world);
        assert_eq!(
            log["records"][0]["event"]["operation"],
            "list_knowledge_assets"
        );
        assert_eq!(log["records"][0]["event"]["asset_count"], 0);
        assert_eq!(
            log["records"][0]["event"]["response_sha256"],
            hex::encode(Sha256::digest(serde_json::to_vec(&empty).unwrap()))
        );
        let admitted = passage("revision-read");
        index.assets.push(IndexEntry {
            content_hash: admitted.revision.clone(),
            asset: json!({"id":admitted.asset.id,"kind":admitted.asset.kind}),
            sequence: 1,
        });
        let discovered = reconcile(&mut world, &index);
        let inspected = inspect_observed(&mut world, admitted.clone());
        assert_eq!(inspected, inspect(&admitted));
        assert!(world.get_resource::<InstalledAssets>().is_none());
        let log = evaluation::snapshot(&world);
        assert_eq!(log["records"].as_array().unwrap().len(), 3);
        assert_eq!(
            log["records"][1]["event"]["response_sha256"],
            hex::encode(Sha256::digest(serde_json::to_vec(&discovered).unwrap()))
        );
        let event = &log["records"][2]["event"];
        assert_eq!(event["operation"], "get_knowledge_asset");
        assert_eq!(event["content_hash"], "revision-read");
        assert_eq!(
            event["response_sha256"],
            hex::encode(Sha256::digest(serde_json::to_vec(&inspected).unwrap()))
        );
        assert_eq!(
            payload_schema(&World::new(), "candidate_runtime.v1").unwrap()
                ["knowledge_observation_operations"],
            json!(evaluation::LIFECYCLE_OPERATIONS)
        );
    }

    #[test]
    fn typed_passage_reads_require_exact_installed_revision_and_record_reference_use() {
        let mut world = World::new();
        world.init_resource::<evaluation::Observations>();
        let one = passage("revision-one");
        assert!(read_installed_passage(&world, one.clone()).is_err());
        install(&mut world, one.clone()).unwrap();
        let read = read_installed_passage(&world, one.clone()).unwrap();
        assert_eq!(read["content_hash"], "revision-one");
        assert_eq!(read["text"], one.asset.payload["text"]);
        assert_eq!(read["released"], true);
        let snapshot = evaluation::snapshot(&world);
        let event = &snapshot["records"][1]["event"];
        assert_eq!(event["operation"], "lookup_source_passage");
        assert_eq!(event["asset_kind"], "corpus_passage.v1");
        assert_eq!(event["content_hash"], "revision-one");
        assert_eq!(
            event["response_sha256"],
            hex::encode(Sha256::digest(serde_json::to_vec(&read).unwrap()))
        );
        let two = passage("revision-two");
        assert!(read_installed_passage(&world, two.clone()).is_err());
        install(&mut world, two.clone()).unwrap();
        assert!(read_installed_passage(&world, one.clone()).is_err());
        assert_eq!(
            read_installed_passage(&world, two.clone()).unwrap()["content_hash"],
            "revision-two"
        );
        let mut wrong = two;
        wrong.asset.kind = "recipe.v1".into();
        assert!(read_installed_passage(&world, wrong).is_err());
        let mut staged = one;
        staged.scope_sha256 = Some("scope".into());
        install(&mut world, staged.clone()).unwrap();
        assert_eq!(
            read_installed_passage(&world, staged).unwrap()["released"],
            false
        );
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
    fn catalog_numeric_equality_preserves_large_integer_precision() {
        assert!(catalog_value_equal(&json!(7), &json!(7.0)));
        assert!(catalog_value_equal(&json!(-7), &json!(-7.0)));
        assert!(catalog_value_equal(&json!({"a":[7]}), &json!({"a":[7.0]})));
        assert!(!catalog_value_equal(
            &json!(9007199254740993u64),
            &json!(9007199254740992.0)
        ));
        assert!(!catalog_value_equal(
            &json!(u64::MAX),
            &json!(18446744073709551616.0)
        ));
        assert!(!catalog_value_equal(
            &json!(i64::MIN),
            &json!(-9223372036854777856.0)
        ));
        assert!(!catalog_value_equal(&json!(7), &json!(7.5)));
        assert_eq!(json_type(&json!(7.0)), "integer");
        assert_eq!(json_type(&json!(7.5)), "number");
    }

    #[test]
    fn candidate_catalog_queries_use_native_filters_without_admission() {
        let mut candidate = passage("catalog-r1");
        candidate.asset.id = "test.catalog".into();
        candidate.asset.kind = "catalog.v1".into();
        candidate.asset.path_kind = "catalog".into();
        candidate.asset.payload = json!({"id":"test.catalog","label":"Synthetic catalog",
        "description":"Synthetic contract fixture","category":{"kind":"Other","value":"fixture"},
        "region":"scope-A","license":{"kind":"Cc0"},"source_version":"one","rows":[{
            "row_id":"row-one","category":{"kind":"Other","value":"fixture"},
            "data":{"size":7,"label":"sample"},
            "provenance":{"source":"Synthetic fixture","source_version":"one","jurisdiction":"scope-A",
                "ingested_at":0,"license":{"kind":"Cc0"},"backlink":null,"supersedes":[]}
        }]});
        let run = |filters: Value| {
            check_candidate(
                &World::new(),
                CandidateCheck {
                    asset: candidate.asset.clone(),
                    driver_cases: vec![],
                    catalog_queries: serde_json::from_value(filters).unwrap(),
                },
            )
        };
        let checked = run(json!([{"size":7},{"size":8}])).unwrap();
        assert_eq!(checked["catalog_cases"][0]["row_count"], 1);
        assert_eq!(checked["catalog_cases"][0]["rows"][0]["data"]["size"], 7);
        assert_eq!(checked["catalog_cases"][1]["row_count"], 0);
        assert_eq!(checked["catalog_cases"][1]["rows"], json!([]));
        assert_eq!(checked["installed"], false);
        assert_eq!(checked["released"], false);
        assert_eq!(checked["model_modified"], false);
        let mut world = World::new();
        install(&mut world, candidate.clone()).unwrap();
        let native = super::super::model_api::handle_catalog_query(
            &world,
            "test.catalog".into(),
            json!({"size":7}),
        )
        .unwrap();
        assert_eq!(
            checked["catalog_cases"][0]["response_sha256"],
            hex::encode(Sha256::digest(
                serde_json::to_vec(&serde_json::to_value(&native).unwrap()).unwrap()
            ))
        );
        assert!(run(json!([{"unknown":1}])).is_err());
        assert!(run(json!([{"size":"wrong-type"}])).is_err());
        assert!(run(json!(vec![json!({}); 17])).is_err());
        assert!(check_candidate(
            &World::new(),
            CandidateCheck {
                asset: passage("other").asset,
                driver_cases: vec![],
                catalog_queries: vec![BTreeMap::new()]
            }
        )
        .is_err());
        let advertised = payload_schema(&World::new(), "catalog.v1").unwrap();
        assert_eq!(
            advertised["candidate_checks"]["catalog.v1"]["query_schema"]["items"]["type"],
            "object"
        );
        assert_eq!(
            payload_schema(&World::new(), "candidate_runtime.v1").unwrap()["candidate_checks"],
            advertised["candidate_checks"]
        );
    }

    #[test]
    fn dynamic_catalog_filters_and_scope_are_explicit_and_revision_bound() {
        let mut candidate = passage("catalog-r1");
        candidate.asset.id = "test.catalog".into();
        candidate.asset.kind = "catalog.v1".into();
        candidate.asset.path_kind = "catalog".into();
        candidate.asset.payload = json!({"id":"test.catalog","label":"Synthetic catalog",
        "description":"Synthetic contract fixture","category":{"kind":"Other","value":"fixture"},
        "region":"scope-A","license":{"kind":"Cc0"},"source_version":"one","rows":[{
            "row_id":"row-one","category":{"kind":"Other","value":"fixture"},
            "data":{"size":7,"label":"sample"},
            "provenance":{"source":"Synthetic fixture","source_version":"one","jurisdiction":"scope-A",
                "ingested_at":0,"license":{"kind":"Cc0"},"backlink":null,"supersedes":[]}
        }]});
        let mut world = World::new();
        world.init_resource::<evaluation::Observations>();
        install(&mut world, candidate.clone()).unwrap();
        let contract = catalog_query_contract(&world, "test.catalog");
        assert_eq!(
            contract["filter_schema"]["properties"]["size"]["type"],
            json!(["integer"])
        );
        assert!(contract["filter_schema"]["properties"]["region"].is_null());
        for invalid in [
            json!({"region":"scope-A"}),
            json!({"size":"7"}),
            json!({"typo":7}),
            Value::Null,
        ] {
            assert!(crate::plugins::model_api::handle_catalog_query(
                &world,
                "test.catalog".into(),
                invalid
            )
            .is_err());
        }
        for (filter, count) in [
            (json!({}), 1),
            (json!({"size":7}), 1),
            (json!({"size":7.0}), 1),
            (json!({"size":8}), 0),
        ] {
            assert_eq!(
                crate::plugins::model_api::handle_catalog_query(
                    &world,
                    "test.catalog".into(),
                    filter
                )
                .unwrap()
                .len(),
                count
            );
        }
        for (scope, decision) in [
            ("scope-A", "declared"),
            ("scope-B", "not_declared"),
            ("SCOPE-A", "not_declared"),
        ] {
            let response = probe_catalog_scope(&world, "test.catalog", scope).unwrap();
            assert_eq!(response["decision"], decision);
            assert_eq!(response["content_hash"], "catalog-r1");
            assert_eq!(response["asset_id"], "test.catalog");
            assert_eq!(response["compatibility_established"], false);
            let snapshot = evaluation::snapshot(&world);
            let event = &snapshot["records"].as_array().unwrap().last().unwrap()["event"];
            assert_eq!(event["operation"], "catalog_scope_probe");
            assert_eq!(event["requested_scope"], scope);
            assert_eq!(event["decision"], decision);
            assert!(event["response_sha256"].as_str().unwrap().len() == 64);
        }
        assert!(probe_catalog_scope(&world, "missing", "scope-A").is_err());
        assert!(probe_catalog_scope(&world, "test.catalog", " ").is_err());
        candidate.revision = "catalog-r2".into();
        candidate.asset.payload["region"] = Value::Null;
        candidate.asset.payload["rows"][0]["data"] = json!({"new_field":true});
        install(&mut world, candidate).unwrap();
        assert!(validate_catalog_filter(&world, "test.catalog", &json!({"size":7})).is_err());
        assert!(
            validate_catalog_filter(&world, "test.catalog", &json!({"new_field":true})).is_ok()
        );
        let result = probe_catalog_scope(&world, "test.catalog", "scope-A").unwrap();
        assert_eq!(result["decision"], "unspecified");
        assert_eq!(result["content_hash"], "catalog-r2");
    }

    #[test]
    fn catalog_replacement_requires_existing_dynamic_ownership() {
        use crate::capability_registry::{CapabilityRegistry, CatalogProviderId};
        let mut candidate = passage("catalog-one");
        candidate.asset.id = "test.catalog".into();
        candidate.asset.kind = "catalog.v1".into();
        candidate.asset.path_kind = "catalog".into();
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
