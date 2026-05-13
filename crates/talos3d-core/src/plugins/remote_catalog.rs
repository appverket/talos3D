//! Bevy plugin that connects the running desktop binary to the talos-catalog
//! HTTP service via a long-poll change feed.
//!
//! # Activation
//!
//! The plugin activates only when the `TALOS3D_CATALOG_URL` environment
//! variable is set. When unset, `RemoteCatalogPlugin::build` logs an info
//! message and returns without registering any systems or resources — the rest
//! of the app is unaffected.
//!
//! Set `TALOS3D_ACCOUNT_ID` (optional UUID) to enable tenant-scoped resolution.
//!
//! # Architecture
//!
//! ```text
//! OS thread "talos3d-catalog-poller"
//!   └── current-thread tokio runtime
//!       ├── ChangePoller (async long-poll loop)
//!       │   └── per-event: fetch blob (cache-then-network)
//!       │       └── tokio mpsc -> std::sync::mpsc bridge
//!       └── publish-job consumer
//!           └── per PublishJob: client.publish_artifact(...)
//!               └── result -> std::sync::mpsc bridge
//!
//! Bevy main thread
//!   PreUpdate: drain_catalog_changes_system
//!     -> MessageWriter<RemoteCatalogChange>
//!   PreUpdate: apply_material_def_changes_system
//!     -> mutates MaterialRegistry
//!     -> MessageWriter<MaterialRegistryReloaded>
//!   PreUpdate: apply_definition_changes_system
//!     -> mutates DefinitionRegistry
//!     -> MessageWriter<DefinitionRegistryReloaded>
//!   PreUpdate: publish_definition_requests_system
//!     -> sends PublishJob to worker thread
//!   PreUpdate: drain_publish_results_system
//!     -> MessageWriter<PublishDefinitionResult>
//! ```
//!
//! The dedicated OS thread with its own `current_thread` runtime keeps
//! async-tokio completely isolated from the Bevy main loop.

#[cfg(not(target_arch = "wasm32"))]
mod inner {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use bevy::prelude::*;
    use serde_json::Value;
    use url::Url;
    use uuid::Uuid;

    use talos3d_catalog_client::{
        definition_publish_request, CatalogCache, ChangeEvent, ChangePoller, PublishScope,
        RemoteCatalogClient, WorkspaceRemoteCache,
    };

    use crate::plugins::materials::{MaterialDef, MaterialRegistry};
    use crate::plugins::modeling::definition::{Definition, DefinitionId, DefinitionRegistry};

    // ---- Messages -----------------------------------------------------------

    /// A raw change event forwarded from the catalog poller, with an optional
    /// pre-fetched body.
    ///
    /// The `body` field is populated for kinds that are in the "auto-fetch"
    /// set (currently `material_def.v1` and `definition.v1`). For other kinds
    /// it is `None`.
    #[derive(Message, Debug, Clone)]
    pub struct RemoteCatalogChange {
        pub event: ChangeEvent,
        /// Pre-fetched artifact body, parsed as JSON. `None` when the fetch
        /// failed or the kind is not in the auto-fetch set.
        pub body: Option<Value>,
    }

    /// Fired after a `material_def.v1` artifact is successfully upserted into
    /// [`MaterialRegistry`].
    #[derive(Message, Debug, Clone)]
    pub struct MaterialRegistryReloaded {
        /// The `id` field of the upserted [`MaterialDef`].
        pub id: String,
    }

    /// Fired after a `definition.v1` artifact is successfully inserted into
    /// [`DefinitionRegistry`].
    #[derive(Message, Debug, Clone)]
    pub struct DefinitionRegistryReloaded {
        /// The [`DefinitionId`] of the inserted [`Definition`].
        pub id: DefinitionId,
    }

