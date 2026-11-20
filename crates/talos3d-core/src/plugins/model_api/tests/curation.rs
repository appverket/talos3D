//! Exercise the agent boundary, including a real signed loopback HTTP feed and
//! unavailable-cloud fallback; no user account, project or live app is touched.
use super::*;
use crate::plugins::{agent_skill_cloud, curation::*};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn signed(key: &SigningKey, mut value: Value) -> Value {
    value["public_key"] = json!(hex::encode(key.verifying_key().to_bytes()));
    let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&value).unwrap()).into();
    value["signature"] = json!(hex::encode(key.sign(&digest).to_bytes()));
    value
}

#[tokio::test]
async fn curation_reconciliation_is_signed_bounded_and_survives_cloud_loss() {
    let signing = SigningKey::from_bytes(&[29; 32]);
    let public = hex::encode(signing.verifying_key().to_bytes());
    let releases = signed(
        &signing,
        json!({"schema":RELEASE_SCHEMA,"sequence":4,"releases":[{
            "artifact_id":"skill:test.withdrawn","revision":"r1","remedy_keys":[{"gap_kind":"missing_path"}],
            "required_tool_ids":["not_in_this_runtime"],"withdrawn":true,"refresh_required":true
        }]}),
    );
    let skills = signed(
        &signing,
        json!({"schema":agent_skill_cloud::SCHEMA,"epoch":4,"skills":[],"withdrawn_skill_ids":["test.withdrawn"]}),
    );
    let app = axum::Router::new()
        .route(
            "/v1/curation/releases",
            axum::routing::get(move || {
                let releases = releases.clone();
                async move { axum::Json(releases) }
            }),
        )
        .route(
            "/v1/corpus/skills",
            axum::routing::get(move || {
                let skills = skills.clone();
                async move { axum::Json(skills) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (sender, receiver) = mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let mut world = init_model_api_test_world();
        world.insert_resource(CurationConnection {
            base_url: format!("http://{address}/v1/curation"),
            trusted_public_keys: vec![public.clone()],
        });
        world.insert_resource(crate::plugins::agent_skills::AgentCorpusConnection {
            base_url: format!("http://{address}/v1/corpus"),
            bearer_token: None,
            trusted_public_keys: vec![public],
        });
        while let Ok(request) = receiver.recv() {
            handle_model_api_request(&mut world, request);
        }
    });
    let server = ModelApiServer::new(ModelApiRequestSender::new(sender));
    let first: Value = server
        .get_agent_curation_status_tool(Parameters(CurationStatusRequest::default()))
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert_eq!(first["release_sequence"], 4);
    assert_eq!(first["freshness"], "verified");
    assert_eq!(first["releases"][0]["compatible"], false);
    assert_eq!(first["notifications_available"], false);
    let corpus: Value = server
        .get_agent_corpus_status_tool()
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert_eq!(corpus["available"], true);
    service.abort();
    let _ = service.await;
    let offline: Value = server
        .get_agent_curation_status_tool(Parameters(CurationStatusRequest::default()))
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert_eq!(offline["freshness"], "unverified");
    assert_eq!(offline["release_sequence"], 4);
    assert_eq!(offline["known_withdrawal_count"], 1);
    let error = server
        .get_agent_skill_tool(Parameters(GetAgentSkillRequest {
            skill_id: "test.withdrawn".into(),
        }))
        .await
        .unwrap_err();
    assert!(error.message.contains("Known withdrawn"), "{error:?}");
    // A new MCP session on the same host sees the retained withdrawal too.
    let next = ModelApiServer::new(server.sender.clone());
    assert!(next
        .get_agent_skill_tool(Parameters(GetAgentSkillRequest {
            skill_id: "test.withdrawn".into()
        }))
        .await
        .unwrap_err()
        .message
        .contains("Known withdrawn"));
    drop(next);
    drop(server);
    worker.await.unwrap();
}

#[tokio::test]
async fn curation_is_discoverable_without_a_configured_service() {
    let (sender, receiver) = mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let mut world = init_model_api_test_world();
        while let Ok(request) = receiver.recv() {
            handle_model_api_request(&mut world, request);
        }
    });
    let server = ModelApiServer::new(ModelApiRequestSender::new(sender));
    for profile in CapabilityProfile::ALL {
        assert!(profile_allows(profile, "get_agent_curation_status"));
    }
    let status: Value = server
        .get_agent_curation_status_tool(Parameters(CurationStatusRequest::default()))
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert_eq!(status["configured"], false);
    assert_eq!(status["freshness"], "unverified");
    assert_eq!(status["release_sequence"], Value::Null);
    assert_eq!(status["shared_export_enabled"], false);
    use crate::plugins::curation::reporting::{ObservationKind, ReportHarnessIncident};
    let recorded: Value = server
        .report_harness_incident_tool(Parameters(ReportHarnessIncident {
            knowledge_request: None,
            event_id: None,
            kind: ObservationKind::CorpusGap,
            applicability: RemedyKey {
                gap_kind: Some("missing_path".into()),
                ..Default::default()
            },
            failure_code: "no_curated_path".into(),
            app_revision: "test-build".into(),
        }))
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert_eq!(recorded["recorded_locally"], true);
    assert_eq!(recorded["submitted"], false);
    let submitted: Value = server
        .submit_pending_harness_incidents_tool()
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert_eq!(submitted["status"], "export_disabled");
    for profile in CapabilityProfile::ALL {
        assert!(profile_allows(profile, "report_harness_incident"));
        assert_eq!(
            profile_allows(profile, "submit_pending_harness_incidents"),
            matches!(
                profile,
                CapabilityProfile::Curation | CapabilityProfile::Full
            )
        );
    }
    drop(server);
    worker.await.unwrap();
}

