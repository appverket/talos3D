//! Async HTTP client for the talos-catalog service.

use url::Url;
use uuid::Uuid;

use crate::{
    dto::{ArtifactResolution, ChangesResponse, PublishArtifactRequest},
    error::CatalogClientError,
};

/// Async HTTP client for the talos-catalog service.
///
/// Clone-cheap: the inner `reqwest::Client` is `Arc`-backed.
#[derive(Debug, Clone)]
pub struct RemoteCatalogClient {
    http: reqwest::Client,
    base_url: Url,
    account_id: Option<Uuid>,
}

impl RemoteCatalogClient {
    /// Create a new client.
    ///
    /// `account_id` is forwarded as the `account` query parameter where the
    /// catalog uses it for tenant-scoped resolution. When `None`, only
    /// `Scope::Shipped` artifacts are visible.
    pub fn new(base_url: Url, account_id: Option<Uuid>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "talos3d-catalog-client/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("build reqwest client");
        Self {
            http,
            base_url,
            account_id,
        }
    }

    /// Resolve an artifact by `canonical_id`.
    ///
    /// Returns `None` on HTTP 404. Other 4xx/5xx status codes return
    /// `Err(CatalogClientError::Status { .. })`.
    pub async fn resolve_artifact(
        &self,
        canonical_id: &str,
        jurisdiction: Option<&str>,
        kind: Option<&str>,
        revision: Option<i32>,
    ) -> Result<Option<ArtifactResolution>, CatalogClientError> {
        let mut url = self
            .base_url
            .join("v1/artifacts")
            .map_err(|e| CatalogClientError::BadResponse(e.to_string()))?;

        {
            let mut q = url.query_pairs_mut();
            q.append_pair("canonical_id", canonical_id);
            if let Some(j) = jurisdiction {
                q.append_pair("jurisdiction", j);
            }
            if let Some(k) = kind {
                q.append_pair("kind", k);
            }
            if let Some(r) = revision {
                q.append_pair("revision", &r.to_string());
            }
            if let Some(acc) = self.account_id {
                q.append_pair("account", &acc.to_string());
            }
        }

        let resp = self.http.get(url).send().await?;
        let status = resp.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CatalogClientError::Status {
                code: status.as_u16(),
                body,
            });
        }

        let resolution: ArtifactResolution = resp.json().await?;
        Ok(Some(resolution))
    }

    /// Publish a new artifact revision.
    ///
    /// Returns the full `ArtifactResolution` for the created row.
    pub async fn publish_artifact(
        &self,
        req: &PublishArtifactRequest,
    ) -> Result<ArtifactResolution, CatalogClientError> {
        let url = self
            .base_url
            .join("v1/artifacts")
            .map_err(|e| CatalogClientError::BadResponse(e.to_string()))?;

        let resp = self.http.post(url).json(req).send().await?;
        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CatalogClientError::Status {
                code: status.as_u16(),
                body,
            });
        }

        let resolution: ArtifactResolution = resp.json().await?;
        Ok(resolution)
    }

    /// Fetch the long-poll change feed.
    ///
    /// - `since`: cursor from the last known event (0 = start of feed).
    /// - `kinds`: artifact kinds to filter by (empty slice = all kinds).
    /// - `limit`: maximum number of events to return.
    pub async fn list_changes(
        &self,
        since: i64,
        kinds: &[&str],
        limit: Option<usize>,
    ) -> Result<ChangesResponse, CatalogClientError> {
        let mut url = self
            .base_url
            .join("v1/changes")
            .map_err(|e| CatalogClientError::BadResponse(e.to_string()))?;

        {
            let mut q = url.query_pairs_mut();
            q.append_pair("since", &since.to_string());
            if !kinds.is_empty() {
                q.append_pair("kinds", &kinds.join(","));
            }
            if let Some(l) = limit {
                q.append_pair("limit", &l.to_string());
            }
            if let Some(acc) = self.account_id {
                q.append_pair("account", &acc.to_string());
            }
        }

        let resp = self.http.get(url).send().await?;
        let status = resp.status();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CatalogClientError::Status {
                code: status.as_u16(),
                body,
            });
        }

        let changes: ChangesResponse = resp.json().await?;
        Ok(changes)
    }

    /// Fetch raw blob bytes by content hash.
    ///
    /// The dev catalog returns the blob inline as `application/octet-stream`.
    /// The production catalog (PP-KBD-3) returns a 302 redirect to a signed R2
    /// URL; reqwest follows redirects automatically.
    pub async fn get_blob(&self, content_hash: &str) -> Result<Vec<u8>, CatalogClientError> {
        let path = format!("v1/blobs/{content_hash}");
        let url = self
            .base_url
            .join(&path)
            .map_err(|e| CatalogClientError::BadResponse(e.to_string()))?;

        let resp = self.http.get(url).send().await?;
        let status = resp.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CatalogClientError::NotFound(content_hash.to_owned()));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CatalogClientError::Status {
                code: status.as_u16(),
                body,
            });
        }

        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }
}
