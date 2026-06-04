use super::*;

#[cfg(feature = "model-api")]
const MODEL_API_DEFAULT_HTTP_PORT: u16 = 24842;
#[cfg(feature = "model-api")]
const MODEL_API_DEFAULT_HTTP_HOST: &str = "127.0.0.1";
#[cfg(feature = "model-api")]
const MODEL_API_INSTANCE_ENV: &str = "TALOS3D_INSTANCE_ID";
#[cfg(feature = "model-api")]
const MODEL_API_PORT_ENV: &str = "TALOS3D_MODEL_API_PORT";
#[cfg(feature = "model-api")]
const MODEL_API_REGISTRY_DIR_ENV: &str = "TALOS3D_INSTANCE_REGISTRY_DIR";
#[cfg(feature = "model-api")]
const MODEL_API_DEFAULT_REGISTRY_DIR: &str = "/tmp/talos3d-instances";
#[cfg(feature = "model-api")]
const MODEL_API_MCP_CONFIG_PATHS_ENV: &str = "TALOS3D_MCP_CONFIG_PATHS";
#[cfg(feature = "model-api")]
const MODEL_API_WRITE_MCP_CONFIG_ENV: &str = "TALOS3D_WRITE_MCP_CONFIG";
#[cfg(feature = "model-api")]
const MODEL_API_MCP_SERVER_NAME: &str = "talos3d";

#[cfg(feature = "model-api")]
#[derive(Resource, Debug)]
pub(super) struct ModelApiDiscoveryCleanup {
    registry_path: PathBuf,
    mcp_config_paths: Vec<PathBuf>,
    http_url: String,
}

#[cfg(feature = "model-api")]
impl ModelApiDiscoveryCleanup {
    pub(super) fn new(runtime_info: &ModelApiRuntimeInfo) -> Self {
        let mcp_config_paths = if should_write_mcp_config() {
            mcp_config_paths()
        } else {
            Vec::new()
        };
        Self {
            registry_path: PathBuf::from(&runtime_info.registry_path),
            mcp_config_paths,
            http_url: runtime_info.http_url.clone(),
        }
    }
}

#[cfg(feature = "model-api")]
impl Drop for ModelApiDiscoveryCleanup {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.registry_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "failed to remove MCP instance registry {}: {error}",
                    self.registry_path.display()
                );
            }
        }
        for path in &self.mcp_config_paths {
            if let Err(error) = remove_matching_mcp_client_config(path, &self.http_url) {
                eprintln!(
                    "failed to clean MCP client config {}: {error}",
                    path.display()
                );
            }
        }
    }
}

#[cfg(feature = "model-api")]
pub(super) fn spawn_model_api_server(
    sender: mpsc::Sender<ModelApiRequest>,
    runtime_info: ModelApiRuntimeInfo,
    http_listener: StdTcpListener,
) {
    let http_sender = sender.clone();

    // Stdio transport (existing)
    let spawn_result = thread::Builder::new()
        .name("talos3d-model-api".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("failed to build model API runtime: {error}");
                    return;
                }
            };

            runtime.block_on(async move {
                let server = ModelApiServer::new(sender);
                let transport = transport::stdio();
                match server.serve(transport).await {
                    Ok(service) => {
                        if let Err(error) = service.waiting().await {
                            eprintln!("model API server failed while waiting: {error}");
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if !message.contains("connection closed") {
                            eprintln!("failed to start model API server: {message}");
                        }
                    }
                }
            });
        });

    if let Err(error) = spawn_result {
        eprintln!("failed to spawn model API server thread: {error}");
    }

    // HTTP transport for streamable MCP clients
    let spawn_result = thread::Builder::new()
        .name("talos3d-model-api-http".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("failed to build model API HTTP runtime: {error}");
                    return;
                }
            };

            runtime.block_on(async move {
                let ct = tokio_util::sync::CancellationToken::new();
                let sender = http_sender;
                let config = StreamableHttpServerConfig::default()
                    .with_stateful_mode(false)
                    .with_json_response(true)
                    .with_cancellation_token(ct.clone());
                let service: StreamableHttpService<ModelApiServer, LocalSessionManager> =
                    StreamableHttpService::new(
                        move || Ok(ModelApiServer::new(sender.clone())),
                        Default::default(),
                        config,
                    );

                let guard = LocalAccessGuard::for_port(runtime_info.http_port);
                let router = axum::Router::new()
                    .nest_service("/mcp", service)
                    .layer(axum::middleware::from_fn_with_state(
                        guard,
                        enforce_local_access,
                    ));
                let addr = format!("{}:{}", runtime_info.http_host, runtime_info.http_port);
                let tcp_listener = match tokio::net::TcpListener::from_std(http_listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        eprintln!("failed to adopt model API HTTP listener on {addr}: {error}");
                        return;
                    }
                };
                eprintln!(
                    "talos3d instance {} MCP {} registry {}",
                    runtime_info.instance_id, runtime_info.http_url, runtime_info.registry_path
                );
                if let Err(error) = axum::serve(tcp_listener, router)
                    .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                    .await
                {
                    eprintln!("model API HTTP server failed: {error}");
                }
            });
        });

    if let Err(error) = spawn_result {
        eprintln!("failed to spawn model API HTTP thread: {error}");
    }
}

