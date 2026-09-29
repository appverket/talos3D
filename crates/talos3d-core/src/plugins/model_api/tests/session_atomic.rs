use super::session_safety::prepared_world;
use super::*;
use crate::curation::procedural_session::{
    CommitPolicyDe, EvalMode, EvalStep, ProceduralSessionRegistry, SessionError, SessionSpec,
};
use crate::curation::{ArgExpr, McpToolId, Postcondition, StepId};
use crate::plugins::procedural_session_mcp::{
    world_commit_with_executor, world_create, world_eval, SessionCommitRequest,
    SessionCreateRequest, SessionEvalRequest,
};

fn append(world: &mut World, request: &SessionCommitRequest, id: &str, tool: &str, args: Value) {
    world_eval(
        world,
        SessionEvalRequest {
            session_id: request.session_id.clone(),
            mode: EvalMode::DryRunAndBind,
            step: EvalStep {
                id: StepId::new(id),
                tool: McpToolId::new(tool),
                args: args
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), ArgExpr::Literal { value: v.clone() }))
                    .collect(),
                bindings: Default::default(),
                essential: true,
                precondition: None,
            },
        },
    )
    .unwrap();
}
fn new_session(world: &mut World, tools: &[&str]) -> SessionCommitRequest {
    let s = world_create(
        world,
        SessionCreateRequest {
            spec: SessionSpec::for_new_structure(
                tools.iter().map(|t| McpToolId::new(*t)).collect(),
            ),
        },
    );
    SessionCommitRequest {
        session_id: s.session_id,
        commit_id: Some(s.snapshot.commit_id),
        options: Default::default(),
    }
}
fn execute(
    world: &mut World,
    request: SessionCommitRequest,
) -> Result<crate::curation::procedural_session::CommitReport, SessionError> {
    world_commit_with_executor(world, request, Some(&mut ModelApiStepExecutor))
}

#[test]
fn procedural_session_compound_commit_has_one_undo_redo_unit() {
    let (mut world, request) = prepared_world();
    append(
        &mut world,
        &request,
        "second",
        "create_box",
        json!({"size":[2,2,2],"center":[4,0,0]}),
    );
    let before = model_summary(&world);
    execute(&mut world, request).unwrap();
    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
    let after = model_summary(&world);
    assert_ne!(before, after);
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(model_summary(&world), before);
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(model_summary(&world), after);
}

#[test]
fn procedural_session_late_failure_preserves_previous_redo_savepoint_and_allocator() {
    let (mut world, _) = prepared_world();
    handle_create_box(
        &mut world,
        CreateBoxRequest {
            center: None,
            half_extents: None,
            size: Some([1., 1., 1.]),
            rotation: None,
            semantic: None,
        },
    )
    .unwrap();
    let saved = model_summary(&world);
    world.resource_mut::<History>().mark_save_point();
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    let request = new_session(&mut world, &["create_box"]);
    append(
        &mut world,
        &request,
        "first",
        "create_box",
        json!({"size":[1,1,1]}),
    );
    append(
        &mut world,
        &request,
        "bad",
        "create_box",
        json!({"size":"not a vector"}),
    );
    let before = model_summary(&world);
    let next = world.resource::<ElementIdAllocator>().next_value();
    assert!(execute(&mut world, request).is_err());
    assert_eq!(model_summary(&world), before);
    assert_eq!(world.resource::<ElementIdAllocator>().next_value(), next);
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(model_summary(&world), saved);
    assert!(world.resource::<History>().at_save_point());
}

#[test]
fn procedural_session_false_postcondition_rolls_back() {
    let (mut world, mut request) = prepared_world();
    request
        .options
        .postconditions
        .push(Postcondition::Relation {
            relation_kind: "hosted_on".into(),
            from: ArgExpr::Literal { value: json!(999) },
            to: ArgExpr::Literal { value: json!(998) },
        });
    let before = model_summary(&world);
    assert!(matches!(
        execute(&mut world, request),
        Err(SessionError::InternalInvocation(_))
    ));
    assert_eq!(model_summary(&world), before);
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
}

#[test]
fn procedural_session_definition_import_is_rolled_back_after_late_failure() {
    use crate::plugins::modeling::definition::{DefinitionLibraryRegistry, DefinitionRegistry};
    let (mut world, _) = prepared_world();
    let mut source = init_model_api_test_world();
    let def = handle_create_definition(&mut source, make_rect_extrusion_request()).unwrap();
    let lib = handle_create_definition_library(&mut source, json!({"name":"test"})).unwrap();
    handle_add_definition_to_library(
        &mut source,
        json!({"library_id":lib.library_id,"definition_id":def.definition_id}),
    )
    .unwrap();
    world.insert_resource(
        source
            .remove_resource::<DefinitionLibraryRegistry>()
            .unwrap(),
    );
    let before_library = serde_json::to_value(handle_list_definition_libraries(&world)).unwrap();
    let request = new_session(&mut world, &["definition.instantiate", "create_box"]);
    append(
        &mut world,
        &request,
        "import",
        "definition.instantiate",
        json!({"library_id":lib.library_id,"definition_id":def.definition_id}),
    );
    append(
        &mut world,
        &request,
        "fail",
        "create_box",
        json!({"size":"invalid"}),
    );
    let before = model_summary(&world);
    assert!(execute(&mut world, request).is_err());
    assert!(world.resource::<DefinitionRegistry>().list().is_empty());
    assert_eq!(
        serde_json::to_value(handle_list_definition_libraries(&world)).unwrap(),
        before_library
    );
    assert_eq!(model_summary(&world), before);
}

