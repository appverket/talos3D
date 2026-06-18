//! Bevy plugin that connects the running desktop binary (or browser) to the
//! talos-catalog HTTP service via a long-poll change feed.
//!
//! # Activation
//!
//! The plugin always inserts the [`CatalogKindRegistry`] and
//! [`CatalogTransportConfig`] resources so other plugins can declare their
//! kinds, but it only spawns the catalog poller/worker when a catalog URL is
//! resolved:
//!
//! - **Native**: `TALOS3D_CATALOG_URL` env var drives
//!   [`ActiveArtifactStore`] selection (cloud vs local-file fallback).
//! - **wasm32**: [`CatalogTransportConfig::url`] if set, else a
//!   `<meta name="talos3d-catalog-url">` in the host document, else inert.
//!
//! Set `TALOS3D_ACCOUNT_ID` / `CatalogTransportConfig::account_id` (optional
//! UUID) to enable tenant-scoped resolution.
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
//! Native:
//!   OS thread "talos3d-catalog-poller"
//!     └── current-thread tokio runtime
//!         ├── ChangePoller (async long-poll loop)
//!         │   └── per-event: fetch blob if descriptor.auto_fetch_body
//!         │       └── tokio mpsc -> std::sync::mpsc bridge
//!         └── publish-job consumer
//!             └── per PublishJob: client.publish_artifact(...)
//!                 └── result -> std::sync::mpsc bridge
//!
//! wasm32 (PP-WCT-1 Stage B):
//!   Bevy Update: wasm_poll_catalog_system (2-second timer, no JS timer dep)
//!     └── when timer fires AND no fetch in-flight:
//!         wasm_bindgen_futures::spawn_local(async {
//!             GET {base}/v1/changes?since={cursor}&kinds=...
//!             for each auto-fetch kind: GET {base}/v1/blobs/{hash}
//!             send CatalogBridgeMessage per event into std::sync::mpsc
//!             advance stored cursor
//!         })
//!   NOTE: publish-from-web is deferred to PP-WCT-2.
//!
//! Bevy main thread (both targets):
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
//! async-tokio completely isolated from the Bevy main loop on native.

// The catalog substrate (kind registry, descriptors, the generic apply /
// publish / seed pipeline, events, and the plugin) is platform-neutral and
// compiles on every target. Only the *transport* — store selection and the
// OS-thread poller/publish workers — is native-only; those items carry their
// own `#[cfg(not(target_arch = "wasm32"))]` inside this module. PP-WCT-1 Stage B
// adds the wasm browser-fetch transport.
mod inner {
    use std::{
        collections::{HashMap, HashSet},
        sync::{Arc, Mutex},
    };
    // Native-only: the worker spawner uses OS threads.
    #[cfg(not(target_arch = "wasm32"))]
    use std::thread;

    use bevy::ecs::system::SystemState;
    use bevy::prelude::*;
    use serde_json::Value;
    use uuid::Uuid;

    use std::sync::atomic::AtomicBool;

    use crate::plugins::materials::{MaterialDef, MaterialRegistry};
    use crate::plugins::modeling::definition::{
        Definition, DefinitionId, DefinitionLibraryRegistry, DefinitionRegistry,
    };
    use crate::storage::wire::{
        definition_publish_request, material_def_publish_request, ChangeEvent,
        PublishArtifactRequest, PublishError, PublishScope,
    };
    // ChangesResponse is only decoded on wasm (the native path uses the store's
    // own async subscription loop which returns ChangeEvents directly).
    #[cfg(target_arch = "wasm32")]
    use crate::storage::wire::ChangesResponse;
    // Native-only: the store trait + concrete stores back the transport workers.
    #[cfg(not(target_arch = "wasm32"))]
    use crate::storage::{ActiveArtifactStore, ArtifactStore, LocalFileArtifactStore};

    // wasm32-only: browser fetch + futures glue for the change-feed transport.
    #[cfg(target_arch = "wasm32")]
    use std::{cell::Cell, rc::Rc, sync::mpsc::Sender};
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_futures::spawn_local;

    // ---- Transport configuration (platform-neutral) -------------------------

