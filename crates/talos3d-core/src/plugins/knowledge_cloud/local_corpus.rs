//! Synced, verified local copy of the shared knowledge corpus.
//!
//! The app starts from its last verified corpus snapshot and keeps it in sync
//! with the backend, so corpus knowledge never depends on network reachability
//! at startup.
//!
//! * **Store** ([`store`]): content-addressed on-disk cache of the last verified
//!   signed indexes (`/assets`, `/skills`) and every revision blob by SHA-256.
//! * **Startup** ([`install_cached_corpus`], `PostStartup`): re-verifies the
//!   cached index signature against the configured public key, re-hashes each
//!   blob and installs every listed asset through the ordinary
//!   [`install`](super::install) path. Runs inside the first `Main` run, before
//!   `First` where MCP requests are polled, so no tool call can observe the
//!   registries before the snapshot is installed.
//! * **Sync** ([`Worker`]): a background thread (never the frame) fetches the
//!   backend indexes at startup, on an interval, when the existing curation
//!   release sequence changes, and on request. Only verified, not-older indexes
//!   are accepted; missing blobs are downloaded, verified and persisted before
//!   the index is replaced. Changes reach the live world through a bounded
//!   per-frame apply queue that ends in [`reconcile`](super::reconcile), so
//!   withdrawals and replaced revisions leave the registries.
//!
//! Nothing here falls back to compiled knowledge: with no cache and no network
//! the registries stay empty and discovery reports explicit gaps.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bevy::{
    prelude::*,
    winit::{EventLoopProxy, EventLoopProxyWrapper, WinitUserEvent},
};
use serde_json::{json, Value};
use tokio::{sync::watch, task::JoinSet};

use super::{
    admit_revision, decode_index, install, reconcile, AdmittedAsset, Index, InstalledAssets,
};
use crate::plugins::{
    agent_skill_cloud::{self as cloud, SkillBundle, SkillIndex},
    agent_skills::AgentCorpusConnection,
    curation::CurationReleaseStore,
};

pub mod store;
pub use store::{IndexKind, LocalCorpusStore, StoreError, SyncRecord, DEFAULT_MAX_BYTES};

/// Default interval between background syncs.
pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::from_secs(300);
/// Assets installed per frame while applying a sync result.
const APPLY_BATCH: usize = 16;
/// Concurrent blob downloads.
const DOWNLOAD_CONCURRENCY: usize = 8;
/// Granularity at which the worker checks for triggers.
const TICK: Duration = Duration::from_secs(2);
const RETRY_START: Duration = Duration::from_secs(5);
/// Bound on retained problem strings in the status.
const MAX_REPORTED: usize = 20;

/// Host configuration of the local corpus cache.
#[derive(Clone, Debug)]
pub struct LocalCorpusConfig {
    /// Absolute cache root. `None` resolves `TALOS3D_CORPUS_CACHE_DIR`, then
    /// `$TALOS3D_STATE_DIR/corpus-cache`, then the per-user data directory.
    pub cache_dir: Option<PathBuf>,
    /// Interval between background syncs.
    pub sync_interval: Duration,
    /// Upper bound of the cache size in bytes.
    pub max_bytes: u64,
    /// Minimum spacing between refreshes triggered by
    /// [`LocalCorpus::request_sync`], so status polling cannot hammer the backend.
    pub min_refresh: Duration,
}

