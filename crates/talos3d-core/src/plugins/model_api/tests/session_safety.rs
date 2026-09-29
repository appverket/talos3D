use super::*;
use crate::curation::procedural_session::{
    CommitOptions, EvalMode, EvalStep, ProceduralSessionConfig, ProceduralSessionRegistry,
    SessionError, SessionSpec, SessionToolRegistry,
};
use crate::curation::{ArgExpr, McpToolId, StepId};
use crate::plugins::procedural_session_mcp::{
    world_commit_with_executor, world_create, world_eval, SessionCommitRequest,
    SessionCreateRequest, SessionEvalRequest,
};

fn prepared_world() -> (World, SessionCommitRequest) {
    let mut world = init_model_api_test_world();
    let mut app = App::new();
    register_model_api_primitive_commands(&mut app);
    world.insert_resource(
        app.world_mut()
            .remove_resource::<CommandRegistry>()
            .unwrap(),
    );
    world.insert_resource(ProceduralSessionRegistry::default());
    world.insert_resource(ProceduralSessionConfig::default());
    let mut tools = SessionToolRegistry::default();
    register_model_api_session_tools(&mut tools);
    world.insert_resource(tools);
    let session = world_create(
        &mut world,
        SessionCreateRequest {
            spec: SessionSpec::for_new_structure(
                [McpToolId::new("create_box")].into_iter().collect(),
            ),
        },
    );
    assert!(session.snapshot.base_model_revision.is_some());
    world_eval(
        &mut world,
        SessionEvalRequest {
            session_id: session.session_id.clone(),
            mode: EvalMode::DryRunAndBind,
            step: EvalStep {
                id: StepId::new("box"),
                tool: McpToolId::new("create_box"),
                args: [(
                    "size".into(),
                    ArgExpr::Literal {
                        value: json!([1, 1, 1]),
                    },
                )]
                .into_iter()
                .collect(),
                bindings: Default::default(),
                essential: true,
                precondition: None,
            },
        },
    )
    .unwrap();
    (
        world,
        SessionCommitRequest {
            session_id: session.session_id,
            commit_id: Some(session.snapshot.commit_id),
            options: CommitOptions::default(),
        },
    )
}

#[test]
fn procedural_session_retry_returns_receipt_without_reapplying_after_undo() {
    let (mut world, request) = prepared_world();
    let first =
        world_commit_with_executor(&mut world, request.clone(), Some(&mut ModelApiStepExecutor))
            .unwrap();
    let entity_count = list_entities(&world).len();
    let revision = world.resource::<History>().revision_token();
    let repeated =
        world_commit_with_executor(&mut world, request.clone(), Some(&mut ModelApiStepExecutor))
            .unwrap();
    assert_eq!(
        first.commit_id,
        request.commit_id.as_ref().unwrap().as_str()
    );
    assert_eq!(first, repeated);
    assert_eq!(list_entities(&world).len(), entity_count);
    assert_eq!(world.resource::<History>().revision_token(), revision);
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    let after_undo = list_entities(&world).len();
    let repeated =
        world_commit_with_executor(&mut world, request.clone(), Some(&mut ModelApiStepExecutor))
            .unwrap();
    assert_eq!(first, repeated);
    assert_eq!(list_entities(&world).len(), after_undo);
    let mut wrong_id = request.clone();
    wrong_id.commit_id = Some("new-id-is-not-a-rerun".into());
    assert!(matches!(
        world_commit_with_executor(&mut world, wrong_id, Some(&mut ModelApiStepExecutor)),
        Err(SessionError::CommitIdentityMismatch { .. })
    ));
    let mut changed_options = request;
    changed_options.options.policy =
        crate::curation::procedural_session::CommitPolicyDe::AcceptPartial;
    assert!(matches!(
        world_commit_with_executor(&mut world, changed_options, Some(&mut ModelApiStepExecutor)),
        Err(SessionError::AlreadyCommitted(_))
    ));
}