    /// Platform-neutral configuration for the catalog change-feed transport.
    ///
    /// Always registered by [`RemoteCatalogPlugin`] (via `init_resource`) so
    /// host apps can insert it before plugin build to supply a catalog URL
    /// without environment variables.
    ///
    /// **Native**: `TALOS3D_CATALOG_URL` / `TALOS3D_ACCOUNT_ID` env vars take
    /// precedence over this resource (they drive [`ActiveArtifactStore`]
    /// selection inside the existing `pick_default_store_system`).
    ///
    /// **wasm32**: URL resolution order is:
    /// 1. [`CatalogTransportConfig::url`] if `Some`.
    /// 2. `<meta name="talos3d-catalog-url" content="…">` in the document.
    /// 3. `None` → transport stays inert.
    ///
    /// Account ID resolution (wasm32):
    /// 1. [`CatalogTransportConfig::account_id`] if `Some`.
    /// 2. `<meta name="talos3d-catalog-account" content="…">` in the document.
    /// 3. `None` → no `&account=…` query parameter appended.
    #[derive(Resource, Default, Clone, Debug)]
    pub struct CatalogTransportConfig {
        /// Base URL of the talos-catalog service, e.g.
        /// `"https://catalog.example.com"`. No trailing slash.
        pub url: Option<String>,
        /// Optional tenant account UUID for scoped artifact resolution.
        pub account_id: Option<Uuid>,
    }

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

    /// Internal message bridging the subscription thread to the Bevy main
    /// thread.
    pub(super) struct CatalogBridgeMessage {
        pub event: ChangeEvent,
        pub body: Option<Value>,
    }

