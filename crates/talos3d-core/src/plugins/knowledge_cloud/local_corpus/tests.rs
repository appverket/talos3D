use std::{future::Future, path::Path, time::Duration};

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::capability_registry::{CapabilityRegistry, CatalogProviderId, PassageRef};
use crate::plugins::{
    agent_skills::AgentSkill,
    corpus_gap::CorpusPassageRegistry,
    knowledge_cloud::{Asset, SCHEMA},
};

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}
fn public_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Sign `value` exactly as the service does: SHA-256 over the canonical JSON
/// without the signature, ed25519 over that digest.
fn sign(key: &SigningKey, mut value: Value) -> Vec<u8> {
    value["public_key"] = json!(public_hex(key));
    let digest = Sha256::digest(serde_json::to_vec(&value).unwrap());
    value["signature"] = json!(hex::encode(key.sign(&digest).to_bytes()));
    serde_json::to_vec(&value).unwrap()
}

fn asset(id: &str, kind: &str, path_kind: &str, payload: Value) -> Asset {
    Asset {
        schema: SCHEMA.into(),
        id: id.into(),
        kind: kind.into(),
        title: "Synthetic".into(),
        description: "Local corpus cache fixture".into(),
        element_classes: vec![],
        path_kind: path_kind.into(),
        required_tool_ids: vec![],
        payload,
        applicability: "Unit tests".into(),
        limitations: vec![],
        evidence: vec!["Synthetic".into()],
        redistribution_rights: "Original synthetic fixture".into(),
        contains_private_context: false,
    }
}
fn passage(text: &str) -> Asset {
    asset(
        "test.cache.passage",
        "corpus_passage.v1",
        "passage",
        json!({"passage_ref":"test.cache.passage","text":text,
            "citation":"Synthetic","license":"cc0"}),
    )
}
fn catalog(version: &str) -> Asset {
    asset(
        "test.cache.catalog",
        "catalog.v1",
        "catalog",
        json!({"id":"test.cache.catalog","label":"Synthetic catalog","description":"fixture",
            "category":{"kind":"Other","value":"fixture"},"region":null,
            "license":{"kind":"Cc0"},"source_version":version,"rows":[]}),
    )
}
fn skill(body: &str) -> SkillBundle {
    let skill: AgentSkill = serde_json::from_value(json!({
        "id":"test.cache.skill","title":"Skill","summary":"A cached skill","body_markdown":body
    }))
    .unwrap();
    SkillBundle {
        schema: cloud::SCHEMA.into(),
        skill,
        resources: BTreeMap::new(),
        provenance: Value::Null,
    }
}

/// Everything the service would publish at one epoch.
struct Release {
    epoch: i64,
    assets: Vec<Asset>,
    skills: Vec<SkillBundle>,
}

impl Release {
    fn blobs(&self) -> Vec<(String, Vec<u8>)> {
        let assets = self.assets.iter().map(|a| serde_json::to_vec(a).unwrap());
        let skills = self.skills.iter().map(|s| serde_json::to_vec(s).unwrap());
        assets.chain(skills).map(|b| (sha(&b), b)).collect()
    }
    fn assets_index(&self, key: &SigningKey) -> Vec<u8> {
        let assets: Vec<_> = self
            .assets
            .iter()
            .enumerate()
            .map(|(i, a)| {
                json!({"content_hash":sha(&serde_json::to_vec(a).unwrap()),
                    "asset":{"id":a.id,"asset_kind":a.kind,"path_kind":a.path_kind},
                    "sequence":i})
            })
            .collect();
        sign(
            key,
            json!({"schema":SCHEMA,"epoch":self.epoch,"assets":assets,"withdrawn_asset_ids":[]}),
        )
    }
    fn skills_index(&self, key: &SigningKey) -> Vec<u8> {
        let skills: Vec<_> = self
            .skills
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({"content_hash":sha(&serde_json::to_vec(s).unwrap()),
                    "skill":s.skill.summary_info(),"sequence":i})
            })
            .collect();
        sign(
            key,
            json!({"schema":cloud::SCHEMA,"epoch":self.epoch,"skills":skills,"withdrawn_skill_ids":[]}),
        )
    }
}

