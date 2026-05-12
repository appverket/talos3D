//! Error type for the catalog client.

/// All errors that can occur when interacting with the talos-catalog service.
#[derive(Debug, thiserror::Error)]
pub enum CatalogClientError {
    /// An HTTP transport error from reqwest.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization or deserialization failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The server returned a non-success status code.
    #[error("server returned {code}: {body}")]
    Status { code: u16, body: String },

    /// An I/O error (typically from cache operations).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The requested resource was not found (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),

    /// The server returned an unexpected or malformed response.
    #[error("bad response: {0}")]
    BadResponse(String),
}