// A real capability unload between the server's welcome and card retrieval.
// No MCP response or classifier is stubbed; the World handler resolves cards.
fn detection_fixture(
    pending_path: std::path::PathBuf,
) -> (
    ModelApiServer,
    std::sync::Arc<std::sync::atomic::AtomicU8>,
    tokio::task::JoinHandle<()>,
) {
    use crate::plugins::authoring_guidance::{AuthoringGuidance, GuidanceCardRegistry};
    use crate::plugins::curation::reporting::HarnessIncidentPolicy;
    use std::sync::{atomic::Ordering, Arc};
    let (sender, receiver) = mpsc::channel();
    let server = ModelApiServer::new(ModelApiRequestSender::new(sender));
    let guidance_state = server.guidance_state.clone();
    let action = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let next_action = action.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let mut world = init_model_api_test_world();
        world.insert_resource(ModelApiRuntimeInfo {
            instance_id: "incident-test".into(),
            app_name: "test".into(),
            pid: 1,
            http_host: "127.0.0.1".into(),
            http_port: 1,
            http_url: "http://127.0.0.1:1/mcp".into(),
            registry_path: String::new(),
            started_at_unix_ms: 1,
            requested_port: None,
        });
        world.insert_resource(AuthoringGuidance {
            guidance_id: "private.project.guidance".into(),
            version: 1,
            prompt_text: "PRIVATE_GUIDANCE_CONTENT".into(),
            ..Default::default()
        });
        let mut registry = GuidanceCardRegistry::default();
        registry.insert(GuidanceCardInfo {
            id: "private.project.required".into(),
            body_markdown: Some("PRIVATE_CARD_CONTENT".into()),
            ..Default::default()
        });
        world.insert_resource(registry);
        world.insert_resource(HarnessIncidentPolicy {
            pending_path: Some(pending_path),
            ..Default::default()
        });
        while let Ok(request) = receiver.recv() {
            if matches!(request, ModelApiRequest::GetGuidanceCard { .. }) {
                match next_action.swap(0, Ordering::SeqCst) {
                    1 => {
                        world.remove_resource::<GuidanceCardRegistry>();
                    }
                    2 => {
                        drop(request);
                        continue;
                    } // failed transport, not absence
                    3 => {
                        world.remove_resource::<GuidanceCardRegistry>();
                        guidance_state.start("same-anchor".into(), None, vec![], vec![], vec![]);
                    }
                    _ => {}
                }
            }
            handle_model_api_request(&mut world, request);
        }
    });
    (server, action, worker)
}

