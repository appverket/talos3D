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
//!       └── ChangePoller (async long-poll loop)
//!           └── per-event: fetch blob (cache-then-network)
//!               └── tokio mpsc -> std::sync::mpsc bridge
//!
//! Bevy main thread
//!   PreUpdate: drain_catalog_changes_system
//!     -> MessageWriter<RemoteCatalogChange>
//!   PreUpdate: apply_material_def_changes_system
//!     -> mutates MaterialRegistry
//!     -> MessageWriter<MaterialRegistryReloaded>
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
        ChangeEvent, ChangePoller, RemoteCatalogClient, WorkspaceRemoteCache,
    };

    use crate::plugins::materials::{MaterialDef, MaterialRegistry};

    // ---- Messages -----------------------------------------------------------

    /// A raw change event forwarded from the catalog poller, with an optional
    /// pre-fetched body.
    ///
    /// The `body` field is populated for kinds that are in the "auto-fetch"
    /// set (currently `material_def.v1`). For other kinds it is `None`.
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

    // ---- Internal bridge message ---------------------------------------------

    /// Internal message bridging the async poller thread to the Bevy main thread.
    pub(super) struct CatalogBridgeMessage {
        pub event: ChangeEvent,
        pub body: Option<Value>,
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
                .add_systems(Startup, spawn_catalog_thread_system)
                .add_systems(
                    PreUpdate,
                    (
                        drain_catalog_changes_system,
                        apply_material_def_changes_system.after(drain_catalog_changes_system),
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

        let cache = match WorkspaceRemoteCache::open(cache_root.clone()) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                error!(error = %e, "failed to open catalog cache; remote catalog disabled");
                return;
            }
        };

        let (std_tx, std_rx) = std::sync::mpsc::channel::<CatalogBridgeMessage>();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let state = RemoteCatalogState {
            base_url: base_url.clone(),
            account_id,
            cache_root: cache_root.clone(),
            rx: Mutex::new(std_rx),
            shutdown_tx,
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

                    // Bridge: tokio mpsc -> std mpsc.
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

    // ---- Unit tests ---------------------------------------------------------

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::Utc;
        use uuid::Uuid;

        fn make_material_def() -> MaterialDef {
            MaterialDef {
                id: "test-mat-001".to_owned(),
                name: "Test Material".to_owned(),
                ..Default::default()
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

        fn build_test_app() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .init_resource::<MaterialRegistry>()
                .add_message::<RemoteCatalogChange>()
                .add_message::<MaterialRegistryReloaded>()
                .add_systems(PreUpdate, apply_material_def_changes_system);
            app
        }

        #[test]
        fn apply_material_def_changes_system_upserts_into_registry() {
            let mut app = build_test_app();
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
            let mut app = build_test_app();
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
            let mut app = build_test_app();
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
    }
}

// Re-export the non-wasm items at module level.
#[cfg(not(target_arch = "wasm32"))]
pub use inner::{
    MaterialRegistryReloaded, RemoteCatalogChange, RemoteCatalogPlugin, RemoteCatalogState,
};
