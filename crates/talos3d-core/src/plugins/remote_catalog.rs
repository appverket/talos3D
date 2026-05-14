//! Bevy plugin that connects the running desktop binary to the talos-catalog
//! HTTP service via a long-poll change feed.
//!
//! # Activation
//!
//! The plugin always inserts the [`CatalogKindRegistry`] resource so other
//! plugins can declare their kinds, but it only spawns the catalog poller +
//! publish worker thread when the `TALOS3D_CATALOG_URL` environment variable
//! is set. When unset, kind registrations remain inert.
//!
//! Set `TALOS3D_ACCOUNT_ID` (optional UUID) to enable tenant-scoped resolution.
//! Set `TALOS3D_PUBLISHED_BY` (optional UUID) to attribute seeded artifacts.
//!
//! # Substrate
//!
//! Per-kind glue lives in [`CatalogKindDescriptor`]s registered in
//! [`CatalogKindRegistry`]. Each descriptor carries:
//!
//! - `kind` — the wire kind string (e.g. `"definition.v1"`).
//! - `auto_fetch_body` — whether the poller should pre-fetch the blob.
//! - `serialize` — `(World, local_id) -> (canonical_id, body)`.
//! - `apply` — `(World, canonical_id, body) -> ()`, mutates the target
//!   registry and emits any kind-specific reload events.
//! - `seed_local_ids` — `World -> Vec<local_id>`, the artifacts to publish
//!   in the one-shot bundled-content seeding pass.
//! - `build_publish_request` — a `talos3d_catalog_client` helper such as
//!   `definition_publish_request`.
//!
//! Adding a new kind (`recipe.v1`, `assembly_pattern.v1`, …) is a single
//! [`CatalogKindRegistry::register`] call from the owning plugin; no new
//! message types, no new systems.
//!
//! # Architecture
//!
//! ```text
//! OS thread "talos3d-catalog-poller"
//!   └── current-thread tokio runtime
//!       ├── ChangePoller (async long-poll loop)
//!       │   └── per-event: fetch blob if descriptor.auto_fetch_body
//!       │       └── tokio mpsc -> std::sync::mpsc bridge
//!       └── publish-job consumer
//!           └── per PublishJob: client.publish_artifact(...)
//!               └── result -> std::sync::mpsc bridge
//!
//! Bevy main thread
//!   PreUpdate: drain_catalog_changes_system
//!     -> MessageWriter<RemoteCatalogChange>
//!   PreUpdate: apply_artifact_changes_system  (exclusive, descriptor-driven)
//!     -> mutates target registry via descriptor.apply
//!     -> MessageWriter<ArtifactApplied>
//!   PreUpdate: publish_artifact_requests_system  (exclusive, descriptor-driven)
//!     -> sends PublishJob to worker thread
//!   PreUpdate: drain_publish_results_system
//!     -> MessageWriter<PublishArtifactResult>
//!   Update: seed_bundled_content_system
//!     -> one-shot, walks every registered kind's seed_local_ids
//! ```
//!
//! The dedicated OS thread with its own `current_thread` runtime keeps
//! async-tokio completely isolated from the Bevy main loop.

#[cfg(not(target_arch = "wasm32"))]
mod inner {
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use bevy::ecs::system::SystemState;
    use bevy::prelude::*;
    use serde_json::Value;
    use url::Url;
    use uuid::Uuid;

    use talos3d_catalog_client::{
        definition_publish_request, material_def_publish_request, CatalogCache, ChangeEvent,
        ChangePoller, PublishArtifactRequest, PublishError, PublishScope, RemoteCatalogClient,
        WorkspaceRemoteCache,
    };

    use crate::plugins::materials::{MaterialDef, MaterialRegistry};
    use crate::plugins::modeling::definition::{
        Definition, DefinitionId, DefinitionLibraryRegistry, DefinitionRegistry,
    };

    // ---- Messages -----------------------------------------------------------

    /// A raw change event forwarded from the catalog poller, with an optional
    /// pre-fetched body.
    ///
    /// The `body` field is populated for kinds whose descriptor has
    /// `auto_fetch_body = true`. For other kinds it is `None` and a consumer
    /// can fetch lazily by content hash.
    #[derive(Message, Debug, Clone)]
    pub struct RemoteCatalogChange {
        pub event: ChangeEvent,
        pub body: Option<Value>,
    }

    /// Fired after a catalog body has been successfully applied to the local
    /// registry via [`CatalogKindDescriptor::apply`].
    ///
    /// Kind-specific reload events (e.g. [`DefinitionRegistryReloaded`],
    /// [`MaterialRegistryReloaded`]) are still emitted by each descriptor's
    /// apply callback for legacy consumers — `ArtifactApplied` is the generic
    /// signal new consumers should prefer.
    #[derive(Message, Debug, Clone)]
    pub struct ArtifactApplied {
        pub kind: &'static str,
        pub canonical_id: String,
    }

    /// Fired after a `material_def.v1` artifact is successfully upserted into
    /// [`MaterialRegistry`].
    ///
    /// Legacy event preserved for downstream consumers. New code should listen
    /// to [`ArtifactApplied`] and filter on `kind == "material_def.v1"`.
    #[derive(Message, Debug, Clone)]
    pub struct MaterialRegistryReloaded {
        pub id: String,
    }

    /// Fired after a `definition.v1` artifact is successfully inserted into
    /// [`DefinitionRegistry`].
    ///
    /// Legacy event preserved for downstream consumers. New code should listen
    /// to [`ArtifactApplied`] and filter on `kind == "definition.v1"`.
    #[derive(Message, Debug, Clone)]
    pub struct DefinitionRegistryReloaded {
        pub id: DefinitionId,
    }

