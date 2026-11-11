use super::*;
use crate::capability_registry::{
    ElementClassAssignment, ElementClassId, GenerateOutput, RecipeFamilyDescriptor, RecipeFamilyId,
};
use crate::plugins::refinement::{
    AuthoringMode, AuthoringProvenance, ClaimGrounding, ObligationSet, RefinementState,
    RefinementStateComponent, SemanticIntent,
};
use std::{collections::HashMap, sync::Arc};

fn fixture(fail: bool) -> (World, u64) {
    let mut world = init_model_api_test_world();
    let root = handle_create_entity(
        &mut world,
        json!({"type":"box","centre":[0.,0.,0.],"half_extents":[1.,1.,1.]}),
    )
    .unwrap();
    let entity = find_entity_by_element_id_readonly(&world, ElementId(root)).unwrap();
    world.entity_mut(entity).insert((
        ElementClassAssignment {
            element_class: ElementClassId("fixture_part".into()),
            active_recipe: None,
        },
        SemanticIntent {
            parameters: json!({"manual_choice":"preserve"}),
            ..Default::default()
        },
        AuthoringProvenance {
            mode: AuthoringMode::Freeform,
            rationale: Some("manual fixture".into()),
        },
    ));
    world
        .resource_mut::<CapabilityRegistry>()
        .register_recipe_family(RecipeFamilyDescriptor {
            id: RecipeFamilyId("fixture_native".into()),
            target_class: ElementClassId("fixture_part".into()),
            label: "Fixture native recipe".into(),
            description: "Command-backed transaction regression".into(),
            parameters: vec![],
            supported_refinement_levels: vec![RefinementState::Schematic],
            obligation_specializations: HashMap::new(),
            promotion_critical_path_specializations: HashMap::new(),
            generate: Arc::new(move |input, world| {
                crate::plugins::commands::enqueue_create_box(
                    world,
                    CreateBoxCommand {
                        centre: Vec3::new(4., 0., 0.),
                        half_extents: Vec3::splat(0.5),
                    },
                );
                crate::plugins::commands::flush_queued_commands(world);
                let entity =
                    find_entity_by_element_id_readonly(world, ElementId(input.element_id)).unwrap();
                world.get_mut::<SemanticIntent>(entity).unwrap().parameters["resolved_content"] =
                    json!(true);
                if fail {
                    Err("injected late native recipe failure".into())
                } else {
                    Ok(GenerateOutput::default())
                }
            }),
        });
    (world, root)
}
fn metadata(world: &World, root: u64) -> Value {
    let e = find_entity_by_element_id_readonly(world, ElementId(root)).unwrap();
    json!({"state":world.get::<RefinementStateComponent>(e),"class":world.get::<ElementClassAssignment>(e),
        "intent":world.get::<SemanticIntent>(e),"provenance":world.get::<AuthoringProvenance>(e),
        "obligations":world.get::<ObligationSet>(e),"grounding":world.get::<ClaimGrounding>(e)})
}
#[test]
fn native_promotion_is_one_history_item_with_exact_metadata_and_member_undo_redo() {
    let (mut world, root) = fixture(false);
    let before_ids = collect_element_ids(&mut world);
    let before = metadata(&world, root);
    let depth = world.resource::<History>().undo_stack_len();
    let result = handle_promote_refinement(
        &mut world,
        root,
        "Schematic".into(),
        Some("fixture_native".into()),
        json!({}),
    )
    .unwrap();
    assert_eq!(result.created_element_ids.len(), 1);
    let after = metadata(&world, root);
    let after_ids = collect_element_ids(&mut world);
    assert_eq!(world.resource::<History>().undo_stack_len(), depth + 1);
    assert_ne!(after, before);
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    crate::plugins::commands::flush_queued_commands(&mut world);
    assert_eq!(collect_element_ids(&mut world), before_ids);
    assert_eq!(metadata(&world, root), before);
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    crate::plugins::commands::flush_queued_commands(&mut world);
    assert_eq!(collect_element_ids(&mut world), after_ids);
    assert_eq!(metadata(&world, root), after);
}
#[test]
fn late_native_failure_restores_members_metadata_history_and_allocator() {
    let (mut world, root) = fixture(true);
    let ids = collect_element_ids(&mut world);
    let before = metadata(&world, root);
    let depth = world.resource::<History>().undo_stack_len();
    let next = world.resource::<ElementIdAllocator>().next_value();
    let error = handle_promote_refinement(
        &mut world,
        root,
        "Schematic".into(),
        Some("fixture_native".into()),
        json!({}),
    )
    .unwrap_err();
    assert!(error.contains("injected late native"), "{error}");
    assert_eq!(collect_element_ids(&mut world), ids);
    assert_eq!(metadata(&world, root), before);
    assert_eq!(world.resource::<History>().undo_stack_len(), depth);
    assert_eq!(world.resource::<ElementIdAllocator>().next_value(), next);
}
#[test]
fn native_recipe_cannot_claim_an_unsupported_higher_state() {
    let (mut world, root) = fixture(false);
    let ids = collect_element_ids(&mut world);
    let before = metadata(&world, root);
    let error = handle_promote_refinement(
        &mut world,
        root,
        "Constructible".into(),
        Some("fixture_native".into()),
        json!({}),
    )
    .unwrap_err();
    assert!(error.contains("does not support Constructible"), "{error}");
    assert_eq!(collect_element_ids(&mut world), ids);
    assert_eq!(metadata(&world, root), before);
}
