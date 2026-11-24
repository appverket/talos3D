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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRequest {
    /// Missing subject, not a claim that a representation exists.
    pub topic: String,
    /// Private, bounded explanation. Export requires separate host permission.
    pub question: String,
}
impl KnowledgeRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.topic.trim().is_empty()
            || self.topic.len() > 160
            || self.question.trim().is_empty()
            || self.question.len() > 2048
            || self
                .topic
                .chars()
                .chain(self.question.chars())
                .any(|c| c.is_control() && c != '\n')
        {
            return Err("Knowledge request needs a bounded topic and question".into());
        }
        Ok(())
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_request: Option<KnowledgeRequest>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_request: Option<KnowledgeRequest>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IncidentStatusRequest {
    pub event_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryOutcome {
    pub event_id: String,
    /// A fresh UUID per outcome; reuse only for transport retries of the same outcome.
    pub feedback_id: String,
    pub artifact_id: String,
    pub revision: String,
    /// succeeded (requested task achieved, including requested state and constraints),
    /// failed (remedy used but the requested result was not achieved; queues a bounded
    /// curator follow-up) or inconclusive (could not determine; evidence only).
    /// This is reported project evidence, not independent acceptance.
    pub outcome: String,
    pub evidence_codes: Vec<String>,
    /// Remaining reference questions from an actual retry, not project design inputs.
    /// Requires the host's separate knowledge-export permission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residual_knowledge_gaps: Vec<KnowledgeRequest>,
}
impl RetryOutcome {
    /// Names every violated field constraint, so an MCP client can correct the
    /// report in one step instead of guessing which bound it missed.
    pub fn validate(&self) -> Result<(), String> {
        let hex64 = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let checks = [
            (
                uuid::Uuid::parse_str(&self.event_id).is_ok(),
                "event_id must be a UUID",
            ),
            (
                uuid::Uuid::parse_str(&self.feedback_id).is_ok(),
                "feedback_id must be a fresh UUID",
            ),
            (
                identifier(&self.artifact_id),
                "artifact_id must be a released artifact identifier",
            ),
            (
                hex64(&self.revision),
                "revision must be the 64-character lowercase hex content hash",
            ),
            (
                ["succeeded", "failed", "inconclusive"].contains(&self.outcome.as_str()),
                "outcome must be succeeded, failed or inconclusive",
            ),
            (
                (1..=16).contains(&self.evidence_codes.len()),
                "evidence_codes must hold 1 to 16 codes",
            ),
            (
                self.evidence_codes.iter().all(|v| identifier(v)),
                "each evidence code must be 1-160 characters of [A-Za-z0-9._:-]",
            ),
            (
                self.residual_knowledge_gaps.len() <= 4
                    && self
                        .residual_knowledge_gaps
                        .iter()
                        .all(|g| g.validate().is_ok()),
                "residual_knowledge_gaps must hold at most 4 valid knowledge requests",
            ),
            (
                self.outcome != "succeeded" || self.residual_knowledge_gaps.is_empty(),
                "a succeeded outcome cannot carry residual_knowledge_gaps",
            ),
        ];
        let violations: Vec<&str> = checks
            .iter()
            .filter(|(ok, _)| !ok)
            .map(|(_, rule)| *rule)
            .collect();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Invalid bounded retry outcome: {}",
                violations.join("; ")
            ))
        }
    }
}

