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
    let transform = descriptors["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["request_kind"] == "core.transform")
        .unwrap();
    assert_eq!(transform["input_schema"]["type"], "object");
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

#[test]
fn semantic_anchor_request_refuses_wrong_host_and_persists_exact_history() {
    use crate::semantics::{test_fixtures::*, SemanticGraph};
    let (mut world, trim) = fixture();
    world.insert_resource(SemanticGraph(roof_edge_fixture()));
    world.init_resource::<crate::plugins::history::SemanticEnforcement>();
    let mut box_id = || {
        handle_create_entity(
            &mut world,
            json!({"type":"box","centre":[4.,0.,0.],"half_extents":[0.5,0.5,0.5]}),
        )
        .unwrap()
    };
    let roof = box_id();
    let wall = box_id();
    let geometry_before = get_entity_snapshot(&world, ElementId(trim)).unwrap();
    let prepare = |world: &mut World, intents: Value| {
        call(
            world,
            "preview",
            json!({
                "request_kind":"core.semantic", "parameters":{"intents":intents},
            }),
        )
        .unwrap()
    };
    let declarations = prepare(
        &mut world,
        json!([
            {"AssignConcept":{"entity":roof,"concept":ROOF_SYSTEM}},
            {"PublishAnchors":{"entity":roof,"anchors":[{"kind":RAKE_EDGE,"role":"north_west"}]}},
            {"AssignConcept":{"entity":wall,"concept":WALL_CLADDING}},
            {"AssignConcept":{"entity":trim,"concept":BARGEBOARD}}
        ]),
    );
    assert_eq!(declarations["can_commit"], true);
    assert!(get_entity_details(&world, ElementId(trim))
        .unwrap()
        .design_concept
        .is_none());
    let depth = world.resource::<History>().undo_stack_len();
    call(
        &mut world,
        "apply",
        json!({"plan_id":declarations["plan_id"]}),
    )
    .unwrap();
    assert_eq!(world.resource::<History>().undo_stack_len(), depth + 1);
    let declared = get_entity_details(&world, ElementId(trim)).unwrap();
    assert_eq!(
        declared.design_concept.as_ref().unwrap().concept.as_str(),
        BARGEBOARD
    );
    let wrong = prepare(
        &mut world,
        json!([
            {"Bind":{"subject":trim,"predicate":"follows","target":{"Entity":wall}}}
        ]),
    );
    assert_eq!(wrong["can_commit"], false);
    let refusal = &wrong["captured"]["semantic_refusals"][0];
    assert!(refusal["violated"].as_str().unwrap().contains("bargeboard"));
    assert!(refusal["observed"]
        .as_str()
        .unwrap()
        .contains(WALL_CLADDING));
    assert!(refusal["repair"]
        .as_str()
        .unwrap()
        .contains(&format!("\"publisher\":{roof}")));
    assert!(refusal["repair"].as_str().unwrap().contains("north_west"));
    assert!(call(&mut world, "apply", json!({"plan_id":wrong["plan_id"]})).is_err());
    assert_eq!(
        get_entity_details(&world, ElementId(trim)).unwrap(),
        declared
    );
    assert_eq!(world.resource::<History>().undo_stack_len(), depth + 1);
    let correct = prepare(
        &mut world,
        json!([
            {"Bind":{"subject":trim,"predicate":"follows","target":{"Anchor":{
                "publisher":roof,"kind":RAKE_EDGE,"role":"north_west"
            }}}}
        ]),
    );
    assert_eq!(correct["can_commit"], true);
    assert!(get_entity_details(&world, ElementId(trim))
        .unwrap()
        .anchor_bindings
        .is_empty());
    call(&mut world, "apply", json!({"plan_id":correct["plan_id"]})).unwrap();
    let bound = get_entity_details(&world, ElementId(trim)).unwrap();
    assert_eq!(bound.anchor_bindings.len(), 1);
    assert_eq!(bound.anchor_bindings[0].anchor_publisher, roof);
    // Reidentification must validate retained bindings and publications too.
    let reidentify = prepare(
        &mut world,
        json!([
            {"AssignConcept":{"entity":trim,"concept":FASCIA}}
        ]),
    );
    assert_eq!(reidentify["can_commit"], false);
    let bad_publisher = prepare(
        &mut world,
        json!([
            {"AssignConcept":{"entity":roof,"concept":WALL_CLADDING}}
        ]),
    );
    assert_eq!(bad_publisher["can_commit"], false);
    let invented_role = prepare(
        &mut world,
        json!([
            {"PublishAnchors":{"entity":roof,"anchors":[{"kind":RAKE_EDGE,"role":"invented"}]}}
        ]),
    );
    assert_eq!(invented_role["can_commit"], false);
    let wrong_predicate = call(
        &mut world,
        "preview",
        json!({
            "request_kind":"core.semantic", "parameters":{"intents":[
                {"Bind":{"subject":trim,"predicate":"follwos","target":{"Anchor":{
                    "publisher":roof,"kind":RAKE_EDGE,"role":"north_west"
                }}}}
            ]}
        }),
    );
    assert!(wrong_predicate
        .unwrap_err()
        .contains("No registered proposition"));
    assert_eq!(get_entity_details(&world, ElementId(trim)).unwrap(), bound);
    assert_eq!(
        get_entity_snapshot(&world, ElementId(trim)).unwrap(),
        geometry_before
    );
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(
        get_entity_details(&world, ElementId(trim)).unwrap(),
        declared
    );
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(get_entity_details(&world, ElementId(trim)).unwrap(), bound);
    let path = std::env::temp_dir().join(format!(
        "talos-semantic-plan-{}.talos3d",
        uuid::Uuid::new_v4()
    ));
    handle_save_project(&mut world, path.to_str().unwrap()).unwrap();
    let mut reloaded = init_model_api_test_world();
    register_model_api_edit_requests(&mut reloaded);
    reloaded.insert_resource(SemanticGraph(roof_edge_fixture()));
    handle_load_project(&mut reloaded, path.to_str().unwrap()).unwrap();
    assert_eq!(
        get_entity_details(&reloaded, ElementId(trim)).unwrap(),
        bound
    );
    assert_eq!(
        get_entity_details(&reloaded, ElementId(roof))
            .unwrap()
            .published_anchors,
        get_entity_details(&world, ElementId(roof))
            .unwrap()
            .published_anchors
    );
    // A withdrawn host anchor is not silently reported as a resolved design.
    let withdrawn = prepare(
        &mut reloaded,
        json!([
            {"PublishAnchors":{"entity":roof,"anchors":[]}}
        ]),
    );
    call(
        &mut reloaded,
        "apply",
        json!({"plan_id":withdrawn["plan_id"]}),
    )
    .unwrap();
    let explanation = crate::plugins::design_explanation::explain_design(&reloaded, trim).unwrap();
    assert!(serde_json::to_string(&explanation)
        .unwrap()
        .contains("Semantic contradiction"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn semantic_declaration_compatibility_tools_are_undoable_and_fence_old_plans() {
    use crate::semantics::{test_fixtures::*, PublishedAnchors, SemanticGraph};
    let (mut world, roof) = fixture();
    world.insert_resource(SemanticGraph(roof_edge_fixture()));
    world.init_resource::<crate::plugins::history::SemanticEnforcement>();
    let old = preview(&mut world, roof);
    let before = get_entity_details(&world, ElementId(roof)).unwrap();
    super::super::concept_tools::handle_assign_concept(
        &mut world,
        super::super::concept_tools::AssignConceptRequest {
            element_id: roof,
            concept_id: ROOF_SYSTEM.into(),
        },
    )
    .unwrap();
    assert!(call(&mut world, "apply", json!({"plan_id":old["plan_id"]})).is_err());
    let declared = get_entity_details(&world, ElementId(roof)).unwrap();
    super::super::concept_tools::handle_publish_anchors(
        &mut world,
        super::super::concept_tools::PublishAnchorsRequest {
            element_id: roof,
            anchors: vec![super::super::concept_tools::PublishAnchorSpec {
                anchor_kind: RAKE_EDGE.into(),
                role: "north_west".into(),
            }],
        },
    )
    .unwrap();
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(
        get_entity_details(&world, ElementId(roof)).unwrap(),
        declared
    );
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    flush_model_api_write_pipeline(&mut world);
    assert_eq!(get_entity_details(&world, ElementId(roof)).unwrap(), before);
    // Even an untracked legacy metadata writer cannot overwrite a captured base.
    let candidate = call(
        &mut world,
        "preview",
        json!({"request_kind":"core.semantic","parameters":{
            "intents":[{"AssignConcept":{"entity":roof,"concept":ROOF_SYSTEM}}]
        }}),
    )
    .unwrap();
    let entity =
        crate::plugins::commands::find_entity_by_element_id(&mut world, ElementId(roof)).unwrap();
    world.entity_mut(entity).insert(PublishedAnchors::default());
    assert!(
        call(&mut world, "apply", json!({"plan_id":candidate["plan_id"]}))
            .unwrap_err()
            .contains("semantic before-state")
    );
}
