//! Minimized private observations. Export is a host decision, never a model
//! argument. Pending records are credential/source-bound and are not manifests.
use super::{agent_skill_cloud, AgentCorpusConnection, CurationConnection, RemedyKey};
use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
pub const OBSERVATION_SCHEMA: &str = "talos3d.curation-observation.v1";
const PENDING_SCHEMA: &str = "talos3d.private-pending-incidents.v1";
const MAX_PENDING: usize = 128;
const MAX_BYTES: usize = 512 * 1024;
const MAX_SUBMIT: usize = 8;
const REQUIRED_GUIDANCE_FAILURE: &str = "negotiated_required_guidance_missing";
const REQUIRED_GUIDANCE_DETECTOR: &str = "negotiated_required_guidance.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    MissingAdvertisedTool,
    BrokenGuidanceReference,
    ExecutionFailure,
    CorpusGap,
    UserCorrection,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub schema: String,
    pub event_id: String,
    pub kind: ObservationKind,
    pub applicability: RemedyKey,
    pub failure_code: String,
    pub app_revision: String,
    pub export_authorized: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportHarnessIncident {
    /// Reuse the returned event_id when retrying the same observation.
    #[serde(default)]
    pub event_id: Option<String>,
    pub kind: ObservationKind,
    pub applicability: RemedyKey,
    pub failure_code: String,
    pub app_revision: String,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}