impl Default for LocalCorpusConfig {
    fn default() -> Self {
        Self {
            cache_dir: None,
            sync_interval: DEFAULT_SYNC_INTERVAL,
            max_bytes: DEFAULT_MAX_BYTES,
            min_refresh: Duration::from_secs(10),
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn resolve_cache_dir(config: &LocalCorpusConfig) -> Result<PathBuf, String> {
    use etcetera::AppStrategy;
    let dir = if let Some(dir) = &config.cache_dir {
        dir.clone()
    } else if let Some(dir) = std::env::var_os("TALOS3D_CORPUS_CACHE_DIR") {
        PathBuf::from(dir)
    } else if let Some(dir) = std::env::var_os("TALOS3D_STATE_DIR") {
        PathBuf::from(dir).join("corpus-cache")
    } else {
        etcetera::choose_app_strategy(etcetera::AppStrategyArgs {
            top_level_domain: "com".to_owned(),
            author: "appverket".to_owned(),
            app_name: "talos3d".to_owned(),
        })
        .map_err(|e| format!("data directory: {e}"))?
        .data_dir()
        .join("corpus-cache")
    };
    if dir.is_absolute() {
        Ok(dir)
    } else {
        Err("Corpus cache directory must be absolute".into())
    }
}

/// Why the cache is not active, or `None` when it should run.
fn disabled_reason(connection: Option<&AgentCorpusConnection>) -> Option<&'static str> {
    if std::env::var("TALOS3D_CORPUS_CACHE").as_deref() == Ok("0") {
        return Some("disabled by TALOS3D_CORPUS_CACHE=0");
    }
    if std::env::var_os("TALOS3D_EVALUATION_KNOWLEDGE_CONNECTION").is_some() {
        return Some("evaluation instance uses scoped admission, never the shared cache");
    }
    match connection {
        Some(c) if !c.base_url.is_empty() && !c.trusted_public_keys.is_empty() => None,
        _ => Some("no corpus connection with a trusted public key is configured"),
    }
}

/// The verified contents of a cached assets index.
#[derive(Debug)]
pub struct CachedAssets {
    /// The verified index.
    pub index: Index,
    /// Every listed asset whose blob verified and admitted.
    pub admitted: Vec<AdmittedAsset>,
    /// Listed assets that could not be admitted from the cache.
    pub problems: Vec<String>,
}

/// `(asset id, content hash, indexed kind)` of each index entry.
fn entries(index: &Index) -> impl Iterator<Item = (&str, &str, &Value)> {
    index.assets.iter().filter_map(|e| {
        Some((
            e.asset["id"].as_str()?,
            e.content_hash.as_str(),
            &e.asset["asset_kind"],
        ))
    })
}

/// Load, verify and admit the cached assets index.
///
/// # Errors
/// The cached index fails signature, schema or scope verification against the
/// configured keys (for example it was signed by a different key).
/// `Ok(None)` when nothing is cached. A blob that is missing or corrupt only
/// drops that asset into [`CachedAssets::problems`].
pub fn load_cached_assets(
    store: &LocalCorpusStore,
    connection: &AgentCorpusConnection,
) -> Result<Option<CachedAssets>, String> {
    let Some(bytes) = store
        .read_index(IndexKind::Assets)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let index = decode_index(&bytes, &connection.trusted_public_keys, None)?;
    let mut admitted = Vec::new();
    let mut problems = Vec::new();
    for (id, hash, kind) in entries(&index) {
        let result = store
            .get_blob(hash)
            .map_err(|e| e.to_string())
            .and_then(|blob| blob.ok_or_else(|| "blob missing".to_owned()))
            .and_then(|blob| admit_revision(&blob, id, hash, kind, None));
        match result {
            Ok(asset) => admitted.push(asset),
            Err(error) => problems.push(format!("{id}@{hash}: {error}")),
        }
    }
    Ok(Some(CachedAssets {
        index,
        admitted,
        problems,
    }))
}

/// Install `assets` not already installed at the same revision. Returns the
/// number installed and the per-asset failures.
fn install_assets(world: &mut World, assets: Vec<AdmittedAsset>) -> (usize, Vec<String>) {
    let mut installed = 0;
    let mut failures = Vec::new();
    for asset in assets {
        let current = world
            .get_resource::<InstalledAssets>()
            .and_then(|r| r.0.get(&asset.asset.id))
            .is_some_and(|i| i.revision == asset.revision);
        if current {
            continue;
        }
        let id = asset.asset.id.clone();
        match install(world, asset) {
            Ok(_) => installed += 1,
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    (installed, failures)
}

#[derive(Default)]
struct Status {
    disabled_reason: Option<String>,
    startup: Option<Value>,
    attempts: u64,
    last_attempt_unix: Option<u64>,
    last_success_unix: Option<u64>,
    last_error: Option<String>,
    applied_epoch: Option<i64>,
    install_failures: Vec<String>,
}

struct Active {
    store: LocalCorpusStore,
    connection: AgentCorpusConnection,
}

struct Shared {
    config: LocalCorpusConfig,
    status: Mutex<Status>,
    active: Mutex<Option<Active>>,
    updates: mpsc::Sender<AssetUpdate>,
    sync_requested: AtomicBool,
    notify: tokio::sync::Notify,
    proxy: Mutex<Option<EventLoopProxy<WinitUserEvent>>>,
    stop: watch::Sender<bool>,
}

impl Shared {
    fn wake(&self) {
        if let Some(proxy) = self.proxy.lock().expect("proxy lock poisoned").as_ref() {
            let _ = proxy.send_event(WinitUserEvent::WakeUp);
        }
    }
    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().expect("corpus status lock poisoned")
    }
}

/// Stops the worker when the last [`LocalCorpus`] handle is dropped.
struct StopGuard(Arc<Shared>);
impl Drop for StopGuard {
    fn drop(&mut self) {
        let _ = self.0.stop.send(true);
    }
}

/// Handle to the local corpus cache: status, cached skills and sync requests.
#[derive(Resource, Clone)]
pub struct LocalCorpus {
    shared: Arc<Shared>,
    _guard: Arc<StopGuard>,
}

impl LocalCorpus {
    fn new(config: LocalCorpusConfig, updates: mpsc::Sender<AssetUpdate>) -> Self {
        let (stop, _) = watch::channel(false);
        let shared = Arc::new(Shared {
            config,
            status: Mutex::default(),
            active: Mutex::default(),
            updates,
            sync_requested: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            proxy: Mutex::default(),
            stop,
        });
        Self {
            _guard: Arc::new(StopGuard(shared.clone())),
            shared,
        }
    }

    fn active(&self) -> Option<(LocalCorpusStore, AgentCorpusConnection)> {
        self.shared
            .active
            .lock()
            .expect("corpus active lock poisoned")
            .as_ref()
            .map(|a| (a.store.clone(), a.connection.clone()))
    }

    /// Ask the background worker to sync soon. Never blocks and never touches
    /// the network on the caller's thread.
    pub fn request_sync(&self) {
        self.shared.sync_requested.store(true, Ordering::Relaxed);
        self.shared.notify.notify_one();
    }

    /// The verified cached skill index, when one exists and still verifies
    /// against the configured keys.
    #[must_use]
    pub fn cached_skill_index(&self) -> Option<SkillIndex> {
        let (store, connection) = self.active()?;
        let bytes = store.read_index(IndexKind::Skills).ok()??;
        cloud::decode_released_index(&bytes, &connection.trusted_public_keys).ok()
    }

    /// A cached skill bundle by exact revision hash. Blobs are immutable, so a
    /// verified local copy is preferred over the network.
    #[must_use]
    pub fn cached_skill_bundle(&self, hash: &str) -> Option<SkillBundle> {
        let (store, _) = self.active()?;
        let bytes = store.get_blob(hash).ok()??;
        cloud::decode_bundle(&bytes, hash).ok()
    }

    /// Cache and sync state for `get_agent_corpus_status`.
    #[must_use]
    pub fn status_json(&self) -> Value {
        let status = self.shared.status();
        let mut out = json!({
            "enabled": status.disabled_reason.is_none(),
            "disabled_reason": status.disabled_reason,
            "sync_interval_seconds": self.shared.config.sync_interval.as_secs(),
            "sync_attempts": status.attempts,
            "last_attempt_unix": status.last_attempt_unix,
            "last_error": status.last_error,
            "applied_epoch": status.applied_epoch,
            "startup": status.startup,
            "install_failures": status.install_failures,
        });
        let state = if status.disabled_reason.is_some() {
            "disabled"
        } else if status.last_attempt_unix.is_none() {
            "pending"
        } else {
            match &status.last_error {
                None => "synced",
                Some(e) if e.starts_with("Cloud corpus") => "offline",
                Some(_) => "rejected",
            }
        };
        out["sync_state"] = json!(state);
        let mut last_success = status.last_success_unix;
        drop(status);
        if let Some((store, connection)) = self.active() {
            last_success = last_success.or(store.read_sync_record().last_success_unix);
            let keys = &connection.trusted_public_keys;
            let assets = store
                .read_index(IndexKind::Assets)
                .ok()
                .flatten()
                .and_then(|b| decode_index(&b, keys, None).ok());
            let skills = self.cached_skill_index();
            out["cache"] = json!({
                "asset_epoch": assets.as_ref().map(|i| i.epoch),
                "asset_count": assets.as_ref().map(|i| i.assets.len()),
                "skill_epoch": skills.as_ref().map(|i| i.epoch),
                "skill_count": skills.as_ref().map(|i| i.skills.len()),
                "size_bytes": store.size_bytes().ok(),
                "max_bytes": self.shared.config.max_bytes,
            });
        }
        out["last_success_unix"] = json!(last_success);
        out["cache_age_seconds"] = json!(last_success.map(|t| now_unix().saturating_sub(t)));
        out["freshness"] = json!(match state {
            "synced" => "verified current at last sync",
            "disabled" => "no local cache",
            _ => "last verified snapshot; backend freshness is unverified",
        });
        out
    }
}

/// A verified assets index and the revisions that changed since the previous
/// one, ready for the live world.
#[derive(Debug)]
struct AssetUpdate {
    index: Index,
    changed: Vec<AdmittedAsset>,
}

/// Adds the local corpus cache and its background sync to an app. Insert an
/// [`AgentCorpusConnection`] resource before the first update.
#[derive(Default)]
pub struct LocalCorpusPlugin(pub LocalCorpusConfig);

impl Plugin for LocalCorpusPlugin {
    fn build(&self, app: &mut App) {
        let (updates, receiver) = mpsc::channel();
        app.insert_resource(LocalCorpus::new(self.0.clone(), updates))
            .insert_resource(ApplyQueue(Mutex::new(Queue {
                receiver,
                pending: VecDeque::new(),
                index: None,
            })))
            .init_resource::<CurationReleaseStore>()
            .add_systems(PreStartup, start_local_corpus)
            .add_systems(PostStartup, install_cached_corpus)
            .add_systems(Update, poll_corpus_updates);
    }
}

fn start_local_corpus(
    corpus: Res<LocalCorpus>,
    connection: Option<Res<AgentCorpusConnection>>,
    curation: Res<CurationReleaseStore>,
    proxy: Option<Res<EventLoopProxyWrapper>>,
) {
    let shared = &corpus.shared;
    if let Some(proxy) = proxy {
        *shared.proxy.lock().expect("proxy lock poisoned") = Some((**proxy).clone());
    }
    let connection = connection.as_deref().cloned();
    if let Some(reason) = disabled_reason(connection.as_ref()) {
        shared.status().disabled_reason = Some(reason.to_owned());
        return;
    }
    let Some(connection) = connection else { return };
    let opened = resolve_cache_dir(&shared.config).and_then(|dir| {
        LocalCorpusStore::open(&dir, &connection, shared.config.max_bytes)
            .map_err(|e| e.to_string())
    });
    let store = match opened {
        Ok(store) => store,
        Err(error) => {
            shared.status().disabled_reason = Some(format!("cache unavailable: {error}"));
            return;
        }
    };
    *shared.active.lock().expect("corpus active lock poisoned") = Some(Active {
        store: store.clone(),
        connection: connection.clone(),
    });
    let worker = Worker {
        baseline: load_baseline(&store, &connection),
        connection,
        store,
        curation: curation.clone(),
        shared: shared.clone(),
    };
    let stop = shared.stop.subscribe();
    let spawned = std::thread::Builder::new()
        .name("talos-corpus-sync".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(worker.run(stop)),
                Err(error) => {
                    worker.shared.status().last_error = Some(format!("sync runtime: {error}"));
                }
            }
        });
    if let Err(error) = spawned {
        shared.status().last_error = Some(format!("sync thread: {error}"));
    }
}

/// Startup: install the last verified snapshot from disk, no network.
fn install_cached_corpus(world: &mut World) {
    let Some(corpus) = world.get_resource::<LocalCorpus>().cloned() else {
        return;
    };
    let Some((store, connection)) = corpus.active() else {
        return;
    };
    let startup = match load_cached_assets(&store, &connection) {
        Ok(None) => json!({"cache_found": false}),
        Err(error) => {
            warn!("Ignoring local corpus cache: {error}");
            json!({"cache_found": true, "cache_rejected": error})
        }
        Ok(Some(cached)) => {
            // The cache is verified, so its epoch is a legitimate rollback floor.
            let curation = world.resource::<CurationReleaseStore>().clone();
            let _ = curation.observe_knowledge_index(
                &connection,
                cached.index.epoch,
                &cached.index.withdrawn_asset_ids,
            );
            let listed = cached.index.assets.len();
            let epoch = cached.index.epoch;
            let (installed, failures) = install_assets(world, cached.admitted);
            corpus.shared.status().install_failures =
                failures.iter().take(MAX_REPORTED).cloned().collect();
            let problems: Vec<_> = cached.problems.into_iter().take(MAX_REPORTED).collect();
            json!({"cache_found": true, "epoch": epoch, "assets_listed": listed,
                "assets_installed": installed, "install_failures": failures.len(),
                "cache_problems": problems})
        }
    };
    corpus.shared.status().startup = Some(startup);
}

struct Queue {
    receiver: mpsc::Receiver<AssetUpdate>,
    pending: VecDeque<AdmittedAsset>,
    index: Option<Index>,
}

#[derive(Resource)]
struct ApplyQueue(Mutex<Queue>);

/// Drain sync results and apply them in bounded batches. Idle frames only try
/// to receive from an empty channel.
fn poll_corpus_updates(queue: Res<ApplyQueue>, corpus: Res<LocalCorpus>, mut commands: Commands) {
    let (batch, finish, more) = {
        let mut q = queue.0.lock().expect("apply queue lock poisoned");
        while let Ok(update) = q.receiver.try_recv() {
            let replaced: BTreeSet<_> = update.changed.iter().map(|a| a.asset.id.clone()).collect();
            q.pending.retain(|a| !replaced.contains(&a.asset.id));
            q.pending.extend(update.changed);
            q.index = Some(update.index);
        }
        if q.pending.is_empty() && q.index.is_none() {
            return;
        }
        let count = q.pending.len().min(APPLY_BATCH);
        let batch: Vec<_> = q.pending.drain(..count).collect();
        let finish = if q.pending.is_empty() {
            q.index.take()
        } else {
            None
        };
        (batch, finish, !q.pending.is_empty())
    };
    if more {
        corpus.shared.wake();
    }
    let corpus = corpus.clone();
    commands.queue(move |world: &mut World| apply_update(world, &corpus, batch, finish));
}

fn apply_update(
    world: &mut World,
    corpus: &LocalCorpus,
    batch: Vec<AdmittedAsset>,
    finish: Option<Index>,
) {
    let (_, failures) = install_assets(world, batch);
    let mut status = corpus.shared.status();
    status.install_failures.extend(failures);
    status.install_failures.truncate(MAX_REPORTED);
    drop(status);
    if let Some(index) = finish {
        reconcile(world, &index);
        corpus.shared.status().applied_epoch = Some(index.epoch);
    }
}

fn load_baseline(store: &LocalCorpusStore, connection: &AgentCorpusConnection) -> Baseline {
    let keys = &connection.trusted_public_keys;
    Baseline {
        assets: store
            .read_index(IndexKind::Assets)
            .ok()
            .flatten()
            .and_then(|b| decode_index(&b, keys, None).ok()),
        skills: store
            .read_index(IndexKind::Skills)
            .ok()
            .flatten()
            .and_then(|b| cloud::decode_released_index(&b, keys).ok()),
    }
}

/// The last verified indexes on disk, used for freshness and delta decisions.
struct Baseline {
    assets: Option<Index>,
    skills: Option<SkillIndex>,
}

fn asset_hashes(baseline: &Baseline) -> BTreeSet<String> {
    baseline
        .assets
        .iter()
        .flat_map(|i| i.assets.iter().map(|e| e.content_hash.to_ascii_lowercase()))
        .collect()
}

fn skill_hashes(baseline: &Baseline) -> BTreeSet<String> {
    baseline
        .skills
        .iter()
        .flat_map(|i| i.skills.iter().map(|e| e.content_hash.to_ascii_lowercase()))
        .collect()
}

impl Baseline {
    /// Every blob hash the cached indexes reference.
    fn referenced(&self) -> BTreeSet<String> {
        let mut all = asset_hashes(self);
        all.extend(skill_hashes(self));
        all
    }
}

struct Worker {
    connection: AgentCorpusConnection,
    store: LocalCorpusStore,
    curation: CurationReleaseStore,
    shared: Arc<Shared>,
    baseline: Baseline,
}

/// Per-blob content validation applied before a download is persisted.
type Validate<'a> = &'a dyn Fn(&str, &[u8]) -> Result<(), String>;

impl Worker {
    async fn run(mut self, mut stop: watch::Receiver<bool>) {
        let interval = self.shared.config.sync_interval;
        let mut backoff = RETRY_START;
        let mut next = Instant::now();
        let mut last_sequence = self.curation.release_sequence();
        let mut last_attempt: Option<Instant> = None;
        while !*stop.borrow() {
            let now = Instant::now();
            let sequence = self.curation.release_sequence();
            // A throttled request stays pending until the spacing has passed.
            let throttle_open = last_attempt
                .is_none_or(|t| now.duration_since(t) >= self.shared.config.min_refresh);
            let requested =
                throttle_open && self.shared.sync_requested.swap(false, Ordering::Relaxed);
            if now >= next || sequence != last_sequence || requested {
                last_sequence = sequence;
                last_attempt = Some(now);
                let result = self.sync_once().await;
                let failed = result.is_err();
                self.record(result);
                next = Instant::now() + if failed { backoff } else { interval };
                backoff = if failed {
                    (backoff * 2).min(interval)
                } else {
                    RETRY_START
                };
            }
            tokio::select! {
                _ = stop.changed() => {}
                _ = tokio::time::sleep(TICK) => {}
                () = self.shared.notify.notified() => {}
            }
        }
    }

