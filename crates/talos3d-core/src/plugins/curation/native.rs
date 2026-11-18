//! Native host delivery. SSE is only a hint; every update is fetched and signed
//! data verified. The background runtime owns I/O, reconnect and cancellation.
use super::*;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use tokio::sync::watch;

const MAX_FRAME_BYTES: usize = 4096;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const CACHE_SCHEMA: &str = "talos3d.curation-cache.v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    schema: String,
    authority: String,
    envelopes: Vec<Value>,
}
impl Cache {
    fn empty(connection: &CurationConnection) -> Self {
        Self {
            schema: CACHE_SCHEMA.into(),
            authority: authority(&connection.base_url, &connection.trusted_public_keys),
            envelopes: Vec::new(),
        }
    }
    fn load(path: &Path, connection: &CurationConnection) -> Result<Self, String> {
        let file = fs::File::open(path).map_err(|e| format!("Curation cache open failed: {e}"))?;
        let mut bytes = Vec::new();
        file.take((MAX_CACHE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err("Curation cache too large".into());
        }
        let cache: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid curation cache".to_string())?;
        if cache.schema != CACHE_SCHEMA
            || cache.authority != authority(&connection.base_url, &connection.trusted_public_keys)
            || cache.envelopes.len() > 64
        {
            return Err("Curation cache source or format mismatch".into());
        }
        // Verify every envelope before restoring any state. Cache metadata is
        // never evidence of release authenticity.
        for envelope in &cache.envelopes {
            decode_manifest(
                &serde_json::to_vec(envelope).map_err(|e| e.to_string())?,
                &connection.trusted_public_keys,
            )?;
        }
        Ok(cache)
    }
    fn restore(
        &self,
        connection: &CurationConnection,
        store: &CurationReleaseStore,
    ) -> Result<(), String> {
        let scratch = CurationReleaseStore::default();
        scratch.bind(connection);
        for envelope in &self.envelopes {
            scratch.accept(decode_manifest(
                &serde_json::to_vec(envelope).map_err(|e| e.to_string())?,
                &connection.trusted_public_keys,
            )?)?;
        }
        let mut restored = scratch.0.lock().expect("curation cache lock poisoned");
        restored.fresh = false;
        restored.cache_restored = true;
        restored.cached_release_sequence = restored.manifest.as_ref().map(|m| m.sequence);
        let mut state = store.0.lock().expect("curation state poisoned");
        if state.authority != self.authority {
            return Err("Curation source changed during cache restore".into());
        }
        // A startup restore cannot overwrite a newer in-process reconciliation.
        if state.manifest.is_none() {
            state.manifest = restored.manifest.take();
            state.tombstones = std::mem::take(&mut restored.tombstones);
            state.fresh = false;
            state.cache_restored = true;
            state.cached_release_sequence = restored.cached_release_sequence;
            // Legacy skill-index withdrawals have a separate authority and may
            // already have been learned by an MCP session before startup.
        }
        Ok(())
    }
    fn record(&mut self, bytes: &[u8], connection: &CurationConnection) -> Result<(), String> {
        let manifest = decode_manifest(bytes, &connection.trusted_public_keys)?;
        let newest: BTreeSet<_> = manifest
            .releases
            .iter()
            .filter(|r| r.withdrawn)
            .map(|r| (r.artifact_id.clone(), r.revision.clone()))
            .collect();
        let mut retained = Vec::new();
        for old in &self.envelopes {
            let previous = decode_manifest(
                &serde_json::to_vec(old).map_err(|e| e.to_string())?,
                &connection.trusted_public_keys,
            )?;
            if previous.sequence > manifest.sequence {
                return Err("Curation cache rollback refused".into());
            }
            let has_needed_proof = previous.releases.iter().any(|r| {
                r.withdrawn && !newest.contains(&(r.artifact_id.clone(), r.revision.clone()))
            });
            if has_needed_proof {
                retained.push(old.clone());
            }
        }
        retained.push(serde_json::from_slice(bytes).map_err(|e| e.to_string())?);
        if retained.len() > 64 {
            return Err("Curation cache proof capacity exhausted".into());
        }
        let candidate = Self {
            schema: CACHE_SCHEMA.into(),
            authority: self.authority.clone(),
            envelopes: retained,
        };
        if serde_json::to_vec(&candidate)
            .map_err(|e| e.to_string())?
            .len()
            > MAX_CACHE_BYTES
        {
            return Err("Curation cache proof bytes exhausted".into());
        }
        *self = candidate;
        Ok(())
    }
    fn write_atomic(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or("Curation cache needs a parent directory")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temporary = parent.join(format!(".curation-cache-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&temporary, path).map_err(|e| e.to_string())?;
            fs::File::open(parent)
                .and_then(|d| d.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

/// Host-owned subscription. Start once per application, retaining the same
/// store inserted into the app world. Drop cancels without blocking the UI.
/// `shutdown` also joins the background thread and belongs in host shutdown.
#[derive(Resource)]
pub struct CurationSubscriber {
    stop: watch::Sender<bool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl CurationSubscriber {
    pub fn start(
        connection: CurationConnection,
        store: CurationReleaseStore,
        cache_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let parsed = reqwest::Url::parse(&connection.base_url)
            .map_err(|_| "Invalid curation service URL")?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(
                "Public curation URLs cannot carry credentials, queries or fragments".into(),
            );
        }
        let base = connection.base_url.trim_end_matches('/');
        if !(base.starts_with("https://")
            || base.starts_with("http://127.0.0.1:")
            || base.starts_with("http://localhost:"))
            || connection.trusted_public_keys.is_empty()
        {
            return Err(
                "Curation subscription needs a trusted HTTPS (or loopback) connection".into(),
            );
        }
        if cache_path.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err("Curation cache path must be host-supplied and absolute".into());
        }
        let (stop, receiver) = watch::channel(false);
        let worker = thread::Builder::new()
            .name("talos-curation-events".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        store.unavailable(format!("Curation runtime unavailable: {error}"));
                        return;
                    }
                };
                runtime.block_on(run(connection, store, cache_path, receiver));
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
    pub fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }
    pub fn shutdown(mut self) -> Result<(), String> {
        self.stop();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "Curation subscriber panicked".to_string())?;
        }
        Ok(())
    }
}
impl Drop for CurationSubscriber {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct Frames {
    bytes: Vec<u8>,
}
impl Frames {
    fn push(&mut self, chunk: &[u8]) -> Result<Option<u64>, String> {
        let mut hint = None;
        // Process bytes incrementally; a huge transport chunk containing many
        // short events does not require a huge retained parser buffer.
        for &byte in chunk {
            if byte != b'\r' {
                self.bytes.push(byte);
            }
            if self.bytes.len() > MAX_FRAME_BYTES {
                return Err("Curation SSE frame exceeds limit".into());
            }
            if self.bytes.ends_with(b"\n\n") {
                let frame =
                    std::str::from_utf8(&self.bytes).map_err(|_| "Invalid curation SSE UTF-8")?;
                let mut event = "";
                let mut data = None;
                for line in frame.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim();
                    }
                    if let Some(v) = line.strip_prefix("data:") {
                        if data.is_some() {
                            return Err("Curation SSE hint must be one line".into());
                        }
                        data = Some(v.trim());
                    }
                }
                if event == "release_sequence" {
                    let sequence = data
                        .ok_or("Missing curation sequence")?
                        .parse::<u64>()
                        .map_err(|_| "Invalid curation sequence")?;
                    hint = Some(hint.map_or(sequence, |old: u64| old.max(sequence)));
                }
                self.bytes.clear();
            }
        }
        Ok(hint)
    }
}
async fn refresh(
    connection: &CurationConnection,
    store: &CurationReleaseStore,
    cache: &mut Cache,
    path: Option<&Path>,
) -> Result<(), String> {
    let expected = authority(&connection.base_url, &connection.trusted_public_keys);
    let bytes = fetch_envelope(connection).await?;
    let manifest = decode_manifest(&bytes, &connection.trusted_public_keys)?;
    let sequence = manifest.sequence;
    store.accept_expected(manifest, Some(&expected))?;
    if let Some(path) = path {
        let result = cache
            .record(&bytes, connection)
            .and_then(|_| cache.write_atomic(path));
        let mut state = store.0.lock().expect("curation state poisoned");
        if result.is_ok() {
            state.cache_restored = true;
            state.cached_release_sequence = Some(sequence);
        }
        state.cache_error = result.err();
    }
    Ok(())
}
async fn connected(
    connection: &CurationConnection,
    store: &CurationReleaseStore,
    cache: &mut Cache,
    path: Option<&Path>,
    client: &reqwest::Client,
) -> Result<(), String> {
    let mut response = client
        .get(format!(
            "{}/events",
            connection.base_url.trim_end_matches('/')
        ))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .map_err(|_| "Curation event connection failed")?;
    if !response.status().is_success() {
        return Err(format!(
            "Curation events HTTP {}",
            response.status().as_u16()
        ));
    }
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
    {
        return Err("Curation event endpoint is not SSE".into());
    }
    store
        .0
        .lock()
        .expect("curation state poisoned")
        .service_events_connected = true;
    // Connect before reconciling so a publication racing the fetch is queued
    // on this stream. Reconnect always fetches, even if all old hints were lost.
    refresh(connection, store, cache, path).await?;
    let mut frames = Frames::default();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(45), response.chunk())
            .await
            .map_err(|_| "Curation event heartbeat timed out")?
            .map_err(|_| "Curation event read failed")?
            .ok_or("Curation event stream closed")?;
        if let Some(sequence) = frames.push(&chunk)? {
            let current = store
                .0
                .lock()
                .expect("curation state poisoned")
                .manifest
                .as_ref()
                .map(|m| m.sequence);
            if current.is_none_or(|current| sequence > current) {
                refresh(connection, store, cache, path).await?;
            }
        }
    }
}
async fn run(
    connection: CurationConnection,
    store: CurationReleaseStore,
    path: Option<PathBuf>,
    mut stop: watch::Receiver<bool>,
) {
    store.bind(&connection);
    let mut cache = Cache::empty(&connection);
    if let Some(path) = path.as_deref().filter(|p| p.exists()) {
        match Cache::load(path, &connection).and_then(|loaded| {
            loaded.restore(&connection, &store)?;
            Ok(loaded)
        }) {
            Ok(loaded) => cache = loaded,
            Err(error) => {
                store.0.lock().expect("curation state poisoned").cache_error = Some(error)
            }
        }
    }
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            store.unavailable(error.to_string());
            return;
        }
    };
    let mut backoff = Duration::from_millis(250);
    loop {
        if *stop.borrow() {
            break;
        }
        let outcome = tokio::select! {_ = stop.changed()=>break, result=connected(&connection,&store,&mut cache,path.as_deref(),&client)=>result};
        store
            .0
            .lock()
            .expect("curation state poisoned")
            .service_events_connected = false;
        if let Err(error) = outcome {
            store.unavailable(error);
        }
        tokio::select! {_ = stop.changed()=>break,_ = tokio::time::sleep(backoff)=>{}}
        backoff = (backoff * 2).min(Duration::from_secs(15));
    }
    store
        .0
        .lock()
        .expect("curation state poisoned")
        .service_events_connected = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn signed(key: &SigningKey, sequence: u64, withdrawn: bool) -> Vec<u8> {
        let mut value = json!({"schema":RELEASE_SCHEMA,"sequence":sequence,"releases":[{
            "artifact_id":"skill:test.cached","revision":"r1","remedy_keys":[{"gap_kind":"missing_path"}],
            "required_tool_ids":[],"withdrawn":withdrawn,"refresh_required":withdrawn
        }],"public_key":hex::encode(key.verifying_key().to_bytes())});
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&value).unwrap()).into();
        value["signature"] = json!(hex::encode(key.sign(&digest).to_bytes()));
        serde_json::to_vec(&value).unwrap()
    }
    fn connection(key: &SigningKey) -> CurationConnection {
        CurationConnection {
            base_url: "http://127.0.0.1:1/v1/curation".into(),
            trusted_public_keys: vec![hex::encode(key.verifying_key().to_bytes())],
        }
    }
    fn scratch() -> PathBuf {
        std::env::temp_dir()
            .join(format!("talos-curation-test-{}", uuid::Uuid::new_v4()))
            .join("releases.json")
    }
    #[test]
    fn signed_cache_restores_withdrawal_and_rejects_source_or_signature_tampering() {
        let key = SigningKey::from_bytes(&[19; 32]);
        let connection = connection(&key);
        let path = scratch();
        let mut cache = Cache::empty(&connection);
        cache.record(&signed(&key, 2, true), &connection).unwrap();
        cache.write_atomic(&path).unwrap();
        let fresh = CurationReleaseStore::default();
        fresh.bind(&connection);
        fresh
            .observe_skill_index(
                &AgentCorpusConnection::default(),
                &agent_skill_cloud::SkillIndex {
                    schema: agent_skill_cloud::SCHEMA.into(),
                    epoch: 1,
                    skills: vec![],
                    withdrawn_skill_ids: vec!["legacy.withdrawn".into()],
                },
            )
            .unwrap();
        Cache::load(&path, &connection)
            .unwrap()
            .restore(&connection, &fresh)
            .unwrap();
        assert!(fresh.revision_withdrawn("skill:test.cached", "r1"));
        assert!(fresh.skill_fallback_blocked("legacy.withdrawn"));
        assert_eq!(
            fresh.status(true, &BTreeSet::new(), None)["freshness"],
            "unverified"
        );
        let mut other = connection.clone();
        other.base_url = "http://127.0.0.1:2/v1/curation".into();
        assert!(Cache::load(&path, &other).is_err());
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["envelopes"][0]["releases"][0]["withdrawn"] = json!(false);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Cache::load(&path, &connection).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn cache_keeps_signed_proof_when_new_snapshot_omits_old_withdrawal() {
        let key = SigningKey::from_bytes(&[20; 32]);
        let connection = connection(&key);
        let mut cache = Cache::empty(&connection);
        cache.record(&signed(&key, 2, true), &connection).unwrap();
        let mut value: Value = serde_json::from_slice(&signed(&key, 3, false)).unwrap();
        value.as_object_mut().unwrap().remove("signature");
        value["releases"][0]["revision"] = json!("r2");
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&value).unwrap()).into();
        value["signature"] = json!(hex::encode(key.sign(&digest).to_bytes()));
        cache
            .record(&serde_json::to_vec(&value).unwrap(), &connection)
            .unwrap();
        assert_eq!(cache.envelopes.len(), 2);
        let restored = CurationReleaseStore::default();
        restored.bind(&connection);
        cache.restore(&connection, &restored).unwrap();
        assert!(restored.revision_withdrawn("skill:test.cached", "r1"));
        assert!(!restored.revision_withdrawn("skill:test.cached", "r2"));
    }
    #[test]
    fn event_frames_are_bounded_fragmented_and_coalesced() {
        let mut frames = Frames::default();
        assert_eq!(
            frames
                .push(b":keepalive\r\n\r\nevent: release_seq")
                .unwrap(),
            None
        );
        assert_eq!(
            frames
                .push(b"uence\ndata: 4\n\nevent: release_sequence\ndata: 8\n\n")
                .unwrap(),
            Some(8)
        );
        assert!(frames.push(&vec![b'x'; MAX_FRAME_BYTES + 1]).is_err());
        assert!(Frames::default()
            .push(b"event: release_sequence\ndata: private-id\n\n")
            .is_err());
    }
    async fn wait_sequence(store: &CurationReleaseStore, sequence: u64) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store.status(true, &BTreeSet::new(), None)["release_sequence"] == sequence {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("signed sequence should reach the host");
    }
    #[tokio::test]
    async fn native_events_deliver_reconnect_and_restore_cached_withdrawals() {
        let key = SigningKey::from_bytes(&[21; 32]);
        let mut connection = connection(&key);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        connection.base_url = format!("http://{address}/v1/curation");
        let body = Arc::new(Mutex::new(signed(&key, 1, false)));
        let (hints, _) = tokio::sync::broadcast::channel::<u64>(16);
        let (halt, mut halted) = watch::channel(false);
        let server_body = body.clone();
        let server_hints = hints.clone();
        let server = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _=halted.changed()=>break,
                    accepted=listener.accept()=>{
                        let (mut socket,_)=accepted.unwrap();let body=server_body.clone();let mut hints=server_hints.subscribe();
                        children.spawn(async move{
                            let mut request=Vec::new();let mut bytes=[0u8;1024];
                            while !request.ends_with(b"\r\n\r\n")&&request.len()<8192 {let count=socket.read(&mut bytes).await.unwrap_or(0);if count==0{return;}request.extend_from_slice(&bytes[..count]);}
                            if request.starts_with(b"GET /v1/curation/releases "){
                                let value=body.lock().unwrap().clone();let header=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",value.len());
                                let _=socket.write_all(header.as_bytes()).await;let _=socket.write_all(&value).await;
                            }else if request.starts_with(b"GET /v1/curation/events "){
                                if socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n:connected\n\n").await.is_err(){return;}
                                while let Ok(sequence)=hints.recv().await{if sequence==u64::MAX{return;}if socket.write_all(format!("event: release_sequence\ndata: {sequence}\n\n").as_bytes()).await.is_err(){return;}}
                            }
                        });
                    }
                }
            }
            children.abort_all();
            while children.join_next().await.is_some() {}
        });
        let path = scratch();
        let store = CurationReleaseStore::default();
        let subscription =
            CurationSubscriber::start(connection.clone(), store.clone(), Some(path.clone()))
                .unwrap();
        wait_sequence(&store, 1).await;
        *body.lock().unwrap() = signed(&key, 2, true);
        hints.send(2).unwrap();
        wait_sequence(&store, 2).await;
        assert!(store.revision_withdrawn("skill:test.cached", "r1"));
        hints.send(u64::MAX).unwrap();
        *body.lock().unwrap() = signed(&key, 3, true);
        wait_sequence(&store, 3).await;
        subscription.shutdown().unwrap();
        assert!(
            !store.status(true, &BTreeSet::new(), None)["service_events_connected"]
                .as_bool()
                .unwrap()
        );
        halt.send(true).unwrap();
        server.await.unwrap();
        let restarted = CurationReleaseStore::default();
        let offline =
            CurationSubscriber::start(connection, restarted.clone(), Some(path.clone())).unwrap();
        wait_sequence(&restarted, 3).await;
        assert!(restarted.revision_withdrawn("skill:test.cached", "r1"));
        assert_eq!(
            restarted.status(true, &BTreeSet::new(), None)["freshness"],
            "unverified"
        );
        offline.shutdown().unwrap();
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
