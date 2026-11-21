//! Host-owned scoped admission and bounded runtime evidence. No MCP setter.
use super::*;
use std::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub manifest_sha256: String,
    pub contract_sha256: String,
    pub slot_id: String,
    pub nonce: String,
    pub arm: String,
    pub native_instance_id: String,
}
impl Scope {
    pub fn digest(&self) -> String {
        digest(&serde_json::to_value(self).unwrap())
    }
}
// Do not derive Debug/Serialize: credentials and arm labels stay host-private.
#[derive(Resource, Clone)]
pub struct Connection {
    pub corpus: AgentCorpusConnection,
    pub scope: Scope,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    base_url: String,
    bearer_token: String,
    trusted_public_keys: Vec<String>,
    evaluation_scope: Scope,
}
fn hash(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn parse(raw: &str, instance: &str) -> Result<Connection, String> {
    if raw.len() > 8192 {
        return Err("Evaluation configuration exceeds bound".into());
    }
    let c: Config = serde_json::from_str(raw).map_err(|_| "Invalid evaluation configuration")?;
    let valid_url = c
        .base_url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|s| s.split_once("/v1/evaluation-stage/"))
        .is_some_and(|(port, attempt)| {
            port.parse::<u16>().is_ok_and(|p| p > 0)
                && uuid::Uuid::parse_str(attempt).is_ok_and(|id| id.to_string() == attempt)
        });
    if !valid_url
        || c.bearer_token.is_empty()
        || c.bearer_token.len() > 4096
        || c.trusted_public_keys.is_empty()
        || c.trusted_public_keys.len() > 8
        || !c.trusted_public_keys.iter().all(|k| hash(k))
        || !hash(&c.evaluation_scope.manifest_sha256)
        || !hash(&c.evaluation_scope.contract_sha256)
        || !hash(&c.evaluation_scope.nonce)
        || !matches!(c.evaluation_scope.arm.as_str(), "baseline" | "candidate")
        || c.evaluation_scope.slot_id.is_empty()
        || c.evaluation_scope.slot_id.len() > 128
        || c.evaluation_scope.native_instance_id != instance
    {
        return Err(
            "Evaluation requires an exact loopback stage, scope, instance and signing key".into(),
        );
    }
    Ok(Connection {
        corpus: AgentCorpusConnection {
            base_url: c.base_url,
            bearer_token: Some(c.bearer_token),
            trusted_public_keys: c.trusted_public_keys,
        },
        scope: c.evaluation_scope,
    })
}
/// Invalid opt-in stops MCP startup, never falls back to ordinary admission.
#[cfg(not(target_arch = "wasm32"))]
pub fn from_environment(instance: &str) -> Result<Option<Connection>, String> {
    let Some(raw) = std::env::var_os("TALOS3D_EVALUATION_KNOWLEDGE_CONNECTION") else {
        return Ok(None);
    };
    if std::env::var("TALOS3D_PROJECT_AUTO_CONNECT").as_deref() != Ok("0")
        || !std::env::var("TALOS3D_STATE_DIR").is_ok_and(|p| std::path::Path::new(&p).is_absolute())
    {
        return Err("Evaluation requires isolated state and disabled project auto-connect".into());
    }
    parse(
        raw.to_str()
            .ok_or("Invalid evaluation configuration encoding")?,
        instance,
    )
    .map(Some)
}
pub fn validate_index(index: &Index, expected: Option<&Scope>) -> Result<(), String> {
    match expected {
        None if index.evaluation_scope.is_none() && index.released != Some(false) => Ok(()),
        Some(scope)
            if index.evaluation_scope.as_ref() == Some(scope)
                && index.released == Some(false)
                && index
                    .assets
                    .iter()
                    .all(|e| e.asset["asset_kind"] == "catalog.v1") =>
        {
            Ok(())
        }
        _ => Err(
            "Knowledge index scope mismatch; evaluation content is not a production release".into(),
        ),
    }
}
#[derive(Resource, Default)]
pub struct Observations(Mutex<Log>);
#[derive(Default)]
struct Log {
    records: Vec<Value>,
    head: String,
    overflow: bool,
}
fn digest(v: &Value) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(v).unwrap()))
}
pub fn record(world: &World, event: Value) {
    let Some(log) = world.get_resource::<Observations>() else {
        return;
    };
    let mut log = log.0.lock().expect("knowledge observation lock");
    if log.records.len() >= 256 {
        log.overflow = true;
        return;
    }
    let item = json!({"sequence":log.records.len()+1,"previous_sha256":log.head,"event":event});
    log.head = digest(&item);
    log.records.push(item);
}
pub fn catalog_query(world: &World, provider: &str, filter: &Value, result: &Value) {
    let installed = world.get_resource::<InstalledAssets>().and_then(|assets| {
        assets
            .0
            .iter()
            .find(|(_, a)| a.kind == "catalog.v1" && a.runtime_id == provider)
    });
    record(
        world,
        json!({"operation":"catalog_query","provider_id":provider,
        "asset_id":installed.map(|(id,_)|id),"content_hash":installed.map(|(_,a)|&a.revision),
        "request_sha256":digest(filter),"response_sha256":digest(result),"row_count":result.as_array().map(Vec::len)}),
    );
}
pub fn catalog_scope_probe(world: &World, provider: &str, requested: &str, result: &Value) {
    record(
        world,
        json!({"operation":"catalog_scope_probe","provider_id":provider,"requested_scope":requested,
        "declared_scope":result["declared_scope"],"decision":result["decision"],"comparison":result["comparison"],
        "asset_id":result["asset_id"],"content_hash":result["content_hash"],
        "request_sha256":digest(&json!({"provider_id":provider,"requested_scope":requested})),"response_sha256":digest(result)}),
    );
}
pub fn snapshot(world: &World) -> Value {
    let Some(log) = world.get_resource::<Observations>() else {
        return json!({"complete":false});
    };
    let log = log.0.lock().expect("knowledge observation lock");
    json!({"schema":"talos3d.knowledge-observations.v1","complete":!log.overflow,
        "records":log.records,"head_sha256":log.head})
}
#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Value {
        json!({"base_url":"http://127.0.0.1:1234/v1/evaluation-stage/00000000-0000-0000-0000-000000000001",
            "bearer_token":"synthetic","trusted_public_keys":["ab".repeat(32)],
            "evaluation_scope":{"manifest_sha256":"aa".repeat(32),"contract_sha256":"bb".repeat(32),
                "slot_id":"opaque-1","nonce":"cc".repeat(32),"arm":"candidate","native_instance_id":"test-native"}})
    }
    #[test]
    fn host_scope_is_exact_and_loopback_only() {
        let original = config();
        assert!(parse(&original.to_string(), "test-native").is_ok());
        assert!(parse(&original.to_string(), "other").is_err());
        for url in [
            "https://example.com/stage",
            "http://127.0.0.1.evil:1234/v1/evaluation-stage/00000000-0000-0000-0000-000000000001",
            "http://127.0.0.1:0/v1/evaluation-stage/00000000-0000-0000-0000-000000000001",
        ] {
            let mut c = original.clone();
            c["base_url"] = json!(url);
            assert!(parse(&c.to_string(), "test-native").is_err());
        }
    }
    #[test]
    fn production_rejects_stage_and_every_binding_is_checked() {
        let scope = parse(&config().to_string(), "test-native").unwrap().scope;
        let mut index = Index {
            schema: SCHEMA.into(),
            epoch: 1,
            assets: vec![],
            withdrawn_asset_ids: vec![],
            evaluation_scope: Some(scope.clone()),
            released: Some(false),
        };
        assert!(validate_index(&index, None).is_err());
        assert!(validate_index(&index, Some(&scope)).is_ok());
        for field in [
            "manifest_sha256",
            "contract_sha256",
            "slot_id",
            "nonce",
            "arm",
            "native_instance_id",
        ] {
            let mut changed = serde_json::to_value(&scope).unwrap();
            changed[field] = json!("changed");
            index.evaluation_scope = Some(serde_json::from_value(changed).unwrap());
            assert!(validate_index(&index, Some(&scope)).is_err());
        }
        index.evaluation_scope = Some(scope.clone());
        index.released = Some(true);
        assert!(validate_index(&index, Some(&scope)).is_err());
        index.evaluation_scope = None;
        index.released = None;
        assert!(validate_index(&index, None).is_ok());
        assert!(validate_index(&index, Some(&scope)).is_err());
    }
    #[test]
    fn bounded_observations_fail_closed_on_overflow() {
        let mut world = World::new();
        world.init_resource::<Observations>();
        record(&world, json!({"operation":"unit"}));
        let v = snapshot(&world);
        assert_eq!(v["head_sha256"], digest(&v["records"][0]));
        for _ in 0..256 {
            record(&world, json!({}));
        }
        assert_eq!(snapshot(&world)["complete"], false);
        assert_eq!(snapshot(&world)["records"].as_array().unwrap().len(), 256);
    }
}