type Routes = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

/// A loopback corpus service on its own thread. Returns its base URL and the
/// mutable route table.
fn serve() -> (String, Routes) {
    let routes: Routes = Arc::default();
    let table = routes.clone();
    let (port_tx, port_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                let table = table.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let body = table.lock().unwrap().get(&path).cloned();
                    let head = if body.is_some() {
                        "200 OK"
                    } else {
                        "404 Not Found"
                    };
                    let body = body.unwrap_or_default();
                    let response = format!(
                        "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
    });
    (
        format!("http://127.0.0.1:{}", port_rx.recv().unwrap()),
        routes,
    )
}

fn publish(routes: &Routes, key: &SigningKey, release: &Release) {
    let mut table = routes.lock().unwrap();
    table.insert("/assets".into(), release.assets_index(key));
    table.insert("/skills".into(), release.skills_index(key));
    for (hash, bytes) in release.blobs() {
        table.insert(format!("/revisions/{hash}"), bytes);
    }
}

fn connection(url: &str, key: &SigningKey) -> AgentCorpusConnection {
    AgentCorpusConnection {
        base_url: url.into(),
        bearer_token: None,
        trusted_public_keys: vec![public_hex(key)],
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

struct Harness {
    worker: Worker,
    corpus: LocalCorpus,
    updates: mpsc::Receiver<AssetUpdate>,
    store: LocalCorpusStore,
}

fn harness(root: &Path, connection: &AgentCorpusConnection) -> Harness {
    let store = LocalCorpusStore::open(root, connection, DEFAULT_MAX_BYTES).unwrap();
    let (tx, updates) = mpsc::channel();
    let corpus = LocalCorpus::new(LocalCorpusConfig::default(), tx);
    *corpus.shared.active.lock().unwrap() = Some(Active {
        store: store.clone(),
        connection: connection.clone(),
    });
    Harness {
        worker: Worker {
            baseline: load_baseline(&store, connection),
            connection: connection.clone(),
            store: store.clone(),
            curation: CurationReleaseStore::default(),
            shared: corpus.shared.clone(),
        },
        corpus,
        updates,
        store,
    }
}

fn apply(world: &mut World, harness: &Harness) {
    let update = harness.updates.try_recv().expect("a sync update");
    apply_update(world, &harness.corpus, update.changed, Some(update.index));
}

/// Persist `release` into the cache the way a completed sync does.
fn seed_cache(root: &Path, connection: &AgentCorpusConnection, key: &SigningKey, r: &Release) {
    let store = LocalCorpusStore::open(root, connection, DEFAULT_MAX_BYTES).unwrap();
    for (hash, bytes) in r.blobs() {
        store.put_blob(&hash, &bytes).unwrap();
    }
    store
        .write_index(IndexKind::Assets, &r.assets_index(key))
        .unwrap();
    store
        .write_index(IndexKind::Skills, &r.skills_index(key))
        .unwrap();
}

fn release(epoch: i64, text: &str, version: &str) -> Release {
    Release {
        epoch,
        assets: vec![passage(text), catalog(version)],
        skills: vec![skill(text)],
    }
}

fn passage_text(world: &World) -> Option<String> {
    world
        .get_resource::<CorpusPassageRegistry>()?
        .get(&PassageRef("test.cache.passage".into()))
        .map(|p| p.text.clone())
}
fn catalog_version(world: &World) -> Option<String> {
    world
        .get_resource::<CapabilityRegistry>()?
        .catalog_provider_descriptor(&CatalogProviderId("test.cache.catalog".into()))
        .map(|d| d.source_version.clone())
}

/// An app started with the cache plugin against `connection`; one update runs
/// the startup schedules.
fn start_app(root: &Path, connection: AgentCorpusConnection) -> App {
    let mut app = App::new();
    app.insert_resource(connection);
    app.add_plugins(LocalCorpusPlugin(LocalCorpusConfig {
        cache_dir: Some(root.to_path_buf()),
        ..Default::default()
    }));
    app.update();
    app
}

#[test]
fn cold_start_installs_every_listed_kind_from_cache_without_network() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    // Nothing listens here: the snapshot must install from disk alone.
    let conn = connection("http://127.0.0.1:9", &signer);
    seed_cache(root.path(), &conn, &signer, &release(5, "from-cache", "v1"));
    let app = start_app(root.path(), conn);
    let world = app.world();
    assert_eq!(passage_text(world).as_deref(), Some("from-cache"));
    assert_eq!(catalog_version(world).as_deref(), Some("v1"));
    assert_eq!(world.resource::<InstalledAssets>().0.len(), 2);
    let status = world.resource::<LocalCorpus>().status_json();
    assert_eq!(status["startup"]["assets_installed"], 2);
    assert_eq!(status["startup"]["epoch"], 5);
    assert_eq!(status["cache"]["asset_count"], 2);
    assert_eq!(status["cache"]["skill_count"], 1);
    assert!(world
        .resource::<LocalCorpus>()
        .cached_skill_index()
        .is_some());
}

#[test]
fn first_start_without_cache_or_network_installs_nothing() {
    let root = tempfile::tempdir().unwrap();
    let app = start_app(root.path(), connection("http://127.0.0.1:9", &key(1)));
    let world = app.world();
    assert!(passage_text(world).is_none());
    assert!(world.get_resource::<InstalledAssets>().is_none());
    let status = world.resource::<LocalCorpus>().status_json();
    assert_eq!(status["startup"]["cache_found"], false);
    assert_eq!(status["enabled"], true);
}

#[test]
fn unconfigured_connection_leaves_the_cache_disabled() {
    let root = tempfile::tempdir().unwrap();
    let app = start_app(root.path(), AgentCorpusConnection::default());
    let status = app.world().resource::<LocalCorpus>().status_json();
    assert_eq!(status["enabled"], false);
    assert_eq!(status["sync_state"], "disabled");
}

#[test]
fn cache_signed_by_another_key_is_rejected_and_ignored() {
    let root = tempfile::tempdir().unwrap();
    let (trusted, attacker) = (key(1), key(2));
    let conn = connection("http://127.0.0.1:9", &trusted);
    // Same directory, content signed by a key the app does not trust.
    seed_cache(root.path(), &conn, &attacker, &release(5, "forged", "v1"));
    let app = start_app(root.path(), conn);
    assert!(passage_text(app.world()).is_none());
    let status = app.world().resource::<LocalCorpus>().status_json();
    assert!(status["startup"]["cache_rejected"].is_string());
}

#[test]
fn tampered_index_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let conn = connection("http://127.0.0.1:9", &signer);
    let r = release(5, "genuine", "v1");
    seed_cache(root.path(), &conn, &signer, &r);
    let store = LocalCorpusStore::open(root.path(), &conn, DEFAULT_MAX_BYTES).unwrap();
    let tampered = String::from_utf8(r.assets_index(&signer))
        .unwrap()
        .replace("\"epoch\":5", "\"epoch\":6");
    store
        .write_index(IndexKind::Assets, tampered.as_bytes())
        .unwrap();
    let app = start_app(root.path(), conn);
    assert!(passage_text(app.world()).is_none());
}

