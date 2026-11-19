//! Transport-independent cloud skill client. No network work runs on the Bevy thread.
//! The host supplies the service URL and its existing signed-in account credential.
use super::agent_skills::{AgentCorpusConnection, AgentSkill, AgentSkillSearch, AgentSkillSummary};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "talos3d.agent-skill-bundle.v1";
pub const MAX_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillResource {
    pub media_type: String,
    pub content: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillBundle {
    pub schema: String,
    pub skill: AgentSkill,
    #[serde(default)]
    pub resources: BTreeMap<String, SkillResource>,
    #[serde(default)]
    pub provenance: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillContribution {
    pub bundle: SkillBundle,
    pub applicability: String,
    pub limitations: Vec<String>,
    pub evidence: Vec<String>,
    pub redistribution_rights: String,
    pub contains_private_context: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    pub content_hash: String,
    pub skill: AgentSkillSummary,
    pub sequence: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillIndex {
    pub schema: String,
    pub epoch: i64,
    pub skills: Vec<SkillIndexEntry>,
    #[serde(default)]
    pub withdrawn_skill_ids: Vec<String>,
}
// Request-time lexical retrieval, not a claim that a reference answers the
// whole question. Supplements can use different wording from the first repair.
fn query_terms(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| {
            !word.is_empty()
                && !matches!(
                    *word,
                    "a" | "an"
                        | "and"
                        | "the"
                        | "of"
                        | "for"
                        | "in"
                        | "on"
                        | "to"
                        | "with"
                        | "from"
                )
        })
        .map(|word| {
            let mut stem = word;
            for suffix in ["ing", "ed", "s"] {
                if word.len() > suffix.len() + 3
                    && word.ends_with(suffix)
                    && !(suffix == "s" && word.ends_with("ss"))
                {
                    stem = &word[..word.len() - suffix.len()];
                    break;
                }
            }
            if stem.len() > 3 && stem.ends_with('e') {
                stem = &stem[..stem.len() - 1];
            }
            stem.to_owned()
        })
        .collect()
}
impl SkillIndex {
    pub fn filter(&self, search: &AgentSkillSearch) -> Vec<AgentSkillSummary> {
        let query = search.query.as_deref().unwrap_or_default().to_lowercase();
        let terms = query_terms(&query);
        let mut hits = Vec::new();
        for entry in &self.skills {
            let skill = &entry.skill;
            if !search
                .tags
                .iter()
                .all(|tag| skill.task_tags.iter().any(|v| v.eq_ignore_ascii_case(tag)))
            {
                continue;
            }
            let text = format!(
                "{} {} {} {}",
                skill.id,
                skill.title,
                skill.summary,
                skill.task_tags.join(" ")
            )
            .to_lowercase();
            let exact = text.contains(&query);
            let words = query_terms(&text);
            let matched = terms.intersection(&words).count();
            // Require at least half the informative query terms, and at least
            // two for multi-term queries. A lone generic overlap is not a hit.
            let relevant =
                !terms.is_empty() && matched >= terms.len().div_ceil(2).max(terms.len().min(2));
            if exact || relevant {
                let mut summary = skill.clone();
                summary.source_path = Some(format!("cloud:{}", entry.content_hash));
                hits.push(((exact, matched), summary));
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0));
        hits.into_iter().map(|(_, summary)| summary).collect()
    }
}

fn url(c: &AgentCorpusConnection, path: &str) -> Result<String, String> {
    if c.base_url.is_empty() {
        return Err("No cloud corpus configured by this application".into());
    }
    let base = c.base_url.trim_end_matches('/');
    if !(base.starts_with("https://")
        || base.starts_with("http://127.0.0.1:")
        || base.starts_with("http://localhost:")
        || base.starts_with("/api/"))
    {
        return Err("Cloud corpus requires HTTPS (loopback HTTP is allowed for tests)".into());
    }
    Ok(format!("{base}{path}"))
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn request(
    c: &AgentCorpusConnection,
    path: &str,
    body: Option<&Value>,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        // A redirect must not forward a private contribution to another endpoint.
        .redirect(if body.is_some() {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::limited(10)
        })
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let endpoint = url(c, path)?;
    let mut r = if let Some(body) = body {
        client.post(endpoint).json(body)
    } else {
        client.get(endpoint)
    };
    if let Some(token) = &c.bearer_token {
        r = r.bearer_auth(token);
    }
    let mut response = r
        .send()
        .await
        .map_err(|_| "Cloud corpus request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Cloud corpus HTTP {}; reconnect account for contributions",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Cloud corpus read failed")?
    {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err("Cloud corpus response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
#[cfg(target_arch = "wasm32")]
async fn request_local(
    c: &AgentCorpusConnection,
    path: &str,
    body: Option<&Value>,
) -> Result<Vec<u8>, String> {
    let endpoint = url(c, path)?;
    let mut r = if body.is_some() {
        gloo_net::http::Request::post(&endpoint)
    } else {
        gloo_net::http::Request::get(&endpoint)
    };
    if body.is_some() {
        r = r.redirect(web_sys::RequestRedirect::Error);
    }
    if let Some(token) = &c.bearer_token {
        r = r.header("Authorization", &format!("Bearer {token}"));
    }
    let r = if let Some(body) = body {
        r.json(body).map_err(|e| e.to_string())?
    } else {
        r.build().map_err(|e| e.to_string())?
    };
    let response = r
        .send()
        .await
        .map_err(|_| "Cloud corpus request failed".to_string())?;
    if !response.ok() {
        return Err(format!(
            "Cloud corpus HTTP {}; sign in for contributions",
            response.status()
        ));
    }
    let bytes = response
        .binary()
        .await
        .map_err(|_| "Cloud corpus read failed")?;
    if bytes.len() > MAX_BYTES {
        return Err("Cloud corpus response too large".into());
    }
    Ok(bytes)
}
// rmcp handlers require Send futures on every target. Browser Fetch futures are
// JS-local; spawn them on the browser executor and await only the Send channel.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn request(
    c: &AgentCorpusConnection,
    path: &str,
    body: Option<&Value>,
) -> Result<Vec<u8>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (c, path, body) = (c.clone(), path.to_string(), body.cloned());
    wasm_bindgen_futures::spawn_local(async move {
        let result = request_local(&c, &path, body.as_ref()).await;
        let _ = tx.send(result);
    });
    rx.await
        .map_err(|_| "Browser corpus request cancelled".to_string())?
}
pub(crate) fn verify_signed_json(bytes: &[u8], trusted_keys: &[String]) -> Result<Value, String> {
    use ed25519_dalek::{Signature, VerifyingKey};
    let mut value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let signature = value
        .as_object_mut()
        .ok_or("Invalid signed corpus index")?
        .remove("signature")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or("Missing corpus signature")?;
    let key = value["public_key"]
        .as_str()
        .ok_or("Missing corpus signing key")?;
    if !trusted_keys.iter().any(|k| k == key) {
        return Err("Cloud corpus signing key is not trusted by this app; configure the operator-verified public key".into());
    }
    let key: [u8; 32] = hex::decode(key)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "Invalid corpus signing key")?;
    let signature: [u8; 64] = hex::decode(signature)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "Invalid corpus signature")?;
    let digest: [u8; 32] =
        Sha256::digest(serde_json::to_vec(&value).map_err(|e| e.to_string())?).into();
    VerifyingKey::from_bytes(&key)
        .map_err(|e| e.to_string())?
        .verify_strict(&digest, &Signature::from_bytes(&signature))
        .map_err(|_| "Invalid corpus signature")?;
    Ok(value)
}
pub fn decode_index(bytes: &[u8], trusted_keys: &[String]) -> Result<SkillIndex, String> {
    serde_json::from_value(verify_signed_json(bytes, trusted_keys)?).map_err(|e| e.to_string())
}
pub async fn index(c: &AgentCorpusConnection) -> Result<SkillIndex, String> {
    let i = decode_index(&request(c, "/skills", None).await?, &c.trusted_public_keys)?;
    if i.schema != SCHEMA || i.skills.len() > 1000 {
        return Err("Unsupported cloud corpus index".into());
    }
    Ok(i)
}
pub fn decode_bundle(bytes: &[u8], hash: &str) -> Result<SkillBundle, String> {
    if hash.len() != 64 || hex::encode(Sha256::digest(bytes)) != hash {
        return Err("Cloud skill checksum mismatch".into());
    }
    let mut b: SkillBundle = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if b.schema != SCHEMA || b.skill.must_read_at_bootstrap {
        return Err("Invalid cloud skill release".into());
    }
    // Immutable candidate bytes do not attest publication. Only the signed
    // released index and host admission may confer shipped trust.
    b.skill.trust_level = super::agent_skills::AgentSkillTrustLevel::ProjectDraft;
    Ok(b)
}
pub async fn bundle(c: &AgentCorpusConnection, hash: &str) -> Result<SkillBundle, String> {
    if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid corpus revision".into());
    }
    decode_bundle(
        &request(c, &format!("/revisions/{hash}"), None).await?,
        hash,
    )
}
pub async fn contribute(
    c: &AgentCorpusConnection,
    request_body: SkillContribution,
) -> Result<Value, String> {
    if request_body.contains_private_context {
        return Err(
            "Generalize the skill before contributing; private knowledge belongs in the project"
                .into(),
        );
    }
    let value = serde_json::to_value(request_body).map_err(|e| e.to_string())?;
    if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > MAX_BYTES {
        return Err("Skill contribution too large".into());
    }
    serde_json::from_slice(&request(c, "/contributions", Some(&value)).await?)
        .map_err(|e| e.to_string())
}
pub fn status(index: &Result<SkillIndex, String>) -> Value {
    match index {
        Ok(i) => {
            json!({"available":true,"epoch":i.epoch,"released_skill_count":i.skills.len(),"refresh":"Check at task boundaries and before authoring; re-fetch changed skill hashes. A change does not rewrite active agent context or authored models."})
        }
        Err(e) => {
            json!({"available":false,"error":e,"fallback":"Bundled skills remain available; cloud freshness is unverified."})
        }
    }
}

pub async fn contribution_index(c: &AgentCorpusConnection) -> Result<Value, String> {
    serde_json::from_slice(&request(c, "/contributions", None).await?).map_err(|e| e.to_string())
}
pub async fn review(c: &AgentCorpusConnection, r: SkillReview) -> Result<Value, String> {
    if r.content_hash.len() != 64 || !r.content_hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid corpus revision".into());
    }
    let path = format!("/contributions/{}/review", r.content_hash);
    let value = serde_json::to_value(r).map_err(|e| e.to_string())?;
    let mut body = value;
    body.as_object_mut().unwrap().remove("content_hash");
    serde_json::from_slice(&request(c, &path, Some(&body)).await?).map_err(|e| e.to_string())
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillReview {
    pub content_hash: String,
    pub decision: String,
    pub confidentiality_reviewed: bool,
    pub rights_reviewed: bool,
    pub regression_evidence: Vec<String>,
    pub reason: String,
}

pub async fn candidate_bundle(
    c: &AgentCorpusConnection,
    hash: &str,
) -> Result<SkillBundle, String> {
    if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid corpus revision".into());
    }
    decode_bundle(
        &request(c, &format!("/contributions/{hash}"), None).await?,
        hash,
    )
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillRevisionRequest {
    pub content_hash: String,
}

/// Typed grounding assets carried by the existing immutable bundle transport.
/// They are read through passage APIs, never installed as executable skills.
fn passage_resources(
    bundle: &SkillBundle,
) -> Result<Vec<super::knowledge_persistence::PersistedPassage>, String> {
    let mut passages = Vec::new();
    for (path, resource) in &bundle.resources {
        if !path.starts_with("passages/") {
            continue;
        }
        if resource.media_type != "application/json" {
            return Err("Invalid passage resource type".into());
        }
        let passage: super::knowledge_persistence::PersistedPassage =
            serde_json::from_str(&resource.content)
                .map_err(|_| "Invalid typed passage resource")?;
        if passage.passage_ref.is_empty()
            || passage.passage_ref.len() > 160
            || passage.text.trim().is_empty()
            || passage.text.len() > 64 * 1024
            || passage.citation.trim().is_empty()
            || passage
                .source_url
                .as_deref()
                .is_none_or(|url| !url.starts_with("https://"))
            || passage.license.as_deref().is_none_or(str::is_empty)
            || passage.proactive_guidance.is_some()
            || passage.meta.is_some()
            || passages
                .iter()
                .any(|p: &super::knowledge_persistence::PersistedPassage| {
                    p.passage_ref == passage.passage_ref
                })
        {
            return Err(
                "Passage requires source/rights and cannot claim bootstrap or curation authority"
                    .into(),
            );
        }
        passages.push(passage);
    }
    Ok(passages)
}

async fn admitted_index(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
) -> Result<SkillIndex, String> {
    let index = index(c).await?;
    store.observe_skill_index(c, &index)?;
    Ok(index)
}

pub async fn released_passage(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
    hash: &str,
    passage_ref: &str,
) -> Result<Value, String> {
    let index = admitted_index(c, store).await?;
    let entry = index
        .skills
        .iter()
        .find(|e| e.content_hash == hash)
        .ok_or("Passage revision is not a current released corpus revision")?;
    let artifact_id = format!("skill:{}", entry.skill.id);
    if store.revision_withdrawn(&artifact_id, hash) {
        return Err("Passage revision withdrawn".into());
    }
    let bundle = bundle(c, hash).await?;
    let passage = passage_resources(&bundle)?
        .into_iter()
        .find(|p| p.passage_ref == passage_ref)
        .ok_or("Passage absent from revision")?;
    let latest = admitted_index(c, store).await?;
    if !latest.skills.iter().any(|e| e.content_hash == hash)
        || store.revision_withdrawn(&artifact_id, hash)
    {
        return Err("Passage revision changed or withdrawn during retrieval; rediscover".into());
    }
    Ok(
        json!({"passage_ref":passage.passage_ref,"text":passage.text,"source":passage.citation,
        "source_url":passage.source_url,"jurisdiction":passage.jurisdiction,"license":passage.license,
        "scope":"shared_release","trust":"reviewed_reference","content_hash":hash,"artifact_id":artifact_id,
        "executable":false,"delivery":"verified_cloud_passage","note":"Grounding only; not structural sizing certification or a recipe. No project or refinement state changed."}),
    )
}

pub async fn search_passages(
    c: &AgentCorpusConnection,
    store: &super::curation::CurationReleaseStore,
    query: &str,
) -> Result<Value, String> {
    let index = admitted_index(c, store).await?;
    let search = AgentSkillSearch {
        query: Some(query.into()),
        tags: Vec::new(),
    };
    let hits = index.filter(&search);
    let mut result = Vec::new();
    for hit in hits.iter().take(16) {
        let entry = index
            .skills
            .iter()
            .find(|e| e.skill.id == hit.id)
            .ok_or("Invalid corpus index")?;
        let artifact_id = format!("skill:{}", hit.id);
        if store.revision_withdrawn(&artifact_id, &entry.content_hash) {
            continue;
        }
        let bundle = bundle(c, &entry.content_hash).await?;
        for p in passage_resources(&bundle)? {
            result.push(json!({"passage_ref":p.passage_ref,"citation":p.citation,"jurisdiction":p.jurisdiction,
                "artifact_id":artifact_id,"content_hash":entry.content_hash,"executable":false,
                "next_tool":"lookup_source_passage","arguments":{"passage_ref":p.passage_ref,"content_hash":entry.content_hash}}));
        }
    }
    Ok(
        json!({"schema":"talos3d.knowledge-search.v1","scope":"reviewed_cloud_references","epoch":index.epoch,
        "passages":result,"truncated":hits.len()>16,"executable":false}),
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn supplemental_references_are_discoverable_by_natural_terms() {
        let make = |id: &str, title: &str, tags: Vec<&str>| {
            let skill: AgentSkill = serde_json::from_value(json!({
                "id":id,"title":title,"summary":"Reference procedure","body_markdown":"Read references.","task_tags":tags
            })).unwrap();
            SkillIndexEntry {
                content_hash: "a".repeat(64),
                skill: skill.summary_info(),
                sequence: 1,
            }
        };
        assert_eq!(query_terms("modules loading"), query_terms("module loaded"));
        let index = SkillIndex {
            schema: SCHEMA.into(),
            epoch: 1,
            withdrawn_skill_ids: vec![],
            skills: vec![
                make(
                    "supplement",
                    "Cache invalidation during module replacement",
                    vec!["cache invalidation", "module replacement"],
                ),
                make(
                    "first",
                    "Module loading and cache invalidation",
                    vec!["loading"],
                ),
                make("unrelated", "Cache invoice audit", vec!["billing"]),
            ],
        };
        let search = AgentSkillSearch {
            query: Some("module loading and cache invalidation".into()),
            tags: vec![],
        };
        let hits = index.filter(&search);
        assert_eq!(
            hits.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "supplement"]
        );
        assert!(hits
            .iter()
            .all(|v| v.source_path.as_deref() == Some(&format!("cloud:{}", "a".repeat(64)))));
        assert_eq!(
            index
                .filter(&AgentSkillSearch {
                    query: Some("modules cache invalidation".into()),
                    tags: vec!["MODULE REPLACEMENT".into()]
                })
                .len(),
            1
        );
        assert!(index
            .filter(&AgentSkillSearch {
                query: Some("unknown transport protocol".into()),
                tags: vec![]
            })
            .is_empty());
        assert_eq!(index.filter(&AgentSkillSearch::default()).len(), 3);
    }

    #[test]
    fn passage_assets_cannot_install_authority_and_require_source_rights() {
        let skill:AgentSkill=serde_json::from_value(json!({"id":"test.references","title":"References","summary":"Grounded references","body_markdown":"Read references."})).unwrap();
        let passage = json!({"passage_ref":"test.concept","text":"A referenced concept.","citation":"Primary source",
            "source_url":"https://example.org/source","license":"original_paraphrase"});
        let mut bundle = SkillBundle {
            schema: SCHEMA.into(),
            skill,
            provenance: Value::Null,
            resources: BTreeMap::from([(
                "passages/concept.json".into(),
                SkillResource {
                    media_type: "application/json".into(),
                    content: passage.to_string(),
                },
            )]),
        };
        assert_eq!(passage_resources(&bundle).unwrap().len(), 1);
        let mut invalid = passage.clone();
        invalid["license"] = Value::Null;
        bundle
            .resources
            .get_mut("passages/concept.json")
            .unwrap()
            .content = invalid.to_string();
        assert!(passage_resources(&bundle).is_err());
        let mut invalid = passage.clone();
        invalid["source_url"] = json!("file:///private");
        bundle
            .resources
            .get_mut("passages/concept.json")
            .unwrap()
            .content = invalid.to_string();
        assert!(passage_resources(&bundle).is_err());
        bundle
            .resources
            .get_mut("passages/concept.json")
            .unwrap()
            .content = passage.to_string();
        let duplicate = bundle.resources["passages/concept.json"].clone();
        bundle
            .resources
            .insert("passages/duplicate.json".into(), duplicate);
        assert!(passage_resources(&bundle).is_err());
    }

    #[test]
    fn signed_index_rejects_forgery_and_untrusted_keys() {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[23u8; 32]);
        let public = hex::encode(key.verifying_key().to_bytes());
        let mut value = json!({"schema":SCHEMA,"epoch":7,"skills":[],"withdrawn_skill_ids":[],"public_key":public});
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&value).unwrap()).into();
        value["signature"] = json!(hex::encode(key.sign(&digest).to_bytes()));
        assert_eq!(
            decode_index(
                &serde_json::to_vec(&value).unwrap(),
                std::slice::from_ref(&public)
            )
            .unwrap()
            .epoch,
            7
        );
        assert!(decode_index(&serde_json::to_vec(&value).unwrap(), &[]).is_err());
        value["epoch"] = json!(8);
        assert!(decode_index(&serde_json::to_vec(&value).unwrap(), &[public]).is_err());
    }
    #[test]
    fn immutable_resource_bytes_are_checked_before_use() {
        let skill:AgentSkill=serde_json::from_value(json!({"id":"core.test.check","title":"Check","summary":"Check interfaces","body_markdown":"Inspect interfaces.","trust_level":"shipped"})).unwrap();
        let b = SkillBundle {
            schema: SCHEMA.into(),
            skill,
            provenance: Value::Null,
            resources: BTreeMap::from([(
                "references/check.md".into(),
                SkillResource {
                    media_type: "text/markdown".into(),
                    content: "Declared interface evidence".into(),
                },
            )]),
        };
        let bytes = serde_json::to_vec(&b).unwrap();
        let hash = hex::encode(Sha256::digest(&bytes));
        assert_eq!(
            decode_bundle(&bytes, &hash).unwrap().resources["references/check.md"].content,
            "Declared interface evidence"
        );
        let mut corrupt = bytes.clone();
        corrupt.push(b' ');
        assert!(decode_bundle(&corrupt, &hash).is_err());
        let mut malicious = b;
        malicious.skill.must_read_at_bootstrap = true;
        let body = serde_json::to_vec(&malicious).unwrap();
        assert!(decode_bundle(&body, &hex::encode(Sha256::digest(&body))).is_err());
        assert_eq!(status(&Err("offline".into()))["available"], false);
    }
}