/// Loopback access guard for the model-api HTTP transport.
///
/// The server binds to `127.0.0.1`, but loopback binding alone does not stop a
/// malicious web page: a site the user visits can have the browser POST
/// JSON-RPC to `http://127.0.0.1:<port>/mcp` (a DNS-rebinding / cross-origin
/// drive-by), driving the full MCP tool surface — including file-path save/load
/// — with no authentication. This guard enforces the MCP Streamable-HTTP
/// requirement to validate `Origin` and reject non-loopback hosts.
///
/// Authn (per-instance bearer token, and the cloud-bridge/gateway path to a
/// web-deployed instance) is a separate, planned layer; this struct is the seam
/// it will extend.
#[cfg(feature = "model-api")]
#[derive(Clone)]
struct LocalAccessGuard {
    allowed_hosts: std::sync::Arc<Vec<String>>,
    allowed_origins: std::sync::Arc<Vec<String>>,
}

#[cfg(feature = "model-api")]
impl LocalAccessGuard {
    fn for_port(port: u16) -> Self {
        Self {
            allowed_hosts: std::sync::Arc::new(vec![
                format!("127.0.0.1:{port}"),
                format!("localhost:{port}"),
                format!("[::1]:{port}"),
            ]),
            allowed_origins: std::sync::Arc::new(vec![
                format!("http://127.0.0.1:{port}"),
                format!("http://localhost:{port}"),
                format!("http://[::1]:{port}"),
            ]),
        }
    }
}

/// Pure access decision, factored out so it can be unit-tested without an HTTP
/// harness. Returns `true` only when the request looks like a local,
/// same-origin (or non-browser) client.
#[cfg(feature = "model-api")]
fn local_access_allowed(
    headers: &axum::http::HeaderMap,
    allowed_hosts: &[String],
    allowed_origins: &[String],
) -> bool {
    // DNS-rebinding defense: the Host header must name a loopback authority we
    // actually bound. A site that resolves its own name to 127.0.0.1 still
    // sends its own name in Host, so this rejects it. Absent Host is rejected;
    // legitimate loopback clients always send it.
    let host_ok = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| allowed_hosts.iter().any(|allowed| allowed == host));
    if !host_ok {
        return false;
    }

    // Cross-origin defense: browsers attach Origin on cross-origin fetch/XHR;
    // non-browser MCP clients omit it. If present, it must be a loopback origin
    // we serve.
    match headers.get(axum::http::header::ORIGIN) {
        Some(origin) => origin
            .to_str()
            .ok()
            .is_some_and(|origin| allowed_origins.iter().any(|allowed| allowed == origin)),
        None => true,
    }
}

#[cfg(feature = "model-api")]
async fn enforce_local_access(
    axum::extract::State(guard): axum::extract::State<LocalAccessGuard>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    if local_access_allowed(
        request.headers(),
        &guard.allowed_hosts,
        &guard.allowed_origins,
    ) {
        Ok(next.run(request).await)
    } else {
        Err(axum::http::StatusCode::FORBIDDEN)
    }
}