// The service may queue a continuation in the same transaction as feedback.
// Retain that receipt with the original host binding, so a second MCP cycle can
// inspect and report against it without a coordinator manufacturing local state.
fn retain_followup(
    records: &mut BTreeMap<String, PendingRecord>,
    parent: &PendingRecord,
    report: &RetryOutcome,
    receipt: &Value,
) -> Result<(), String> {
    if receipt["followup"].is_null() {
        return Ok(());
    }
    let incident = receipt["followup"]["incident_id"]
        .as_str()
        .filter(|v| uuid::Uuid::parse_str(v).is_ok())
        .ok_or("Invalid continuation receipt")?;
    let mut child = parent.clone();
    child.observation.event_id = report.feedback_id.clone();
    child.incident_id = Some(incident.into());
    child.submitted = true;
    child.feedback.clear();
    child.detector = None;
    if let Some(old) = records.get(&report.feedback_id) {
        if old.observation != child.observation
            || old.source != child.source
            || old.credential_fingerprint != child.credential_fingerprint
            || old.incident_id != child.incident_id
        {
            return Err("Continuation conflicts with an existing private record".into());
        }
    } else {
        records.insert(report.feedback_id.clone(), child);
    }
    Ok(())
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
        let named = self.applicability.named_fields();
        if named.iter().all(|(_, v)| v.is_none()) {
            return Err("Incidents require at least one applicability identifier (gap_kind, element_class, path_kind, target_state, capability_id or contract_revision)".into());
        }
        let checked = [
            ("failure_code", &self.failure_code),
            ("app_revision", &self.app_revision),
        ]
        .into_iter()
        .chain(
            named
                .into_iter()
                .filter_map(|(n, v)| v.as_ref().map(|v| (n, v))),
        );
        for (name, value) in checked {
            if !identifier(value) {
                // Name the field and the rule so a caller corrects only that value
                // instead of discarding valid specific fields such as capability_id.
                return Err(format!("Incident field '{name}' must be 1..160 characters of letters, digits and . _ : - (no spaces, '@' or prose); other fields were accepted"));
            }
        }
        if let Some(request) = &self.knowledge_request {
            request.validate()?;
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
    /// Separate authorization for bounded prose, never inferred from identifier-only export.
    pub knowledge_export_enabled: bool,
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
    #[serde(default)]
    incident_id: Option<String>,
    #[serde(default)]
    feedback: Vec<PendingFeedback>,
    /// Local provenance only. Legacy/explicit records cannot impersonate the
    /// host's detector merely by supplying identical observation fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detector: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingFeedback {
    report: RetryOutcome,
    submitted: bool,
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
        if [REQUIRED_GUIDANCE_FAILURE, "operation_unsuccessful"]
            .contains(&request.failure_code.as_str())
        {
            return Err("This failure code is reserved for verified host detection".into());
        }
        self.report_inner(request, policy, connection, bearer, None)
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
                knowledge_request: None,
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
            Some(REQUIRED_GUIDANCE_DETECTOR),
        )
    }

    #[cfg(feature = "model-api")]
    pub(crate) fn report_operation_failure(
        &self,
        capability: &'static str,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        bearer: Option<&str>,
    ) -> Result<Value, String> {
        self.report_inner(
            ReportHarnessIncident {
                event_id: None,
                kind: ObservationKind::ExecutionFailure,
                applicability: RemedyKey {
                    capability_id: Some(capability.into()),
                    ..Default::default()
                },
                failure_code: "operation_unsuccessful".into(),
                app_revision: format!("core.{}", env!("CARGO_PKG_VERSION")),
                knowledge_request: None,
            },
            policy,
            connection,
            bearer,
            Some("host.operation_result.v1"),
        )
    }

    fn report_inner(
        &self,
        request: ReportHarnessIncident,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        bearer: Option<&str>,
        detector: Option<&str>,
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
            knowledge_request: request.knowledge_request,
        };
        observation.validate_local()?;
        let mut state = self.load(policy)?;
        // Coalesce this detector's signal per core version and host identity,
        // including after restart/submission. Check and insert under one lock.
        // Explicit reports retain their caller-selected event semantics.
        if let Some(detector_name) = detector {
            if let Some(old) = state.records.values().find(|record| {
                record.detector.as_deref() == detector
                    && record.observation.knowledge_request == observation.knowledge_request
                    && record.observation.kind == observation.kind
                    && record.observation.applicability == observation.applicability
                    && record.observation.failure_code == observation.failure_code
                    && record.observation.app_revision == observation.app_revision
                    && record.source == source
                    && record.credential_fingerprint == credential_fingerprint
            }) {
                observation.event_id = old.observation.event_id.clone();
            } else if let Some(old) = read_archived_detector(
                policy,
                &observation,
                &source,
                &credential_fingerprint,
                detector_name,
            )? {
                observation.event_id = old.observation.event_id;
            }
        }
        let archived = read_archived(policy, &observation.event_id)?;
        if let Some(old) = state
            .records
            .get(&observation.event_id)
            .or(archived.as_ref())
        {
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
        let record = PendingRecord {
            observation: observation.clone(),
            source,
            credential_fingerprint,
            submitted: false,
            incident_id: None,
            feedback: Vec::new(),
            detector: detector.map(str::to_owned),
        };
        let mut candidate = state.records.clone();
        candidate.insert(observation.event_id.clone(), record);
        persist_active(policy, &mut candidate)?;
        state.records = candidate;
        Ok(
            json!({"event_id":observation.event_id,"recorded_locally":true,"duplicate":false,"submitted":false,"export_enabled":policy.export_enabled,"durability":if policy.pending_path.is_some(){"private_host_file"}else{"application_lifetime"},"note":"Recording is local. Export requires explicit host policy and the original service/credential; no investigation or provider execution is started."}),
        )
    }
    pub fn report_gap(
        &self,
        topic: String,
        question: String,
        kind: String,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        bearer: Option<&str>,
    ) -> Result<Value, String> {
        let request = KnowledgeRequest { topic, question };
        request.validate()?;
        // Stable identity per source/account/content; no raw topic in public remedy identifiers.
        let identity = serde_json::to_vec(&(
            source(connection)?,
            fingerprint(bearer),
            &request,
            &kind,
            env!("CARGO_PKG_VERSION"),
        ))
        .map_err(|_| "Invalid knowledge request")?;
        let digest = Sha256::digest(identity);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        self.report(
            ReportHarnessIncident {
                event_id: Some(uuid::Uuid::from_bytes(bytes).to_string()),
                kind: ObservationKind::CorpusGap,
                applicability: RemedyKey {
                    gap_kind: Some("missing_knowledge".into()),
                    path_kind: Some(kind),
                    ..Default::default()
                },
                failure_code: "corpus_coverage_missing".into(),
                app_revision: format!("core.{}", env!("CARGO_PKG_VERSION")),
                knowledge_request: Some(request),
            },
            policy,
            connection,
            bearer,
        )
    }

    pub async fn incident_status(
        &self,
        event_id: &str,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        corpus: &AgentCorpusConnection,
    ) -> Result<Value, String> {
        let record = self.record_for_access(event_id, policy, connection, corpus, true)?;
        let Some(id) = record.incident_id else {
            return Ok(
                json!({"event_id":event_id,"status":"local_only","submitted":false,
                "knowledge_export_enabled":policy.knowledge_export_enabled}),
            );
        };
        let auth = AgentCorpusConnection {
            base_url: connection.base_url.clone(),
            bearer_token: corpus.bearer_token.clone(),
            trusted_public_keys: Vec::new(),
        };
        let bytes = agent_skill_cloud::request(&auth, &format!("/incidents/{id}"), None).await?;
        let mut result: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid incident status")?;
        result["event_id"] = json!(event_id);
        Ok(result)
    }

    fn bound_record(
        &self,
        event_id: &str,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        corpus: &AgentCorpusConnection,
    ) -> Result<PendingRecord, String> {
        self.record_for_access(event_id, policy, connection, corpus, false)
    }

    fn record_for_access(
        &self,
        event_id: &str,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        corpus: &AgentCorpusConnection,
        read_submitted_status: bool,
    ) -> Result<PendingRecord, String> {
        uuid::Uuid::parse_str(event_id).map_err(|_| "Invalid event identity")?;
        let state = self.load(policy)?;
        let record = match state.records.get(event_id) {
            Some(record) => record.clone(),
            None => read_archived(policy, event_id)?
                .ok_or("Incident unavailable for this host identity")?,
        };
        // A short-lived credential is not a stable account identity. For an
        // already submitted receipt only, the read must reach the original
        // service, which authorizes the incident against the current account.
        // No private observation is returned or sent by this exception, and
        // export/feedback retain their exact credential binding.
        let remote_status_read = read_submitted_status
            && record.submitted
            && record
                .incident_id
                .as_deref()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            && corpus
                .bearer_token
                .as_deref()
                .is_some_and(|token| !token.is_empty());
        if record.source != source(connection)?
            || (!remote_status_read
                && record.credential_fingerprint != fingerprint(corpus.bearer_token.as_deref()))
        {
            return Err("Incident unavailable for this host identity".into());
        }
        if record.incident_id.is_some() {
            let origin = reqwest::Url::parse(&connection.base_url)
                .map_err(|_| "Invalid incident source")?
                .origin();
            let credential_origin = reqwest::Url::parse(&corpus.base_url)
                .map_err(|_| "Invalid credential source")?
                .origin();
            if origin != credential_origin {
                return Err("Incident unavailable for this host identity".into());
            }
        }
        Ok(record.clone())
    }

    /// Submitted receipts are account-owned on the original service. A renewed
    /// credential must prove that ownership before any queued feedback is sent.
    /// Unsubmitted observations never follow a new credential automatically.
    async fn authorize_feedback(
        &self,
        event_id: &str,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        corpus: &AgentCorpusConnection,
    ) -> Result<PendingRecord, String> {
        if let Ok(record) = self.bound_record(event_id, policy, connection, corpus) {
            return Ok(record);
        }
        if !policy.export_enabled {
            return Err("Host export policy is disabled".into());
        }
        let before = self.record_for_access(event_id, policy, connection, corpus, true)?;
        if !before.submitted {
            return Err("Incident has not been submitted".into());
        }
        let incident = before
            .incident_id
            .as_deref()
            .ok_or("Incident has not been submitted")?;
        let status = self
            .incident_status(event_id, policy, connection, corpus)
            .await?;
        if status["incident_id"].as_str() != Some(incident) {
            return Err("Incident ownership response identity mismatch".into());
        }
        let mut state = self.load(policy)?;
        let mut candidate = state.records.clone();
        let current = active_record(&mut candidate, policy, event_id)?;
        if !current.submitted
            || current.source != before.source
            || current.incident_id != before.incident_id
            || current.credential_fingerprint != before.credential_fingerprint
        {
            return Err("Incident authority changed during feedback authorization".into());
        }
        current.credential_fingerprint = fingerprint(corpus.bearer_token.as_deref());
        let authorized = current.clone();
        persist_active(policy, &mut candidate)?;
        state.records = candidate;
        Ok(authorized)
    }

    pub async fn feedback(
        &self,
        request: RetryOutcome,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        corpus: &AgentCorpusConnection,
    ) -> Result<Value, String> {
        request.validate()?;
        if !policy.export_enabled {
            return Err("Host export policy is disabled".into());
        }
        if !request.residual_knowledge_gaps.is_empty() && !policy.knowledge_export_enabled {
            return Err("Host knowledge export policy is disabled".into());
        }
        let record = self
            .authorize_feedback(&request.event_id, policy, connection, corpus)
            .await?;
        if record.incident_id.is_none() {
            return Err("Incident has not been submitted".into());
        }
        {
            let mut state = self.load(policy)?;
            let mut candidate = state.records.clone();
            let current = active_record(&mut candidate, policy, &request.event_id)?;
            if current.source != record.source
                || current.incident_id != record.incident_id
                || current.credential_fingerprint != fingerprint(corpus.bearer_token.as_deref())
            {
                return Err("Incident authority changed during feedback recording".into());
            }
            if let Some(old) = current
                .feedback
                .iter()
                .find(|v| v.report.feedback_id == request.feedback_id)
            {
                if serde_json::to_value(&old.report).ok() != serde_json::to_value(&request).ok() {
                    return Err("Feedback identity already has another payload".into());
                }
            } else {
                if current.feedback.len() >= 32 {
                    return Err("Private retry feedback capacity reached".into());
                }
                current.feedback.push(PendingFeedback {
                    report: request.clone(),
                    submitted: false,
                });
                persist_active(policy, &mut candidate)?;
                state.records = candidate;
            }
        }
        let delivery = self.submit_feedback(policy, connection, corpus).await?;
        let current = self.bound_record(&request.event_id, policy, connection, corpus)?;
        let submitted = current
            .feedback
            .iter()
            .any(|v| v.report.feedback_id == request.feedback_id && v.submitted);
        Ok(
            json!({"recorded_locally":true,"submitted":submitted,"feedback_id":request.feedback_id,
            "independently_verified":false,"delivery":delivery}),
        )
    }

    pub async fn submit_feedback(
        &self,
        policy: &HarnessIncidentPolicy,
        connection: &CurationConnection,
        corpus: &AgentCorpusConnection,
    ) -> Result<Value, String> {
        if !policy.export_enabled {
            return Ok(json!({"status":"export_disabled","submitted_count":0}));
        }
        let records = self.load(policy)?.records.clone();
        let auth = AgentCorpusConnection {
            base_url: connection.base_url.clone(),
            bearer_token: corpus.bearer_token.clone(),
            trusted_public_keys: Vec::new(),
        };
        let mut submitted = 0;
        for (event_id, record) in records {
            if !record.feedback.iter().any(|f| !f.submitted)
                || self
                    .authorize_feedback(&event_id, policy, connection, corpus)
                    .await
                    .is_err()
            {
                continue;
            }
            let record = self.bound_record(&event_id, policy, connection, corpus)?;
            let Some(ref id) = record.incident_id else {
                continue;
            };
            for pending in record.feedback.iter().filter(|v| !v.submitted) {
                // Revoking prose export also fences previously queued residual questions.
                if !pending.report.residual_knowledge_gaps.is_empty()
                    && !policy.knowledge_export_enabled
                {
                    continue;
                }
                if submitted >= MAX_SUBMIT {
                    return Ok(json!({"status":"batch_complete","submitted_count":submitted}));
                }
                let body = serde_json::to_value(&pending.report).map_err(|_| "Invalid outcome")?;
                let bytes = match agent_skill_cloud::request(
                    &auth,
                    &format!("/incidents/{id}/feedback"),
                    Some(&body),
                )
                .await
                {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return Ok(
                            json!({"status":"delivery_unavailable","submitted_count":submitted}),
                        )
                    }
                };
                let receipt: Value =
                    serde_json::from_slice(&bytes).map_err(|_| "Invalid outcome receipt")?;
                if receipt["recorded"] != true {
                    return Err("Invalid outcome receipt".into());
                }
                let mut state = self.load(policy)?;
                let mut candidate = state.records.clone();
                retain_followup(&mut candidate, &record, &pending.report, &receipt)?;
                if let Some(current) = candidate.get_mut(&event_id).and_then(|r| {
                    r.feedback
                        .iter_mut()
                        .find(|v| v.report.feedback_id == pending.report.feedback_id)
                }) {
                    current.submitted = true;
                }
                persist_active(policy, &mut candidate)?;
                state.records = candidate;
                submitted += 1;
            }
        }
        Ok(json!({"status":"submitted","submitted_count":submitted}))
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
                        && (r.observation.knowledge_request.is_none() || policy.knowledge_export_enabled)
                        && r.source == source
                        && r.credential_fingerprint == credential_fingerprint
                        // Older explicit reports may use the now-reserved
                        // code. Retain those locally for host review; never
                        // export them as an automatic detector observation.
                        && (r.observation.failure_code != REQUIRED_GUIDANCE_FAILURE
                            || r.detector.as_deref() == Some(REQUIRED_GUIDANCE_DETECTOR))
                        && (r.observation.failure_code != "operation_unsuccessful"
                            || r.detector.as_deref() == Some("host.operation_result.v1"))
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
                current.incident_id = receipt["incident_id"].as_str().map(str::to_owned);
            }
            persist_active(policy, &mut candidate)?;
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
        if record.feedback.len() > 32 {
            return Err("Invalid feedback capacity".into());
        }
        for feedback in &record.feedback {
            feedback.report.validate()?;
            if feedback.report.event_id != *id {
                return Err("Invalid feedback identity".into());
            }
        }
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
// Acknowledged history is not pending delivery. Keep it on disk, addressable
// by event id, without loading an unbounded history into the active outbox.
fn detector_archive_key(
    observation: &Observation,
    source: &str,
    credential: &Option<String>,
    detector: &str,
) -> String {
    let bytes = serde_json::to_vec(&(
        detector,
        &observation.kind,
        &observation.applicability,
        &observation.failure_code,
        &observation.app_revision,
        &observation.knowledge_request,
        source,
        credential,
    ))
    .expect("typed detector identity");
    format!("detector-{}", hex::encode(Sha256::digest(bytes)))
}