    /// Request the desktop to publish a locally-registered artifact of any
    /// registered kind to the remote catalog.
    ///
    /// `local_id` is the kind-specific lookup key consumed by the descriptor's
    /// `serialize` callback. `canonical_id_override` lets callers override the
    /// descriptor-derived canonical id; leave it `None` for the default.
    #[derive(Message, Debug, Clone)]
    pub struct PublishArtifactRequested {
        pub kind: &'static str,
        pub local_id: String,
        pub canonical_id_override: Option<String>,
        pub scope: PublishScope,
        pub jurisdiction: Vec<String>,
        pub owner_org_id: Option<Uuid>,
        pub published_by: Uuid,
    }

    /// Outcome of a [`PublishArtifactRequested`] request.
    #[derive(Message, Debug, Clone)]
    pub struct PublishArtifactResult {
        pub kind: &'static str,
        pub local_id: String,
        pub canonical_id: String,
        pub outcome: PublishArtifactOutcome,
    }

    /// Detailed outcome of an artifact publish attempt.
    #[derive(Debug, Clone)]
    pub enum PublishArtifactOutcome {
        /// The artifact was successfully created or revised in the catalog.
        Published {
            artifact_id: Uuid,
            revision: i32,
            content_hash: String,
        },
        /// No local artifact with the requested id was found by the
        /// descriptor's `serialize` callback.
        NotFound,
        /// The publish failed. The inner string carries a human-readable
        /// reason.
        Failed(String),
    }

    // ---- Substrate ----------------------------------------------------------

    /// `(world, local_id) -> Result<(canonical_id, body), error>`.
    pub type ArtifactSerializer = fn(&World, &str) -> Result<(String, Value), String>;

    /// `(world, canonical_id, body) -> Result<(), error>`.
    ///
    /// The callback owns mutation of the target registry and emission of any
    /// kind-specific reload events.
    pub type ArtifactApplier = fn(&mut World, &str, &Value) -> Result<(), String>;

    /// `world -> Vec<local_id>` — every local artifact of this kind that
    /// should be considered for the one-shot bundled-content seeding pass.
    pub type SeedLocalIds = fn(&World) -> Vec<String>;

    /// Thin alias for the `talos3d_catalog_client` per-kind publish-request
    /// helper (e.g. [`definition_publish_request`]).
    pub type PublishRequestBuilder = fn(
        canonical_id: String,
        body: Value,
        scope: PublishScope,
        jurisdiction: Vec<String>,
        owner_org_id: Option<Uuid>,
        published_by: Uuid,
    ) -> Result<PublishArtifactRequest, PublishError>;

    /// Domain glue for a single artifact `kind` on the wire.
    ///
    /// Plugins register one descriptor per kind they own. Adding `recipe.v1`,
    /// `assembly_pattern.v1`, `code_rule_pack.v1`, `source_passage.v1`, …
    /// is a single [`CatalogKindRegistry::register`] call from the owning
    /// plugin — no new message types or systems required.
    #[derive(Clone)]
    pub struct CatalogKindDescriptor {
        pub kind: &'static str,
        pub auto_fetch_body: bool,
        pub serialize: ArtifactSerializer,
        pub apply: ArtifactApplier,
        pub seed_local_ids: SeedLocalIds,
        pub build_publish_request: PublishRequestBuilder,
    }

    /// Resource: kind → descriptor lookup table. Always present (even when
    /// the catalog poller is disabled) so other plugins can register kinds
    /// without depending on `TALOS3D_CATALOG_URL`.
    #[derive(Resource, Default)]
    pub struct CatalogKindRegistry {
        by_kind: HashMap<&'static str, CatalogKindDescriptor>,
    }

    impl CatalogKindRegistry {
        pub fn register(&mut self, descriptor: CatalogKindDescriptor) {
            self.by_kind.insert(descriptor.kind, descriptor);
        }

        pub fn get(&self, kind: &str) -> Option<&CatalogKindDescriptor> {
            self.by_kind.get(kind)
        }

        pub fn all(&self) -> impl Iterator<Item = &CatalogKindDescriptor> {
            self.by_kind.values()
        }

        pub fn auto_fetch_kinds(&self) -> Vec<String> {
            self.by_kind
                .values()
                .filter(|d| d.auto_fetch_body)
                .map(|d| d.kind.to_string())
                .collect()
        }
    }

    /// Configuration for the one-shot bundled-content seeding pass.
    ///
    /// `published_by` defaults to `TALOS3D_PUBLISHED_BY` (parsed as a UUID)
    /// when set, otherwise the nil UUID — a stable identifier for "bundled
    /// content seed". Catalog dedupes by content hash, so re-running is
    /// idempotent on the wire even without the [`BundledContentSeeded`] guard.
    #[derive(Resource, Debug, Clone)]
    pub struct BundledContentSeedConfig {
        pub seed_bundled_on_start: bool,
        pub scope: PublishScope,
        pub jurisdiction: Vec<String>,
        pub owner_org_id: Option<Uuid>,
        pub published_by: Uuid,
    }

    impl Default for BundledContentSeedConfig {
        fn default() -> Self {
            let published_by = std::env::var("TALOS3D_PUBLISHED_BY")
                .ok()
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or(Uuid::nil());
            Self {
                seed_bundled_on_start: true,
                scope: PublishScope::Shipped,
                jurisdiction: Vec::new(),
                owner_org_id: None,
                published_by,
            }
        }
    }