#[test]
fn tampered_blob_drops_only_that_asset() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let conn = connection("http://127.0.0.1:9", &signer);
    let r = release(5, "genuine", "v1");
    seed_cache(root.path(), &conn, &signer, &r);
    let store = LocalCorpusStore::open(root.path(), &conn, DEFAULT_MAX_BYTES).unwrap();
    let hash = sha(&serde_json::to_vec(&r.assets[0]).unwrap());
    std::fs::write(store.blob_path(&hash), b"{\"tampered\":true}").unwrap();
    let app = start_app(root.path(), conn);
    let world = app.world();
    assert!(passage_text(world).is_none());
    assert_eq!(catalog_version(world).as_deref(), Some("v1"));
    let status = world.resource::<LocalCorpus>().status_json();
    assert_eq!(status["startup"]["assets_installed"], 1);
    assert_eq!(
        status["startup"]["cache_problems"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn sync_installs_a_new_revision_and_withdraws_a_removed_asset() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let (url, routes) = serve();
    let conn = connection(&url, &signer);
    publish(&routes, &signer, &release(1, "one", "v1"));
    let mut h = harness(root.path(), &conn);
    let mut world = World::new();

    block_on(h.worker.sync_once()).unwrap();
    apply(&mut world, &h);
    assert_eq!(passage_text(&world).as_deref(), Some("one"));
    assert_eq!(catalog_version(&world).as_deref(), Some("v1"));
    assert!(h.store.read_index(IndexKind::Assets).unwrap().is_some());

    // Epoch 2: the passage gets a new revision, the catalog is withdrawn.
    let second = Release {
        epoch: 2,
        assets: vec![passage("two")],
        skills: vec![skill("two")],
    };
    publish(&routes, &signer, &second);
    block_on(h.worker.sync_once()).unwrap();
    apply(&mut world, &h);
    assert_eq!(passage_text(&world).as_deref(), Some("two"));
    assert!(catalog_version(&world).is_none());
    assert_eq!(world.resource::<InstalledAssets>().0.len(), 1);
    assert_eq!(h.corpus.status_json()["applied_epoch"], 2);

    // Withdrawn and superseded blobs are evicted; referenced ones remain.
    let expected: BTreeSet<_> = second.blobs().into_iter().map(|(hash, _)| hash).collect();
    assert_eq!(h.store.blob_hashes().unwrap(), expected);
}

#[test]
fn unchanged_sync_downloads_nothing_and_stays_verified() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let (url, routes) = serve();
    let conn = connection(&url, &signer);
    let r = release(1, "one", "v1");
    publish(&routes, &signer, &r);
    let mut h = harness(root.path(), &conn);
    block_on(h.worker.sync_once()).unwrap();
    let mut world = World::new();
    apply(&mut world, &h);
    // Make revisions unfetchable: a no-op sync must not need them.
    routes
        .lock()
        .unwrap()
        .retain(|path, _| !path.starts_with("/revisions/"));
    block_on(h.worker.sync_once()).unwrap();
    let update = h.updates.try_recv().unwrap();
    assert!(update.changed.is_empty());
    assert_eq!(update.index.epoch, 1);
}