async fn negotiate_detection(server: &ModelApiServer) {
    let welcome: AgentWelcome = server
        .negotiate_agent_session_tool(Parameters(AgentHello {
            task: Some("PRIVATE_USER_TASK".into()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert!(welcome
        .required_guidance_card_ids
        .contains(&"private.project.required".into()));
}

async fn fetch_detection_card(
    server: &ModelApiServer,
    id: &str,
) -> Result<CallToolResult, McpError> {
    server
        .get_guidance_card_tool(Parameters(GetGuidanceCardRequest { card_id: id.into() }))
        .await
}

#[tokio::test]
async fn negotiated_guidance_loss_is_private_deduplicated_and_never_exports() {
    use std::sync::atomic::Ordering;
    let dir = std::env::temp_dir().join(format!("talos-incident-{}", uuid::Uuid::new_v4()));
    let path = dir.join("pending.json");
    let (server, action, worker) = detection_fixture(path.clone());
    negotiate_detection(&server).await;
    fetch_detection_card(&server, "private.project.required")
        .await
        .unwrap();
    assert!(!path.exists());
    action.store(1, Ordering::SeqCst);
    let first = fetch_detection_card(&server, "private.project.required")
        .await
        .unwrap_err();
    assert!(first.message.contains("guidance card not found"));
    assert!(first.message.contains("recorded locally (event_id:"));
    let bytes = std::fs::read(&path).unwrap();
    let stored: Value = serde_json::from_slice(&bytes).unwrap();
    let records = stored["records"].as_object().unwrap();
    assert_eq!(records.len(), 1);
    let (event_id, record) = records.iter().next().unwrap();
    assert!(first.message.contains(event_id));
    assert_eq!(record["observation"]["export_authorized"], false);
    assert_eq!(
        record["observation"]["failure_code"],
        "negotiated_required_guidance_missing"
    );
    assert_eq!(record["submitted"], false);
    for private in [
        "private.project",
        "PRIVATE_USER_TASK",
        "PRIVATE_GUIDANCE_CONTENT",
        "PRIVATE_CARD_CONTENT",
    ] {
        assert!(!String::from_utf8_lossy(&bytes).contains(private));
    }
    // Both entry points share the detector; retries retain the same event.
    assert!(fetch_detection_card(&server, "private.project.required")
        .await
        .unwrap_err()
        .message
        .contains(event_id));
    assert!(server
        .get_agent_guidance_packet_tool(Parameters(GuidanceDeliveryRequest::default()))
        .await
        .unwrap_err()
        .message
        .contains(event_id));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let submitted: Value = server
        .submit_pending_harness_incidents_tool()
        .await
        .unwrap()
        .into_typed()
        .unwrap();
    assert_eq!(submitted["status"], "export_disabled");
    // Ordinary renegotiation replaces the old required-card contract.
    server
        .negotiate_agent_session_tool(Parameters(AgentHello::default()))
        .await
        .unwrap();
    assert!(!fetch_detection_card(&server, "private.project.required")
        .await
        .unwrap_err()
        .message
        .contains("incident"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    drop(server);
    worker.await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn automatic_incident_ignores_unadvertised_ids_transport_loss_and_renegotiation() {
    use std::sync::atomic::Ordering;
    let dir = std::env::temp_dir().join(format!("talos-incident-{}", uuid::Uuid::new_v4()));
    let path = dir.join("pending.json");
    let (server, action, worker) = detection_fixture(path.clone());
    // No negotiated welcome, and arbitrary unknown id even after negotiation.
    assert!(!fetch_detection_card(&server, "unknown")
        .await
        .unwrap_err()
        .message
        .contains("incident"));
    negotiate_detection(&server).await;
    assert!(!fetch_detection_card(&server, "unknown")
        .await
        .unwrap_err()
        .message
        .contains("incident"));
    action.store(2, Ordering::SeqCst);
    assert!(!fetch_detection_card(&server, "private.project.required")
        .await
        .unwrap_err()
        .message
        .contains("incident"));
    // A replacement welcome while resolution is in flight invalidates proof.
    action.store(3, Ordering::SeqCst);
    assert!(!fetch_detection_card(&server, "private.project.required")
        .await
        .unwrap_err()
        .message
        .contains("incident"));
    assert!(!path.exists());
    drop(server);
    worker.await.unwrap();
}

#[tokio::test]
async fn automatic_incident_storage_failure_preserves_the_guidance_error() {
    use std::sync::atomic::Ordering;
    let dir = std::env::temp_dir().join(format!("talos-incident-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let blocker = dir.join("not-a-directory");
    std::fs::write(&blocker, "private sentinel").unwrap();
    let (server, action, worker) = detection_fixture(blocker.join("pending.json"));
    negotiate_detection(&server).await;
    action.store(1, Ordering::SeqCst);
    let error = fetch_detection_card(&server, "private.project.required")
        .await
        .unwrap_err();
    assert!(error.message.contains("guidance card not found"));
    assert!(error.message.contains("capture unavailable"));
    assert!(!error.message.contains(dir.to_str().unwrap()));
    assert!(!error.message.contains("recorded locally"));
    assert_eq!(
        std::fs::read_to_string(blocker).unwrap(),
        "private sentinel"
    );
    // Storage failure never terminates the app request worker.
    fetch_detection_card(&server, "dkg.snapshot.start")
        .await
        .unwrap();
    drop(server);
    worker.await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
