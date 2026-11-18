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