#[test]
fn procedural_session_pending_user_commands_are_refused_without_consuming_them() {
    let (mut world, request) = prepared_world();
    let command = crate::plugins::commands::CreateBoxCommand {
        centre: Vec3::ZERO,
        half_extents: Vec3::ONE,
    };
    send_event(&mut world, command);
    let before = model_summary(&world);
    assert!(matches!(
        execute(&mut world, request),
        Err(SessionError::UnsupportedTransaction { .. })
    ));
    assert_eq!(model_summary(&world), before);
    flush_model_api_write_pipeline(&mut world);
    assert_ne!(model_summary(&world), before);
    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
}

fn register_failing_constraint(world: &mut World) {
    register_failing_constraint_as(
        world,
        crate::capability_registry::ConstraintRole::Validation,
    );
}

fn register_failing_constraint_as(
    world: &mut World,
    role: crate::capability_registry::ConstraintRole,
) {
    use crate::capability_registry::{
        Applicability, ConstraintDescriptor, ConstraintId, Finding, FindingId, Severity,
    };
    world
        .resource_mut::<CapabilityRegistry>()
        .register_constraint(ConstraintDescriptor {
            id: ConstraintId("test.atomic".into()),
            label: "test".into(),
            description: "test".into(),
            applicability: Applicability::any(),
            default_severity: Severity::Error,
            rationale: "regression".into(),
            source_backlink: None,
            role,
            validator: std::sync::Arc::new(move |entity, world| {
                vec![Finding {
                    id: FindingId("live-failure".into()),
                    constraint_id: ConstraintId("test.atomic".into()),
                    subject: world.get::<ElementId>(entity).unwrap().0,
                    severity: Severity::Error,
                    message: "real validation failure".into(),
                    rationale: "test".into(),
                    backlink: None,
                    emitted_at: 0,
                    role,
                }]
            }),
        });
}

#[test]
fn procedural_session_real_validation_controls_acceptance_and_reports_live_findings() {
    let (mut world, request) = prepared_world();
    register_failing_constraint(&mut world);
    let before = model_summary(&world);
    assert!(matches!(
        execute(&mut world, request.clone()),
        Err(SessionError::CommitNotClean { .. })
    ));
    assert_eq!(model_summary(&world), before);
    let session = world.resource::<ProceduralSessionRegistry>();
    assert_eq!(
        session.get(&request.session_id).unwrap().findings[0].id,
        "live-failure"
    );
    let mut request = new_session(&mut world, &["create_box"]);
    append(
        &mut world,
        &request,
        "box",
        "create_box",
        json!({"size":[1,1,1]}),
    );
    request.options.policy = CommitPolicyDe::AcceptPartial;
    let receipt = execute(&mut world, request).unwrap();
    assert_eq!(receipt.post_commit_findings[0].id, "live-failure");
    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
}

#[test]
fn procedural_session_unsupported_step_is_rejected_before_first_effect() {
    let (mut world, _) = prepared_world();
    let request = new_session(&mut world, &["create_box", "parametric.create"]);
    append(
        &mut world,
        &request,
        "box",
        "create_box",
        json!({"size":[1,1,1]}),
    );
    append(
        &mut world,
        &request,
        "parametric",
        "parametric.create",
        json!({}),
    );
    let before = world.resource::<History>().revision_token();
    assert!(matches!(
        execute(&mut world, request),
        Err(SessionError::UnsupportedTransaction { .. })
    ));
    assert_eq!(world.resource::<History>().revision_token(), before);
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
}

#[test]
fn procedural_session_read_only_does_not_dirty_document_or_consume_history_actions() {
    let (mut world, _) = prepared_world();
    world.insert_resource(crate::plugins::document_state::DocumentState::default());
    let request = new_session(&mut world, &["model_summary"]);
    append(&mut world, &request, "query", "model_summary", json!({}));
    execute(&mut world, request).unwrap();
    assert!(
        !world
            .resource::<crate::plugins::document_state::DocumentState>()
            .dirty
    );
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
    let request = new_session(&mut world, &["model_summary"]);
    append(&mut world, &request, "query", "model_summary", json!({}));
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    assert!(matches!(
        execute(&mut world, request),
        Err(SessionError::UnsupportedTransaction { .. })
    ));
    assert!(!world.resource::<PendingCommandQueue>().is_empty());
}

#[test]
fn procedural_session_waivers_need_current_ids_and_nonempty_rationale() {
    use crate::curation::procedural_session::Waiver;
    for (id, reason, accepted) in [
        ("live-failure", "", false),
        ("unknown", "reason", false),
        ("live-failure", "reviewed test finding", true),
    ] {
        let (mut world, mut request) = prepared_world();
        register_failing_constraint(&mut world);
        request.options.policy = CommitPolicyDe::AcceptWithWaivers;
        request.options.waivers = vec![Waiver {
            finding_id: id.into(),
            justification: reason.into(),
        }];
        let before = model_summary(&world);
        let result = execute(&mut world, request);
        assert_eq!(result.is_ok(), accepted);
        if !accepted {
            assert_eq!(model_summary(&world), before);
        }
    }
}

#[test]
fn procedural_session_acceptance_does_not_hide_findings_behind_ui_budget() {
    use crate::capability_registry::ConstraintRole;
    use crate::plugins::validation::DiscoveryFindingsBudget;
    let (mut world, request) = prepared_world();
    register_failing_constraint_as(&mut world, ConstraintRole::Discovery);
    world.insert_resource(DiscoveryFindingsBudget::with_capacity(0));
    let before = model_summary(&world);
    assert!(matches!(
        execute(&mut world, request),
        Err(SessionError::CommitNotClean { .. })
    ));
    assert_eq!(model_summary(&world), before);
    assert_eq!(world.resource::<DiscoveryFindingsBudget>().max_per_sweep, 0);
}
