//! Integration tests for `talos3d-catalog-client`.
//!
//! Each test spins up a minimal axum server that implements the catalog wire
//! shape just well enough to exercise the client. The server binds an ephemeral
//! port; no global state is shared between tests.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::Utc;
use talos3d_catalog_client::{
    ArtifactResolution, CatalogCache, ChangeEvent, ChangePoller, ChangesResponse,
    PublishArtifactRequest, RemoteCatalogClient, WorkspaceRemoteCache,
};
use tempfile::TempDir;
use tokio::net::TcpListener;
use url::Url;
use uuid::Uuid;

// ---- Test server state ------------------------------------------------------

#[derive(Clone)]
struct TestState {
    artifacts: Arc<Mutex<Vec<ArtifactResolution>>>,
    blobs: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    changes: Arc<Mutex<Vec<ChangeEvent>>>,
}

impl TestState {
    fn new() -> Self {
        Self {
            artifacts: Arc::new(Mutex::new(Vec::new())),
            blobs: Arc::new(Mutex::new(HashMap::new())),
            changes: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

// ---- Handlers ---------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ArtifactQuery {
    canonical_id: Option<String>,
}

async fn get_artifacts(
    Query(q): Query<ArtifactQuery>,
    State(state): State<TestState>,
) -> impl IntoResponse {
    let artifacts = state.artifacts.lock().unwrap();
    match q.canonical_id {
        Some(id) => {
            if let Some(a) = artifacts.iter().find(|a| a.canonical_id == id) {
                Json(a.clone()).into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        None => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn post_artifact(
    State(state): State<TestState>,
    Json(req): Json<PublishArtifactRequest>,
) -> impl IntoResponse {
    use sha2::{Digest, Sha256};

    let body_bytes = serde_json::to_vec(&req.body).unwrap();
    let hash = hex::encode(Sha256::digest(&body_bytes));

    let resolution = ArtifactResolution {
        artifact_id: Uuid::new_v4(),
        kind: req.kind.clone(),
        canonical_id: req.canonical_id.clone(),
        revision: 1,
        scope: req.scope.clone(),
        trust: req.trust.clone(),
        content_hash: hash.clone(),
        body: req.body.clone(),
        body_schema_rev: req.body_schema_rev,
        jurisdiction: req.jurisdiction.clone(),
        pack_release_manifest_hash: None,
        supersedes: None,
        blob_url: Some(format!("/v1/blobs/{hash}")),
    };

    // Store blob.
    state
        .blobs
        .lock()
        .unwrap()
        .insert(hash.clone(), body_bytes.clone());

    // Store artifact.
    state.artifacts.lock().unwrap().push(resolution.clone());

    // Emit a change event.
    let cursor = {
        let mut changes = state.changes.lock().unwrap();
        let cursor = (changes.len() as i64) + 1;
        changes.push(ChangeEvent {
            cursor,
            op: "publish".to_owned(),
            artifact_id: resolution.artifact_id,
            canonical_id: req.canonical_id.clone(),
            kind: req.kind.clone(),
            revision: 1,
            scope: req.scope.clone(),
            jurisdiction: req.jurisdiction.clone(),
            content_hash: hash.clone(),
            manifest_hash: None,
            owner_org_id: req.owner_org_id,
            published_at: Utc::now(),
        });
        cursor
    };

    tracing::debug!(cursor, canonical_id = %req.canonical_id, "artifact published");
    (StatusCode::CREATED, Json(resolution))
}

async fn get_blob(Path(hash): Path<String>, State(state): State<TestState>) -> impl IntoResponse {
    let blobs = state.blobs.lock().unwrap();
    match blobs.get(&hash) {
        Some(bytes) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            bytes.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct ChangesQuery {
    since: Option<i64>,
    #[serde(default)]
    _kinds: String,
    limit: Option<usize>,
}

async fn get_changes(
    Query(q): Query<ChangesQuery>,
    State(state): State<TestState>,
) -> impl IntoResponse {
    let since = q.since.unwrap_or(0);
    let limit = q.limit.unwrap_or(usize::MAX);
    let changes = state.changes.lock().unwrap();
    let filtered: Vec<ChangeEvent> = changes
        .iter()
        .filter(|e| e.cursor > since)
        .take(limit)
        .cloned()
        .collect();
    let next_cursor = filtered.last().map(|e| e.cursor).unwrap_or(since);
    Json(ChangesResponse {
        changes: filtered,
        next_cursor,
    })
}

// ---- Server bootstrap -------------------------------------------------------

async fn start_server(state: TestState) -> SocketAddr {
    let router = Router::new()
        .route("/v1/artifacts", get(get_artifacts).post(post_artifact))
        .route("/v1/blobs/:hash", get(get_blob))
        .route("/v1/changes", get(get_changes))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn client_for(addr: SocketAddr) -> RemoteCatalogClient {
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    RemoteCatalogClient::new(url, None)
}

fn example_publish_request(kind: &str, canonical_id: &str) -> PublishArtifactRequest {
    PublishArtifactRequest {
        kind: kind.to_owned(),
        canonical_id: canonical_id.to_owned(),
        body: serde_json::json!({ "name": "Test Material", "roughness": 0.5 }),
        body_schema_rev: 1,
        scope: "shipped".to_owned(),
        trust: "published".to_owned(),
        jurisdiction: vec![],
        owner_org_id: None,
        dependencies: vec![],
        published_by: Uuid::new_v4(),
    }
}

// ---- Tests ------------------------------------------------------------------

#[tokio::test]
async fn publish_and_resolve_round_trips() {
    let state = TestState::new();
    let addr = start_server(state).await;
    let client = client_for(addr);

    let req = example_publish_request("material_def.v1", "test.material/round_trip");
    let published = client
        .publish_artifact(&req)
        .await
        .expect("publish should succeed");

    assert_eq!(published.canonical_id, req.canonical_id);
    assert_eq!(published.kind, req.kind);
    assert_eq!(published.body, req.body);

    let resolved = client
        .resolve_artifact("test.material/round_trip", None, None, None)
        .await
        .expect("resolve should succeed")
        .expect("artifact should exist");

    assert_eq!(resolved.canonical_id, published.canonical_id);
    assert_eq!(resolved.content_hash, published.content_hash);
    assert_eq!(resolved.body, published.body);
}

#[tokio::test]
async fn get_blob_returns_bytes() {
    let state = TestState::new();
    let addr = start_server(state).await;
    let client = client_for(addr);

    let req = example_publish_request("material_def.v1", "test.material/blob_bytes");
    let published = client.publish_artifact(&req).await.unwrap();

    let blob = client.get_blob(&published.content_hash).await.unwrap();

    // The blob is the canonical JSON of the published body.
    let expected_bytes = serde_json::to_vec(&req.body).unwrap();
    assert_eq!(blob, expected_bytes);
}

#[tokio::test]
async fn change_poller_delivers_events_then_idles() {
    let state = TestState::new();
    let addr = start_server(state.clone()).await;
    let client = client_for(addr);

    // Pre-populate 3 events via publish.
    for i in 0..3 {
        let req = example_publish_request("material_def.v1", &format!("test.material/poller_{i}"));
        client.publish_artifact(&req).await.unwrap();
    }

    let dir = TempDir::new().unwrap();
    let cache: Arc<dyn CatalogCache> =
        Arc::new(WorkspaceRemoteCache::open(dir.path().to_path_buf()).unwrap());

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let poller = ChangePoller::new(
        client,
        cache,
        vec!["material_def.v1".to_owned()],
        Duration::from_millis(50),
    );

    let handle = tokio::spawn(poller.run(tx, shutdown_rx));

    let mut received = Vec::new();
    let timeout = tokio::time::Instant::now() + Duration::from_secs(5);
    while received.len() < 3 && tokio::time::Instant::now() < timeout {
        if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            received.push(event);
        }
    }
    assert_eq!(received.len(), 3, "should receive exactly 3 events");

    // Send shutdown and wait for the poller to exit.
    shutdown_tx.send(true).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("poller should exit within 2s after shutdown")
        .expect("poller task should not panic");
    assert!(result.is_ok(), "poller should exit cleanly: {result:?}");
}

#[tokio::test]
async fn change_poller_resumes_from_cursor() {
    let state = TestState::new();
    let addr = start_server(state.clone()).await;
    let client = client_for(addr);

    // Publish 3 events (cursors 1, 2, 3 in the test server).
    for i in 0..3 {
        let req = example_publish_request("material_def.v1", &format!("test.material/resume_{i}"));
        client.publish_artifact(&req).await.unwrap();
    }

    let dir = TempDir::new().unwrap();
    let cache: Arc<dyn CatalogCache> =
        Arc::new(WorkspaceRemoteCache::open(dir.path().to_path_buf()).unwrap());

    // Pre-seed cursor = 2; poller should only deliver event with cursor = 3.
    cache.write_cursor(2).unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let poller = ChangePoller::new(
        client,
        cache,
        vec!["material_def.v1".to_owned()],
        Duration::from_millis(50),
    );

    tokio::spawn(poller.run(tx, shutdown_rx));

    // Expect exactly 1 event (cursor=3).
    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("should receive an event within timeout")
        .expect("channel should be open");

    assert_eq!(
        event.cursor, 3,
        "should resume from cursor=2 and deliver cursor=3"
    );

    // No more events should be ready (feed is caught up).
    let maybe_extra = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(
        maybe_extra.is_err(),
        "no additional events expected after catching up"
    );

    shutdown_tx.send(true).unwrap();
}