#[test]
fn older_epoch_and_foreign_signatures_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let (url, routes) = serve();
    let conn = connection(&url, &signer);
    publish(&routes, &signer, &release(5, "five", "v1"));
    let mut h = harness(root.path(), &conn);
    block_on(h.worker.sync_once()).unwrap();
    let cached = h.store.read_index(IndexKind::Assets).unwrap();

    publish(&routes, &signer, &release(4, "rollback", "v0"));
    let error = block_on(h.worker.sync_once()).unwrap_err();
    assert!(
        error.contains("older") || error.contains("rollback"),
        "{error}"
    );
    assert_eq!(h.store.read_index(IndexKind::Assets).unwrap(), cached);

    publish(&routes, &key(2), &release(9, "forged", "v9"));
    assert!(block_on(h.worker.sync_once()).is_err());
    assert_eq!(h.store.read_index(IndexKind::Assets).unwrap(), cached);
}

#[test]
fn a_blob_that_does_not_match_its_hash_is_not_cached() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let (url, routes) = serve();
    let conn = connection(&url, &signer);
    let r = release(1, "one", "v1");
    publish(&routes, &signer, &r);
    let victim = sha(&serde_json::to_vec(&r.assets[0]).unwrap());
    routes
        .lock()
        .unwrap()
        .insert(format!("/revisions/{victim}"), b"not the revision".to_vec());
    let mut h = harness(root.path(), &conn);
    assert!(block_on(h.worker.sync_once()).is_err());
    assert!(!h.store.has_blob(&victim));
    // The index is only persisted once every blob it lists is on disk.
    assert!(h.store.read_index(IndexKind::Assets).unwrap().is_none());
}