    fn record(&self, result: Result<(), String>) {
        let mut status = self.shared.status();
        status.attempts += 1;
        status.last_attempt_unix = Some(now_unix());
        match result {
            Ok(()) => {
                status.last_success_unix = status.last_attempt_unix;
                status.last_error = None;
            }
            Err(error) => status.last_error = Some(error),
        }
    }

    /// Fetch, verify, persist and hand over the current backend corpus.
    async fn sync_once(&mut self) -> Result<(), String> {
        let assets = self.sync_assets().await;
        let skills = self.sync_skills().await;
        let update = assets?;
        skills?;
        self.evict();
        self.store
            .write_sync_record(&SyncRecord {
                last_success_unix: Some(now_unix()),
            })
            .map_err(|e| e.to_string())?;
        if let Some(update) = update {
            if self.shared.updates.send(update).is_ok() {
                self.shared.wake();
            }
        }
        Ok(())
    }

    fn evict(&self) {
        let _ = self.store.evict_unreferenced(&self.baseline.referenced());
    }

    async fn sync_assets(&mut self) -> Result<Option<AssetUpdate>, String> {
        let c = &self.connection;
        let bytes = cloud::request(c, "/assets", None).await?;
        let index = decode_index(&bytes, &c.trusted_public_keys, None)?;
        self.curation
            .observe_knowledge_index(c, index.epoch, &index.withdrawn_asset_ids)?;
        if self
            .baseline
            .assets
            .as_ref()
            .is_some_and(|old| index.epoch < old.epoch)
        {
            return Err("Knowledge index older than the verified local copy; refused".into());
        }
        let meta: BTreeMap<&str, (&str, &Value)> = entries(&index)
            .map(|(id, hash, kind)| (hash, (id, kind)))
            .collect();
        let hashes: Vec<String> = meta.keys().map(|h| (*h).to_owned()).collect();
        let validate = |hash: &str, blob: &[u8]| -> Result<(), String> {
            let (id, kind) = meta.get(hash).ok_or("unlisted revision")?;
            admit_revision(blob, id, hash, kind, None).map(|_| ())
        };
        let other = skill_hashes(&self.baseline);
        let fetched = self.fetch_missing(&hashes, &other, &validate).await?;
        let known: BTreeSet<_> = self
            .baseline
            .assets
            .iter()
            .flat_map(|i| entries(i).map(|(id, hash, _)| (id.to_owned(), hash.to_owned())))
            .collect();
        let mut changed = Vec::new();
        for (id, hash, kind) in entries(&index) {
            if known.contains(&(id.to_owned(), hash.to_owned())) && !fetched.contains(hash) {
                continue;
            }
            let blob = self
                .store
                .get_blob(hash)
                .map_err(|e| e.to_string())?
                .ok_or("blob missing after download")?;
            changed.push(admit_revision(&blob, id, hash, kind, None)?);
        }
        // The index replaces the cached one only after every blob it lists is
        // verified and on disk, so the cache is always self-consistent.
        self.store
            .write_index(IndexKind::Assets, &bytes)
            .map_err(|e| e.to_string())?;
        self.baseline.assets = Some(index.clone());
        Ok(Some(AssetUpdate { index, changed }))
    }