impl Observation {
    fn validate_local(&self) -> Result<(), String> {
        if self.schema != OBSERVATION_SCHEMA || uuid::Uuid::parse_str(&self.event_id).is_err() {
            return Err("Invalid incident schema or event identity".into());
        }
        let fields = self.applicability.fields();
        if !identifier(&self.failure_code)
            || !identifier(&self.app_revision)
            || fields.iter().all(|v| v.is_none())
            || !fields.into_iter().flatten().all(|v| identifier(v))
        {
            return Err("Incidents require bounded public failure/applicability identifiers; no prose, prompts or geometry".into());
        }
        if self.export_authorized {
            return Err("Local incident records cannot grant export authority".into());
        }
        Ok(())
    }
}
/// Set only by trusted host configuration. No MCP tool can mutate this policy.
#[derive(Clone, Default, Resource)]
pub struct HarnessIncidentPolicy {
    pub export_enabled: bool,
    /// Optional host-owned private cache, outside project/source repositories.
    pub pending_path: Option<PathBuf>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRecord {
    observation: Observation,
    source: String,
    credential_fingerprint: Option<String>,
    submitted: bool,
    /// Local provenance only. Legacy/explicit records cannot impersonate the
    /// host's detector merely by supplying identical observation fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detector: Option<String>,
}
#[derive(Default)]
struct PendingState {
    loaded_path: Option<PathBuf>,
    initialized: bool,
    records: BTreeMap<String, PendingRecord>,
}
#[derive(Clone, Default, Resource)]
pub struct HarnessIncidentStore(Arc<Mutex<PendingState>>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingFile {
    schema: String,
    records: BTreeMap<String, PendingRecord>,
}
fn source(connection: &CurationConnection) -> Result<String, String> {
    if connection.base_url.is_empty() {
        return Ok(String::new());
    }
    // This URL is persisted in a private cache. Reject embedded credentials.
    let url =
        reqwest::Url::parse(&connection.base_url).map_err(|_| "Invalid incident service URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Incident service URL cannot contain credentials, queries or fragments".into());
    }
    if !(url.scheme() == "https"
        || (url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))))
    {
        return Err("Incident service requires HTTPS or test loopback".into());
    }
    Ok(connection.base_url.trim_end_matches('/').into())
}
fn fingerprint(token: Option<&str>) -> Option<String> {
    token
        .filter(|v| !v.is_empty())
        .map(|v| hex::encode(Sha256::digest(v.as_bytes())))
}
impl HarnessIncidentStore {
    fn load<'a>(
        &'a self,
        policy: &HarnessIncidentPolicy,
    ) -> Result<std::sync::MutexGuard<'a, PendingState>, String> {
        let mut state = self.0.lock().expect("pending incidents poisoned");
        if !state.initialized || state.loaded_path != policy.pending_path {
            let records = if let Some(path) = &policy.pending_path {
                read_pending(path)?
            } else {
                BTreeMap::new()
            };
            state.records = records;
            state.loaded_path = policy.pending_path.clone();
            state.initialized = true;
        }
        Ok(state)
    }
    pub fn report(
        &self,
        request: ReportHarnessIncident,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        bearer: Option<&str>,
    ) -> Result<Value, String> {
        if request.failure_code == REQUIRED_GUIDANCE_FAILURE {
            return Err("This failure code is reserved for verified host detection".into());
        }
        self.report_inner(request, policy, connection, bearer, false)
    }

    /// Only the host's verified negotiated-guidance detector calls this. No
    /// caller-supplied identifiers or session/project content enter the record.
    #[cfg(feature = "model-api")]
    pub(crate) fn report_missing_required_guidance(
        &self,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        bearer: Option<&str>,
    ) -> Result<Value, String> {
        self.report_inner(
            ReportHarnessIncident {
                event_id: None,
                kind: ObservationKind::BrokenGuidanceReference,
                applicability: RemedyKey {
                    gap_kind: Some("required_guidance_unavailable".into()),
                    capability_id: Some("core.model_api".into()),
                    contract_revision: Some("agent_welcome.v2".into()),
                    ..Default::default()
                },
                failure_code: REQUIRED_GUIDANCE_FAILURE.into(),
                app_revision: format!("core.{}", env!("CARGO_PKG_VERSION")),
            },
            policy,
            connection,
            bearer,
            true,
        )
    }

    fn report_inner(
        &self,
        request: ReportHarnessIncident,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        bearer: Option<&str>,
        deduplicate_content: bool,
    ) -> Result<Value, String> {
        let source = source(connection)?;
        let credential_fingerprint = fingerprint(bearer);
        let mut observation = Observation {
            schema: OBSERVATION_SCHEMA.into(),
            event_id: match request.event_id {
                Some(id) => uuid::Uuid::parse_str(&id)
                    .map_err(|_| "Invalid incident event identity")?
                    .to_string(),
                None => uuid::Uuid::new_v4().to_string(),
            },
            kind: request.kind,
            applicability: request.applicability,
            failure_code: request.failure_code,
            app_revision: request.app_revision,
            export_authorized: false,
        };
        observation.validate_local()?;
        let mut state = self.load(policy)?;
        // Coalesce this detector's signal per core version and host identity,
        // including after restart/submission. Check and insert under one lock.
        // Explicit reports retain their caller-selected event semantics.
        if deduplicate_content {
            if let Some(old) = state.records.values().find(|record| {
                record.detector.as_deref() == Some(REQUIRED_GUIDANCE_DETECTOR)
                    && record.observation.kind == observation.kind
                    && record.observation.applicability == observation.applicability
                    && record.observation.failure_code == observation.failure_code
                    && record.observation.app_revision == observation.app_revision
                    && record.source == source
                    && record.credential_fingerprint == credential_fingerprint
            }) {
                observation.event_id = old.observation.event_id.clone();
            }
        }
        if let Some(old) = state.records.get(&observation.event_id) {
            if old.observation != observation
                || old.source != source
                || old.credential_fingerprint != credential_fingerprint
            {
                return Err(
                    "Incident event identity conflicts with existing local content or identity"
                        .into(),
                );
            }
            return Ok(
                json!({"event_id":observation.event_id,"recorded_locally":true,"duplicate":true,"submitted":old.submitted,"export_enabled":policy.export_enabled,"durability":if policy.pending_path.is_some(){"private_host_file"}else{"application_lifetime"}}),
            );
        }
        if state.records.len() >= MAX_PENDING {
            return Err("Private pending incident capacity reached; host review is required before retaining more reports".into());
        }
        let record = PendingRecord {
            observation: observation.clone(),
            source,
            credential_fingerprint,
            submitted: false,
            detector: deduplicate_content.then(|| REQUIRED_GUIDANCE_DETECTOR.into()),
        };
        let mut candidate = state.records.clone();
        candidate.insert(observation.event_id.clone(), record);
        persist(policy, &candidate)?;
        state.records = candidate;
        Ok(
            json!({"event_id":observation.event_id,"recorded_locally":true,"duplicate":false,"submitted":false,"export_enabled":policy.export_enabled,"durability":if policy.pending_path.is_some(){"private_host_file"}else{"application_lifetime"},"note":"Recording is local. Export requires explicit host policy and the original service/credential; no investigation or provider execution is started."}),
        )
    }
    pub async fn submit(
        &self,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        corpus: &AgentCorpusConnection,
    ) -> Result<Value, String> {
        if !policy.export_enabled {
            return Ok(
                json!({"submitted_count":0,"status":"export_disabled","note":"Only the user/operator host setting can authorize shared export. Recording locally does not enable it."}),
            );
        }
        let source = source(connection)?;
        let credential_fingerprint = fingerprint(corpus.bearer_token.as_deref());
        if source.is_empty() || credential_fingerprint.is_none() {
            return Ok(
                json!({"submitted_count":0,"status":"host_identity_unavailable","note":"Reports remain local until the original authenticated host identity is available. Unbound reports are never automatically adopted."}),
            );
        }
        let same_origin = reqwest::Url::parse(&source)
            .ok()
            .zip(reqwest::Url::parse(&corpus.base_url).ok())
            .is_some_and(|(target, credential_source)| {
                target.origin() == credential_source.origin()
            });
        if !same_origin {
            return Ok(
                json!({"submitted_count":0,"status":"credential_origin_mismatch","note":"The host credential belongs to another service origin; reports remain local."}),
            );
        }
        let selected = {
            let state = self.load(policy)?;
            state
                .records
                .values()
                .filter(|r| {
                    !r.submitted
                        && r.source == source
                        && r.credential_fingerprint == credential_fingerprint
                })
                .take(MAX_SUBMIT)
                .cloned()
                .collect::<Vec<_>>()
        };
        if selected.is_empty() {
            return Ok(
                json!({"submitted_count":0,"status":"no_eligible_pending_reports","note":"Reports recorded for another source or credential, including a rotated credential, remain local and require explicit host review; no automatic rebind occurs."}),
            );
        }
        let authenticated = AgentCorpusConnection {
            base_url: source.clone(),
            bearer_token: corpus.bearer_token.clone(),
            trusted_public_keys: Vec::new(),
        };
        let mut submitted = 0;
        for record in selected {
            let mut observation = record.observation.clone();
            observation.export_authorized = true;
            // Stable event id and byte-equivalent typed payload make transport
            // retries safe even if a receipt was lost before local persistence.
            let body =
                serde_json::to_value(&observation).map_err(|_| "Incident serialization failed")?;
            let response = match agent_skill_cloud::request(
                &authenticated,
                "/incidents",
                Some(&body),
            )
            .await
            {
                Ok(bytes) => bytes,
                Err(_) => {
                    return Ok(
                        json!({"submitted_count":submitted,"status":"delivery_unavailable","note":"Remaining reports stay pending; retry uses the same event identities. No credential or remote error body is exposed."}),
                    )
                }
            };
            let receipt: Value =
                serde_json::from_slice(&response).map_err(|_| "Invalid incident receipt")?;
            if receipt
                .get("incident_id")
                .and_then(Value::as_str)
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
                .is_none()
            {
                return Err("Invalid incident receipt; report remains pending".into());
            }
            let mut state = self.load(policy)?;
            let mut candidate = state.records.clone();
            if let Some(current) = candidate.get_mut(&record.observation.event_id) {
                if current.source != source
                    || current.credential_fingerprint != credential_fingerprint
                    || current.observation != record.observation
                {
                    return Err("Local report identity changed during delivery".into());
                }
                current.submitted = true;
            }
            persist(policy, &candidate)?;
            state.records = candidate;
            submitted += 1;
        }
        Ok(
            json!({"submitted_count":submitted,"status":"submitted","note":"Delivery records an incident, not a diagnosis, accepted remedy or project repair. Additional eligible reports can be submitted in another bounded call."}),
        )
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn read_pending(path: &Path) -> Result<BTreeMap<String, PendingRecord>, String> {
    use std::io::Read;
    if !path.is_absolute() {
        return Err("Private incident cache path must be absolute and host-configured".into());
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(_) => return Err("Private incident cache unavailable".into()),
    };
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Private incident cache read failed")?;
    if bytes.len() > MAX_BYTES {
        return Err("Private incident cache exceeds capacity".into());
    }
    let parsed: PendingFile =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private incident cache")?;
    if parsed.schema != PENDING_SCHEMA || parsed.records.len() > MAX_PENDING {
        return Err("Invalid private incident cache format or capacity".into());
    }
    for (id, record) in &parsed.records {
        record.observation.validate_local()?;
        if id != &record.observation.event_id {
            return Err("Invalid private incident cache identity".into());
        }
    }
    Ok(parsed.records)
}
#[cfg(target_arch = "wasm32")]
fn read_pending(_: &Path) -> Result<BTreeMap<String, PendingRecord>, String> {
    Err("Persistent private incident storage needs a host adapter on this target".into())
}
fn persist(
    policy: &HarnessIncidentPolicy,
    records: &BTreeMap<String, PendingRecord>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&PendingFile {
        schema: PENDING_SCHEMA.into(),
        records: records.clone(),
    })
    .map_err(|_| "Private incident serialization failed")?;
    if bytes.len() > MAX_BYTES {
        return Err("Private incident byte capacity reached".into());
    }
    if let Some(path) = &policy.pending_path {
        write_pending(path, &bytes)?;
    }
    Ok(())
}
#[cfg(not(target_arch = "wasm32"))]
fn write_pending(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if !path.is_absolute() {
        return Err("Private incident cache must be absolute".into());
    }
    let parent = path
        .parent()
        .ok_or("Private incident cache needs parent directory")?;
    std::fs::create_dir_all(parent).map_err(|_| "Private incident directory unavailable")?;
    let temp = parent.join(format!(".pending-incidents-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .map_err(|_| "Private incident write unavailable")?;
        file.write_all(bytes)
            .map_err(|_| "Private incident write failed")?;
        file.sync_all()
            .map_err(|_| "Private incident sync failed")?;
        std::fs::rename(&temp, path).map_err(|_| "Private incident replace failed")?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|d| d.sync_all())
            .map_err(|_| "Private incident directory sync failed")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
#[cfg(target_arch = "wasm32")]
fn write_pending(_: &Path, _: &[u8]) -> Result<(), String> {
    Err("Persistent private incident storage needs a host adapter on this target".into())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[cfg(feature = "model-api")]
    #[test]
    fn automatic_signal_deduplicates_atomically_across_restart_and_identity() {
        let dir = std::env::temp_dir().join(format!("talos-dedup-{}", uuid::Uuid::new_v4()));
        let policy = HarnessIncidentPolicy {
            pending_path: Some(dir.join("pending.json")),
            ..Default::default()
        };
        let connection = CurationConnection {
            base_url: "https://example.test/v1/curation".into(),
            ..Default::default()
        };
        let store = HarnessIncidentStore::default();
        let ids = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        store
                            .report_missing_required_guidance(
                                &policy,
                                &connection,
                                Some("original"),
                            )
                            .unwrap()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().unwrap()["event_id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        });
        assert!(ids.iter().all(|id| *id == ids[0]));
        let restarted = HarnessIncidentStore::default();
        let same = restarted
            .report_missing_required_guidance(&policy, &connection, Some("original"))
            .unwrap();
        assert_eq!(same["event_id"], ids[0]);
        assert_eq!(same["duplicate"], true);
        let rotated = restarted
            .report_missing_required_guidance(&policy, &connection, Some("rotated"))
            .unwrap();
        assert_ne!(rotated["event_id"], same["event_id"]);
        let other_source = CurationConnection {
            base_url: "https://other.test/v1/curation".into(),
            ..Default::default()
        };
        assert_ne!(
            restarted
                .report_missing_required_guidance(&policy, &other_source, Some("original"))
                .unwrap()["event_id"],
            same["event_id"]
        );
        assert_eq!(restarted.load(&policy).unwrap().records.len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn automatic_signal_respects_capacity_without_evicting_explicit_reports() {
        let store = HarnessIncidentStore::default();
        let policy = HarnessIncidentPolicy::default();
        let connection = CurationConnection::default();
        for _ in 0..MAX_PENDING {
            store
                .report(request(None), &policy, &connection, None)
                .unwrap();
        }
        assert!(store
            .report_missing_required_guidance(&policy, &connection, None)
            .unwrap_err()
            .contains("capacity"));
        assert_eq!(store.load(&policy).unwrap().records.len(), MAX_PENDING);
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn explicit_reports_cannot_claim_or_preseed_host_detection() {
        let store = HarnessIncidentStore::default();
        let policy = HarnessIncidentPolicy::default();
        let connection = CurationConnection::default();
        let mut spoof = request(None);
        spoof.failure_code = REQUIRED_GUIDANCE_FAILURE.into();
        assert!(store
            .report(spoof, &policy, &connection, None)
            .unwrap_err()
            .contains("reserved"));
        let first = store
            .report_missing_required_guidance(&policy, &connection, None)
            .unwrap();
        let first_id = first["event_id"].as_str().unwrap();
        // Simulate a legacy record created before the code became reserved.
        store
            .load(&policy)
            .unwrap()
            .records
            .get_mut(first_id)
            .unwrap()
            .detector = None;
        let second = store
            .report_missing_required_guidance(&policy, &connection, None)
            .unwrap();
        assert_ne!(first["event_id"], second["event_id"]);
        let second_id = second["event_id"].as_str().unwrap();
        store
            .load(&policy)
            .unwrap()
            .records
            .get_mut(second_id)
            .unwrap()
            .submitted = true;
        let duplicate = store
            .report_missing_required_guidance(&policy, &connection, None)
            .unwrap();
        assert_eq!(duplicate["event_id"], second["event_id"]);
        assert_eq!(duplicate["submitted"], true);
    }

    fn request(id: Option<String>) -> ReportHarnessIncident {
        ReportHarnessIncident {
            event_id: id,
            kind: ObservationKind::CorpusGap,
            applicability: RemedyKey {
                gap_kind: Some("missing_path".into()),
                ..Default::default()
            },
            failure_code: "no_curated_path".into(),
            app_revision: "test-build-1".into(),
        }
    }
    fn connection() -> CurationConnection {
        CurationConnection {
            base_url: "http://127.0.0.1:1/v1/curation".into(),
            trusted_public_keys: vec![],
        }
    }
    #[test]
    fn report_is_private_durable_idempotent_and_conflicts_are_rejected() {
        let path = std::env::temp_dir()
            .join(format!("talos-incidents-{}", uuid::Uuid::new_v4()))
            .join("pending.json");
        let policy = HarnessIncidentPolicy {
            pending_path: Some(path.clone()),
            ..Default::default()
        };
        let store = HarnessIncidentStore::default();
        let request = request(None);
        let receipt = store
            .report(
                request.clone(),
                &policy,
                &connection(),
                Some("synthetic-credential-a"),
            )
            .unwrap();
        assert_eq!(receipt["submitted"], false);
        assert_eq!(receipt["export_enabled"], false);
        let mut uppercase = request.clone();
        uppercase.event_id = Some(receipt["event_id"].as_str().unwrap().to_uppercase());
        assert_eq!(
            store
                .report(
                    uppercase,
                    &policy,
                    &connection(),
                    Some("synthetic-credential-a")
                )
                .unwrap()["duplicate"],
            true
        );
        let id = receipt["event_id"].as_str().unwrap().to_string();
        let reloaded = HarnessIncidentStore::default();
        assert_eq!(
            reloaded
                .report(
                    super::tests::request(Some(id.clone())),
                    &policy,
                    &connection(),
                    Some("synthetic-credential-a")
                )
                .unwrap()["duplicate"],
            true
        );
        let mut conflict = super::tests::request(Some(id.clone()));
        conflict.failure_code = "different_failure".into();
        assert!(reloaded
            .report(
                conflict,
                &policy,
                &connection(),
                Some("synthetic-credential-a")
            )
            .is_err());
        assert!(reloaded
            .report(
                super::tests::request(Some(id)),
                &policy,
                &connection(),
                Some("synthetic-credential-b")
            )
            .is_err());
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains("synthetic-credential-a"));
        assert!(!receipt.to_string().contains("fingerprint"));
        let mut injected = serde_json::to_value(request).unwrap();
        injected["export_enabled"] = json!(true);
        assert!(serde_json::from_value::<ReportHarnessIncident>(injected).is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn export_off_identity_changes_and_offline_retry_never_leak_private_values() {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let received = seen.clone();
        let app=axum::Router::new().route("/v1/curation/incidents",axum::routing::post(move|headers:axum::http::HeaderMap,axum::Json(value):axum::Json<Value>|{
            let received=received.clone();async move{
                assert_eq!(headers.get("authorization").unwrap(),"Bearer synthetic-credential-a");
                let mut received=received.lock().unwrap();
                let first_loss=value["failure_code"]=="receipt_loss"&&!received.iter().any(|old|old["event_id"]==value["event_id"]);
                received.push(value);
                if first_loss{return axum::Json(json!({"receipt_lost":true}));}
                axum::Json(json!({"incident_id":uuid::Uuid::new_v4().to_string(),"duplicate":false}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let backend = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let connection = CurationConnection {
            base_url: format!("http://{address}/v1/curation"),
            trusted_public_keys: vec![],
        };
        let store = HarnessIncidentStore::default();
        let off = HarnessIncidentPolicy::default();
        let corpus = AgentCorpusConnection {
            base_url: format!("http://{address}/v1/corpus"),
            bearer_token: Some("synthetic-credential-a".into()),
            ..Default::default()
        };
        let receipt = store
            .report(
                request(None),
                &off,
                &connection,
                corpus.bearer_token.as_deref(),
            )
            .unwrap();
        assert_eq!(
            store.submit(&off, &connection, &corpus).await.unwrap()["status"],
            "export_disabled"
        );
        assert!(seen.lock().unwrap().is_empty());
        let enabled = HarnessIncidentPolicy {
            export_enabled: true,
            ..Default::default()
        };
        let changed = AgentCorpusConnection {
            base_url: corpus.base_url.clone(),
            bearer_token: Some("synthetic-credential-b".into()),
            ..Default::default()
        };
        assert_eq!(
            store.submit(&enabled, &connection, &changed).await.unwrap()["status"],
            "no_eligible_pending_reports"
        );
        assert!(seen.lock().unwrap().is_empty());
        let wrong_source = super::tests::connection();
        assert_eq!(
            store
                .submit(&enabled, &wrong_source, &corpus)
                .await
                .unwrap()["status"],
            "credential_origin_mismatch"
        );
        assert!(seen.lock().unwrap().is_empty());
        let result = store.submit(&enabled, &connection, &corpus).await.unwrap();
        assert_eq!(result["submitted_count"], 1);
        assert_eq!(seen.lock().unwrap()[0]["event_id"], receipt["event_id"]);
        assert_eq!(seen.lock().unwrap()[0]["export_authorized"], true);
        assert!(!result.to_string().contains("synthetic-credential"));
        assert!(!result.to_string().contains("fingerprint"));
        assert_eq!(
            store.submit(&enabled, &connection, &corpus).await.unwrap()["submitted_count"],
            0
        );
        let mut lost = request(None);
        lost.failure_code = "receipt_loss".into();
        let loss_receipt = store
            .report(lost, &off, &connection, corpus.bearer_token.as_deref())
            .unwrap();
        assert!(store.submit(&enabled, &connection, &corpus).await.is_err());
        assert_eq!(
            store.submit(&enabled, &connection, &corpus).await.unwrap()["submitted_count"],
            1
        );
        {
            let received = seen.lock().unwrap();
            let retries = received
                .iter()
                .filter(|v| v["event_id"] == loss_receipt["event_id"])
                .collect::<Vec<_>>();
            assert_eq!(retries.len(), 2);
            assert_eq!(retries[0], retries[1]);
        }
        backend.abort();
        let _ = backend.await;
        let pending = store
            .report(
                request(None),
                &off,
                &connection,
                corpus.bearer_token.as_deref(),
            )
            .unwrap();
        assert_eq!(
            store.submit(&enabled, &connection, &corpus).await.unwrap()["status"],
            "delivery_unavailable"
        );
        let state = store.load(&off).unwrap();
        assert!(!state.records[pending["event_id"].as_str().unwrap()].submitted);
    }
    #[tokio::test]
    async fn private_report_post_never_follows_redirects() {
        use axum::response::IntoResponse;
        let deliveries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let target = deliveries.clone();
        let app = axum::Router::new()
            .route(
                "/v1/curation/incidents",
                axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::TEMPORARY_REDIRECT,
                        [(axum::http::header::LOCATION, "/unexpected-recipient")],
                    )
                        .into_response()
                }),
            )
            .route(
                "/unexpected-recipient",
                axum::routing::post(move || {
                    let target = target.clone();
                    async move {
                        target.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(json!({"incident_id":uuid::Uuid::new_v4().to_string()}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let backend = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let connection = CurationConnection {
            base_url: format!("http://{address}/v1/curation"),
            trusted_public_keys: vec![],
        };
        let corpus = AgentCorpusConnection {
            base_url: format!("http://{address}/v1/corpus"),
            bearer_token: Some("synthetic-credential".into()),
            trusted_public_keys: vec![],
        };
        let store = HarnessIncidentStore::default();
        let policy = HarnessIncidentPolicy {
            export_enabled: true,
            ..Default::default()
        };
        store
            .report(
                request(None),
                &policy,
                &connection,
                corpus.bearer_token.as_deref(),
            )
            .unwrap();
        assert_eq!(
            store.submit(&policy, &connection, &corpus).await.unwrap()["status"],
            "delivery_unavailable"
        );
        assert_eq!(deliveries.load(std::sync::atomic::Ordering::SeqCst), 0);
        backend.abort();
        let _ = backend.await;
    }

    #[tokio::test]
    async fn unbound_reports_are_not_adopted_by_later_signin() {
        let store = HarnessIncidentStore::default();
        let policy = HarnessIncidentPolicy {
            export_enabled: true,
            ..Default::default()
        };
        store
            .report(request(None), &policy, &connection(), None)
            .unwrap();
        let corpus = AgentCorpusConnection {
            base_url: "http://127.0.0.1:1/v1/corpus".into(),
            bearer_token: Some("new-identity".into()),
            ..Default::default()
        };
        assert_eq!(
            store.submit(&policy, &connection(), &corpus).await.unwrap()["status"],
            "no_eligible_pending_reports"
        );
    }
}