fn archive_path(policy: &HarnessIncidentPolicy, key: &str) -> Result<Option<PathBuf>, String> {
    if !(uuid::Uuid::parse_str(key).is_ok()
        || key
            .strip_prefix("detector-")
            .is_some_and(|v| v.len() == 64 && v.bytes().all(|c| c.is_ascii_hexdigit())))
    {
        return Err("Invalid private receipt identity".into());
    }
    Ok(policy.pending_path.as_ref().map(|path| {
        let mut directory = path.as_os_str().to_os_string();
        directory.push(".receipts");
        PathBuf::from(directory).join(format!("{key}.json"))
    }))
}

fn read_archive_key(
    policy: &HarnessIncidentPolicy,
    key: &str,
) -> Result<Option<PendingRecord>, String> {
    let Some(path) = archive_path(policy, key)? else {
        return Ok(None);
    };
    let records = read_pending(&path)?;
    if records.is_empty() {
        return Ok(None);
    }
    if records.len() != 1 {
        return Err("Invalid private receipt archive".into());
    }
    let (_, record) = records.into_iter().next().unwrap();
    if !record.submitted
        || record
            .incident_id
            .as_deref()
            .is_none_or(|id| uuid::Uuid::parse_str(id).is_err())
        || record.feedback.iter().any(|f| !f.submitted)
    {
        return Err("Unacknowledged record in receipt archive".into());
    }
    Ok(Some(record))
}