    async fn sync_skills(&mut self) -> Result<(), String> {
        let c = &self.connection;
        let bytes = cloud::request(c, "/skills", None).await?;
        let index = cloud::decode_released_index(&bytes, &c.trusted_public_keys)?;
        self.curation.observe_skill_index(c, &index)?;
        if self
            .baseline
            .skills
            .as_ref()
            .is_some_and(|old| index.epoch < old.epoch)
        {
            return Err("Skill index older than the verified local copy; refused".into());
        }
        let hashes: Vec<String> = index
            .skills
            .iter()
            .map(|e| e.content_hash.clone())
            .collect();
        let validate = |hash: &str, blob: &[u8]| cloud::decode_bundle(blob, hash).map(|_| ());
        let other = asset_hashes(&self.baseline);
        self.fetch_missing(&hashes, &other, &validate).await?;
        self.store
            .write_index(IndexKind::Skills, &bytes)
            .map_err(|e| e.to_string())?;
        self.baseline.skills = Some(index);
        Ok(())
    }

    /// Download, validate and persist the blobs among `wanted` that are not
    /// already stored. Returns the hashes fetched. A full cache is first
    /// relieved of blobs that neither index references.
    async fn fetch_missing(
        &self,
        wanted: &[String],
        also_referenced: &BTreeSet<String>,
        validate: Validate<'_>,
    ) -> Result<BTreeSet<String>, String> {
        let mut missing: Vec<String> = Vec::new();
        for hash in wanted {
            let present = match self.store.get_blob(hash) {
                Ok(blob) => blob.is_some(),
                Err(StoreError::HashMismatch(_)) => false,
                Err(error) => return Err(error.to_string()),
            };
            if !present && !missing.contains(hash) {
                missing.push(hash.clone());
            }
        }
        let mut fetched = BTreeSet::new();
        let mut set = JoinSet::new();
        let mut queue = missing.into_iter();
        loop {
            while set.len() < DOWNLOAD_CONCURRENCY {
                let Some(hash) = queue.next() else { break };
                let c = self.connection.clone();
                set.spawn(async move {
                    let blob = cloud::request(&c, &format!("/revisions/{hash}"), None).await?;
                    Ok::<_, String>((hash, blob))
                });
            }
            let Some(joined) = set.join_next().await else {
                break;
            };
            let (hash, blob) = joined.map_err(|e| e.to_string())??;
            validate(&hash, &blob)?;
            self.persist(&hash, &blob, wanted, also_referenced)?;
            fetched.insert(hash);
        }
        Ok(fetched)
    }

    fn persist(
        &self,
        hash: &str,
        blob: &[u8],
        wanted: &[String],
        also_referenced: &BTreeSet<String>,
    ) -> Result<(), String> {
        match self.store.put_blob(hash, blob) {
            Err(StoreError::Full { .. }) => {
                let keep: BTreeSet<String> = wanted
                    .iter()
                    .map(|h| h.to_ascii_lowercase())
                    .chain(also_referenced.iter().cloned())
                    .collect();
                self.store
                    .evict_unreferenced(&keep)
                    .map_err(|e| e.to_string())?;
                self.store.put_blob(hash, blob).map_err(|e| e.to_string())
            }
            other => other.map_err(|e| e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
