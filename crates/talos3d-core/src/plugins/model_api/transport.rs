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

                let router = axum::Router::new().nest_service("/mcp", service);
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
