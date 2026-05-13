//! Live subscriber demo for the talos-catalog change feed.
//!
//! Connects to a running catalog at `TALOS3D_CATALOG_URL` (default
//! `http://127.0.0.1:18010`) and prints each `ChangeEvent` as it arrives.
//!
//! This is PP-KBD-4b's closure of the integration-test gap: it proves the
//! `talos3d-catalog-client` crate talks to the real catalog binary end-to-end,
//! without needing to boot the full Bevy desktop binary.
//!
//! Usage:
//!
//! ```sh
//! cargo run -p talos3d-catalog-client --example subscribe
//! # in another terminal: publish a material via curl (see the demo script).
//! ```

use std::{sync::Arc, time::Duration};

use talos3d_catalog_client::{
    ChangePoller, RemoteCatalogClient, WorkspaceRemoteCache,
};
use tokio::sync::{mpsc, watch};
use url::Url;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,talos3d_catalog_client=debug")
        .init();

    let base_url: Url = std::env::var("TALOS3D_CATALOG_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:18010".into())
        .parse()?;
    let account_id = std::env::var("TALOS3D_ACCOUNT_ID")
        .ok()
        .and_then(|s| uuid::Uuid::parse_str(&s).ok());

    let cache_root = tempfile::tempdir()?.keep();
    let cache = Arc::new(WorkspaceRemoteCache::open(cache_root)?);
    let client = RemoteCatalogClient::new(base_url.clone(), account_id);

    eprintln!("subscribing to {base_url} (account={account_id:?})");
    eprintln!("kinds: material_def.v1, material_spec.v1, recipe.v1, definition.v1");
    eprintln!("(Ctrl+C to exit)\n");

    let (tx, mut rx) = mpsc::channel(64);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let poller = ChangePoller::new(
        client,
        cache,
        vec![
            "material_def.v1".into(),
            "material_spec.v1".into(),
            "recipe.v1".into(),
            "definition.v1".into(),
        ],
        Duration::from_secs(2),
    );

    let poller_handle = tokio::spawn(async move {
        if let Err(e) = poller.run(tx, shutdown_rx).await {
            eprintln!("poller exited with error: {e}");
        }
    });

    // Ctrl+C handler.
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nshutdown requested");
        let _ = shutdown_tx_clone.send(true);
    });

    while let Some(event) = rx.recv().await {
        println!(
            "cursor={cursor:>4} op={op:9} kind={kind:25} canonical_id={cid} content_hash={hash}…",
            cursor = event.cursor,
            op = event.op,
            kind = event.kind,
            cid = event.canonical_id,
            hash = &event.content_hash[..16.min(event.content_hash.len())],
        );
    }

    let _ = poller_handle.await;
    Ok(())
}