#[test]
fn procedural_session_intervening_edit_undo_redo_and_replacement_are_stale() {
    for change in ["edit", "undo", "redo", "replace"] {
        let (mut world, request) = prepared_world();
        if change == "replace" {
            world.resource_mut::<History>().clear();
        } else {
            handle_create_box(
                &mut world,
                CreateBoxRequest {
                    center: Some([4., 0., 0.]),
                    half_extents: None,
                    size: Some([1., 1., 1.]),
                    rotation: None,
                    semantic: None,
                },
            )
            .unwrap();
            if change == "undo" || change == "redo" {
                world.resource_mut::<PendingCommandQueue>().queue_undo();
                flush_model_api_write_pipeline(&mut world);
            }
            if change == "redo" {
                world.resource_mut::<PendingCommandQueue>().queue_redo();
                flush_model_api_write_pipeline(&mut world);
            }
        }
        let before = model_summary(&world);
        let revision = world.resource::<History>().revision_token();
        assert!(
            matches!(
                world_commit_with_executor(&mut world, request, Some(&mut ModelApiStepExecutor)),
                Err(SessionError::StaleModel { .. })
            ),
            "{change}"
        );
        assert_eq!(
            model_summary(&world),
            before,
            "{change} must refuse without mutation"
        );
        assert_eq!(world.resource::<History>().revision_token(), revision);
    }
}

#[test]
fn procedural_session_document_identity_prevents_equal_revision_aliasing() {
    let (mut world, request) = prepared_world();
    let previous = world.resource::<History>().revision_token();
    // A different loaded world may have the same local revision counter.
    world.insert_resource(History::default());
    let current = world.resource::<History>().revision_token();
    assert_eq!(previous.revision, current.revision);
    assert_ne!(previous.document_id, current.document_id);
    assert!(matches!(
        world_commit_with_executor(&mut world, request, Some(&mut ModelApiStepExecutor)),
        Err(SessionError::StaleModel { .. })
    ));
}

#[test]
fn procedural_session_committed_script_is_frozen_and_rerun_needs_new_session() {
    let (mut world, request) = prepared_world();
    world_commit_with_executor(&mut world, request.clone(), Some(&mut ModelApiStepExecutor))
        .unwrap();
    let mut step = {
        let registry = world.resource::<ProceduralSessionRegistry>();
        let session = registry.get(&request.session_id).unwrap();
        assert!(session.committed.is_some());
        EvalStep {
            id: StepId::new("another"),
            tool: McpToolId::new("create_box"),
            args: Default::default(),
            bindings: Default::default(),
            essential: true,
            precondition: None,
        }
    };
    step.args.insert(
        "size".into(),
        ArgExpr::Literal {
            value: json!([1, 1, 1]),
        },
    );
    assert!(matches!(
        world_eval(
            &mut world,
            SessionEvalRequest {
                session_id: request.session_id.clone(),
                step: step.clone(),
                mode: EvalMode::DryRunAndBind,
            }
        ),
        Err(SessionError::AlreadyCommitted(_))
    ));
    let next = world_create(
        &mut world,
        SessionCreateRequest {
            spec: SessionSpec::for_new_structure(
                [McpToolId::new("create_box")].into_iter().collect(),
            ),
        },
    );
    assert_ne!(next.snapshot.commit_id, request.commit_id.unwrap());
    world_eval(
        &mut world,
        SessionEvalRequest {
            session_id: next.session_id.clone(),
            step,
            mode: EvalMode::DryRunAndBind,
        },
    )
    .unwrap();
    let count = list_entities(&world).len();
    world_commit_with_executor(
        &mut world,
        SessionCommitRequest {
            session_id: next.session_id,
            commit_id: Some(next.snapshot.commit_id),
            options: Default::default(),
        },
        Some(&mut ModelApiStepExecutor),
    )
    .unwrap();
    assert_eq!(list_entities(&world).len(), count + 1);
}