#[cfg(feature = "model-api")]
pub(super) fn resolve_model_api_runtime() -> Result<(ModelApiRuntimeInfo, StdTcpListener), String> {
    let app_name = current_app_name();
    let pid = std::process::id();
    let started_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_millis();
    let instance_id = env::var(MODEL_API_INSTANCE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{app_name}-{pid}-{started_at_unix_ms}"));
    let requested_port = env::var(MODEL_API_PORT_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value.parse::<u16>().map_err(|error| {
                format!(
                    "invalid {} value {:?}: {}",
                    MODEL_API_PORT_ENV, value, error
                )
            })
        })
        .transpose()?;
    let registry_dir = env::var(MODEL_API_REGISTRY_DIR_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(MODEL_API_DEFAULT_REGISTRY_DIR));

    let listener = bind_model_api_listener(requested_port)?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to configure model API HTTP listener: {error}"))?;
    let http_port = listener
        .local_addr()
        .map_err(|error| format!("failed to read model API HTTP listener address: {error}"))?
        .port();
    let http_url = format!("http://{MODEL_API_DEFAULT_HTTP_HOST}:{http_port}/mcp");
    let registry_path = write_instance_registry_manifest(
        &registry_dir,
        &instance_id,
        &app_name,
        pid,
        http_port,
        started_at_unix_ms,
        requested_port,
    )?;
    let runtime_info = ModelApiRuntimeInfo {
        instance_id,
        app_name,
        pid,
        http_host: MODEL_API_DEFAULT_HTTP_HOST.to_string(),
        http_port,
        http_url,
        registry_path: registry_path.display().to_string(),
        started_at_unix_ms,
        requested_port,
    };
    write_local_mcp_client_configs(&runtime_info);

    Ok((runtime_info, listener))
}

#[cfg(feature = "model-api")]
fn bind_model_api_listener(requested_port: Option<u16>) -> Result<StdTcpListener, String> {
    let preferred_port = requested_port.unwrap_or(MODEL_API_DEFAULT_HTTP_PORT);
    let preferred_addr = format!("{MODEL_API_DEFAULT_HTTP_HOST}:{preferred_port}");
    match StdTcpListener::bind(&preferred_addr) {
        Ok(listener) => Ok(listener),
        Err(error) if requested_port.is_none() && preferred_port == MODEL_API_DEFAULT_HTTP_PORT => {
            let fallback_addr = format!("{MODEL_API_DEFAULT_HTTP_HOST}:0");
            let listener = StdTcpListener::bind(&fallback_addr).map_err(|fallback_error| {
                format!(
                    "failed to bind model API HTTP on {preferred_addr} ({error}) and fallback {fallback_addr} ({fallback_error})"
                )
            })?;
            eprintln!(
                "model API default port {} was busy; using auto-assigned port {}",
                MODEL_API_DEFAULT_HTTP_PORT,
                listener
                    .local_addr()
                    .map_err(|addr_error| format!(
                        "failed to read fallback listener address: {addr_error}"
                    ))?
                    .port()
            );
            Ok(listener)
        }
        Err(error) => Err(format!(
            "failed to bind model API HTTP on {preferred_addr}: {error}"
        )),
    }
}

#[cfg(feature = "model-api")]
fn current_app_name() -> String {
    env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "talos3d".to_string())
}

#[cfg(feature = "model-api")]
fn write_instance_registry_manifest(
    registry_dir: &Path,
    instance_id: &str,
    app_name: &str,
    pid: u32,
    http_port: u16,
    started_at_unix_ms: u128,
    requested_port: Option<u16>,
) -> Result<PathBuf, String> {
    fs::create_dir_all(registry_dir).map_err(|error| {
        format!(
            "failed to create instance registry directory {}: {error}",
            registry_dir.display()
        )
    })?;
    let registry_path = registry_dir.join(format!("{instance_id}.json"));
    let manifest = serde_json::json!({
        "instance_id": instance_id,
        "app_name": app_name,
        "pid": pid,
        "http_host": MODEL_API_DEFAULT_HTTP_HOST,
        "http_port": http_port,
        "http_url": format!("http://{MODEL_API_DEFAULT_HTTP_HOST}:{http_port}/mcp"),
        "registry_path": registry_path.display().to_string(),
        "started_at_unix_ms": started_at_unix_ms,
        "requested_port": requested_port
    });
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("failed to serialize instance manifest: {error}"))?;
    fs::write(&registry_path, bytes).map_err(|error| {
        format!(
            "failed to write instance manifest {}: {error}",
            registry_path.display()
        )
    })?;
    Ok(registry_path)
}

#[cfg(feature = "model-api")]
fn write_local_mcp_client_configs(runtime_info: &ModelApiRuntimeInfo) {
    if !should_write_mcp_config() {
        return;
    }

    for path in mcp_config_paths() {
        if let Err(error) = write_mcp_client_config(&path, &runtime_info.http_url) {
            eprintln!(
                "failed to write MCP client config {}: {error}",
                path.display()
            );
        }
    }
}