    /// Request the desktop to publish a [`Definition`] to the remote catalog.
    ///
    /// Dispatch this message to trigger an async HTTP publish on the catalog
    /// worker thread. The result is delivered as a [`PublishDefinitionResult`]
    /// message in a subsequent frame.
    #[derive(Message, Debug, Clone)]
    pub struct PublishDefinitionRequested {
        /// Local id used to look up the [`Definition`] in [`DefinitionRegistry`].
        pub definition_id: DefinitionId,
        /// `canonical_id` to publish under in the catalog.
        pub canonical_id: String,
        /// Distribution scope.
        pub scope: PublishScope,
        /// ISO country codes. Empty = universal.
        pub jurisdiction: Vec<String>,
        /// Required when `scope` is [`PublishScope::Org`].
        pub owner_org_id: Option<Uuid>,
        /// Operator or account id of the publisher.
        pub published_by: Uuid,
    }

    /// Outcome of a [`PublishDefinitionRequested`] request.
    #[derive(Message, Debug, Clone)]
    pub struct PublishDefinitionResult {
        /// Echoed back from the originating [`PublishDefinitionRequested`].
        pub definition_id: DefinitionId,
        /// Echoed back from the originating [`PublishDefinitionRequested`].
        pub canonical_id: String,
        /// Whether the publish succeeded or failed.
        pub outcome: PublishDefinitionOutcome,
    }

    /// Detailed outcome of a definition publish attempt.
    #[derive(Debug, Clone)]
    pub enum PublishDefinitionOutcome {
        /// The artifact was successfully created or revised in the catalog.
        Published {
            artifact_id: Uuid,
            revision: i32,
            content_hash: String,
        },
        /// No definition with the requested id was found in [`DefinitionRegistry`].
        NotFound,
        /// The publish failed. The inner string carries a human-readable reason.
        Failed(String),
    }

    // ---- Internal bridge types ----------------------------------------------

    /// Internal message bridging the async poller thread to the Bevy main thread.
    pub(super) struct CatalogBridgeMessage {
        pub event: ChangeEvent,
        pub body: Option<Value>,
    }

    /// A publish job sent from Bevy to the catalog worker thread.
    pub(super) struct PublishJob {
        pub definition_id: DefinitionId,
        pub canonical_id: String,
        pub request: talos3d_catalog_client::PublishArtifactRequest,
    }

    /// A publish result sent from the catalog worker thread back to Bevy.
    pub(super) struct PublishJobResult {
        pub definition_id: DefinitionId,
        pub canonical_id: String,
        pub outcome: PublishDefinitionOutcome,
    }

    // ---- Resource -----------------------------------------------------------

    /// Bevy resource that holds the live connection between the poller thread
    /// and the Bevy main thread.
    ///
    /// Inserted only when `TALOS3D_CATALOG_URL` is set.
    #[derive(Resource)]
    pub struct RemoteCatalogState {
        pub base_url: Url,
        pub account_id: Option<Uuid>,
        pub cache_root: PathBuf,
        pub(super) rx: Mutex<std::sync::mpsc::Receiver<CatalogBridgeMessage>>,
        /// Send `true` to request a clean shutdown of the poller thread.
        pub shutdown_tx: tokio::sync::watch::Sender<bool>,
        /// Send publish jobs to the catalog worker thread.
        pub(super) publish_tx: Mutex<std::sync::mpsc::Sender<PublishJob>>,
        /// Drain completed publish results from the worker thread.
        pub(super) publish_results_rx: Mutex<std::sync::mpsc::Receiver<PublishJobResult>>,
    }

    // ---- Plugin -------------------------------------------------------------

    /// Bevy plugin that subscribes to the talos-catalog change feed and
    /// hot-reloads local registries when new artifacts arrive.
    pub struct RemoteCatalogPlugin;

    impl Plugin for RemoteCatalogPlugin {
        fn build(&self, app: &mut App) {
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
                .add_message::<MaterialRegistryReloaded>()
                .add_message::<DefinitionRegistryReloaded>()
                .add_message::<PublishDefinitionRequested>()
                .add_message::<PublishDefinitionResult>()
                .add_systems(Startup, spawn_catalog_thread_system)
                .add_systems(
                    PreUpdate,
                    (
                        drain_catalog_changes_system,
                        apply_material_def_changes_system.after(drain_catalog_changes_system),
                        apply_definition_changes_system.after(drain_catalog_changes_system),
                        publish_definition_requests_system,
                        drain_publish_results_system,
                    ),
                );

            // Stash config for the Startup system.
            app.insert_resource(CatalogConfig {
                base_url,
                account_id,
            });
        }
    }