    /// One-shot guard for [`seed_bundled_content_system`]. Inserted after the
    /// first pass so it never fires again in this session.
    #[derive(Resource, Default)]
    pub struct BundledContentSeeded;

    // ---- Internal bridge types ----------------------------------------------

    /// Internal message bridging the async poller thread to the Bevy main
    /// thread.
    pub(super) struct CatalogBridgeMessage {
        pub event: ChangeEvent,
        pub body: Option<Value>,
    }

    /// A publish job sent from Bevy to the catalog worker thread. The `kind`
    /// + `local_id` round-trip through the worker unchanged so the result
    /// can be routed back as a strongly-typed [`PublishArtifactResult`].
    pub(super) struct PublishJob {
        pub kind: &'static str,
        pub local_id: String,
        pub canonical_id: String,
        pub request: PublishArtifactRequest,
    }

    /// A publish result sent from the catalog worker thread back to Bevy.
    pub(super) struct PublishJobResult {
        pub kind: &'static str,
        pub local_id: String,
        pub canonical_id: String,
        pub outcome: PublishArtifactOutcome,
    }

    // ---- Resource -----------------------------------------------------------

    /// Bevy resource that holds the live connection between the poller thread
    /// and the Bevy main thread. Inserted only when `TALOS3D_CATALOG_URL` is
    /// set.
    #[derive(Resource)]
    pub struct RemoteCatalogState {
        pub base_url: Url,
        pub account_id: Option<Uuid>,
        pub cache_root: PathBuf,
        pub(super) rx: Mutex<std::sync::mpsc::Receiver<CatalogBridgeMessage>>,
        pub shutdown_tx: tokio::sync::watch::Sender<bool>,
        pub(super) publish_tx: Mutex<std::sync::mpsc::Sender<PublishJob>>,
        pub(super) publish_results_rx: Mutex<std::sync::mpsc::Receiver<PublishJobResult>>,
    }

    // ---- Plugin -------------------------------------------------------------

    /// Bevy plugin that subscribes to the talos-catalog change feed, applies
    /// incoming artifacts via registered kind descriptors, publishes local
    /// artifacts on request, and runs a one-shot bundled-content seeding
    /// pass.
    pub struct RemoteCatalogPlugin;

    impl Plugin for RemoteCatalogPlugin {
        fn build(&self, app: &mut App) {
            // The kind registry is always available so other plugins can
            // register their kinds regardless of whether the catalog poller
            // is enabled in this run.
            app.init_resource::<CatalogKindRegistry>();
            register_builtin_kinds(app);

            let catalog_url = match std::env::var("TALOS3D_CATALOG_URL") {
                Ok(v) => v,
                Err(_) => {
                    info!("remote catalog disabled (TALOS3D_CATALOG_URL unset)");
                    return;
                }
            };

            let base_url = match Url::parse(&catalog_url) {
                Ok(u) => u,
                Err(e) => {
                    error!(url = %catalog_url, error = %e, "TALOS3D_CATALOG_URL is not a valid URL");
                    return;
                }
            };

            let account_id = std::env::var("TALOS3D_ACCOUNT_ID")
                .ok()
                .and_then(|s| s.parse::<Uuid>().ok());

            app.add_message::<RemoteCatalogChange>()
                .add_message::<ArtifactApplied>()
                .add_message::<MaterialRegistryReloaded>()
                .add_message::<DefinitionRegistryReloaded>()
                .add_message::<PublishArtifactRequested>()
                .add_message::<PublishArtifactResult>()
                .init_resource::<BundledContentSeedConfig>()
                // Poller is spawned in PostStartup so any plugin that
                // registers a kind in Startup is reflected in the
                // poller's auto-fetch-kinds subscription.
                .add_systems(PostStartup, spawn_catalog_thread_system)
                .add_systems(
                    PreUpdate,
                    (
                        drain_catalog_changes_system,
                        apply_artifact_changes_system.after(drain_catalog_changes_system),
                        publish_artifact_requests_system,
                        drain_publish_results_system,
                        log_publish_outcomes_system.after(drain_publish_results_system),
                    ),
                )
                .add_systems(Update, seed_bundled_content_system);

            app.insert_resource(CatalogConfig {
                base_url,
                account_id,
            });
        }
    }

    // ---- Startup config (consumed by spawn_catalog_thread_system) -----------

    #[derive(Resource)]
    struct CatalogConfig {
        base_url: Url,
        account_id: Option<Uuid>,
    }

    // ---- Built-in kinds (definition.v1 + material_def.v1) -------------------

    /// Registers the core's two built-in kinds. New kinds owned by other
    /// crates register themselves the same way from their own plugins.
    fn register_builtin_kinds(app: &mut App) {
        let mut registry = app.world_mut().resource_mut::<CatalogKindRegistry>();
        registry.register(CatalogKindDescriptor {
            kind: "definition.v1",
            auto_fetch_body: true,
            serialize: serialize_definition,
            apply: apply_definition,
            seed_local_ids: seed_local_ids_definition,
            build_publish_request: definition_publish_request,
        });
        registry.register(CatalogKindDescriptor {
            kind: "material_def.v1",
            auto_fetch_body: true,
            serialize: serialize_material_def,
            apply: apply_material_def,
            seed_local_ids: seed_local_ids_material_def,
            build_publish_request: material_def_publish_request,
        });
    }

    // ---- definition.v1 descriptor callbacks ---------------------------------