#[cfg(feature = "model-api")]
fn should_write_mcp_config() -> bool {
    !env::var(MODEL_API_WRITE_MCP_CONFIG_ENV)
        .ok()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no"))
}

#[cfg(feature = "model-api")]
fn mcp_config_paths() -> Vec<PathBuf> {
    if let Ok(value) = env::var(MODEL_API_MCP_CONFIG_PATHS_ENV) {
        let paths: Vec<PathBuf> = env::split_paths(&value).collect();
        if !paths.is_empty() {
            return paths;
        }
    }

    let current_dir = match env::current_dir() {
        Ok(path) => path,
        Err(_) => return Vec::new(),
    };
    default_mcp_config_paths_from(&current_dir)
}

#[cfg(feature = "model-api")]
fn default_mcp_config_paths_from(current_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for dir in current_dir.ancestors() {
        if is_talos3d_core_root(dir) || is_talos3d_workspace_root(dir) {
            let path = dir.join(".mcp.json");
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }
    paths
}

#[cfg(feature = "model-api")]
fn is_talos3d_core_root(dir: &Path) -> bool {
    dir.join("docs/MCP_MODEL_API.md").is_file()
        && dir.join("app/Cargo.toml").is_file()
        && dir.join("crates/talos3d-core").is_dir()
}

#[cfg(feature = "model-api")]
fn is_talos3d_workspace_root(dir: &Path) -> bool {
    dir.join("AGENTS.md").is_file()
        && dir.join("talos3d-core/docs/MCP_MODEL_API.md").is_file()
        && dir.join("talos3d-core/app/Cargo.toml").is_file()
}

#[cfg(feature = "model-api")]
fn write_mcp_client_config(path: &Path, http_url: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let existing = match fs::read_to_string(path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let config = merged_mcp_client_config(existing.as_deref(), http_url)?;
    let bytes = serde_json::to_vec_pretty(&config)
        .map_err(|error| format!("failed to serialize MCP client config: {error}"))?;
    fs::write(path, [bytes.as_slice(), b"\n"].concat()).map_err(|error| error.to_string())
}

#[cfg(feature = "model-api")]
fn remove_matching_mcp_client_config(path: &Path, http_url: &str) -> Result<(), String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let mut root = serde_json::from_str::<serde_json::Value>(&contents)
        .map_err(|error| format!("existing config is not valid JSON: {error}"))?;
    let Some(servers) = root
        .get_mut("mcpServers")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };
    let remove = servers
        .get(MODEL_API_MCP_SERVER_NAME)
        .and_then(serde_json::Value::as_object)
        .and_then(|server| server.get("url").or_else(|| server.get("http_url")))
        .and_then(serde_json::Value::as_str)
        == Some(http_url);
    if !remove {
        return Ok(());
    }
    servers.remove(MODEL_API_MCP_SERVER_NAME);
    let bytes = serde_json::to_vec_pretty(&root)
        .map_err(|error| format!("failed to serialize MCP client config: {error}"))?;
    fs::write(path, [bytes.as_slice(), b"\n"].concat()).map_err(|error| error.to_string())
}

