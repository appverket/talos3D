//! Provider-neutral, public release reconciliation. No private incidents or
//! project intent belong in this manifest. Hosts opt in by supplying a connection.
//! Network work is async; command and presentation paths only read retained state.
use super::{agent_skill_cloud, agent_skills::AgentCorpusConnection};
use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

#[cfg(all(feature = "model-api", not(target_arch = "wasm32")))]
mod native;
#[cfg(all(feature = "model-api", not(target_arch = "wasm32")))]
pub use native::CurationSubscriber;

pub mod reporting;

pub const RELEASE_SCHEMA: &str = "talos3d.curation-release.v1";
const MAX_ARTIFACTS: usize = 1000;
const MAX_TOMBSTONES: usize = 4096;
const MAX_NOTICES: usize = 16;
const MAX_NOTICE_BYTES: usize = 16 * 1024;

/// Explicit host configuration. An empty URL disables network reconciliation.
/// This public feed never receives the signed-in user's credential.
#[derive(Clone, Default, Resource)]
pub struct CurationConnection {
    /// Exact service base, e.g. `https://example/v1/curation`.
    pub base_url: String,
    pub trusted_public_keys: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RemedyKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_revision: Option<String>,
}
impl RemedyKey {
    fn fields(&self) -> [&Option<String>; 6] {
        self.named_fields().map(|(_, value)| value)
    }
    pub(crate) fn named_fields(&self) -> [(&'static str, &Option<String>); 6] {
        [
            ("gap_kind", &self.gap_kind),
            ("element_class", &self.element_class),
            ("path_kind", &self.path_kind),
            ("target_state", &self.target_state),
            ("capability_id", &self.capability_id),
            ("contract_revision", &self.contract_revision),
        ]
    }
    /// A release's declared constraints must all occur in the local observation.
    /// Missing local data is not evidence that a constrained remedy applies.
    pub fn matches(&self, observation: &Self) -> bool {
        self.fields()
            .iter()
            .zip(observation.fields())
            .all(|(required, observed)| {
                required
                    .as_ref()
                    .is_none_or(|value| observed.as_ref() == Some(value))
            })
    }
    fn validate(&self) -> Result<(), String> {
        if self.fields().iter().all(|v| v.is_none()) {
            return Err("Remedy applicability must name at least one public field".into());
        }
        for field in self.fields().into_iter().flatten() {
            token(field)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CurationStatusRequest {
    #[serde(default)]
    pub observation: Option<RemedyKey>,
    /// Narrow to one public artifact identity when the bounded notice list is truncated.
    #[serde(default)]
    pub artifact_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseArtifact {
    /// Exact locally installed executable artifact required. Older clients reject
    /// this explicit field instead of advertising compatibility from tool names.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub runtime_required: bool,
    pub artifact_id: String,
    pub revision: String,
    #[serde(default)]
    pub remedy_keys: Vec<RemedyKey>,
    #[serde(default)]
    pub required_tool_ids: Vec<String>,
    pub withdrawn: bool,
    pub refresh_required: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: String,
    pub sequence: u64,
    pub releases: Vec<ReleaseArtifact>,
}
fn token(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
    {
        return Err("Release identifiers must be bounded public tokens".into());
    }
    Ok(())
}
impl ReleaseManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RELEASE_SCHEMA || self.releases.len() > MAX_ARTIFACTS {
            return Err("Unsupported or oversized curation release manifest".into());
        }
        let mut revisions = BTreeSet::new();
        let mut active_ids = BTreeSet::new();
        for release in &self.releases {
            if release.artifact_id.starts_with("app:") != release.runtime_required {
                return Err("Executable releases require exact installed runtime evidence".into());
            }
            token(&release.artifact_id)?;
            token(&release.revision)?;
            if !release.artifact_id.contains(':')
                || !revisions.insert((&release.artifact_id, &release.revision))
                || (!release.withdrawn && !active_ids.insert(&release.artifact_id))
            {
                return Err("Release identities must be namespaced, revisions unique, and at most one active revision per artifact".into());
            }
            if release.remedy_keys.len() > 64 || release.required_tool_ids.len() > 64 {
                return Err("Release applicability exceeds bounds".into());
            }
            for key in &release.remedy_keys {
                key.validate()?;
            }
            for tool in &release.required_tool_ids {
                token(tool)?;
            }
            if release.refresh_required && !release.withdrawn {
                return Err("Refresh-required is reserved for withdrawn artifacts".into());
            }
        }
        Ok(())
    }
}

pub fn decode_manifest(bytes: &[u8], trusted_keys: &[String]) -> Result<ReleaseManifest, String> {
    if bytes.len() > agent_skill_cloud::MAX_BYTES {
        return Err("Release manifest too large".into());
    }
    let mut value = agent_skill_cloud::verify_signed_json(bytes, trusted_keys)?;
    value
        .as_object_mut()
        .ok_or("Invalid release envelope")?
        .remove("public_key");
    let manifest: ReleaseManifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
    manifest.validate()?;
    Ok(manifest)
}
async fn fetch_envelope(connection: &CurationConnection) -> Result<Vec<u8>, String> {
    let public_connection = AgentCorpusConnection {
        base_url: connection.base_url.clone(),
        bearer_token: None,
        trusted_public_keys: connection.trusted_public_keys.clone(),
    };
    agent_skill_cloud::request(&public_connection, "/releases", None).await
}
pub async fn fetch_manifest(connection: &CurationConnection) -> Result<ReleaseManifest, String> {
    decode_manifest(
        &fetch_envelope(connection).await?,
        &connection.trusted_public_keys,
    )
}

#[derive(Default)]
struct ReleaseState {
    installed_apps: std::collections::BTreeMap<String, String>,
    authority: String,
    manifest: Option<ReleaseManifest>,
    // A later manifest can replace an artifact, but never forget an already
    // withdrawn revision. No global sequence is used as an edit-plan revision.
    tombstones: BTreeSet<(String, String)>,
    // Only successfully issued host pins survive ordinary manifest replacement.
    // Bounded process-local ledger; restart requires rebind to current releases.
    verified_revisions: BTreeSet<(String, String)>,
    fresh: bool,
    error: Option<String>,
    overflow: bool,
    conflicting_sequence: Option<u64>,
    knowledge_authority: String,
    knowledge_epoch: Option<i64>,
    skill_authority: String,
    skill_epoch: Option<i64>,
    skill_tombstones: BTreeSet<String>,
    skill_overflow: bool,
    service_events_connected: bool,
    cache_restored: bool,
    cached_release_sequence: Option<u64>,
    cache_error: Option<String>,
}
/// Application-lifetime state shared across MCP sessions. It is deliberately
/// not project state and does not mutate authored models. Hosts must persist
/// trusted release evidence separately to retain it across process restarts.
#[derive(Clone, Default, Resource)]
pub struct CurationReleaseStore(Arc<Mutex<ReleaseState>>);

fn authority(base: &str, keys: &[String]) -> String {
    let mut keys = keys.to_vec();
    keys.sort();
    keys.dedup();
    format!("{}|{}", base.trim_end_matches('/'), keys.join(","))
}
// Source-scoped failures apply to all declarations; unrelated active release
// churn does not. Only successful host pins consume the historical pin ledger.
fn dependency_state_ready(state: &ReleaseState) -> Result<(), String> {
    if state.manifest.is_none() {
        return Err(
            "Harness dependency verification unavailable; no authenticated release state".into(),
        );
    }
    if state.authority.split('|').next().is_none_or(str::is_empty) {
        return Err("Harness dependency verifier is not bound to a host source".into());
    }
    if state.overflow || state.conflicting_sequence.is_some() {
        return Err("Harness dependency verification unavailable: withdrawal evidence is exhausted or conflicting".into());
    }
    Ok(())
}
impl CurationReleaseStore {
    /// Host composition only: network manifests never install executable code.
    pub fn register_installed_app(&self, id: String, revision: String) {
        self.0
            .lock()
            .expect("curation state poisoned")
            .installed_apps
            .insert(id, revision);
    }
    pub fn installed_apps(&self) -> std::collections::BTreeMap<String, String> {
        self.0
            .lock()
            .expect("curation state poisoned")
            .installed_apps
            .clone()
    }
    pub fn bind(&self, connection: &CurationConnection) {
        let mut state = self.0.lock().expect("curation state poisoned");
        let key = authority(&connection.base_url, &connection.trusted_public_keys);
        if state.authority != key {
            state.authority = key;
            state.manifest = None;
            state.tombstones.clear();
            state.verified_revisions.clear();
            state.fresh = false;
            state.error = None;
            state.overflow = false;
            state.conflicting_sequence = None;
            state.service_events_connected = false;
            state.cache_restored = false;
            state.cached_release_sequence = None;
            state.cache_error = None;
        }
    }
    /// Accept a manifest already verified by the host. Prefer `reconcile` for
    /// network data, which also fences a host connection change during the fetch.
    pub fn accept(&self, manifest: ReleaseManifest) -> Result<(), String> {
        self.accept_expected(manifest, None)
    }
    fn accept_expected(
        &self,
        manifest: ReleaseManifest,
        expected: Option<&str>,
    ) -> Result<(), String> {
        manifest.validate()?;
        let mut state = self.0.lock().expect("curation state poisoned");
        if expected.is_some_and(|key| key != state.authority) {
            return Err("Curation connection changed while refreshing".into());
        }
        if state
            .conflicting_sequence
            .is_some_and(|sequence| manifest.sequence <= sequence)
        {
            return Err(
                "Conflicting curation release sequence requires a newer signed release".into(),
            );
        }
        if let Some(old) = &state.manifest {
            if manifest.sequence < old.sequence {
                return Err("Curation release rollback refused".into());
            }
            if manifest.sequence == old.sequence && &manifest != old {
                return Err("Conflicting curation release sequence".into());
            }
        }
        let mut tombstones = state.tombstones.clone();
        for r in &manifest.releases {
            if r.withdrawn {
                tombstones.insert((r.artifact_id.clone(), r.revision.clone()));
            }
        }
        if tombstones.len() > MAX_TOMBSTONES {
            state.overflow = true;
            state.fresh = false;
            return Err("Curation withdrawal cache full; fallback is disabled".into());
        }
        state.tombstones = tombstones;
        state.manifest = Some(manifest);
        state.conflicting_sequence = None;
        state.fresh = true;
        state.error = None;
        Ok(())
    }
    pub async fn reconcile(&self, connection: &CurationConnection) -> Result<(), String> {
        self.bind(connection);
        let expected = authority(&connection.base_url, &connection.trusted_public_keys);
        let result = fetch_manifest(connection)
            .await
            .and_then(|manifest| self.accept_expected(manifest, Some(&expected)));
        if let Err(error) = &result {
            let mut state = self.0.lock().expect("curation state poisoned");
            if state.authority == expected {
                state.fresh = false;
                state.error = Some(error.clone());
            }
        }
        result
    }
    pub fn unavailable(&self, error: String) {
        let mut state = self.0.lock().expect("curation state poisoned");
        state.fresh = false;
        state.error = Some(error);
    }
    pub fn observe_knowledge_index(
        &self,
        connection: &super::agent_skills::AgentCorpusConnection,
        epoch: i64,
        withdrawn: &[String],
    ) -> Result<(), String> {
        let mut state = self.0.lock().expect("curation state poisoned");
        let key = authority(&connection.base_url, &connection.trusted_public_keys);
        if state.knowledge_authority != key {
            state.knowledge_authority = key;
            state.knowledge_epoch = None;
        }
        if state.knowledge_epoch.is_some_and(|old| epoch < old) {
            return Err("Knowledge index rollback refused".into());
        }
        if withdrawn.len() > MAX_TOMBSTONES {
            return Err("Knowledge withdrawal index exceeds bound".into());
        }
        state.knowledge_epoch = Some(epoch);
        Ok(())
    }

    pub fn observe_skill_index(
        &self,
        connection: &AgentCorpusConnection,
        index: &agent_skill_cloud::SkillIndex,
    ) -> Result<(), String> {
        let mut state = self.0.lock().expect("curation state poisoned");
        let key = authority(&connection.base_url, &connection.trusted_public_keys);
        if state.skill_authority != key {
            state.skill_authority = key;
            state.skill_epoch = None;
            state.skill_tombstones.clear();
            state.skill_overflow = false;
        }
        if state.skill_epoch.is_some_and(|epoch| index.epoch < epoch) {
            return Err("Cloud skill index rollback refused".into());
        }
        state.skill_epoch = Some(index.epoch);
        for id in &index.withdrawn_skill_ids {
            if state.skill_tombstones.len() == MAX_TOMBSTONES
                && !state.skill_tombstones.contains(id)
            {
                state.skill_overflow = true;
                return Err("Cloud skill withdrawal cache full; fallback is disabled".into());
            }
            state.skill_tombstones.insert(id.clone());
        }
        Ok(())
    }
    pub fn skill_fallback_blocked(&self, skill_id: &str) -> bool {
        let state = self.0.lock().expect("curation state poisoned");
        let artifact_id = format!("skill:{skill_id}");
        state.overflow
            || state.skill_overflow
            || state.skill_tombstones.contains(skill_id)
            || state.tombstones.iter().any(|(id, _)| id == &artifact_id)
    }
    /// Pin an exact currently released entry for trusted host/planner declaration.
    /// Authentication is last-known: offline cache reuse is not a freshness claim.
    /// The host must ensure this artifact really is used by the installed planner.
    pub fn pin_dependency(
        &self,
        artifact_id: &str,
        revision: &str,
    ) -> Result<super::authored_edit_plan::dependencies::HarnessDependency, String> {
        // This is a host registration boundary, not an input/command hot path.
        // Validate everything under one lock before retaining any evidence.
        let mut state = self.0.lock().expect("curation state poisoned");
        dependency_state_ready(&state)?;
        let dependency = super::authored_edit_plan::dependencies::HarnessDependency {
            authority_digest: blake3::hash(state.authority.as_bytes())
                .to_hex()
                .to_string(),
            artifact_id: artifact_id.into(),
            revision: revision.into(),
        };
        super::authored_edit_plan::dependencies::normalize(&mut vec![dependency.clone()])?;
        let key = (artifact_id.to_owned(), revision.to_owned());
        if state.tombstones.contains(&key) {
            return Err("Cannot pin a withdrawn harness dependency".into());
        }
        if !state.manifest.as_ref().is_some_and(|manifest| {
            manifest
                .releases
                .iter()
                .any(|r| r.artifact_id == artifact_id && r.revision == revision && !r.withdrawn)
        }) {
            return Err(
                "Harness dependency is not an authenticated currently released entry".into(),
            );
        }
        if !state.verified_revisions.contains(&key)
            && state.verified_revisions.len() >= MAX_TOMBSTONES
        {
            return Err("Harness dependency pin capacity reached; existing pins remain valid. Restart and rebind against current verified releases to recover.".into());
        }
        state.verified_revisions.insert(key);
        Ok(dependency)
    }

    /// Bounded in-memory admission against authenticated last-known knowledge.
    /// Ordinary new revisions do not invalidate an older trusted pin; ALL known
    /// withdrawals do. No service availability or global sequence fence is used.
    pub fn check_dependencies(
        &self,
        dependencies: &[super::authored_edit_plan::dependencies::HarnessDependency],
    ) -> Result<(), String> {
        use super::authored_edit_plan::dependencies::{normalize, MAX_HARNESS_DEPENDENCIES};
        if dependencies.is_empty() {
            return Ok(());
        }
        if dependencies.len() > MAX_HARNESS_DEPENDENCIES {
            return Err("Too many harness dependencies".into());
        }
        normalize(&mut dependencies.to_vec())?;
        let state = self.0.lock().expect("curation state poisoned");
        dependency_state_ready(&state)?;
        let authority = blake3::hash(state.authority.as_bytes())
            .to_hex()
            .to_string();
        for dependency in dependencies {
            if dependency.authority_digest != authority {
                return Err("Harness dependency authority changed; restore its trusted host source or create a new preview".into());
            }
            let key = (dependency.artifact_id.clone(), dependency.revision.clone());
            if state.tombstones.contains(&key) {
                return Err(format!(
                    "Harness dependency {}@{} is withdrawn",
                    dependency.artifact_id, dependency.revision
                ));
            }
            if !state.verified_revisions.contains(&key) {
                return Err(
                    "Unknown harness dependency revision; no authenticated evidence was retained"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// Live bounded diagnostics, deliberately outside an immutable plan digest.
    /// A successful past reconciliation is never a current-network freshness claim.
    pub fn dependency_evidence(&self) -> Value {
        let state = self.0.lock().expect("curation state poisoned");
        let verification_error = state.error.is_some() || dependency_state_ready(&state).is_err();
        json!({
            "release_sequence": state.manifest.as_ref().map(|manifest| manifest.sequence),
            "freshness": if state.fresh && !verification_error {
                "verified_at_last_reconciliation"
            } else {
                "last_known_unverified"
            },
            "freshness_scope": "last_reconciliation",
            "verification_error": verification_error,
            "cache_error": state.cache_error.is_some(),
        })
    }

    pub fn revision_withdrawn(&self, artifact_id: &str, revision: &str) -> bool {
        let state = self.0.lock().expect("curation state poisoned");
        state.overflow
            || state
                .tombstones
                .contains(&(artifact_id.into(), revision.into()))
    }
    pub fn status(
        &self,
        configured: bool,
        available_tools: &BTreeSet<String>,
        observation: Option<&RemedyKey>,
    ) -> Value {
        self.status_for(configured, available_tools, observation, None)
    }
    pub fn status_for(
        &self,
        configured: bool,
        available_tools: &BTreeSet<String>,
        observation: Option<&RemedyKey>,
        artifact_id: Option<&str>,
    ) -> Value {
        let state = self.0.lock().expect("curation state poisoned");
        let matching = state
            .manifest
            .iter()
            .flat_map(|m| &m.releases)
            .filter(|r| {
                artifact_id.is_none_or(|id| r.artifact_id == id)
                    && observation.is_none_or(|o| r.remedy_keys.iter().any(|key| key.matches(o)))
            })
            .collect::<Vec<_>>();
        let mut notices = Vec::new();
        let mut notice_bytes = 0;
        for r in matching.iter().take(MAX_NOTICES) {
            let missing = r
                .required_tool_ids
                .iter()
                .filter(|id| !available_tools.contains(*id))
                .collect::<Vec<_>>();
            let notice = json!({"artifact_id":r.artifact_id,"revision":r.revision,
                "remedy_keys":r.remedy_keys.iter().take(4).collect::<Vec<_>>(),
                "applicability_truncated":r.remedy_keys.len()>4,
                "withdrawn":r.withdrawn,"refresh_required":r.refresh_required,
                "compatible":missing.is_empty() && (!r.runtime_required || state.installed_apps.get(&r.artifact_id)==Some(&r.revision)),
                "runtime_required":r.runtime_required,"installed_runtime_revision":state.installed_apps.get(&r.artifact_id),"missing_required_tool_ids":missing.iter().take(8).collect::<Vec<_>>(),
                "missing_tool_count":missing.len()});
            let size = serde_json::to_vec(&notice)
                .expect("release notice serializes")
                .len();
            if notice_bytes + size > MAX_NOTICE_BYTES {
                break;
            }
            notice_bytes += size;
            notices.push(notice);
        }
        json!({"configured":configured,"freshness":if state.fresh {"verified"} else {"unverified"},
            "release_sequence":state.manifest.as_ref().map(|m|m.sequence),
            "matching_release_count":matching.len(),"truncated":matching.len()>notices.len(),"releases":notices,
            "known_withdrawal_count":state.tombstones.len(),
            "known_skill_withdrawal_count":state.skill_tombstones.len(),
            "freshness_scope":"last_reconciliation","error":state.error,
            "notifications_available":false,"retention":if state.cache_restored {"signed_host_cache"} else {"application_lifetime"},
            "service_events_connected":state.service_events_connected,"cached_release_sequence":state.cached_release_sequence,"cache_error":state.cache_error,
            "automatic_export_enabled":false,
            "next_action":"Refresh applicable artifacts at a safe task boundary; re-plan current intent against current project state. Narrow by public artifact_id or observation if truncated. Do not replay failed calls or alter authored geometry automatically.",
            "note":"Delivery is not acknowledgement or project verification. Offline freshness is unknown; known withdrawals still suppress stale fallback. An app update may be required."})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executable_release_requires_exact_host_installed_artifact() {
        let store = CurationReleaseStore::default();
        let mut app = release("app:example.capability", "revision-one", false);
        assert!(manifest(1, vec![app.clone()]).validate().is_err());
        app.runtime_required = true;
        store.accept(manifest(1, vec![app.clone()])).unwrap();
        assert_eq!(
            store.status(true, &BTreeSet::new(), None)["releases"][0]["compatible"],
            false
        );
        store.register_installed_app(app.artifact_id.clone(), "other-revision".into());
        assert_eq!(
            store.status(true, &BTreeSet::new(), None)["releases"][0]["compatible"],
            false
        );
        store.register_installed_app(app.artifact_id.clone(), app.revision.clone());
        assert_eq!(
            store.status(true, &BTreeSet::new(), None)["releases"][0]["compatible"],
            true
        );
        // Ordinary skill wire format remains compatible with existing clients.
        assert!(serde_json::to_value(release("skill:test", "r1", false))
            .unwrap()
            .get("runtime_required")
            .is_none());
    }
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};
    fn release(id: &str, revision: &str, withdrawn: bool) -> ReleaseArtifact {
        ReleaseArtifact {
            runtime_required: false,
            artifact_id: id.into(),
            revision: revision.into(),
            remedy_keys: vec![RemedyKey {
                gap_kind: Some("missing_path".into()),
                ..Default::default()
            }],
            required_tool_ids: vec![],
            withdrawn,
            refresh_required: withdrawn,
        }
    }
    fn manifest(sequence: u64, releases: Vec<ReleaseArtifact>) -> ReleaseManifest {
        ReleaseManifest {
            schema: RELEASE_SCHEMA.into(),
            sequence,
            releases,
        }
    }
    #[test]
    fn signed_manifest_rejects_forgery_unknown_fields_and_private_ids() {
        let key = SigningKey::from_bytes(&[31; 32]);
        let public = hex::encode(key.verifying_key().to_bytes());
        let mut value =
            serde_json::to_value(manifest(1, vec![release("skill:core.test", "r1", false)]))
                .unwrap();
        value["public_key"] = json!(public);
        let sign = |v: &mut Value| {
            v.as_object_mut().unwrap().remove("signature");
            let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(v).unwrap()).into();
            v["signature"] = json!(hex::encode(key.sign(&digest).to_bytes()));
        };
        sign(&mut value);
        assert_eq!(
            decode_manifest(
                &serde_json::to_vec(&value).unwrap(),
                std::slice::from_ref(&public)
            )
            .unwrap()
            .sequence,
            1
        );
        assert!(decode_manifest(&serde_json::to_vec(&value).unwrap(), &[]).is_err());
        value["sequence"] = json!(2);
        assert!(decode_manifest(
            &serde_json::to_vec(&value).unwrap(),
            std::slice::from_ref(&public)
        )
        .is_err());
        value["incident_id"] = json!("private-membership");
        sign(&mut value);
        assert!(decode_manifest(&serde_json::to_vec(&value).unwrap(), &[public]).is_err());
    }
    #[test]
    fn rollback_conflict_and_outage_do_not_forget_withdrawals() {
        let store = CurationReleaseStore::default();
        store
            .accept(manifest(2, vec![release("skill:core.test", "r1", true)]))
            .unwrap();
        assert!(store.accept(manifest(1, vec![])).is_err());
        assert!(store.accept(manifest(2, vec![])).is_err());
        store
            .accept(manifest(3, vec![release("skill:core.test", "r2", false)]))
            .unwrap();
        store.unavailable("offline".into());
        assert!(store.revision_withdrawn("skill:core.test", "r1"));
        assert!(!store.revision_withdrawn("skill:core.test", "r2"));
        assert!(store.skill_fallback_blocked("core.test"));
        let status = store.status(true, &BTreeSet::new(), None);
        assert_eq!(status["freshness"], "unverified");
        assert_eq!(status["release_sequence"], 3);
        assert_eq!(status["notifications_available"], false);
    }
    #[test]
    fn local_matching_requires_all_declared_constraints_and_reports_runtime_gap() {
        let store = CurationReleaseStore::default();
        let mut r = release("skill:core.test", "r1", false);
        r.remedy_keys[0].element_class = Some("widget".into());
        r.required_tool_ids = vec!["create_widget".into()];
        store.accept(manifest(1, vec![r])).unwrap();
        let mut observed = RemedyKey {
            gap_kind: Some("missing_path".into()),
            ..Default::default()
        };
        assert_eq!(
            store.status(true, &BTreeSet::new(), Some(&observed))["matching_release_count"],
            0
        );
        observed.element_class = Some("widget".into());
        let status = store.status(true, &BTreeSet::new(), Some(&observed));
        assert_eq!(status["releases"][0]["compatible"], false);
        assert_eq!(
            status["releases"][0]["missing_required_tool_ids"][0],
            "create_widget"
        );
        assert_eq!(
            store.status(
                true,
                &BTreeSet::from(["create_widget".into()]),
                Some(&observed)
            )["releases"][0]["compatible"],
            true
        );
    }
    #[test]
    fn cold_manifest_contains_old_withdrawal_and_one_active_replacement() {
        let store = CurationReleaseStore::default();
        let full = manifest(
            5,
            vec![
                release("skill:core.test", "r1", true),
                release("skill:core.test", "r2", false),
            ],
        );
        store.accept(full.clone()).unwrap();
        assert!(store.revision_withdrawn("skill:core.test", "r1"));
        assert!(!store.revision_withdrawn("skill:core.test", "r2"));
        assert!(store.skill_fallback_blocked("core.test"));
        let mut invalid = full.clone();
        invalid
            .releases
            .push(release("skill:core.test", "r3", false));
        assert!(invalid.validate().is_err());
        let mut contradictory = full;
        contradictory
            .releases
            .push(release("skill:core.test", "r2", true));
        assert!(contradictory.validate().is_err());
    }

    #[test]
    fn legacy_index_withdrawal_survives_outage_and_rejects_rollback() {
        let store = CurationReleaseStore::default();
        let connection = AgentCorpusConnection::default();
        let mut index = agent_skill_cloud::SkillIndex {
            schema: agent_skill_cloud::SCHEMA.into(),
            epoch: 3,
            skills: vec![],
            withdrawn_skill_ids: vec!["core.test".into()],
        };
        store.observe_skill_index(&connection, &index).unwrap();
        store.unavailable("offline".into());
        assert!(store.skill_fallback_blocked("core.test"));
        index.epoch = 2;
        index.withdrawn_skill_ids.clear();
        assert!(store.observe_skill_index(&connection, &index).is_err());
        assert!(store.skill_fallback_blocked("core.test"));
        assert!(!store.skill_fallback_blocked("core.other"));
    }
    fn bound_dependency_store() -> CurationReleaseStore {
        let store = CurationReleaseStore::default();
        store.bind(&CurationConnection {
            base_url: "https://curation.example.test/v1/curation".into(),
            trusted_public_keys: vec!["ab".repeat(32)],
        });
        store
    }

    #[test]
    fn unrelated_manifest_churn_does_not_consume_pins_or_block_existing_plan_dependencies() {
        let store = bound_dependency_store();
        store
            .accept(manifest(1, vec![release("skill:used", "r1", false)]))
            .unwrap();
        let dependency = store.pin_dependency("skill:used", "r1").unwrap();
        for batch in 0..5 {
            store
                .accept(manifest(
                    batch + 2,
                    (0..1000)
                        .map(|id| {
                            release(
                                &format!("skill:unrelated.{}", batch * 1000 + id),
                                "r1",
                                false,
                            )
                        })
                        .collect(),
                ))
                .unwrap();
            store
                .check_dependencies(std::slice::from_ref(&dependency))
                .unwrap();
        }
        assert_eq!(store.0.lock().unwrap().verified_revisions.len(), 1);
        // Previously issued refs remain usable, but a fresh host declaration
        // cannot claim a revision no longer in the current authenticated manifest.
        assert!(store.pin_dependency("skill:used", "r1").is_err());
        assert_eq!(store.0.lock().unwrap().verified_revisions.len(), 1);
    }

    #[test]
    fn failed_pins_never_consume_evidence_and_full_ledger_preserves_existing_pins() {
        let store = bound_dependency_store();
        let mut first = None;
        for (batch, start) in (0..MAX_TOMBSTONES).step_by(1000).enumerate() {
            let end = (start + 1000).min(MAX_TOMBSTONES);
            store
                .accept(manifest(
                    batch as u64 + 1,
                    (start..end)
                        .map(|id| release(&format!("skill:pinned.{id}"), "r1", false))
                        .collect(),
                ))
                .unwrap();
            for id in start..end {
                let pin = store
                    .pin_dependency(&format!("skill:pinned.{id}"), "r1")
                    .unwrap();
                first.get_or_insert(pin);
            }
        }
        store
            .accept(manifest(6, vec![release("skill:extra", "r1", false)]))
            .unwrap();
        for (artifact, revision) in [
            ("skill:extra", "r1"),
            ("skill:missing", "r1"),
            ("bad", "r1"),
        ] {
            assert!(store.pin_dependency(artifact, revision).is_err());
            assert_eq!(
                store.0.lock().unwrap().verified_revisions.len(),
                MAX_TOMBSTONES
            );
        }
        store
            .check_dependencies(std::slice::from_ref(first.as_ref().unwrap()))
            .unwrap();

        let store = bound_dependency_store();
        store
            .accept(manifest(1, vec![release("skill:used", "r1", false)]))
            .unwrap();
        let pin = store.pin_dependency("skill:used", "r1").unwrap();
        store
            .accept(manifest(2, vec![release("skill:used", "r1", true)]))
            .unwrap();
        assert!(store.pin_dependency("skill:used", "r1").is_err());
        assert!(store.check_dependencies(&[pin]).is_err());
        assert_eq!(store.0.lock().unwrap().verified_revisions.len(), 1);

        // Rejected source states must not index otherwise present active entries.
        for rejection in ["unbound", "overflow", "conflict"] {
            let store = bound_dependency_store();
            store
                .accept(manifest(1, vec![release("skill:unused", "r1", false)]))
                .unwrap();
            {
                let mut state = store.0.lock().unwrap();
                match rejection {
                    "unbound" => state.authority.clear(),
                    "overflow" => state.overflow = true,
                    _ => state.conflicting_sequence = Some(1),
                }
            }
            assert!(store.pin_dependency("skill:unused", "r1").is_err());
            assert!(store.0.lock().unwrap().verified_revisions.is_empty());
        }
    }

    #[test]
    fn status_is_bounded_and_does_not_acknowledge_or_mutate_releases() {
        let store = CurationReleaseStore::default();
        store
            .accept(manifest(
                1,
                (0..32)
                    .map(|i| release(&format!("skill:test.{i}"), "r1", false))
                    .collect(),
            ))
            .unwrap();
        let first = store.status(true, &BTreeSet::new(), None);
        assert_eq!(first["releases"].as_array().unwrap().len(), MAX_NOTICES);
        assert_eq!(first["truncated"], true);
        assert_eq!(first, store.status(true, &BTreeSet::new(), None));
        assert_eq!(first["automatic_export_enabled"], false);
    }
}