    fn serialize_definition(world: &World, local_id: &str) -> Result<(String, Value), String> {
        let did = DefinitionId(local_id.to_owned());

        // Live registry first, then any of the loaded libraries so library
        // contents are addressable for seeding without first being
        // instantiated into the runtime registry.
        let (def, library_id) = if let Some(def) = world.resource::<DefinitionRegistry>().get(&did)
        {
            (def.clone(), None)
        } else {
            let libraries = world.resource::<DefinitionLibraryRegistry>();
            let found = libraries
                .list()
                .into_iter()
                .find_map(|lib| lib.get(&did).map(|d| (d.clone(), Some(lib.id.0.clone()))));
            match found {
                Some(pair) => pair,
                None => {
                    return Err(format!(
                        "definition '{}' not found in registry or libraries",
                        local_id
                    ))
                }
            }
        };

        let canonical_id = match library_id {
            Some(lib_id) => format!("{}::{}", lib_id, local_id),
            None => format!("local::{}", local_id),
        };
        let body = serde_json::to_value(&def).map_err(|e| e.to_string())?;
        Ok((canonical_id, body))
    }

    fn apply_definition(world: &mut World, _canonical_id: &str, body: &Value) -> Result<(), String> {
        let def: Definition = serde_json::from_value(body.clone()).map_err(|e| e.to_string())?;
        let id = def.id.clone();
        world.resource_mut::<DefinitionRegistry>().insert(def);
        info!(id = %id.as_str(), "definition.v1 hot-reloaded from catalog");
        if let Some(mut messages) = world.get_resource_mut::<Messages<DefinitionRegistryReloaded>>()
        {
            messages.write(DefinitionRegistryReloaded { id });
        }
        Ok(())
    }

    fn seed_local_ids_definition(world: &World) -> Vec<String> {
        let libraries = world.resource::<DefinitionLibraryRegistry>();
        let mut ids = Vec::new();
        for library in libraries.list() {
            for def in library.definitions.values() {
                ids.push(def.id.0.clone());
            }
        }
        ids
    }

    // ---- material_def.v1 descriptor callbacks -------------------------------

    fn serialize_material_def(world: &World, local_id: &str) -> Result<(String, Value), String> {
        let registry = world.resource::<MaterialRegistry>();
        let def = registry
            .get(local_id)
            .ok_or_else(|| format!("material '{}' not found in registry", local_id))?;
        let canonical_id = format!("bundled::{}", local_id);
        let body = serde_json::to_value(def).map_err(|e| e.to_string())?;
        Ok((canonical_id, body))
    }

    fn apply_material_def(
        world: &mut World,
        _canonical_id: &str,
        body: &Value,
    ) -> Result<(), String> {
        let def: MaterialDef = serde_json::from_value(body.clone()).map_err(|e| e.to_string())?;
        let id = world.resource_mut::<MaterialRegistry>().upsert(def);
        info!(id = %id, "material_def hot-reloaded from catalog");
        if let Some(mut messages) = world.get_resource_mut::<Messages<MaterialRegistryReloaded>>() {
            messages.write(MaterialRegistryReloaded { id });
        }
        Ok(())
    }

    fn seed_local_ids_material_def(world: &World) -> Vec<String> {
        world
            .resource::<MaterialRegistry>()
            .all()
            .map(|m| m.id.clone())
            .collect()
    }

    // ---- Wire-layer systems -------------------------------------------------