    /// A publish job sent from Bevy to the publish-worker thread. The
    /// `kind` + `local_id` round-trip through the worker unchanged so the
    /// result can be routed back as a strongly-typed
    /// [`PublishArtifactResult`].
    ///
    /// On wasm32 the fields are not yet read — the publish worker that
    /// consumes them is native-only, and the browser publish path is
    /// PP-WCT-2. The type stays in the platform-neutral channel contract.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(super) struct PublishJob {
        pub kind: &'static str,
        pub local_id: String,
        pub canonical_id: String,
        pub request: PublishArtifactRequest,
    }

    /// A publish result sent from the publish-worker thread back to Bevy.
    pub(super) struct PublishJobResult {
        pub kind: &'static str,
        pub local_id: String,
        pub canonical_id: String,
        pub outcome: PublishArtifactOutcome,
    }

    // ---- wasm32 poll state (NonSend resource) --------------------------------

    /// Timer resource for the wasm browser-fetch poll loop.
    ///
    /// A plain `Resource` is fine here because `Timer` is `Send`; only
    /// `WasmCatalogPollState` (which holds `Rc`) must be `NonSend`.
    #[cfg(target_arch = "wasm32")]
    #[derive(Resource)]
    pub(super) struct WasmCatalogPollTimer {
        pub timer: Timer,
    }

    /// Non-send resource that holds the wasm poll loop state.
    ///
    /// Uses `Rc<Cell<…>>` because wasm32 is single-threaded: we share the
    /// cursor and in-flight flag between the Bevy system and the
    /// `spawn_local` future without requiring `Send`. Do NOT attempt to make
    /// these `Arc<AtomicXxx>` — the wasm target has no scheduler preemption
    /// and the borrow discipline of `Rc` is correct here.
    ///
    /// `publish_tx` is held only to satisfy the `RemoteCatalogState` channel
    /// contract; publish-from-web is deferred to PP-WCT-2 and the receiver
    /// is intentionally dropped after construction so any accidental send
    /// would immediately produce a disconnected-channel error rather than a
    /// silent backlog.
    #[cfg(target_arch = "wasm32")]
    pub(super) struct WasmCatalogPollState {
        /// Base URL (no trailing slash).
        pub base_url: String,
        /// Optional account UUID for `&account=` query param.
        pub account_id: Option<Uuid>,
        /// Cursor advanced after each successful poll.
        pub cursor: Rc<Cell<i64>>,
        /// `true` while a `spawn_local` fetch is in-flight.
        pub in_flight: Rc<Cell<bool>>,
        /// Clone of the sender side of the change-message channel.
        pub change_tx: Sender<CatalogBridgeMessage>,
        /// Snapshot of the auto-fetch kind set taken at transport spawn time.
        pub auto_fetch_kinds: HashSet<String>,
    }

    // ---- Resource -----------------------------------------------------------

    /// Bevy resource that holds the channels into the substrate's two
    /// worker threads (publish + subscription). Inserted by
    /// [`spawn_store_workers_system`] in `PostStartup` whenever the
    /// substrate is active.
    ///
    /// The name `RemoteCatalogState` is preserved for diff readability;
    /// internally the resource is now store-agnostic.
    #[derive(Resource)]
    pub struct RemoteCatalogState {
        pub description: String,
        pub(super) rx: Mutex<std::sync::mpsc::Receiver<CatalogBridgeMessage>>,
        pub shutdown: Arc<AtomicBool>,
        pub(super) publish_tx: Mutex<std::sync::mpsc::Sender<PublishJob>>,
        pub(super) publish_results_rx: Mutex<std::sync::mpsc::Receiver<PublishJobResult>>,
    }

    // ---- Plugin -------------------------------------------------------------

    /// Bevy plugin that wires the artifact substrate (kind registry,
    /// generic publish/apply pipeline, seeding) onto an
    /// [`ActiveArtifactStore`].
    ///
    /// Store selection at `Startup`:
    ///
    /// - If a plugin earlier in the chain has already inserted
    ///   [`ActiveArtifactStore`], the substrate uses that.
    /// - Otherwise, if `TALOS3D_CATALOG_URL` is set, the substrate
    ///   constructs a [`CloudArtifactStore`].
    /// - Otherwise, the substrate constructs a
    ///   [`LocalFileArtifactStore`] under the OS cache root.
    pub struct RemoteCatalogPlugin;

    impl Plugin for RemoteCatalogPlugin {
        fn build(&self, app: &mut App) {
            // The kind registry and transport config are always available so
            // other plugins can declare kinds and supply a URL before the
            // transport is spawned.
            app.init_resource::<CatalogKindRegistry>();
            app.init_resource::<CatalogTransportConfig>();
            register_builtin_kinds(app);

            app.add_message::<RemoteCatalogChange>()
                .add_message::<ArtifactApplied>()
                .add_message::<MaterialRegistryReloaded>()
                .add_message::<DefinitionRegistryReloaded>()
                .add_message::<PublishArtifactRequested>()
                .add_message::<PublishArtifactResult>()
                .init_resource::<BundledContentSeedConfig>()
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

            // Transport (store selection + worker spawn) is platform-specific.
            //
            // Native: the default-store fallback runs in PostStartup so the
            // Commands buffer from any Startup plugin (e.g. a cloud-store
            // plugin) has flushed and an existing ActiveArtifactStore is
            // observable; `.chain()` enforces fallback-before-worker-spawn.
            //
            // wasm32 (PP-WCT-1 Stage B): spawn_wasm_transport_system inserts
            // RemoteCatalogState and the NonSend WasmCatalogPollState when a
            // URL is resolvable; wasm_poll_catalog_system drives the 2-second
            // fetch loop each Update tick thereafter.
            #[cfg(not(target_arch = "wasm32"))]
            app.add_systems(
                PostStartup,
                (pick_default_store_system, spawn_store_workers_system).chain(),
            );
            #[cfg(target_arch = "wasm32")]
            {
                app.add_systems(PostStartup, spawn_wasm_transport_system);
                app.add_systems(Update, wasm_poll_catalog_system);
            }
        }
    }

    /// Startup system: installs the default
    /// [`LocalFileArtifactStore`] if no other plugin has installed an
    /// [`ActiveArtifactStore`] yet.
    ///
    /// Cloud-side stores (e.g. the `CloudArtifactStore` provided by
    /// `talos3d-catalog-client`'s Bevy plugin in
    /// `appverket-infra/services/products/talos3d/talos3d-catalog-client`)
    /// run their own Startup system *before* this one — when
    /// `TALOS3D_CATALOG_URL` is set their plugin inserts the cloud store
    /// first and this default no-ops.
    #[cfg(not(target_arch = "wasm32"))]
    fn pick_default_store_system(
        mut commands: Commands,
        existing: Option<Res<ActiveArtifactStore>>,
    ) {
        if existing.is_some() {
            return;
        }
        match LocalFileArtifactStore::open_default() {
            Ok(store) => {
                info!(
                    description = store.description(),
                    "artifact substrate: using local-file store (default)"
                );
                commands.insert_resource(ActiveArtifactStore::new(Arc::new(store)));
            }
            Err(e) => {
                error!(error = %e, "failed to open local-file store; substrate disabled");
            }
        }
    }

    // ---- wasm32 transport systems -------------------------------------------

    /// wasm32 browser-fetch catalog change-feed transport (PP-WCT-1 Stage B).
    ///
    /// # What this does
    ///
    /// Runs once at `PostStartup` as an exclusive world system. Resolves the
    /// catalog base URL from [`CatalogTransportConfig`] (priority 1) or from
    /// the document `<meta name="talos3d-catalog-url">` element (priority 2).
    /// If no URL can be resolved the substrate remains inert — no
    /// [`RemoteCatalogState`] is inserted.
    ///
    /// When a URL is available it:
    /// - Creates the `std::sync::mpsc` channels that the shared drain/apply
    ///   pipeline expects.
    /// - Inserts [`RemoteCatalogState`] (the `Send` resource that the
    ///   platform-neutral apply path drains).
    /// - Inserts [`WasmCatalogPollTimer`] (2-second repeating Bevy timer).
    /// - Inserts [`WasmCatalogPollState`] as a `NonSend` resource (holds
    ///   `Rc<Cell<…>>` — wasm is single-threaded).
    ///
    /// # Not yet implemented (PP-WCT-2)
    ///
    /// Publish-from-web: the publish channels are created and wired into
    /// `RemoteCatalogState` for structural completeness, but the publish
    /// receiver is immediately dropped. Any [`PublishArtifactRequested`]
    /// message on wasm will log a warning and be silently dropped by the
    /// existing `publish_artifact_requests_system` when it tries to send into
    /// the disconnected channel.
    ///
    /// # Live end-to-end verification
    ///
    /// The browser + catalog end-to-end is verified at deploy time; there is
    /// no running browser or catalog service in CI. The pure ingest path
    /// (change event → [`CatalogBridgeMessage`] → apply → registry) is
    /// covered by native unit tests via `ingest_change_event`.
    #[cfg(target_arch = "wasm32")]
    fn spawn_wasm_transport_system(world: &mut World) {
        // 1. Resolve base URL.
        let base_url = {
            let config = world.resource::<CatalogTransportConfig>();
            match resolve_wasm_url(config) {
                Some(u) => u,
                None => {
                    bevy::log::debug!(
                        "remote_catalog (wasm): no catalog URL configured \
                         (set CatalogTransportConfig::url or \
                         <meta name=\"talos3d-catalog-url\"> in the document); \
                         substrate present but inert"
                    );
                    return;
                }
            }
        };

        // 2. Resolve optional account id.
        let account_id = {
            let config = world.resource::<CatalogTransportConfig>();
            resolve_wasm_account_id(config)
        };

        // 3. Snapshot auto-fetch kinds.
        let auto_fetch_kinds: HashSet<String> = world
            .resource::<CatalogKindRegistry>()
            .auto_fetch_kinds()
            .into_iter()
            .collect();

        // 4. Create channels.
        let (change_tx, change_rx) = std::sync::mpsc::channel::<CatalogBridgeMessage>();
        // Publish channels: receiver dropped immediately — PP-WCT-2.
        let (publish_tx, _publish_rx_dropped) = std::sync::mpsc::channel::<PublishJob>();
        let (_publish_results_tx_dropped, publish_results_rx) =
            std::sync::mpsc::channel::<PublishJobResult>();
        let shutdown = Arc::new(AtomicBool::new(false));

        bevy::log::info!(
            "remote_catalog (wasm): browser fetch transport active at {base_url} \
             (publish-from-web deferred to PP-WCT-2)"
        );

        // 5. Insert the Send resource the apply pipeline drains.
        world.insert_resource(RemoteCatalogState {
            description: format!("browser-fetch:{base_url}"),
            rx: Mutex::new(change_rx),
            shutdown,
            publish_tx: Mutex::new(publish_tx),
            publish_results_rx: Mutex::new(publish_results_rx),
        });

        // 6. Insert the poll timer (2-second repeating).
        world.insert_resource(WasmCatalogPollTimer {
            timer: Timer::from_seconds(2.0, TimerMode::Repeating),
        });

        // 7. Insert the NonSend poll state (requires exclusive world access —
        // the system already is exclusive because it takes `&mut World`).
        world.insert_non_send_resource(WasmCatalogPollState {
            base_url,
            account_id,
            cursor: Rc::new(Cell::new(0i64)),
            in_flight: Rc::new(Cell::new(false)),
            change_tx,
            auto_fetch_kinds,
        });
    }

    /// Resolves the catalog base URL for wasm:
    /// 1. [`CatalogTransportConfig::url`] if `Some`.
    /// 2. `<meta name="talos3d-catalog-url" content="…">` in the document.
    /// 3. `None`.
    #[cfg(target_arch = "wasm32")]
    fn resolve_wasm_url(config: &CatalogTransportConfig) -> Option<String> {
        if let Some(url) = &config.url {
            return Some(url.clone());
        }
        read_meta_content("talos3d-catalog-url")
    }

    /// Resolves the optional account UUID for wasm:
    /// 1. [`CatalogTransportConfig::account_id`] if `Some`.
    /// 2. `<meta name="talos3d-catalog-account" content="…">` parsed as UUID.
    /// 3. `None`.
    #[cfg(target_arch = "wasm32")]
    fn resolve_wasm_account_id(config: &CatalogTransportConfig) -> Option<Uuid> {
        if let Some(id) = config.account_id {
            return Some(id);
        }
        read_meta_content("talos3d-catalog-account").and_then(|s| Uuid::parse_str(&s).ok())
    }

    /// Reads `content` attribute of `<meta name="{name}">` from the document.
    /// Returns `None` if the document or element is absent, or if the
    /// attribute is missing / empty.
    #[cfg(target_arch = "wasm32")]
    fn read_meta_content(name: &str) -> Option<String> {
        let window = web_sys::window()?;
        let document = window.document()?;
        let selector = format!("meta[name=\"{name}\"]");
        // query_selector returns Result<Option<Element>, JsValue>.
        let element = document.query_selector(&selector).ok().flatten()?;
        let content = element.get_attribute("content")?;
        if content.is_empty() {
            None
        } else {
            Some(content)
        }
    }

    /// wasm32 Update system: ticks the poll timer and, when it fires and no
    /// fetch is already in-flight, spawns a `spawn_local` future that:
    ///
    /// 1. `GET {base}/v1/changes?since={cursor}&kinds={kinds}&limit=200`
    ///    (with optional `&account={uuid}`).
    /// 2. For each returned [`ChangeEvent`] whose `kind` is in the
    ///    auto-fetch set: `GET {base}/v1/blobs/{content_hash}`.
    /// 3. Sends each [`CatalogBridgeMessage`] into the `std::sync::mpsc`
    ///    channel that [`drain_catalog_changes_system`] drains each tick.
    /// 4. Advances the stored cursor to `next_cursor`.
    /// 5. Clears the in-flight flag (also clears it on error so the next
    ///    tick retries — errors are logged, not fatal).
    ///
    /// # Live end-to-end verification
    ///
    /// The browser + catalog end-to-end is verified at deploy time; there is
    /// no running browser or catalog service in CI. See `ingest_change_event`
    /// for the native-testable pure transform layer.
    #[cfg(target_arch = "wasm32")]
    fn wasm_poll_catalog_system(
        time: Res<Time>,
        poll_timer: Option<ResMut<WasmCatalogPollTimer>>,
        poll_state: Option<NonSend<WasmCatalogPollState>>,
    ) {
        let (Some(mut poll_timer), Some(poll_state)) = (poll_timer, poll_state) else {
            return;
        };

        poll_timer.timer.tick(time.delta());
        if !poll_timer.timer.just_finished() {
            return;
        }
        if poll_state.in_flight.get() {
            return;
        }

        poll_state.in_flight.set(true);

        // Clone the Rc handles + owned values to move into the async future.
        // Rc is safe here because wasm is single-threaded and spawn_local
        // never crosses a thread boundary.
        let base_url = poll_state.base_url.clone();
        let account_id = poll_state.account_id;
        let cursor_cell = Rc::clone(&poll_state.cursor);
        let in_flight_cell = Rc::clone(&poll_state.in_flight);
        let change_tx = poll_state.change_tx.clone();
        let auto_fetch_kinds = poll_state.auto_fetch_kinds.clone();

        spawn_local(async move {
            let result = wasm_fetch_poll(
                &base_url,
                account_id,
                &cursor_cell,
                &change_tx,
                &auto_fetch_kinds,
            )
            .await;
            if let Err(err) = result {
                bevy::log::warn!(
                    "remote_catalog (wasm): poll error: {}; will retry next tick",
                    err
                );
            }
            in_flight_cell.set(false);
        });
    }

    /// Async inner body of the wasm poll loop. Separated from the system so
    /// error propagation is clean — the system clears `in_flight` after `await`.
    #[cfg(target_arch = "wasm32")]
    async fn wasm_fetch_poll(
        base_url: &str,
        account_id: Option<Uuid>,
        cursor_cell: &Rc<Cell<i64>>,
        change_tx: &Sender<CatalogBridgeMessage>,
        auto_fetch_kinds: &HashSet<String>,
    ) -> Result<(), String> {
        use gloo_net::http::Request;

        let cursor = cursor_cell.get();

        // 1. Build the changes URL. The `kinds` query param is the
        // comma-separated subscription filter. We use the auto-fetch kinds,
        // which matches the native subscription exactly (it also filters on
        // `registry.auto_fetch_kinds()` — see `spawn_store_workers_system`).
        // Today every built-in kind is auto-fetch, so this covers all kinds;
        // subscribing to non-auto-fetch kinds (delivered with `body: None`) is
        // future work shared with the native path.
        let kinds_param: String = auto_fetch_kinds
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");

        let mut url = format!("{base_url}/v1/changes?since={cursor}&kinds={kinds_param}&limit=200");
        if let Some(acct) = account_id {
            url.push_str(&format!("&account={acct}"));
        }

        let changes_resp = Request::get(&url)
            .send()
            .await
            .map_err(|e| format!("changes GET failed: {e}"))?;

        if !changes_resp.ok() {
            return Err(format!(
                "changes GET returned status {}",
                changes_resp.status()
            ));
        }

        let payload: ChangesResponse = changes_resp
            .json::<ChangesResponse>()
            .await
            .map_err(|e| format!("changes JSON parse failed: {e}"))?;

        // 2. For each event: optionally fetch blob, then send message.
        for event in payload.changes {
            let content_hash = event.content_hash.clone();
            let kind = event.kind.clone();

            let body: Option<Value> = if auto_fetch_kinds.contains(&kind) {
                let blob_url = format!("{base_url}/v1/blobs/{content_hash}");
                match Request::get(&blob_url).send().await {
                    Ok(resp) if resp.ok() => match resp.json::<Value>().await {
                        Ok(v) => Some(v),
                        Err(e) => {
                            bevy::log::warn!(
                                "remote_catalog (wasm): blob JSON parse failed \
                                     for hash={content_hash}: {e}"
                            );
                            None
                        }
                    },
                    Ok(resp) => {
                        bevy::log::warn!(
                            "remote_catalog (wasm): blob GET status {} \
                             for hash={content_hash}",
                            resp.status()
                        );
                        None
                    }
                    Err(e) => {
                        bevy::log::warn!(
                            "remote_catalog (wasm): blob GET failed \
                             for hash={content_hash}: {e}"
                        );
                        None
                    }
                }
            } else {
                None
            };

            // Use ingest_change_event to fold the blob decision into the
            // canonical message type — but since we already fetched above,
            // we pass a simple closure that returns the pre-fetched body.
            let already_fetched = body;
            let msg = ingest_change_event(event, auto_fetch_kinds, |_hash| already_fetched.clone());
            if change_tx.send(msg).is_err() {
                // Receiver dropped (app shutting down).
                return Err("change_tx channel closed".to_owned());
            }
        }

        // 3. Advance cursor.
        cursor_cell.set(payload.next_cursor);
        Ok(())
    }

    // ---- Platform-neutral ingest helper -------------------------------------

    /// Converts a [`ChangeEvent`] into a [`CatalogBridgeMessage`], calling
    /// `fetch_blob` only when the event's `kind` is in `auto_fetch_kinds`.
    ///
    /// This function is intentionally free of wasm/fetch types so it can be
    /// unit-tested on the native host target. The caller supplies a
    /// `fetch_blob` closure that abstracts the transport (OS blocking read,
    /// async browser fetch result, or a test stub).
    ///
    /// `fetch_blob` receives the `content_hash` and returns `Some(Value)` on
    /// success or `None` on failure (the caller is responsible for logging).
    // Used by the wasm transport (cfg'd away on native) and by tests; the
    // native non-test build never calls it directly, hence the scoped allow.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(super) fn ingest_change_event(
        event: ChangeEvent,
        auto_fetch_kinds: &HashSet<String>,
        fetch_blob: impl FnOnce(&str) -> Option<Value>,
    ) -> CatalogBridgeMessage {
        let body = if auto_fetch_kinds.contains(&event.kind) {
            fetch_blob(&event.content_hash)
        } else {
            None
        };
        CatalogBridgeMessage { event, body }
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

        // Libraries are the source of truth for bundled content — their
        // namespaced canonical_id is what the catalog already knows the
        // artifact by. Only fall back to the live DefinitionRegistry
        // when the id is genuinely local (a user-authored definition not
        // promoted to a library).
        //
        // Looking up live-first would race with apply-from-catalog:
        // after a hot-reload the same id sits in both the live registry
        // and the library, and we'd round-trip the bundled artifact
        // under a `local::` namespace, which the catalog rejects with
        // 409 "content_hash already exists under a different
        // canonical_id".
        let libraries = world.resource::<DefinitionLibraryRegistry>();
        let from_library = libraries
            .list()
            .into_iter()
            .find_map(|lib| lib.get(&did).map(|d| (d.clone(), Some(lib.id.0.clone()))));
        let (def, library_id) = match from_library {
            Some(pair) => pair,
            None => match world.resource::<DefinitionRegistry>().get(&did) {
                Some(def) => (def.clone(), None),
                None => {
                    return Err(format!(
                        "definition '{}' not found in registry or libraries",
                        local_id
                    ))
                }
            },
        };

        let canonical_id = match library_id {
            Some(lib_id) => format!("{}::{}", lib_id, local_id),
            None => format!("local::{}", local_id),
        };
        let body = serde_json::to_value(&def).map_err(|e| e.to_string())?;
        Ok((canonical_id, body))
    }

    fn apply_definition(
        world: &mut World,
        _canonical_id: &str,
        body: &Value,
    ) -> Result<(), String> {
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

    /// Spawns the substrate's worker threads:
    ///
    /// - A **publish worker** that consumes [`PublishJob`]s from the
    ///   Bevy main thread and forwards each to `store.put(...)`.
    /// - A **subscription worker** that calls
    ///   `store.run_subscription(...)` and bridges incoming events back
    ///   to the Bevy main thread as [`CatalogBridgeMessage`]s.
    ///
    /// Inserts the [`RemoteCatalogState`] resource holding the channels.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_store_workers_system(
        mut commands: Commands,
        store_res: Option<Res<ActiveArtifactStore>>,
        registry: Res<CatalogKindRegistry>,
    ) {
        let Some(store_res) = store_res else {
            // No store available — substrate is inert. Apply / publish
            // systems early-return when RemoteCatalogState is absent.
            return;
        };
        let store: Arc<dyn ArtifactStore> = store_res.0.clone();
        let description = store.description().to_owned();

        // Snapshot the auto-fetch kinds at spawn time. Kinds registered
        // after PostStartup will not influence the subscription
        // (acceptable for the static-by-plugin-build model we use today).
        let subscription_kinds: Vec<String> = registry.auto_fetch_kinds();
        let auto_fetch_kinds: Vec<String> = subscription_kinds.clone();

        let (change_tx, change_rx) = std::sync::mpsc::channel::<CatalogBridgeMessage>();
        let (publish_tx, publish_rx) = std::sync::mpsc::channel::<PublishJob>();
        let (publish_results_tx, publish_results_rx) =
            std::sync::mpsc::channel::<PublishJobResult>();
        let shutdown = Arc::new(AtomicBool::new(false));

        commands.insert_resource(RemoteCatalogState {
            description: description.clone(),
            rx: Mutex::new(change_rx),
            shutdown: shutdown.clone(),
            publish_tx: Mutex::new(publish_tx),
            publish_results_rx: Mutex::new(publish_results_rx),
        });

        // --- Publish-worker thread ---
        let publish_store = store.clone();
        let publish_results_tx_for_thread = publish_results_tx.clone();
        thread::Builder::new()
            .name("talos3d-artifact-publish-worker".into())
            .spawn(move || {
                while let Ok(job) = publish_rx.recv() {
                    let kind = job.kind;
                    let local_id = job.local_id.clone();
                    let canonical_id = job.canonical_id.clone();

                    let outcome = match publish_store.put(&job.request) {
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

                    if publish_results_tx_for_thread
                        .send(PublishJobResult {
                            kind,
                            local_id,
                            canonical_id,
                            outcome,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn publish worker thread");

        // --- Subscription-worker thread ---
        let subscription_store = store.clone();
        let subscription_store_for_callback = subscription_store.clone();
        let subscription_shutdown = shutdown.clone();
        let since_cursor = subscription_store.read_cursor();
        let change_tx_for_thread = change_tx.clone();
        thread::Builder::new()
            .name("talos3d-artifact-subscription-worker".into())
            .spawn(move || {
                let on_event: crate::storage::artifact_store::StoreEventCallback =
                    Box::new(move |event: ChangeEvent| {
                        // For local-file stores, the blob is reachable via
                        // `get_blob`; for cloud stores we pre-fetch via
                        // the cache warm-up inside CloudArtifactStore. In
                        // either case we attempt a synchronous read
                        // through the store now to deliver the body
                        // alongside the event when the kind is in the
                        // auto-fetch set.
                        let body = if auto_fetch_kinds.iter().any(|k| k == &event.kind) {
                            match subscription_store_for_callback.get_blob(&event.content_hash) {
                                Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                                    .map_err(|e| {
                                        warn!(
                                            hash = %event.content_hash,
                                            error = %e,
                                            "blob is not valid JSON"
                                        );
                                    })
                                    .ok(),
                                Err(e) => {
                                    warn!(
                                        hash = %event.content_hash,
                                        canonical_id = %event.canonical_id,
                                        error = %e,
                                        "failed to fetch blob for catalog event"
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        let _ = change_tx_for_thread.send(CatalogBridgeMessage { event, body });
                    });

                if let Err(e) = subscription_store.run_subscription(
                    subscription_kinds,
                    since_cursor,
                    on_event,
                    subscription_shutdown,
                ) {
                    error!(error = %e, "artifact subscription exited with error");
                } else {
                    info!(store = %description, "artifact subscription exited cleanly");
                }
            })
            .expect("spawn subscription worker thread");
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
        mut reader_state: Local<
            Option<SystemState<MessageReader<'static, 'static, RemoteCatalogChange>>>,
        >,
    ) {
        let state = reader_state.get_or_insert_with(|| SystemState::new(world));
        let changes: Vec<RemoteCatalogChange> = {
            let mut reader = state
                .get_mut(world)
                .expect("remote catalog change reader should validate");
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
        mut reader_state: Local<
            Option<SystemState<MessageReader<'static, 'static, PublishArtifactRequested>>>,
        >,
    ) {
        if world.get_resource::<RemoteCatalogState>().is_none() {
            return;
        }

        let state = reader_state.get_or_insert_with(|| SystemState::new(world));
        let requests: Vec<PublishArtifactRequested> = {
            let mut reader = state
                .get_mut(world)
                .expect("publish artifact request reader should validate");
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
        if let Some(mut messages) = world.get_resource_mut::<Messages<PublishArtifactRequested>>() {
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
            DefinitionKind, DefinitionVisibility, Interface, ParameterSchema,
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
                visibility: DefinitionVisibility::PublicRoot,
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
                owner_account_id: None,
                owner_project_id: None,
                owner_workspace_id: None,
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

            app.insert_resource(RemoteCatalogState {
                description: "test://fake".to_owned(),
                rx: Mutex::new(change_rx),
                shutdown: Arc::new(AtomicBool::new(false)),
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

            assert!(app.world().resource::<MaterialRegistry>().contains(&def_id));
            assert_eq!(
                app.world()
                    .resource::<Messages<MaterialRegistryReloaded>>()
                    .len(),
                1
            );
            assert_eq!(app.world().resource::<Messages<ArtifactApplied>>().len(), 1);
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

            assert_eq!(app.world().resource::<Messages<ArtifactApplied>>().len(), 0);
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
            assert_eq!(app.world().resource::<Messages<ArtifactApplied>>().len(), 0);
        }

        // ---- ingest_change_event unit tests ---------------------------------

        fn make_auto_fetch_set(kinds: &[&str]) -> HashSet<String> {
            kinds.iter().map(|s| s.to_string()).collect()
        }

        #[test]
        fn ingest_auto_fetch_kind_calls_fetcher_and_sets_body() {
            let event = make_change_event("definition.v1");
            let auto_fetch = make_auto_fetch_set(&["definition.v1"]);
            let stub_body = serde_json::json!({ "id": "stub-def", "name": "stub" });
            let stub_body_clone = stub_body.clone();
            let mut fetcher_called = false;

            let msg = ingest_change_event(event.clone(), &auto_fetch, |hash| {
                fetcher_called = true;
                assert_eq!(hash, event.content_hash);
                Some(stub_body_clone.clone())
            });

            assert!(fetcher_called, "fetcher must be called for auto-fetch kind");
            assert_eq!(msg.body.as_ref(), Some(&stub_body));
            assert_eq!(msg.event.kind, "definition.v1");
        }

        #[test]
        fn ingest_non_auto_fetch_kind_skips_fetcher_and_body_is_none() {
            let event = make_change_event("recipe.v1");
            let auto_fetch = make_auto_fetch_set(&["definition.v1"]); // recipe.v1 NOT in set

            let msg = ingest_change_event(event.clone(), &auto_fetch, |_hash| {
                panic!("fetcher must NOT be called for non-auto-fetch kind");
            });

            assert!(
                msg.body.is_none(),
                "body must be None for non-auto-fetch kind"
            );
            assert_eq!(msg.event.kind, "recipe.v1");
        }

        // ---- end-to-end native ingest→apply test ----------------------------

        /// Builds a `ChangesResponse` with a `definition.v1` event, runs each
        /// event through `ingest_change_event` with a stub blob, pushes the
        /// resulting `CatalogBridgeMessage`s through the existing apply path,
        /// and asserts that the [`DefinitionRegistry`] gained the entry and
        /// [`DefinitionRegistryReloaded`] fired.
        ///
        /// This test proves the ingest→apply contract on the native host target
        /// without any browser or catalog service.
        #[test]
        fn ingest_to_apply_definition_e2e() {
            let def = make_definition("e2e-def-001");
            let def_id = def.id.clone();
            let def_body = serde_json::to_value(&def).unwrap();
            let event = make_change_event("definition.v1");

            let auto_fetch = make_auto_fetch_set(&["definition.v1"]);
            let stub_body = def_body.clone();
            let msg = ingest_change_event(event, &auto_fetch, |_hash| Some(stub_body));

            // Push message through the apply path.
            let mut app = build_apply_test_app();
            app.update();

            app.world_mut()
                .resource_mut::<Messages<RemoteCatalogChange>>()
                .write(RemoteCatalogChange {
                    event: msg.event,
                    body: msg.body,
                });

            app.update();

            assert!(
                app.world()
                    .resource::<DefinitionRegistry>()
                    .get(&def_id)
                    .is_some(),
                "definition must be present in registry after apply"
            );
            assert_eq!(
                app.world()
                    .resource::<Messages<DefinitionRegistryReloaded>>()
                    .len(),
                1,
                "DefinitionRegistryReloaded must fire exactly once"
            );
            assert_eq!(
                app.world().resource::<Messages<ArtifactApplied>>().len(),
                1,
                "ArtifactApplied must fire exactly once"
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
            assert!(matches!(result.outcome, PublishArtifactOutcome::Failed(_)));
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

// Re-export the catalog substrate at module level. These are platform-neutral
// (the native-only transport functions stay private inside `inner`).
pub use crate::storage::wire::PublishScope;
pub use inner::{
    ArtifactApplied, ArtifactApplier, ArtifactSerializer, BundledContentSeedConfig,
    BundledContentSeeded, CatalogKindDescriptor, CatalogKindRegistry, CatalogTransportConfig,
    DefinitionRegistryReloaded, MaterialRegistryReloaded, PublishArtifactOutcome,
    PublishArtifactRequested, PublishArtifactResult, PublishRequestBuilder, RemoteCatalogChange,
    RemoteCatalogPlugin, RemoteCatalogState, SeedLocalIds,
};