    // ---- Startup config (consumed by spawn_catalog_thread_system) -----------

    /// Temporary resource holding parsed configuration. Consumed in
    /// `spawn_catalog_thread_system` and replaced with [`RemoteCatalogState`].
    #[derive(Resource)]
    struct CatalogConfig {
        base_url: Url,
        account_id: Option<Uuid>,
    }

    // ---- Systems ------------------------------------------------------------

    /// Spawns the poller OS thread and inserts [`RemoteCatalogState`].
    fn spawn_catalog_thread_system(mut commands: Commands, config: Option<Res<CatalogConfig>>) {
        let Some(config) = config else { return };

        let base_url = config.base_url.clone();
        let account_id = config.account_id;

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

        // Publish channels: Bevy -> worker (jobs) and worker -> Bevy (results).
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

                    // Kinds whose body we pre-fetch before forwarding to Bevy.
                    let auto_fetch_kinds = [
                        "material_def.v1",
                        "material_spec.v1",
                        "recipe.v1",
                        "definition.v1",
                    ];
                    let subscription_kinds: Vec<String> =
                        auto_fetch_kinds.iter().map(|s| s.to_string()).collect();

                    // Bridge: tokio mpsc -> std mpsc (change events).
                    let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::channel::<ChangeEvent>(256);
                    let client_for_blob = client.clone();
                    let cache_for_blob = cache.clone();
                    let std_tx_bridge = std_tx.clone();

                    tokio::spawn(async move {
                        while let Some(event) = tokio_rx.recv().await {
                            let body = if auto_fetch_kinds.contains(&event.kind.as_str()) {
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
                                // Bevy side dropped; exit.
                                break;
                            }
                        }
                    });

                    // Publish-job consumer: bridge std mpsc -> tokio and back.
                    let client_for_publish = client.clone();
                    let (publish_tokio_tx, mut publish_tokio_rx) =
                        tokio::sync::mpsc::channel::<PublishJob>(64);

                    // Adapter: drain the std::mpsc publish queue into the tokio
                    // channel so the async consumer can use `.recv().await`.
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
                            let definition_id = job.definition_id.clone();
                            let canonical_id = job.canonical_id.clone();

                            let outcome =
                                match client_for_publish.publish_artifact(&job.request).await {
                                    Ok(resolution) => PublishDefinitionOutcome::Published {
                                        artifact_id: resolution.artifact_id,
                                        revision: resolution.revision,
                                        content_hash: resolution.content_hash,
                                    },
                                    Err(e) => {
                                        warn!(
                                            canonical_id = %canonical_id,
                                            error = %e,
                                            "definition publish failed"
                                        );
                                        PublishDefinitionOutcome::Failed(e.to_string())
                                    }
                                };

                            let result = PublishJobResult {
                                definition_id,
                                canonical_id,
                                outcome,
                            };
                            if publish_results_std_tx.send(result).is_err() {
                                // Bevy side dropped.
                                break;
                            }
                        }
                    });

                    let poller = ChangePoller::new(
                        client,
                        cache,
                        subscription_kinds,
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

    /// Drains the std::sync::mpsc receiver and writes [`RemoteCatalogChange`]
    /// messages into the Bevy message system.
    ///
    /// Runs every frame in `PreUpdate`.
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

    /// Applies incoming `material_def.v1` messages to the [`MaterialRegistry`].
    ///
    /// Runs every frame in `PreUpdate`, after [`drain_catalog_changes_system`].
    fn apply_material_def_changes_system(
        mut reader: MessageReader<RemoteCatalogChange>,
        mut registry: ResMut<MaterialRegistry>,
        mut writer: MessageWriter<MaterialRegistryReloaded>,
    ) {
        for change in reader.read() {
            if change.event.kind != "material_def.v1" {
                continue;
            }
            let Some(body) = &change.body else {
                continue;
            };
            match serde_json::from_value::<MaterialDef>(body.clone()) {
                Ok(def) => {
                    let id = registry.upsert(def);
                    info!(id = %id, "material_def hot-reloaded from catalog");
                    writer.write(MaterialRegistryReloaded { id });
                }
                Err(e) => {
                    warn!(
                        canonical_id = %change.event.canonical_id,
                        error = %e,
                        "failed to deserialize material_def body from catalog"
                    );
                }
            }
        }
    }

    /// Applies incoming `definition.v1` messages to the [`DefinitionRegistry`].
    ///
    /// Runs every frame in `PreUpdate`, after [`drain_catalog_changes_system`].
    fn apply_definition_changes_system(
        mut reader: MessageReader<RemoteCatalogChange>,
        mut registry: ResMut<DefinitionRegistry>,
        mut writer: MessageWriter<DefinitionRegistryReloaded>,
    ) {
        for change in reader.read() {
            if change.event.kind != "definition.v1" {
                continue;
            }
            let Some(body) = &change.body else {
                continue;
            };
            match serde_json::from_value::<Definition>(body.clone()) {
                Ok(def) => {
                    let id = def.id.clone();
                    registry.insert(def);
                    info!(id = %id.as_str(), "definition.v1 hot-reloaded from catalog");
                    writer.write(DefinitionRegistryReloaded { id });
                }
                Err(e) => {
                    warn!(
                        canonical_id = %change.event.canonical_id,
                        error = %e,
                        "failed to deserialize definition.v1 body from catalog"
                    );
                }
            }
        }
    }

    /// Handles [`PublishDefinitionRequested`] messages by serializing the
    /// [`Definition`] and sending a publish job to the catalog worker thread.
    ///
    /// Runs every frame in `PreUpdate`.
    fn publish_definition_requests_system(
        mut reader: MessageReader<PublishDefinitionRequested>,
        registry: Res<DefinitionRegistry>,
        state: Option<Res<RemoteCatalogState>>,
        mut writer: MessageWriter<PublishDefinitionResult>,
    ) {
        let Some(state) = state else { return };

        for req in reader.read() {
            let definition_id = req.definition_id.clone();
            let canonical_id = req.canonical_id.clone();

            // Look up the definition.
            let Some(def) = registry.get(&definition_id) else {
                warn!(
                    id = %definition_id.as_str(),
                    canonical_id = %canonical_id,
                    "PublishDefinitionRequested: definition not found in registry"
                );
                writer.write(PublishDefinitionResult {
                    definition_id,
                    canonical_id,
                    outcome: PublishDefinitionOutcome::NotFound,
                });
                continue;
            };

            // Serialize to JSON.
            let body = match serde_json::to_value(def) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        id = %definition_id.as_str(),
                        error = %e,
                        "PublishDefinitionRequested: failed to serialize definition"
                    );
                    writer.write(PublishDefinitionResult {
                        definition_id,
                        canonical_id,
                        outcome: PublishDefinitionOutcome::Failed(e.to_string()),
                    });
                    continue;
                }
            };

            // Build the publish request.
            let publish_req = match definition_publish_request(
                canonical_id.clone(),
                body,
                req.scope,
                req.jurisdiction.clone(),
                req.owner_org_id,
                req.published_by,
            ) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        id = %definition_id.as_str(),
                        error = %e,
                        "PublishDefinitionRequested: invalid publish parameters"
                    );
                    writer.write(PublishDefinitionResult {
                        definition_id,
                        canonical_id,
                        outcome: PublishDefinitionOutcome::Failed(e.to_string()),
                    });
                    continue;
                }
            };

            // Forward to the worker thread.
            let job = PublishJob {
                definition_id: definition_id.clone(),
                canonical_id: canonical_id.clone(),
                request: publish_req,
            };
            if let Err(e) = state.publish_tx.lock().unwrap().send(job) {
                warn!(
                    id = %definition_id.as_str(),
                    error = %e,
                    "publish_tx channel closed; dropping publish request"
                );
                writer.write(PublishDefinitionResult {
                    definition_id,
                    canonical_id,
                    outcome: PublishDefinitionOutcome::Failed(
                        "publish worker channel closed".to_owned(),
                    ),
                });
            }
        }
    }

    /// Drains completed publish results from the catalog worker thread and
    /// emits [`PublishDefinitionResult`] messages.
    ///
    /// Runs every frame in `PreUpdate`.
    fn drain_publish_results_system(
        state: Option<Res<RemoteCatalogState>>,
        mut writer: MessageWriter<PublishDefinitionResult>,
    ) {
        let Some(state) = state else { return };
        let rx = state.publish_results_rx.lock().unwrap();
        while let Ok(result) = rx.try_recv() {
            writer.write(PublishDefinitionResult {
                definition_id: result.definition_id,
                canonical_id: result.canonical_id,
                outcome: result.outcome,
            });
        }
    }

    // ---- Unit tests ---------------------------------------------------------

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::Utc;
        use uuid::Uuid;

        use crate::plugins::modeling::definition::{
            DefinitionId, DefinitionKind, Interface, ParameterSchema,
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
                domain_data: serde_json::Value::Null,
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

        fn build_material_test_app() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .init_resource::<MaterialRegistry>()
                .add_message::<RemoteCatalogChange>()
                .add_message::<MaterialRegistryReloaded>()
                .add_systems(PreUpdate, apply_material_def_changes_system);
            app
        }

        fn build_definition_test_app() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .init_resource::<DefinitionRegistry>()
                .add_message::<RemoteCatalogChange>()
                .add_message::<DefinitionRegistryReloaded>()
                .add_systems(PreUpdate, apply_definition_changes_system);
            app
        }

        fn build_publish_test_app() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .init_resource::<DefinitionRegistry>()
                .add_message::<PublishDefinitionRequested>()
                .add_message::<PublishDefinitionResult>()
                .add_systems(PreUpdate, publish_definition_requests_system);
            app
        }

        // ---- material_def tests (pre-existing, unchanged) -------------------

        #[test]
        fn apply_material_def_changes_system_upserts_into_registry() {
            let mut app = build_material_test_app();
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

            let registry = app.world().resource::<MaterialRegistry>();
            assert!(
                registry.contains(&def_id),
                "MaterialRegistry must contain the upserted def"
            );

            let reloaded_count = app
                .world()
                .resource::<Messages<MaterialRegistryReloaded>>()
                .len();
            assert_eq!(
                reloaded_count, 1,
                "one MaterialRegistryReloaded message expected"
            );
        }

        #[test]
        fn apply_material_def_changes_system_ignores_other_kinds() {
            let mut app = build_material_test_app();
            app.update();

            let def = make_material_def();
            let body = serde_json::to_value(&def).unwrap();

            let event = make_change_event("recipe.v1");
            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(body),
                });

            app.update();

            let registry = app.world().resource::<MaterialRegistry>();
            assert_eq!(
                registry.count(),
                0,
                "registry must be empty for non-material kinds"
            );

            let reloaded_count = app
                .world()
                .resource::<Messages<MaterialRegistryReloaded>>()
                .len();
            assert_eq!(
                reloaded_count, 0,
                "no MaterialRegistryReloaded for other kinds"
            );
        }

        #[test]
        fn apply_material_def_changes_system_logs_and_skips_on_malformed_body() {
            let mut app = build_material_test_app();
            app.update();

            // Body is an integer, not a MaterialDef object.
            let bad_body = serde_json::Value::Number(serde_json::Number::from(42));

            let event = make_change_event("material_def.v1");
            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(bad_body),
                });

            app.update();

            let registry = app.world().resource::<MaterialRegistry>();
            assert_eq!(
                registry.count(),
                0,
                "registry must be empty after malformed body"
            );

            let reloaded_count = app
                .world()
                .resource::<Messages<MaterialRegistryReloaded>>()
                .len();
            assert_eq!(reloaded_count, 0, "no message fired for malformed body");
        }

        // ---- definition.v1 consume tests ------------------------------------

        #[test]
        fn apply_definition_changes_system_upserts_into_registry() {
            let mut app = build_definition_test_app();
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

            let registry = app.world().resource::<DefinitionRegistry>();
            assert!(
                registry.get(&def_id).is_some(),
                "DefinitionRegistry must contain the inserted definition"
            );

            let reloaded_count = app
                .world()
                .resource::<Messages<DefinitionRegistryReloaded>>()
                .len();
            assert_eq!(
                reloaded_count, 1,
                "one DefinitionRegistryReloaded message expected"
            );
        }

        #[test]
        fn apply_definition_changes_system_ignores_other_kinds() {
            let mut app = build_definition_test_app();
            app.update();

            let def = make_definition("def-catalog-002");
            let body = serde_json::to_value(&def).unwrap();

            let event = make_change_event("material_def.v1");
            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(body),
                });

            app.update();

            let registry = app.world().resource::<DefinitionRegistry>();
            assert_eq!(
                registry.list().len(),
                0,
                "DefinitionRegistry must be empty for non-definition kinds"
            );

            let reloaded_count = app
                .world()
                .resource::<Messages<DefinitionRegistryReloaded>>()
                .len();
            assert_eq!(
                reloaded_count, 0,
                "no DefinitionRegistryReloaded for other kinds"
            );
        }

        #[test]
        fn apply_definition_changes_system_logs_and_skips_on_malformed_body() {
            let mut app = build_definition_test_app();
            app.update();

            // Body is an integer, not a Definition object.
            let bad_body = serde_json::Value::Number(serde_json::Number::from(99));

            let event = make_change_event("definition.v1");
            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event,
                    body: Some(bad_body),
                });

            app.update();

            let registry = app.world().resource::<DefinitionRegistry>();
            assert_eq!(
                registry.list().len(),
                0,
                "DefinitionRegistry must be empty after malformed body"
            );

            let reloaded_count = app
                .world()
                .resource::<Messages<DefinitionRegistryReloaded>>()
                .len();
            assert_eq!(
                reloaded_count, 0,
                "no message fired for malformed definition body"
            );
        }

        // ---- publish glue tests ---------------------------------------------

        #[test]
        fn publish_definition_requests_system_emits_not_found_when_id_missing() {
            let mut app = build_publish_test_app();
            app.update();

            let missing_id = DefinitionId("does-not-exist-in-registry".to_owned());
            // Note: no RemoteCatalogState — publish_definition_requests_system
            // early-returns when the state resource is absent, so the
            // not-found path is exercised by injecting a dummy state-absent
            // scenario via the registry lookup failing before state access.
            //
            // To exercise the not-found branch we need the state present.
            // Build minimal channels so we can insert RemoteCatalogState.
            let (pub_tx, _pub_rx) = std::sync::mpsc::channel::<PublishJob>();
            let (_res_tx, res_rx) = std::sync::mpsc::channel::<PublishJobResult>();
            let (change_tx, change_rx) = std::sync::mpsc::channel::<CatalogBridgeMessage>();
            drop(change_tx); // we don't need to send changes in this test
            let (shutdown_tx, _) = tokio::sync::watch::channel(false);

            let state = RemoteCatalogState {
                base_url: "http://127.0.0.1:18010".parse().unwrap(),
                account_id: None,
                cache_root: std::path::PathBuf::from("/tmp"),
                rx: Mutex::new(change_rx),
                shutdown_tx,
                publish_tx: Mutex::new(pub_tx),
                publish_results_rx: Mutex::new(res_rx),
            };
            app.insert_resource(state);

            app.world_mut()
                .resource_mut::<Messages<PublishDefinitionRequested>>()
                .write(PublishDefinitionRequested {
                    definition_id: missing_id.clone(),
                    canonical_id: "com.example/missing".to_owned(),
                    scope: PublishScope::Shipped,
                    jurisdiction: vec![],
                    owner_org_id: None,
                    published_by: Uuid::new_v4(),
                });

            app.update();

            let results = app.world().resource::<Messages<PublishDefinitionResult>>();
            assert_eq!(results.len(), 1, "expected exactly one result message");

            let result = results.iter_current_update_messages().next().unwrap();
            assert_eq!(result.definition_id, missing_id);
            assert!(
                matches!(result.outcome, PublishDefinitionOutcome::NotFound),
                "expected NotFound outcome"
            );
        }
    }
}

// Re-export the non-wasm items at module level.
#[cfg(not(target_arch = "wasm32"))]
pub use inner::{
    DefinitionRegistryReloaded, MaterialRegistryReloaded, PublishDefinitionOutcome,
    PublishDefinitionRequested, PublishDefinitionResult, RemoteCatalogChange, RemoteCatalogPlugin,
    RemoteCatalogState,
};