    /// Spawns the poller OS thread and inserts [`RemoteCatalogState`].
    fn spawn_catalog_thread_system(
        mut commands: Commands,
        config: Option<Res<CatalogConfig>>,
        registry: Res<CatalogKindRegistry>,
    ) {
        let Some(config) = config else { return };

        let base_url = config.base_url.clone();
        let account_id = config.account_id;
        // Snapshot the auto-fetch kinds at spawn time. Kinds registered
        // after PostStartup will not influence the poller's subscription
        // (acceptable for the static-by-plugin-build model we use today).
        let auto_fetch_kinds: Vec<String> = registry.auto_fetch_kinds();

        commands.remove_resource::<CatalogConfig>();

        let cache_root = match WorkspaceRemoteCache::discover_default_cache_root() {
            Ok(root) => root,
            Err(e) => {
                error!(error = %e, "failed to discover catalog cache root; remote catalog disabled");
                return;
            }
        };

        let cache: Arc<dyn CatalogCache> = match WorkspaceRemoteCache::open(cache_root.clone()) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                error!(error = %e, "failed to open catalog cache; remote catalog disabled");
                return;
            }
        };

        let (std_tx, std_rx) = std::sync::mpsc::channel::<CatalogBridgeMessage>();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let (publish_jobs_std_tx, publish_jobs_std_rx) = std::sync::mpsc::channel::<PublishJob>();
        let (publish_results_std_tx, publish_results_std_rx) =
            std::sync::mpsc::channel::<PublishJobResult>();

        let state = RemoteCatalogState {
            base_url: base_url.clone(),
            account_id,
            cache_root: cache_root.clone(),
            rx: Mutex::new(std_rx),
            shutdown_tx,
            publish_tx: Mutex::new(publish_jobs_std_tx),
            publish_results_rx: Mutex::new(publish_results_std_rx),
        };
        commands.insert_resource(state);

        let spawn_result = thread::Builder::new()
            .name("talos3d-catalog-poller".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build tokio rt for catalog poller");

                rt.block_on(async move {
                    let client = RemoteCatalogClient::new(base_url.clone(), account_id);

                    let auto_fetch: Vec<String> = auto_fetch_kinds.clone();

                    // Bridge: tokio mpsc -> std mpsc (change events).
                    let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::channel::<ChangeEvent>(256);
                    let client_for_blob = client.clone();
                    let cache_for_blob = cache.clone();
                    let std_tx_bridge = std_tx.clone();

                    tokio::spawn(async move {
                        while let Some(event) = tokio_rx.recv().await {
                            let body = if auto_fetch.iter().any(|k| k == &event.kind) {
                                let hash = event.content_hash.clone();
                                let bytes = match cache_for_blob.read_blob(&hash) {
                                    Some(b) => Some(b),
                                    None => match client_for_blob.get_blob(&hash).await {
                                        Ok(b) => {
                                            let _ = cache_for_blob.write_blob(&hash, &b);
                                            Some(b)
                                        }
                                        Err(e) => {
                                            warn!(
                                                hash = %hash,
                                                canonical_id = %event.canonical_id,
                                                error = %e,
                                                "failed to fetch blob for catalog event"
                                            );
                                            None
                                        }
                                    },
                                };

                                bytes.and_then(|b| {
                                    serde_json::from_slice::<Value>(&b)
                                        .map_err(|e| {
                                            warn!(
                                                hash = %event.content_hash,
                                                error = %e,
                                                "blob is not valid JSON"
                                            );
                                        })
                                        .ok()
                                })
                            } else {
                                None
                            };

                            let msg = CatalogBridgeMessage { event, body };
                            if std_tx_bridge.send(msg).is_err() {
                                break;
                            }
                        }
                    });

                    // Publish-job consumer.
                    let client_for_publish = client.clone();
                    let (publish_tokio_tx, mut publish_tokio_rx) =
                        tokio::sync::mpsc::channel::<PublishJob>(64);

                    let publish_tokio_tx_bridge = publish_tokio_tx.clone();
                    std::thread::Builder::new()
                        .name("talos3d-catalog-publish-bridge".into())
                        .spawn(move || {
                            while let Ok(job) = publish_jobs_std_rx.recv() {
                                if publish_tokio_tx_bridge.blocking_send(job).is_err() {
                                    break;
                                }
                            }
                        })
                        .expect("spawn publish bridge thread");

                    tokio::spawn(async move {
                        while let Some(job) = publish_tokio_rx.recv().await {
                            let kind = job.kind;
                            let local_id = job.local_id.clone();
                            let canonical_id = job.canonical_id.clone();

                            let outcome =
                                match client_for_publish.publish_artifact(&job.request).await {
                                    Ok(resolution) => PublishArtifactOutcome::Published {
                                        artifact_id: resolution.artifact_id,
                                        revision: resolution.revision,
                                        content_hash: resolution.content_hash,
                                    },
                                    Err(e) => {
                                        warn!(
                                            kind = kind,
                                            canonical_id = %canonical_id,
                                            error = %e,
                                            "artifact publish failed"
                                        );
                                        PublishArtifactOutcome::Failed(e.to_string())
                                    }
                                };

                            let result = PublishJobResult {
                                kind,
                                local_id,
                                canonical_id,
                                outcome,
                            };
                            if publish_results_std_tx.send(result).is_err() {
                                break;
                            }
                        }
                    });

                    let poller = ChangePoller::new(
                        client,
                        cache,
                        auto_fetch_kinds,
                        Duration::from_secs(5),
                    );

                    if let Err(e) = poller.run(tokio_tx, shutdown_rx).await {
                        error!(error = %e, "catalog poller exited with error");
                    } else {
                        info!("catalog poller exited cleanly");
                    }
                });
            });

        if let Err(e) = spawn_result {
            error!(error = %e, "failed to spawn catalog poller thread");
        }
    }

    /// Drains the std::sync::mpsc receiver into a [`RemoteCatalogChange`]
    /// message stream.
    fn drain_catalog_changes_system(
        state: Option<Res<RemoteCatalogState>>,
        mut writer: MessageWriter<RemoteCatalogChange>,
    ) {
        let Some(state) = state else { return };
        let rx = state.rx.lock().unwrap();
        while let Ok(msg) = rx.try_recv() {
            writer.write(RemoteCatalogChange {
                event: msg.event,
                body: msg.body,
            });
        }
    }

    // ---- Descriptor-driven apply / publish / seed ---------------------------

    /// Reads [`RemoteCatalogChange`] events and dispatches each to the
    /// matching kind descriptor's `apply` callback.
    fn apply_artifact_changes_system(
        world: &mut World,
        mut reader_state: Local<Option<SystemState<MessageReader<'static, 'static, RemoteCatalogChange>>>>,
    ) {
        let state = reader_state.get_or_insert_with(|| SystemState::new(world));
        let changes: Vec<RemoteCatalogChange> = {
            let mut reader = state.get_mut(world);
            reader.read().cloned().collect()
        };

        for change in changes {
            let descriptor = match world
                .resource::<CatalogKindRegistry>()
                .get(change.event.kind.as_str())
                .cloned()
            {
                Some(d) => d,
                None => continue,
            };
            let Some(body) = change.body.as_ref() else {
                continue;
            };
            let canonical_id = change.event.canonical_id.clone();
            match (descriptor.apply)(world, &canonical_id, body) {
                Ok(()) => {
                    if let Some(mut messages) =
                        world.get_resource_mut::<Messages<ArtifactApplied>>()
                    {
                        messages.write(ArtifactApplied {
                            kind: descriptor.kind,
                            canonical_id,
                        });
                    }
                }
                Err(error) => {
                    warn!(
                        kind = descriptor.kind,
                        canonical_id = %canonical_id,
                        error = %error,
                        "failed to apply catalog body"
                    );
                }
            }
        }
    }

    /// Reads [`PublishArtifactRequested`] messages and forwards a typed
    /// publish job to the worker thread for each.
    fn publish_artifact_requests_system(
        world: &mut World,
        mut reader_state: Local<Option<SystemState<MessageReader<'static, 'static, PublishArtifactRequested>>>>,
    ) {
        if world.get_resource::<RemoteCatalogState>().is_none() {
            return;
        }

        let state = reader_state.get_or_insert_with(|| SystemState::new(world));
        let requests: Vec<PublishArtifactRequested> = {
            let mut reader = state.get_mut(world);
            reader.read().cloned().collect()
        };

        for req in requests {
            let descriptor = match world
                .resource::<CatalogKindRegistry>()
                .get(req.kind)
                .cloned()
            {
                Some(d) => d,
                None => {
                    warn!(
                        kind = req.kind,
                        local_id = %req.local_id,
                        "PublishArtifactRequested: no descriptor registered for kind"
                    );
                    write_publish_result(
                        world,
                        PublishArtifactResult {
                            kind: req.kind,
                            local_id: req.local_id,
                            canonical_id: req.canonical_id_override.unwrap_or_default(),
                            outcome: PublishArtifactOutcome::Failed(format!(
                                "no descriptor registered for kind '{}'",
                                req.kind
                            )),
                        },
                    );
                    continue;
                }
            };

            // Run the serializer.
            let (default_canonical_id, body) = match (descriptor.serialize)(world, &req.local_id) {
                Ok(pair) => pair,
                Err(error) => {
                    warn!(
                        kind = descriptor.kind,
                        local_id = %req.local_id,
                        error = %error,
                        "PublishArtifactRequested: serialize failed"
                    );
                    write_publish_result(
                        world,
                        PublishArtifactResult {
                            kind: descriptor.kind,
                            local_id: req.local_id,
                            canonical_id: req.canonical_id_override.unwrap_or_default(),
                            outcome: PublishArtifactOutcome::NotFound,
                        },
                    );
                    continue;
                }
            };

            let canonical_id = req
                .canonical_id_override
                .clone()
                .unwrap_or(default_canonical_id);

            // Build the publish request.
            let publish_req = match (descriptor.build_publish_request)(
                canonical_id.clone(),
                body,
                req.scope,
                req.jurisdiction.clone(),
                req.owner_org_id,
                req.published_by,
            ) {
                Ok(r) => r,
                Err(error) => {
                    warn!(
                        kind = descriptor.kind,
                        canonical_id = %canonical_id,
                        error = %error,
                        "PublishArtifactRequested: invalid publish parameters"
                    );
                    write_publish_result(
                        world,
                        PublishArtifactResult {
                            kind: descriptor.kind,
                            local_id: req.local_id,
                            canonical_id,
                            outcome: PublishArtifactOutcome::Failed(error.to_string()),
                        },
                    );
                    continue;
                }
            };

            // Forward to the worker thread.
            let job = PublishJob {
                kind: descriptor.kind,
                local_id: req.local_id.clone(),
                canonical_id: canonical_id.clone(),
                request: publish_req,
            };
            let send_result = world
                .resource::<RemoteCatalogState>()
                .publish_tx
                .lock()
                .unwrap()
                .send(job);
            if let Err(error) = send_result {
                warn!(
                    kind = descriptor.kind,
                    canonical_id = %canonical_id,
                    error = %error,
                    "publish_tx channel closed; dropping publish request"
                );
                write_publish_result(
                    world,
                    PublishArtifactResult {
                        kind: descriptor.kind,
                        local_id: req.local_id,
                        canonical_id,
                        outcome: PublishArtifactOutcome::Failed(
                            "publish worker channel closed".to_owned(),
                        ),
                    },
                );
            }
        }
    }

    fn write_publish_result(world: &mut World, result: PublishArtifactResult) {
        if let Some(mut messages) = world.get_resource_mut::<Messages<PublishArtifactResult>>() {
            messages.write(result);
        }
    }

    /// Drains worker-thread publish results into the Bevy message bus.
    fn drain_publish_results_system(
        state: Option<Res<RemoteCatalogState>>,
        mut writer: MessageWriter<PublishArtifactResult>,
    ) {
        let Some(state) = state else { return };
        let rx = state.publish_results_rx.lock().unwrap();
        while let Ok(result) = rx.try_recv() {
            writer.write(PublishArtifactResult {
                kind: result.kind,
                local_id: result.local_id,
                canonical_id: result.canonical_id,
                outcome: result.outcome,
            });
        }
    }

    /// One-shot system that publishes every locally-registered artifact in
    /// every registered kind on first opportunity. The catalog dedupes by
    /// content hash, so re-running is idempotent.
    ///
    /// Implemented as an exclusive system because each descriptor's
    /// `seed_local_ids` callback needs read access to arbitrary world
    /// resources owned by the kind's domain plugin (e.g.
    /// `DefinitionLibraryRegistry` for `definition.v1`,
    /// `MaterialRegistry` for `material_def.v1`).
    fn seed_bundled_content_system(world: &mut World) {
        // Gate conditions — keep the exclusive borrow short.
        if world.get_resource::<RemoteCatalogState>().is_none() {
            return;
        }
        if world.get_resource::<BundledContentSeeded>().is_some() {
            return;
        }
        let config = world.resource::<BundledContentSeedConfig>().clone();
        if !config.seed_bundled_on_start {
            world.insert_resource(BundledContentSeeded);
            return;
        }

        // Snapshot the descriptor list (clones are cheap — fn pointers).
        let descriptors: Vec<CatalogKindDescriptor> = world
            .resource::<CatalogKindRegistry>()
            .all()
            .cloned()
            .collect();

        // First pass: drive each descriptor's `seed_local_ids` against the
        // live world. Collect results before touching the message bus so we
        // never alias-borrow the world.
        let mut requests: Vec<PublishArtifactRequested> = Vec::new();
        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        for descriptor in &descriptors {
            let local_ids = (descriptor.seed_local_ids)(world);
            counts.insert(descriptor.kind, local_ids.len());
            for local_id in local_ids {
                requests.push(PublishArtifactRequested {
                    kind: descriptor.kind,
                    local_id,
                    canonical_id_override: None,
                    scope: config.scope,
                    jurisdiction: config.jurisdiction.clone(),
                    owner_org_id: config.owner_org_id,
                    published_by: config.published_by,
                });
            }
        }

        // Second pass: emit publish messages.
        if let Some(mut messages) =
            world.get_resource_mut::<Messages<PublishArtifactRequested>>()
        {
            for req in requests {
                messages.write(req);
            }
        }

        info!(
            counts = ?counts,
            "seeding bundled artifacts to remote catalog"
        );
        world.insert_resource(BundledContentSeeded);
    }

    /// Logs publish outcomes so the operator can see success / failure from
    /// desktop logs without writing a custom subscriber.
    fn log_publish_outcomes_system(mut reader: MessageReader<PublishArtifactResult>) {
        for result in reader.read() {
            match &result.outcome {
                PublishArtifactOutcome::Published {
                    artifact_id,
                    revision,
                    content_hash,
                } => {
                    info!(
                        kind = result.kind,
                        canonical_id = %result.canonical_id,
                        artifact_id = %artifact_id,
                        revision = revision,
                        content_hash = %content_hash,
                        "artifact published to catalog"
                    );
                }
                PublishArtifactOutcome::NotFound => {
                    warn!(
                        kind = result.kind,
                        canonical_id = %result.canonical_id,
                        local_id = %result.local_id,
                        "artifact publish: local lookup failed"
                    );
                }
                PublishArtifactOutcome::Failed(error) => {
                    warn!(
                        kind = result.kind,
                        canonical_id = %result.canonical_id,
                        error = %error,
                        "artifact publish failed"
                    );
                }
            }
        }
    }

    // ---- Unit tests ---------------------------------------------------------

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::Utc;

        use crate::plugins::modeling::definition::{
            DefinitionKind, Interface, ParameterSchema,
        };

        fn make_material_def() -> MaterialDef {
            MaterialDef {
                id: "test-mat-001".to_owned(),
                name: "Test Material".to_owned(),
                ..Default::default()
            }
        }

        fn make_definition(id: &str) -> Definition {
            Definition {
                id: DefinitionId(id.to_owned()),
                base_definition_id: None,
                name: format!("Test {id}"),
                definition_kind: DefinitionKind::Solid,
                definition_version: 1,
                interface: Interface {
                    parameters: ParameterSchema::default(),
                    void_declaration: None,
                    external_context_requirements: Vec::new(),
                },
                evaluators: Vec::new(),
                representations: Vec::new(),
                compound: None,
                material_assignment: None,
                domain_data: Value::Null,
            }
        }

        fn make_change_event(kind: &str) -> ChangeEvent {
            ChangeEvent {
                cursor: 1,
                op: "publish".to_owned(),
                artifact_id: Uuid::new_v4(),
                canonical_id: "test.material/foo".to_owned(),
                kind: kind.to_owned(),
                revision: 1,
                scope: "shipped".to_owned(),
                jurisdiction: vec![],
                content_hash: "abc123".to_owned(),
                manifest_hash: None,
                owner_org_id: None,
                published_at: Utc::now(),
            }
        }

        /// Builds a test app with built-in kinds registered and the apply
        /// system wired, but without spawning the poller thread.
        fn build_apply_test_app() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .init_resource::<MaterialRegistry>()
                .init_resource::<DefinitionRegistry>()
                .init_resource::<DefinitionLibraryRegistry>()
                .init_resource::<CatalogKindRegistry>()
                .add_message::<RemoteCatalogChange>()
                .add_message::<ArtifactApplied>()
                .add_message::<MaterialRegistryReloaded>()
                .add_message::<DefinitionRegistryReloaded>()
                .add_systems(PreUpdate, apply_artifact_changes_system);
            register_builtin_kinds(&mut app);
            app
        }

        /// Builds a test app with the publish system wired and a fake
        /// RemoteCatalogState so we can exercise serialise / not-found paths
        /// without a live worker thread.
        fn build_publish_test_app() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .init_resource::<MaterialRegistry>()
                .init_resource::<DefinitionRegistry>()
                .init_resource::<DefinitionLibraryRegistry>()
                .init_resource::<CatalogKindRegistry>()
                .add_message::<PublishArtifactRequested>()
                .add_message::<PublishArtifactResult>()
                .add_systems(PreUpdate, publish_artifact_requests_system);
            register_builtin_kinds(&mut app);

            // Insert a fake state so the system doesn't early-return.
            let (pub_tx, _pub_rx) = std::sync::mpsc::channel::<PublishJob>();
            let (_res_tx, res_rx) = std::sync::mpsc::channel::<PublishJobResult>();
            let (change_tx, change_rx) = std::sync::mpsc::channel::<CatalogBridgeMessage>();
            drop(change_tx);
            let (shutdown_tx, _) = tokio::sync::watch::channel(false);

            app.insert_resource(RemoteCatalogState {
                base_url: "http://127.0.0.1:18010".parse().unwrap(),
                account_id: None,
                cache_root: std::path::PathBuf::from("/tmp"),
                rx: Mutex::new(change_rx),
                shutdown_tx,
                publish_tx: Mutex::new(pub_tx),
                publish_results_rx: Mutex::new(res_rx),
            });
            app
        }

        // ---- apply tests ----------------------------------------------------

        #[test]
        fn apply_material_def_upserts_into_registry_and_emits_reload() {
            let mut app = build_apply_test_app();
            app.update();

            let def = make_material_def();
            let def_id = def.id.clone();
            let body = serde_json::to_value(&def).unwrap();
            let event = make_change_event("material_def.v1");

            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(body),
                });

            app.update();

            assert!(app
                .world()
                .resource::<MaterialRegistry>()
                .contains(&def_id));
            assert_eq!(
                app.world()
                    .resource::<Messages<MaterialRegistryReloaded>>()
                    .len(),
                1
            );
            assert_eq!(
                app.world().resource::<Messages<ArtifactApplied>>().len(),
                1
            );
        }

        #[test]
        fn apply_definition_inserts_into_registry_and_emits_reload() {
            let mut app = build_apply_test_app();
            app.update();

            let def = make_definition("def-catalog-001");
            let def_id = def.id.clone();
            let body = serde_json::to_value(&def).unwrap();
            let event = make_change_event("definition.v1");

            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(body),
                });

            app.update();

            assert!(app
                .world()
                .resource::<DefinitionRegistry>()
                .get(&def_id)
                .is_some());
            assert_eq!(
                app.world()
                    .resource::<Messages<DefinitionRegistryReloaded>>()
                    .len(),
                1
            );
        }

        #[test]
        fn apply_unknown_kind_is_no_op() {
            let mut app = build_apply_test_app();
            app.update();

            let event = make_change_event("totally.unknown.v1");
            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(Value::Null),
                });

            app.update();

            assert_eq!(
                app.world().resource::<Messages<ArtifactApplied>>().len(),
                0
            );
        }

        #[test]
        fn apply_malformed_body_logs_and_skips() {
            let mut app = build_apply_test_app();
            app.update();

            let bad_body = Value::Number(serde_json::Number::from(42));
            let event = make_change_event("material_def.v1");
            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(bad_body),
                });

            app.update();

            assert_eq!(app.world().resource::<MaterialRegistry>().count(), 0);
            assert_eq!(
                app.world()
                    .resource::<Messages<MaterialRegistryReloaded>>()
                    .len(),
                0
            );
            assert_eq!(
                app.world().resource::<Messages<ArtifactApplied>>().len(),
                0
            );
        }

        // ---- publish tests --------------------------------------------------

        #[test]
        fn publish_unknown_kind_returns_failed() {
            let mut app = build_publish_test_app();
            app.update();

            app.world_mut()
                .resource_mut::<Messages<PublishArtifactRequested>>()
                .write(PublishArtifactRequested {
                    kind: "totally.unknown.v1",
                    local_id: "x".to_owned(),
                    canonical_id_override: Some("test/x".to_owned()),
                    scope: PublishScope::Shipped,
                    jurisdiction: vec![],
                    owner_org_id: None,
                    published_by: Uuid::new_v4(),
                });

            app.update();

            let results = app.world().resource::<Messages<PublishArtifactResult>>();
            assert_eq!(results.len(), 1);
            let result = results.iter_current_update_messages().next().unwrap();
            assert!(matches!(
                result.outcome,
                PublishArtifactOutcome::Failed(_)
            ));
        }

        #[test]
        fn publish_missing_definition_returns_not_found() {
            let mut app = build_publish_test_app();
            app.update();

            app.world_mut()
                .resource_mut::<Messages<PublishArtifactRequested>>()
                .write(PublishArtifactRequested {
                    kind: "definition.v1",
                    local_id: "does-not-exist-in-registry".to_owned(),
                    canonical_id_override: None,
                    scope: PublishScope::Shipped,
                    jurisdiction: vec![],
                    owner_org_id: None,
                    published_by: Uuid::new_v4(),
                });

            app.update();

            let results = app.world().resource::<Messages<PublishArtifactResult>>();
            assert_eq!(results.len(), 1);
            let result = results.iter_current_update_messages().next().unwrap();
            assert_eq!(result.kind, "definition.v1");
            assert!(matches!(result.outcome, PublishArtifactOutcome::NotFound));
        }
    }
}

// Re-export the non-wasm items at module level.
#[cfg(not(target_arch = "wasm32"))]
pub use inner::{
    ArtifactApplied, ArtifactApplier, ArtifactSerializer, BundledContentSeedConfig,
    BundledContentSeeded, CatalogKindDescriptor, CatalogKindRegistry, DefinitionRegistryReloaded,
    MaterialRegistryReloaded, PublishArtifactOutcome, PublishArtifactRequested,
    PublishArtifactResult, PublishRequestBuilder, RemoteCatalogChange, RemoteCatalogPlugin,
    RemoteCatalogState, SeedLocalIds,
};
#[cfg(not(target_arch = "wasm32"))]
pub use talos3d_catalog_client::PublishScope;