#[cfg(feature = "model-api")]
fn merged_mcp_client_config(
    existing: Option<&str>,
    http_url: &str,
) -> Result<serde_json::Value, String> {
    let mut root = match existing {
        Some(contents) if !contents.trim().is_empty() => {
            serde_json::from_str::<serde_json::Value>(contents)
                .map_err(|error| format!("existing config is not valid JSON: {error}"))?
        }
        _ => serde_json::json!({}),
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let object = root.as_object_mut().expect("root was normalized to object");
    let servers = object
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    servers
        .as_object_mut()
        .expect("mcpServers was normalized to object")
        .insert(
            MODEL_API_MCP_SERVER_NAME.to_string(),
            serde_json::json!({ "url": http_url }),
        );
    Ok(root)
}

#[cfg(feature = "model-api")]
pub(super) fn annotate_window_title_with_model_api_instance(
    runtime_info: Res<ModelApiRuntimeInfo>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    if window.title.contains(&runtime_info.instance_id) {
        return;
    }
    window.title = format!(
        "{} [{} @ {}]",
        window.title, runtime_info.instance_id, runtime_info.http_port
    );
}

#[cfg(all(test, feature = "model-api"))]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, HeaderValue};

    fn guard() -> LocalAccessGuard {
        LocalAccessGuard::for_port(24842)
    }

    fn allowed(headers: &HeaderMap) -> bool {
        let g = guard();
        local_access_allowed(headers, &g.allowed_hosts, &g.allowed_origins)
    }

    #[test]
    fn loopback_client_without_origin_is_allowed() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:24842"));
        assert!(allowed(&headers));
    }

    #[test]
    fn localhost_host_alias_is_allowed() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:24842"));
        assert!(allowed(&headers));
    }

    #[test]
    fn same_origin_loopback_request_is_allowed() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:24842"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:24842"),
        );
        assert!(allowed(&headers));
    }

    #[test]
    fn missing_host_is_rejected() {
        let headers = HeaderMap::new();
        assert!(!allowed(&headers));
    }

    #[test]
    fn dns_rebinding_host_is_rejected() {
        // Attacker site whose name resolves to 127.0.0.1 still carries its own
        // authority in the Host header.
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("evil.example:24842"));
        assert!(!allowed(&headers));
    }

    #[test]
    fn cross_origin_browser_request_is_rejected() {
        // Host is spoofable-but-correct via proxies; the Origin attached by a
        // browser on a cross-origin fetch is what blocks the drive-by.
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:24842"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!allowed(&headers));
    }

    #[test]
    fn wrong_loopback_port_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:9999"));
        assert!(!allowed(&headers));
    }

    #[test]
    fn mcp_client_config_merge_preserves_existing_servers() {
        let existing = r#"{
  "mcpServers": {
    "other": { "command": "other-mcp" }
  }
}"#;

        let config = merged_mcp_client_config(Some(existing), "http://127.0.0.1:24842/mcp")
            .expect("config should merge");

        assert_eq!(
            config["mcpServers"]["talos3d"]["url"],
            "http://127.0.0.1:24842/mcp"
        );
        assert_eq!(config["mcpServers"]["other"]["command"], "other-mcp");
    }

    #[test]
    fn default_mcp_config_paths_include_core_and_outer_workspace_roots() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let workspace = temp.path();
        let core = workspace.join("talos3d-core");
        fs::create_dir_all(core.join("docs")).expect("docs dir should be created");
        fs::create_dir_all(core.join("app")).expect("app dir should be created");
        fs::create_dir_all(core.join("crates/talos3d-core")).expect("crate dir should be created");
        fs::write(workspace.join("AGENTS.md"), "").expect("workspace marker should be written");
        fs::write(core.join("docs/MCP_MODEL_API.md"), "")
            .expect("MCP doc marker should be written");
        fs::write(core.join("app/Cargo.toml"), "").expect("app marker should be written");

        let paths = default_mcp_config_paths_from(&core.join("app"));

        assert_eq!(
            paths,
            vec![core.join(".mcp.json"), workspace.join(".mcp.json")]
        );
    }

    #[test]
    fn cleanup_removes_only_matching_runtime_discovery_records() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let registry_path = temp.path().join("instance.json");
        let matching_config = temp.path().join("matching.mcp.json");
        let newer_config = temp.path().join("newer.mcp.json");
        fs::write(&registry_path, "{}").expect("registry should be written");
        fs::write(
            &matching_config,
            r#"{"mcpServers":{"talos3d":{"url":"http://127.0.0.1:24842/mcp"},"other":{"command":"ok"}}}"#,
        )
        .expect("matching config should be written");
        fs::write(
            &newer_config,
            r#"{"mcpServers":{"talos3d":{"url":"http://127.0.0.1:24999/mcp"}}}"#,
        )
        .expect("newer config should be written");

        {
            let cleanup = ModelApiDiscoveryCleanup {
                registry_path: registry_path.clone(),
                mcp_config_paths: vec![matching_config.clone(), newer_config.clone()],
                http_url: "http://127.0.0.1:24842/mcp".into(),
            };
            drop(cleanup);
        }

        assert!(!registry_path.exists());
        let matching: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(matching_config).unwrap()).unwrap();
        assert!(matching["mcpServers"].get("talos3d").is_none());
        assert_eq!(matching["mcpServers"]["other"]["command"], "ok");
        let newer: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(newer_config).unwrap()).unwrap();
        assert_eq!(
            newer["mcpServers"]["talos3d"]["url"],
            "http://127.0.0.1:24999/mcp"
        );
    }
}