#[test]
fn offline_with_a_cache_reports_age_and_offline_state() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let conn = connection("http://127.0.0.1:9", &signer);
    seed_cache(root.path(), &conn, &signer, &release(3, "cached", "v1"));
    let mut h = harness(root.path(), &conn);
    h.store
        .write_sync_record(&SyncRecord {
            last_success_unix: Some(now_unix() - 120),
        })
        .unwrap();
    let result = block_on(h.worker.sync_once());
    assert!(result.is_err());
    h.worker.record(result);
    let status = h.corpus.status_json();
    assert_eq!(status["sync_state"], "offline");
    assert_eq!(status["cache"]["asset_epoch"], 3);
    assert!(status["cache_age_seconds"].as_u64().unwrap() >= 120);
    assert!(status["last_error"].is_string());
}

#[test]
fn skills_are_served_from_the_verified_local_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let (url, routes) = serve();
    let conn = connection(&url, &signer);
    let r = release(1, "skill-body", "v1");
    publish(&routes, &signer, &r);
    let mut h = harness(root.path(), &conn);
    block_on(h.worker.sync_once()).unwrap();
    let index = h.corpus.cached_skill_index().unwrap();
    assert_eq!(index.skills.len(), 1);
    let bundle = h
        .corpus
        .cached_skill_bundle(&index.skills[0].content_hash)
        .unwrap();
    assert_eq!(bundle.skill.body_markdown, "skill-body");
}

#[test]
fn a_full_cache_relieves_itself_of_unreferenced_blobs() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let (url, routes) = serve();
    let conn = connection(&url, &signer);
    publish(&routes, &signer, &release(1, "one", "v1"));
    let mut h = harness(root.path(), &conn);
    block_on(h.worker.sync_once()).unwrap();
    let used = h.store.size_bytes().unwrap();
    // A bound that fits one release but not two at once.
    h.store = LocalCorpusStore::open(root.path(), &conn, used + 64).unwrap();
    h.worker.store = h.store.clone();
    let second = release(2, "two-two-two", "v2");
    publish(&routes, &signer, &second);
    block_on(h.worker.sync_once()).unwrap();
    let expected: BTreeSet<_> = second.blobs().into_iter().map(|(hash, _)| hash).collect();
    assert_eq!(h.store.blob_hashes().unwrap(), expected);
}

#[test]
fn the_running_app_picks_up_a_published_revision_in_the_background() {
    let root = tempfile::tempdir().unwrap();
    let signer = key(1);
    let (url, routes) = serve();
    publish(&routes, &signer, &release(1, "one", "v1"));
    let mut app = App::new();
    app.insert_resource(connection(&url, &signer));
    app.add_plugins(LocalCorpusPlugin(LocalCorpusConfig {
        cache_dir: Some(root.path().to_path_buf()),
        min_refresh: Duration::ZERO,
        ..Default::default()
    }));
    app.update();
    let wait = |app: &mut App, done: &dyn Fn(&World) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(app.world()) {
            assert!(Instant::now() < deadline, "sync did not arrive");
            std::thread::sleep(Duration::from_millis(50));
            app.update();
        }
    };
    wait(&mut app, &|w| passage_text(w).as_deref() == Some("one"));
    publish(&routes, &signer, &release(2, "two", "v2"));
    app.world().resource::<LocalCorpus>().request_sync();
    wait(&mut app, &|w| passage_text(w).as_deref() == Some("two"));
    assert_eq!(catalog_version(app.world()).as_deref(), Some("v2"));
    assert_eq!(
        app.world().resource::<LocalCorpus>().status_json()["sync_state"],
        "synced"
    );
}