fn read_archived(
    policy: &HarnessIncidentPolicy,
    event_id: &str,
) -> Result<Option<PendingRecord>, String> {
    let record = read_archive_key(policy, event_id)?;
    if record
        .as_ref()
        .is_some_and(|r| r.observation.event_id != event_id)
    {
        return Err("Private receipt identity mismatch".into());
    }
    Ok(record)
}

fn read_archived_detector(
    policy: &HarnessIncidentPolicy,
    observation: &Observation,
    source: &str,
    credential: &Option<String>,
    detector: &str,
) -> Result<Option<PendingRecord>, String> {
    let key = detector_archive_key(observation, source, credential, detector);
    let record = read_archive_key(policy, &key)?;
    if record.as_ref().is_some_and(|r| {
        r.detector.as_deref() != Some(detector)
            || detector_archive_key(
                &r.observation,
                &r.source,
                &r.credential_fingerprint,
                detector,
            ) != key
    }) {
        return Err("Private detector receipt identity mismatch".into());
    }
    Ok(record)
}

#[cfg(not(target_arch = "wasm32"))]
fn archive_record(policy: &HarnessIncidentPolicy, record: &PendingRecord) -> Result<(), String> {
    let path = archive_path(policy, &record.observation.event_id)?
        .ok_or("Persistent receipt storage required")?;
    let directory = path.parent().ok_or("Private receipt directory required")?;
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(directory) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if !std::fs::symlink_metadata(directory)
                .map_err(|_| "Private receipt directory unavailable")?
                .is_dir()
            {
                return Err("Invalid private receipt directory".into());
            }
        }
        Err(_) => return Err("Private receipt directory unavailable".into()),
    }
    let records = BTreeMap::from([(record.observation.event_id.clone(), record.clone())]);
    let bytes = serde_json::to_vec(&PendingFile {
        schema: PENDING_SCHEMA.into(),
        records,
    })
    .map_err(|_| "Private receipt serialization failed")?;
    write_pending(&path, &bytes)?;
    if let Some(detector) = &record.detector {
        let key = detector_archive_key(
            &record.observation,
            &record.source,
            &record.credential_fingerprint,
            detector,
        );
        write_pending(&archive_path(policy, &key)?.unwrap(), &bytes)?;
    }
    Ok(())
}
#[cfg(target_arch = "wasm32")]
fn archive_record(_: &HarnessIncidentPolicy, _: &PendingRecord) -> Result<(), String> {
    Err("Persistent private receipt storage needs a host adapter".into())
}

