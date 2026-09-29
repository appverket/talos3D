use super::*;
use crate::plugins::authored_edit_plan::{AuthoredEditPlanRegistry, PlanId};
use crate::plugins::model_api::request::handle_edit_plan_request as call;

fn fixture() -> (World, u64) {
    let mut world = init_model_api_test_world();
    register_model_api_edit_requests(&mut world);
    let id = handle_create_entity(
        &mut world,
        json!({"type":"box","centre":[0.,0.,0.],"half_extents":[0.5,0.5,0.5]}),
    )
    .unwrap();
    (world, id)
}
fn preview(world: &mut World, id: u64) -> Value {
    call(
        world,
        "preview",
        json!({"request_kind":"core.transform", "parameters":{
        "element_ids":[id],"operation":"move","axis":"x","value":2.}}),
    )
    .unwrap()
}
#[test]
fn discovered_request_captures_without_mutation_and_applies_once_with_exact_history() {
    let (mut world, id) = fixture();
    let before = get_entity_snapshot(&world, ElementId(id)).unwrap();
    let revision = world.resource::<History>().revision_token();
    let descriptors = call(&mut world, "list", Value::Null).unwrap();
    assert_eq!(descriptors["requests"][0]["request_kind"], "core.transform");
    assert_eq!(descriptors["requests"][0]["input_schema"]["type"], "object");
    let candidate = preview(&mut world, id);
    assert_eq!(world.resource::<History>().revision_token(), revision);
    assert_eq!(get_entity_snapshot(&world, ElementId(id)).unwrap(), before);
    assert_eq!(candidate["can_commit"], true);
    assert_eq!(
        call(
            &mut world,
            "inspect",
            json!({"plan_id":candidate["plan_id"]})
        )
        .unwrap(),
        candidate
    );
    let depth = world.resource::<History>().undo_stack_len();
    let receipt = call(&mut world, "apply", json!({"plan_id":candidate["plan_id"]})).unwrap();
    assert_eq!(receipt["digest"], candidate["digest"]);
    assert_eq!(world.resource::<History>().undo_stack_len(), depth + 1);
    assert_eq!(
        get_entity_snapshot(&world, ElementId(id)).unwrap(),
        candidate["captured"]["after"][0]
    );
    assert!(call(&mut world, "apply", json!({"plan_id":candidate["plan_id"]})).is_err());
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(get_entity_snapshot(&world, ElementId(id)).unwrap(), before);
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(
        get_entity_snapshot(&world, ElementId(id)).unwrap(),
        candidate["captured"]["after"][0]
    );
}
#[test]
fn old_agent_proposal_refuses_after_a_manual_edit_and_even_after_undo() {
    let (mut world, id) = fixture();
    let candidate = preview(&mut world, id);
    handle_set_property(&mut world, id, "half_extents", json!([1.5, 0.5, 0.5])).unwrap();
    let changed = get_entity_snapshot(&world, ElementId(id)).unwrap();
    assert!(
        call(&mut world, "apply", json!({"plan_id":candidate["plan_id"]}))
            .unwrap_err()
            .contains("stale")
    );
    assert_eq!(get_entity_snapshot(&world, ElementId(id)).unwrap(), changed);
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert!(
        call(&mut world, "apply", json!({"plan_id":candidate["plan_id"]}))
            .unwrap_err()
            .contains("stale")
    );
}
#[test]
fn live_native_gesture_fences_rpc_in_same_app_dispatch() {
    let (mut world, id) = fixture();
    let before = get_entity_snapshot(&world, ElementId(id)).unwrap();
    let candidate = preview(&mut world, id);
    world.resource_mut::<TransformState>().mode = TransformMode::Moving;
    assert!(call(
        &mut world,
        "preview",
        json!({"request_kind":"core.transform","parameters":{}})
    )
    .unwrap_err()
    .contains("Interactive edit"));
    assert!(
        call(&mut world, "apply", json!({"plan_id":candidate["plan_id"]}))
            .unwrap_err()
            .contains("Interactive edit")
    );
    let (response, mut guard_receiver) = tokio::sync::oneshot::channel();
    let (target_response, mut target_receiver) = tokio::sync::oneshot::channel();
    request::handle_model_api_request(
        &mut world,
        ModelApiRequest::Guarded {
            request: Box::new(ModelApiRequest::EditPlan {
                action: "apply".into(),
                parameters: json!({"plan_id":candidate["plan_id"]}),
                response: target_response,
            }),
            response,
        },
    );
    assert!(guard_receiver
        .try_recv()
        .unwrap()
        .unwrap_err()
        .contains("Interactive edit"));
    assert!(target_receiver.try_recv().is_err());
    assert_eq!(get_entity_snapshot(&world, ElementId(id)).unwrap(), before);
    let id = PlanId(candidate["plan_id"].as_str().unwrap().into());
    assert!(world
        .resource::<AuthoredEditPlanRegistry>()
        .get(&id)
        .is_some());
}
