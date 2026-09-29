//! Actual HTTP contract: two clients, one bearer, one endpoint, independent state.
use super::*;
use std::time::Duration;

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    session: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Option<String>) {
    let mut request = client
        .post(url)
        .bearer_auth("test-instance-token")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18");
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    let response = request
        .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "HTTP {}", response.status());
    let session = response
        .headers()
        .get("Mcp-Session-Id")
        .map(|v| v.to_str().unwrap().to_string());
    let body = response.text().await.unwrap();
    let value = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["id"] == 1)
        .unwrap_or_else(|| serde_json::from_str(&body).expect("JSON or SSE result"));
    (value, session)
}

async fn initialize(client: &reqwest::Client, url: &str) -> String {
    let (_, session) = rpc(
        client,
        url,
        None,
        "initialize",
        json!({
            "protocolVersion":"2025-06-18", "capabilities":{},
            "clientInfo":{"name":"profile-isolation-test", "version":"1"}
        }),
    )
    .await;
    let session = session.expect("stateful session id");
    let response = client
        .post(url)
        .bearer_auth("test-instance-token")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    session
}

fn tool_value(response: serde_json::Value) -> serde_json::Value {
    assert!(response.get("error").is_none(), "{response}");
    assert_ne!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn http_sessions_isolate_profiles_and_preserve_authentication() {
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut world = init_model_api_test_world();
        let mut app = App::new();
        register_model_api_primitive_commands(&mut app);
        world.insert_resource(
            app.world_mut()
                .remove_resource::<CommandRegistry>()
                .unwrap(),
        );
        world.spawn((
            ElementId(1),
            PlanePrimitive {
                corner_a: Vec2::ZERO,
                corner_b: Vec2::ONE,
                elevation: 0.0,
            },
            ShapeRotation::default(),
        ));
        while let Ok(request) = receiver.recv_timeout(Duration::from_secs(10)) {
            handle_model_api_request(&mut world, request);
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let router = super::super::runtime_transport::model_api_http_router(
        ModelApiRequestSender::new(sender),
        port,
        ModelApiAuthentication::from_test_credentials("pairing-code", "test-instance-token", true),
        CapabilityProfile::Authoring,
        cancellation.clone(),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let a = initialize(&client, &url).await;
    let b = initialize(&client, &url).await;
    assert_ne!(a, b);
    let (switched, _) = rpc(
        &client,
        &url,
        Some(&a),
        "tools/call",
        json!({"name":"set_session_profile", "arguments":{"profile":"inspection"}}),
    )
    .await;
    assert_eq!(tool_value(switched)["active_profile"], "inspection");
    for (session, can_edit) in [(&a, false), (&b, true)] {
        let (list, _) = rpc(&client, &url, Some(session), "tools/list", json!({})).await;
        let names: Vec<_> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.contains(&"create_box"), can_edit);
        for read in ["get_authoring_provenance", "get_claim_grounding"] {
            assert!(names.contains(&read));
            let (response, _) = rpc(
                &client,
                &url,
                Some(session),
                "tools/call",
                json!({"name":read, "arguments":{"element_id":1}}),
            )
            .await;
            tool_value(response);
        }
    }
    let (denied, _) = rpc(
        &client,
        &url,
        Some(&a),
        "tools/call",
        json!({"name":"create_box", "arguments":{"size":[1,1,1]}}),
    )
    .await;
    assert!(denied["error"]["message"]
        .as_str()
        .unwrap()
        .contains("inspection"));
    let (allowed, _) = rpc(
        &client,
        &url,
        Some(&b),
        "tools/call",
        json!({"name":"create_box", "arguments":{"size":[1,1,1]}}),
    )
    .await;
    tool_value(allowed);
    // Session identity never replaces authorization; origin restrictions still apply.
    let unauthenticated = client
        .post(&url)
        .header("Mcp-Session-Id", &b)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let hostile = client
        .post(&url)
        .bearer_auth("test-instance-token")
        .header("Origin", "https://untrusted.example")
        .header("Mcp-Session-Id", &b)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(hostile.status(), reqwest::StatusCode::FORBIDDEN);
    for session in [&a, &b] {
        assert!(client
            .delete(&url)
            .bearer_auth("test-instance-token")
            .header("Mcp-Session-Id", session)
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
    }
    let deleted = client
        .post(&url)
        .bearer_auth("test-instance-token")
        .header("Mcp-Session-Id", &a)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NOT_FOUND);
    let c = initialize(&client, &url).await;
    let (fresh, _) = rpc(
        &client,
        &url,
        Some(&c),
        "tools/call",
        json!({"name":"set_session_profile", "arguments":{}}),
    )
    .await;
    assert_eq!(tool_value(fresh)["active_profile"], "authoring");
    client
        .delete(&url)
        .bearer_auth("test-instance-token")
        .header("Mcp-Session-Id", &c)
        .send()
        .await
        .unwrap();
    cancellation.cancel();
    server.abort();
    let _ = server.await;
    tokio::task::spawn_blocking(move || worker.join().unwrap())
        .await
        .unwrap();
}