fn active_record<'a>(
    records: &'a mut BTreeMap<String, PendingRecord>,
    policy: &HarnessIncidentPolicy,
    event_id: &str,
) -> Result<&'a mut PendingRecord, String> {
    if !records.contains_key(event_id) {
        // Read current durable state after any await, never restore a stale
        // pre-authorization snapshot over more recent feedback.
        let current = read_archived(policy, event_id)?.ok_or("Incident unavailable")?;
        records.insert(event_id.to_owned(), current);
    }
    records
        .get_mut(event_id)
        .ok_or_else(|| "Incident unavailable".into())
}

fn persist_active(
    policy: &HarnessIncidentPolicy,
    records: &mut BTreeMap<String, PendingRecord>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&PendingFile {
        schema: PENDING_SCHEMA.into(),
        records: records.clone(),
    })
    .map_err(|_| "Private incident serialization failed")?;
    if records.len() > MAX_PENDING || bytes.len() > MAX_BYTES {
        if policy.pending_path.is_some() {
            let acknowledged: Vec<_> = records
                .values()
                .filter(|r| {
                    r.submitted
                        && r.incident_id
                            .as_deref()
                            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                        && r.feedback.iter().all(|f| f.submitted)
                })
                .cloned()
                .collect();
            for record in acknowledged {
                // Archive and fsync first. A failed active-file replace leaves
                // harmless duplicate history, never erased pending delivery.
                archive_record(policy, &record)?;
                records.remove(&record.observation.event_id);
            }
        }
        if records.len() > MAX_PENDING {
            return Err(
                "Private pending incident capacity reached; undelivered reports remain retained"
                    .into(),
            );
        }
    }
    persist(policy, records)
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
    #[test]
    fn invalid_retry_outcome_names_every_violated_field() {
        let report = super::RetryOutcome {
            event_id: uuid::Uuid::new_v4().to_string(),
            feedback_id: "fb-retry-1".into(),
            artifact_id: "knowledge:abc".into(),
            revision: "a".repeat(64),
            outcome: "failed".into(),
            evidence_codes: vec!["penetration_1.2e-7m".into(), "bad code".into()],
            residual_knowledge_gaps: vec![],
        };
        let error = report.validate().unwrap_err();
        assert!(
            error.contains("feedback_id must be a fresh UUID"),
            "{error}"
        );
        assert!(error.contains("each evidence code"), "{error}");
        assert!(!error.contains("event_id"), "{error}");
        let valid = super::RetryOutcome {
            feedback_id: uuid::Uuid::new_v4().to_string(),
            evidence_codes: vec!["promotion_blocked".into()],
            ..report
        };
        assert!(valid.validate().is_ok());
    }

    #[test]
    fn invalid_incident_field_is_named_so_specific_fields_survive() {
        let observation = |app_revision: &str| super::Observation {
            schema: super::OBSERVATION_SCHEMA.into(),
            event_id: uuid::Uuid::new_v4().to_string(),
            kind: super::ObservationKind::CorpusGap,
            applicability: crate::plugins::curation::RemedyKey {
                element_class: Some("roof_system".into()),
                capability_id: Some("fink_truss_prefabricated".into()),
                ..Default::default()
            },
            failure_code: "no_executable_path".into(),
            app_revision: app_revision.into(),
            export_authorized: false,
            knowledge_request: None,
        };
        let error = observation("architecture.component_structure@83")
            .validate_local()
            .unwrap_err();
        assert!(
            error.contains("'app_revision'") && error.contains("'@'"),
            "{error}"
        );
        observation("architecture.component_structure.83")
            .validate_local()
            .unwrap();
    }

    #[cfg(feature = "model-api")]
    #[tokio::test]
    async fn submitted_status_uses_server_account_authority_after_token_renewal() {
        use axum::{
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
            routing::get,
            Router,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let app = Router::new().route(
            "/v1/curation/incidents/{id}",
            get(move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    if headers.get("authorization").and_then(|v| v.to_str().ok())
                        != Some("Bearer renewed-same-account")
                    {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    axum::Json(json!({"state":"queued","released_remedies":[]})).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let connection = CurationConnection {
            base_url: format!("http://{address}/v1/curation"),
            ..Default::default()
        };
        let original = AgentCorpusConnection {
            base_url: format!("http://{address}/v1/corpus"),
            bearer_token: Some("original".into()),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let policy = HarnessIncidentPolicy {
            pending_path: Some(dir.path().join("pending.json")),
            ..Default::default()
        };
        let store = HarnessIncidentStore::default();
        let receipt = store
            .report_gap(
                "roof_system".into(),
                "Executable path?".into(),
                "parametric".into(),
                &policy,
                &connection,
                original.bearer_token.as_deref(),
            )
            .unwrap();
        let event = receipt["event_id"].as_str().unwrap();
        let renewed = AgentCorpusConnection {
            bearer_token: Some("renewed-same-account".into()),
            ..original.clone()
        };
        // An unsubmitted private observation never crosses the new identity.
        assert!(store
            .incident_status(event, &policy, &connection, &renewed)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        {
            let mut state = store.load(&policy).unwrap();
            let record = state.records.get_mut(event).unwrap();
            record.submitted = true;
            record.incident_id = Some(uuid::Uuid::new_v4().to_string());
            archive_record(&policy, record).unwrap();
            state.records.remove(event);
            persist(&policy, &state.records).unwrap();
        }
        let status = store
            .incident_status(event, &policy, &connection, &renewed)
            .await
            .unwrap();
        assert_eq!(status["state"], "queued");
        assert!(status.get("observation").is_none());
        // Reading does not rebind export or feedback ownership.
        assert!(store
            .bound_record(event, &policy, &connection, &renewed)
            .is_err());
        assert!(store
            .bound_record(event, &policy, &connection, &original)
            .is_ok());
        let other_account = AgentCorpusConnection {
            bearer_token: Some("other-account".into()),
            ..original.clone()
        };
        assert!(store
            .incident_status(event, &policy, &connection, &other_account)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let wrong_origin = AgentCorpusConnection {
            base_url: "http://127.0.0.1:1/v1/corpus".into(),
            ..renewed.clone()
        };
        assert!(store
            .incident_status(event, &policy, &connection, &wrong_origin)
            .await
            .is_err());
        let changed_source = CurationConnection {
            base_url: format!("http://{address}/another-session"),
            ..Default::default()
        };
        assert!(store
            .incident_status(event, &policy, &changed_source, &renewed)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[cfg(feature = "model-api")]
    #[tokio::test]
    async fn renewed_feedback_requires_server_ownership_before_any_private_write_or_send() {
        use axum::{
            extract::Path as AxumPath,
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
            routing::{get, post},
            Router,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let seen_reads = reads.clone();
        let seen_posts = posts.clone();
        let app = Router::new()
            .route(
                "/v1/curation/incidents/{id}",
                get(move |AxumPath(id): AxumPath<String>, headers: HeaderMap| {
                    let reads = seen_reads.clone();
                    async move {
                        reads.fetch_add(1, Ordering::SeqCst);
                        match headers.get("authorization").and_then(|h| h.to_str().ok()) {
                            Some("Bearer renewed-owner") => {
                                axum::Json(json!({"incident_id":id,"state":"released"}))
                                    .into_response()
                            }
                            Some("Bearer wrong-receipt") => {
                                axum::Json(json!({"incident_id":uuid::Uuid::new_v4().to_string()}))
                                    .into_response()
                            }
                            _ => StatusCode::NOT_FOUND.into_response(),
                        }
                    }
                }),
            )
            .route(
                "/v1/curation/incidents/{id}/feedback",
                post(move |headers: HeaderMap| {
                    let posts = seen_posts.clone();
                    async move {
                        posts.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            headers.get("authorization").unwrap(),
                            "Bearer renewed-owner"
                        );
                        axum::Json(json!({"recorded":true}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let policy = HarnessIncidentPolicy {
            export_enabled: true,
            knowledge_export_enabled: true,
            pending_path: Some(dir.path().join("pending.json")),
        };
        let connection = CurationConnection {
            base_url: format!("http://{address}/v1/curation"),
            ..Default::default()
        };
        let original = AgentCorpusConnection {
            base_url: format!("http://{address}/v1/corpus"),
            bearer_token: Some("old-owner".into()),
            ..Default::default()
        };
        let store = HarnessIncidentStore::default();
        let receipt = store
            .report_gap(
                "unit-topic".into(),
                "unit-question".into(),
                "passage".into(),
                &policy,
                &connection,
                original.bearer_token.as_deref(),
            )
            .unwrap();
        let event = receipt["event_id"].as_str().unwrap();
        let renewed = AgentCorpusConnection {
            bearer_token: Some("renewed-owner".into()),
            ..original.clone()
        };
        // An unsubmitted private observation cannot be rebound or exported.
        assert!(store
            .authorize_feedback(event, &policy, &connection, &renewed)
            .await
            .is_err());
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        {
            let mut state = store.load(&policy).unwrap();
            let row = state.records.get_mut(event).unwrap();
            row.submitted = true;
            row.incident_id = Some(uuid::Uuid::new_v4().to_string());
            archive_record(&policy, row).unwrap();
            state.records.remove(event);
            persist(&policy, &state.records).unwrap();
        }
        let request = RetryOutcome {
            event_id: event.into(),
            feedback_id: uuid::Uuid::new_v4().to_string(),
            artifact_id: "skill:unit".into(),
            revision: "a".repeat(64),
            outcome: "succeeded".into(),
            evidence_codes: vec!["unit.retry".into()],
            residual_knowledge_gaps: vec![],
        };
        for token in ["other-account", "wrong-receipt"] {
            let unauthorized = AgentCorpusConnection {
                bearer_token: Some(token.into()),
                ..original.clone()
            };
            assert!(store
                .feedback(request.clone(), &policy, &connection, &unauthorized)
                .await
                .is_err());
            assert!(store
                .bound_record(event, &policy, &connection, &original)
                .unwrap()
                .feedback
                .is_empty());
        }
        assert_eq!(posts.load(Ordering::SeqCst), 0);
        let wrong_origin = AgentCorpusConnection {
            base_url: "http://127.0.0.1:1/v1/corpus".into(),
            ..renewed.clone()
        };
        assert!(store
            .feedback(request.clone(), &policy, &connection, &wrong_origin)
            .await
            .is_err());
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let result = store
            .feedback(request.clone(), &policy, &connection, &renewed)
            .await
            .unwrap();
        assert_eq!(result["submitted"], true);
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        let restarted = HarnessIncidentStore::default();
        let record = restarted
            .bound_record(event, &policy, &connection, &renewed)
            .unwrap();
        assert!(record.feedback[0].submitted);
        assert!(restarted
            .bound_record(event, &policy, &connection, &original)
            .is_err());
        assert_eq!(
            restarted
                .feedback(request, &policy, &connection, &renewed)
                .await
                .unwrap()["submitted"],
            true
        );
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn knowledge_opt_in_and_retry_outbox_survive_restart_without_rebinding() {
        let dir = tempfile::tempdir().unwrap();
        let policy = HarnessIncidentPolicy {
            export_enabled: true,
            knowledge_export_enabled: false,
            pending_path: Some(dir.path().join("pending.json")),
        };
        let connection = CurationConnection {
            base_url: "http://127.0.0.1:1/v1/curation".into(),
            trusted_public_keys: vec![],
        };
        let corpus = AgentCorpusConnection {
            base_url: "http://127.0.0.1:1/v1/corpus".into(),
            bearer_token: Some("synthetic".into()),
            trusted_public_keys: vec![],
        };
        let store = HarnessIncidentStore::default();
        let gap = || {
            store
                .report_gap(
                    "attic trusses".into(),
                    "What are they and how are they designed?".into(),
                    "passage".into(),
                    &policy,
                    &connection,
                    corpus.bearer_token.as_deref(),
                )
                .unwrap()
        };
        let first = gap();
        let event = first["event_id"].as_str().unwrap();
        assert_eq!(gap()["duplicate"], true);
        assert_eq!(
            store.submit(&policy, &connection, &corpus).await.unwrap()["status"],
            "no_eligible_pending_reports"
        );
        // Simulate a previously acknowledged intake; the offline outcome must be retained.
        {
            let mut state = store.load(&policy).unwrap();
            let row = state.records.get_mut(event).unwrap();
            row.submitted = true;
            row.incident_id = Some(uuid::Uuid::new_v4().to_string());
            persist(&policy, &state.records).unwrap();
        }
        let report = RetryOutcome {
            event_id: event.into(),
            feedback_id: uuid::Uuid::new_v4().to_string(),
            artifact_id: "skill:architecture.attic".into(),
            revision: "a".repeat(64),
            outcome: "inconclusive".into(),
            evidence_codes: vec!["lookup.unavailable".into()],
            residual_knowledge_gaps: vec![],
        };
        let receipt = store
            .feedback(report.clone(), &policy, &connection, &corpus)
            .await
            .unwrap();
        assert_eq!(receipt["recorded_locally"], true);
        assert_eq!(receipt["submitted"], false);
        let restarted = HarnessIncidentStore::default();
        assert_eq!(
            restarted
                .bound_record(event, &policy, &connection, &corpus)
                .unwrap()
                .feedback
                .len(),
            1
        );
        let parent = restarted
            .bound_record(event, &policy, &connection, &corpus)
            .unwrap();
        let mut records = BTreeMap::new();
        let continuation = json!({"followup":{"incident_id":uuid::Uuid::new_v4().to_string()}});
        retain_followup(&mut records, &parent, &report, &continuation).unwrap();
        retain_followup(&mut records, &parent, &report, &continuation).unwrap();
        assert_eq!(records.len(), 1);
        let child = &records[&report.feedback_id];
        assert!(child.submitted && child.feedback.is_empty() && child.detector.is_none());
        assert_eq!(child.credential_fingerprint, parent.credential_fingerprint);
        let changed_receipt = json!({"followup":{"incident_id":uuid::Uuid::new_v4().to_string()}});
        assert!(retain_followup(&mut records, &parent, &report, &changed_receipt).is_err());
        let mut residual = report.clone();
        residual.feedback_id = uuid::Uuid::new_v4().to_string();
        residual.residual_knowledge_gaps = vec![KnowledgeRequest {
            topic: "remaining reference".into(),
            question: "What storage guidance is recorded?".into(),
        }];
        assert!(store
            .feedback(residual.clone(), &policy, &connection, &corpus)
            .await
            .unwrap_err()
            .contains("knowledge export"));
        let mut allowed = policy.clone();
        allowed.knowledge_export_enabled = true;
        assert_eq!(
            store
                .feedback(residual.clone(), &allowed, &connection, &corpus)
                .await
                .unwrap()["submitted"],
            false
        );
        // Revoked knowledge permission prevents a later outbox flush.
        assert_eq!(
            store
                .submit_feedback(&policy, &connection, &corpus)
                .await
                .unwrap()["submitted_count"],
            0
        );
        residual.outcome = "succeeded".into();
        assert!(residual.validate().is_err());
        residual.outcome = "inconclusive".into();
        residual.residual_knowledge_gaps[0].question = "x".repeat(2049);
        assert!(residual.validate().is_err());
        let mut changed = report;
        changed.outcome = "succeeded".into();
        assert!(restarted
            .feedback(changed, &policy, &connection, &corpus)
            .await
            .is_err());
        let rotated = AgentCorpusConnection {
            bearer_token: Some("rotated".into()),
            ..corpus
        };
        assert!(restarted
            .bound_record(event, &policy, &connection, &rotated)
            .is_err());
    }

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
    #[tokio::test]
    async fn explicit_reports_cannot_claim_or_preseed_host_detection() {
        let store = HarnessIncidentStore::default();
        let policy = HarnessIncidentPolicy {
            export_enabled: true,
            ..Default::default()
        };
        let connection = CurationConnection {
            base_url: "https://example.test/v1/curation".into(),
            ..Default::default()
        };
        let mut spoof = request(None);
        spoof.failure_code = REQUIRED_GUIDANCE_FAILURE.into();
        assert!(store
            .report(spoof, &policy, &connection, Some("original"))
            .unwrap_err()
            .contains("reserved"));
        let first = store
            .report_missing_required_guidance(&policy, &connection, Some("original"))
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
            .report_missing_required_guidance(&policy, &connection, Some("original"))
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
            .report_missing_required_guidance(&policy, &connection, Some("original"))
            .unwrap();
        assert_eq!(duplicate["event_id"], second["event_id"]);
        assert_eq!(duplicate["submitted"], true);
        // The only unsubmitted record is the unmarked legacy spoof. Selecting
        // nothing must succeed without contacting this deliberately unavailable
        // service, even though host export and matching identity are present.
        let corpus = AgentCorpusConnection {
            base_url: "https://example.test/v1/corpus".into(),
            bearer_token: Some("original".into()),
            ..Default::default()
        };
        let submit = store.submit(&policy, &connection, &corpus).await.unwrap();
        assert_eq!(submit["status"], "no_eligible_pending_reports");
        assert_eq!(store.load(&policy).unwrap().records.len(), 2);
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn acknowledged_receipts_leave_active_capacity_without_losing_identity_or_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let policy = HarnessIncidentPolicy {
            pending_path: Some(dir.path().join("pending.json")),
            export_enabled: true,
            ..Default::default()
        };
        let store = HarnessIncidentStore::default();
        let connection = connection();
        let corpus = AgentCorpusConnection {
            base_url: "http://127.0.0.1:1/v1/corpus".into(),
            bearer_token: Some("original".into()),
            ..Default::default()
        };
        let automatic = store
            .report_missing_required_guidance(&policy, &connection, Some("original"))
            .unwrap();
        let automatic_id = automatic["event_id"].as_str().unwrap().to_owned();
        let mut acknowledged = Vec::new();
        let mut pending = Vec::new();
        {
            let mut state = store.load(&policy).unwrap();
            for n in 0..MAX_PENDING {
                let id = if n == 0 {
                    automatic_id.clone()
                } else {
                    uuid::Uuid::new_v4().to_string()
                };
                let mut record = state.records[&automatic_id].clone();
                record.observation.event_id = id.clone();
                if n != 0 {
                    record.detector = None;
                    record.observation.failure_code = "no_curated_path".into();
                }
                record.submitted = n < MAX_PENDING - 2;
                record.incident_id = record.submitted.then(|| uuid::Uuid::new_v4().to_string());
                if record.submitted {
                    acknowledged.push(id.clone());
                } else {
                    pending.push(id.clone());
                }
                state.records.insert(id, record);
            }
            persist(&policy, &state.records).unwrap();
        }
        let fresh = store
            .report(request(None), &policy, &connection, Some("original"))
            .unwrap();
        assert_eq!(store.load(&policy).unwrap().records.len(), 3);
        let restarted = HarnessIncidentStore::default();
        for id in &pending {
            assert!(
                !restarted
                    .bound_record(id, &policy, &connection, &corpus)
                    .unwrap()
                    .submitted
            );
        }
        for id in &acknowledged {
            assert!(
                restarted
                    .bound_record(id, &policy, &connection, &corpus)
                    .unwrap()
                    .submitted
            );
        }
        assert_eq!(
            restarted
                .report_missing_required_guidance(&policy, &connection, Some("original"))
                .unwrap()["event_id"],
            automatic["event_id"]
        );
        assert_eq!(restarted.load(&policy).unwrap().records.len(), 3);
        let mut changed = corpus.clone();
        changed.bearer_token = Some("other".into());
        assert!(restarted
            .bound_record(&automatic_id, &policy, &connection, &changed)
            .is_err());
        // Rehydrated pending feedback may not be archived, even though the
        // original observation has already been acknowledged by the server.
        let mut record = restarted
            .bound_record(&acknowledged[1], &policy, &connection, &corpus)
            .unwrap();
        record.feedback.push(PendingFeedback {
            submitted: false,
            report: RetryOutcome {
                event_id: acknowledged[1].clone(),
                feedback_id: uuid::Uuid::new_v4().to_string(),
                artifact_id: "synthetic".into(),
                revision: "a".repeat(64),
                outcome: "failed".into(),
                evidence_codes: vec!["synthetic".into()],
                residual_knowledge_gaps: vec![],
            },
        });
        let mut records = restarted.load(&policy).unwrap().records.clone();
        records.insert(acknowledged[1].clone(), record);
        for n in 0..MAX_PENDING {
            let mut old = restarted
                .bound_record(&automatic_id, &policy, &connection, &corpus)
                .unwrap();
            old.observation.event_id = uuid::Uuid::new_v4().to_string();
            old.detector = None;
            old.observation.failure_code = format!("synthetic_{n}");
            records.insert(old.observation.event_id.clone(), old);
        }
        persist_active(&policy, &mut records).unwrap();
        assert_eq!(records.len(), 4);
        assert!(!records[&acknowledged[1]].feedback[0].submitted);
        assert!(records.contains_key(fresh["event_id"].as_str().unwrap()));
    }

    #[test]
    fn archival_failure_preserves_original_active_file() {
        let dir = tempfile::tempdir().unwrap();
        let policy = HarnessIncidentPolicy {
            pending_path: Some(dir.path().join("pending.json")),
            ..Default::default()
        };
        let store = HarnessIncidentStore::default();
        let event = store
            .report(request(None), &policy, &connection(), Some("original"))
            .unwrap();
        let mut records = store.load(&policy).unwrap().records.clone();
        let mut acknowledged = records[event["event_id"].as_str().unwrap()].clone();
        acknowledged.submitted = true;
        acknowledged.incident_id = Some(uuid::Uuid::new_v4().to_string());
        for _ in 0..MAX_PENDING {
            acknowledged.observation.event_id = uuid::Uuid::new_v4().to_string();
            records.insert(
                acknowledged.observation.event_id.clone(),
                acknowledged.clone(),
            );
        }
        let before = std::fs::read(policy.pending_path.as_ref().unwrap()).unwrap();
        std::fs::write(
            policy
                .pending_path
                .as_ref()
                .unwrap()
                .with_file_name("pending.json.receipts"),
            b"not a directory",
        )
        .unwrap();
        assert!(persist_active(&policy, &mut records).is_err());
        assert_eq!(
            std::fs::read(policy.pending_path.as_ref().unwrap()).unwrap(),
            before
        );
    }

    fn request(id: Option<String>) -> ReportHarnessIncident {
        ReportHarnessIncident {
            knowledge_request: None,
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
