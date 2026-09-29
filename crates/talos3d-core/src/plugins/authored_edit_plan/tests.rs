use super::*;
use crate::plugins::{
    history::{apply_pending_history_commands_for_test as drain, SemanticEnforcement},
    modeling::{
        generic_factory::PrimitiveFactory,
        generic_snapshot::PrimitiveSnapshot,
        primitives::{BoxPrimitive, ShapeRotation},
    },
};

fn world() -> World {
    let mut world = World::new();
    world.init_resource::<History>();
    world.init_resource::<ElementIdAllocator>();
    world.init_resource::<PendingCommandQueue>();
    world.init_resource::<SemanticEnforcement>();
    world.init_resource::<AuthoredEditPlanRegistry>();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);
    world
}
fn snapshot(id: u64, x: f32) -> BoxedEntity {
    PrimitiveSnapshot {
        element_id: ElementId(id),
        primitive: BoxPrimitive {
            centre: Vec3::new(x, 0.5, 0.),
            half_extents: Vec3::splat(0.5),
        },
        rotation: ShapeRotation::default(),
        material_assignment: None,
        opening_context: None,
        subobject_display_overrides: None,
    }
    .into()
}
fn candidate(
    world: &World,
    interaction: Option<InteractionId>,
    before: Vec<BoxedEntity>,
    after: Vec<BoxedEntity>,
) -> AuthoredEditPlan {
    AuthoredEditPlan::capture(
        world,
        interaction,
        world.resource::<History>().revision_token(),
        PlanContext {
            planner_id: "test".into(),
            intent: "move".into(),
            ..Default::default()
        },
        before,
        after,
        SemanticPlan::none(),
    )
    .unwrap()
}
fn publish(world: &mut World, plan: AuthoredEditPlan) -> Arc<AuthoredEditPlan> {
    world
        .resource_mut::<AuthoredEditPlanRegistry>()
        .publish(plan)
        .unwrap()
}

#[test]
fn heterogeneous_snapshot_changes_apply_once_and_undo_redo_exactly() {
    let mut world = world();
    let before = vec![snapshot(1, 0.), snapshot(2, 1.)];
    let after = vec![snapshot(1, 5.), snapshot(3, 8.)];
    for s in &before {
        s.apply_to(&mut world);
    }
    snapshot(9, 40.).apply_to(&mut world);
    let plan = candidate(&world, None, before.clone(), after.clone());
    assert_eq!(plan.created_ids(), &[ElementId(3)]);
    assert_eq!(plan.removed_ids(), &[ElementId(2)]);
    let plan = publish(&mut world, plan);
    queue_captured_plan(&mut world, plan.plan_id()).unwrap();
    assert_eq!(
        queue_captured_plan(&mut world, plan.plan_id()),
        Err(PlanError::PendingWork)
    );
    drain(&mut world);
    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
    assert!(world.resource::<ElementIdAllocator>().next_value() > 3);
    for s in &after {
        assert_eq!(capture_snapshot(&world, s.element_id()).as_ref(), Some(s));
    }
    assert!(!entity_exists(&world, ElementId(2)));
    assert_eq!(
        queue_captured_plan(&mut world, plan.plan_id()),
        Err(PlanError::UnknownPlan)
    );
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    for s in &before {
        assert_eq!(capture_snapshot(&world, s.element_id()).as_ref(), Some(s));
    }
    assert!(!entity_exists(&world, ElementId(3)));
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    drain(&mut world);
    for s in &after {
        assert_eq!(capture_snapshot(&world, s.element_id()).as_ref(), Some(s));
    }
    assert!(!entity_exists(&world, ElementId(2)));
    assert_eq!(
        capture_snapshot(&world, ElementId(9)),
        Some(snapshot(9, 40.))
    );
}

#[test]
fn interaction_preserves_originals_supersedes_candidates_and_cancels() {
    let mut world = world();
    snapshot(1, 0.).apply_to(&mut world);
    let base = world.resource::<History>().revision_token();
    let id = world
        .resource_mut::<AuthoredEditPlanRegistry>()
        .begin_interaction(base)
        .unwrap();
    let first = candidate(
        &world,
        Some(id.clone()),
        vec![snapshot(1, 0.)],
        vec![snapshot(1, 1.)],
    );
    let first = publish(&mut world, first);
    let second = candidate(
        &world,
        Some(id.clone()),
        vec![snapshot(1, 0.)],
        vec![snapshot(1, 2.)],
    );
    let second = publish(&mut world, second);
    assert_ne!(first.plan_id(), second.plan_id());
    assert_eq!(
        queue_captured_plan(&mut world, first.plan_id()),
        Err(PlanError::UnknownPlan)
    );
    assert_eq!(
        world
            .resource::<AuthoredEditPlanRegistry>()
            .active(&id)
            .unwrap()
            .plan_id(),
        second.plan_id()
    );
    let bad = candidate(
        &world,
        Some(id.clone()),
        vec![snapshot(1, 1.)],
        vec![snapshot(1, 3.)],
    );
    assert!(matches!(
        world
            .resource_mut::<AuthoredEditPlanRegistry>()
            .publish(bad),
        Err(PlanError::InvalidSnapshots(_))
    ));
    assert_eq!(
        capture_snapshot(&world, ElementId(1)),
        Some(snapshot(1, 0.))
    );
    world.resource_mut::<AuthoredEditPlanRegistry>().cancel(&id);
    assert_eq!(
        queue_captured_plan(&mut world, second.plan_id()),
        Err(PlanError::UnknownPlan)
    );
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
}

#[test]
fn revision_and_before_state_are_checked_again_at_history_drain() {
    let mut world = world();
    snapshot(1, 0.).apply_to(&mut world);
    let plan = candidate(&world, None, vec![snapshot(1, 0.)], vec![snapshot(1, 1.)]);
    let plan = publish(&mut world, plan);
    queue_captured_plan(&mut world, plan.plan_id()).unwrap();
    // A legacy direct writer changes the model between queue and drain.
    snapshot(1, 8.).apply_to(&mut world);
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)),
        Some(snapshot(1, 8.))
    );
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
    assert!(world
        .resource::<SemanticEnforcement>()
        .last_refusal()
        .unwrap()
        .reason
        .contains("before-state"));
    let plan = candidate(&world, None, vec![snapshot(1, 8.)], vec![snapshot(1, 9.)]);
    let plan = publish(&mut world, plan);
    queue_captured_plan(&mut world, plan.plan_id()).unwrap();
    world.resource_mut::<History>().clear();
    drain(&mut world);
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
    assert!(world
        .resource::<SemanticEnforcement>()
        .last_refusal()
        .unwrap()
        .reason
        .contains("stale"));
}

#[test]
fn undo_redo_and_document_replacement_never_revive_old_plans() {
    let mut world = world();
    let old = candidate(&world, None, vec![], vec![snapshot(9, 9.)]);
    let old = publish(&mut world, old);
    let edit = candidate(&world, None, vec![], vec![snapshot(1, 1.)]);
    let edit = publish(&mut world, edit);
    queue_captured_plan(&mut world, edit.plan_id()).unwrap();
    drain(&mut world);
    assert!(matches!(
        queue_captured_plan(&mut world, old.plan_id()),
        Err(PlanError::Refused(_))
    ));
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    assert!(matches!(
        queue_captured_plan(&mut world, old.plan_id()),
        Err(PlanError::Refused(_))
    ));
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    drain(&mut world);
    assert!(matches!(
        queue_captured_plan(&mut world, old.plan_id()),
        Err(PlanError::Refused(_))
    ));
    world.resource_mut::<History>().clear();
    assert!(matches!(
        queue_captured_plan(&mut world, old.plan_id()),
        Err(PlanError::Refused(_))
    ));
    assert!(!entity_exists(&world, ElementId(9)));
}

#[test]
fn pending_actions_and_groups_are_not_absorbed_or_drained() {
    let mut world = world();
    let plan = candidate(&world, None, vec![], vec![snapshot(1, 1.)]);
    let plan = publish(&mut world, plan);
    world
        .resource_mut::<PendingCommandQueue>()
        .begin_group("other user action");
    assert_eq!(
        queue_captured_plan(&mut world, plan.plan_id()),
        Err(PlanError::PendingWork)
    );
    world.resource_mut::<PendingCommandQueue>().end_group();
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    assert_eq!(
        queue_captured_plan(&mut world, plan.plan_id()),
        Err(PlanError::PendingWork)
    );
    assert!(!world.resource::<PendingCommandQueue>().is_empty());
    assert!(world
        .resource::<AuthoredEditPlanRegistry>()
        .get(plan.plan_id())
        .is_some());
}

#[test]
fn bounds_eviction_and_republication_cannot_revive_a_candidate() {
    let world = world();
    let mut registry = AuthoredEditPlanRegistry::with_limits(1, 4096);
    let first = registry
        .publish(candidate(&world, None, vec![], vec![snapshot(1, 1.)]))
        .unwrap();
    let old_id = first.plan_id().clone();
    let same_content = candidate(&world, None, vec![], vec![snapshot(1, 1.)]);
    assert_eq!(first.digest(), same_content.digest());
    let changed = candidate(&world, None, vec![], vec![snapshot(1, 2.)]);
    assert_ne!(first.digest(), changed.digest());
    registry.publish(changed).unwrap();
    assert!(registry.get(&old_id).is_none());
    let recovered = Arc::try_unwrap(first).unwrap();
    let republished = registry.publish(recovered).unwrap();
    assert_ne!(republished.plan_id(), &old_id);
    assert!(registry.get(&old_id).is_none());
    assert_eq!(registry.plans.len(), 1);
    registry
        .begin_interaction(world.resource::<History>().revision_token())
        .unwrap();
    assert_eq!(
        registry.begin_interaction(world.resource::<History>().revision_token()),
        Err(PlanError::Capacity)
    );
    let mut tiny = AuthoredEditPlanRegistry::with_limits(1, 10);
    assert!(matches!(
        tiny.publish(same_content),
        Err(PlanError::Capacity)
    ));
}

#[test]
fn duplicate_identities_and_stale_capture_are_rejected() {
    let mut world = world();
    let base = world.resource::<History>().revision_token();
    assert!(matches!(
        AuthoredEditPlan::capture(
            &world,
            None,
            base.clone(),
            PlanContext::default(),
            vec![],
            vec![snapshot(1, 0.), snapshot(1, 1.)],
            SemanticPlan::none()
        ),
        Err(PlanError::InvalidSnapshots(_))
    ));
    world.resource_mut::<History>().clear();
    assert!(matches!(
        AuthoredEditPlan::capture(
            &world,
            None,
            base,
            PlanContext::default(),
            vec![],
            vec![],
            SemanticPlan::none()
        ),
        Err(PlanError::Stale)
    ));
}

#[test]
fn refused_semantic_intent_is_visible_in_preview_and_has_no_effects() {
    use crate::semantics::{
        components::ConceptAssignment,
        test_fixtures::{roof_edge_fixture, BARGEBOARD, WALL_CLADDING},
        BindTarget, PlanIntent, PredicateId,
    };
    let mut world = world();
    world.insert_resource(SemanticGraph(roof_edge_fixture()));
    world.spawn((ElementId(51), ConceptAssignment::new(BARGEBOARD)));
    world.spawn((ElementId(41), ConceptAssignment::new(WALL_CLADDING)));
    let semantic = SemanticPlan::none().with(PlanIntent::Bind {
        subject: ElementId(51),
        predicate: PredicateId::new("follows"),
        target: BindTarget::Entity(ElementId(41)),
    });
    let plan = AuthoredEditPlan::capture(
        &world,
        None,
        world.resource::<History>().revision_token(),
        PlanContext::default(),
        vec![],
        vec![snapshot(1, 1.)],
        semantic,
    )
    .unwrap();
    assert!(!plan.can_commit());
    assert!(!plan.context().findings.is_empty());
    let plan = publish(&mut world, plan);
    assert!(matches!(
        queue_captured_plan(&mut world, plan.plan_id()),
        Err(PlanError::Refused(_))
    ));
    assert!(!entity_exists(&world, ElementId(1)));
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
}

#[test]
fn downgrade_preview_exposes_kernel_loss_manifest_with_planner_context() {
    use crate::semantics::{
        components::ConceptAssignment,
        test_fixtures::{roof_edge_fixture, RAKE_EDGE, ROOF_SYSTEM},
        ConceptId, PlanIntent,
    };
    let mut world = world();
    world.insert_resource(SemanticGraph(roof_edge_fixture()));
    let entity = world
        .spawn((ElementId(388), ConceptAssignment::new(ROOF_SYSTEM)))
        .id();
    let base = world.resource::<History>().revision_token();
    let plan = AuthoredEditPlan::capture(
        &world,
        None,
        base.clone(),
        PlanContext {
            assumptions: vec!["Reference geometry remains unchanged".into()],
            unresolved_decisions: vec!["Replacement semantic classification".into()],
            ..Default::default()
        },
        vec![],
        vec![],
        SemanticPlan::none().with(PlanIntent::RemoveConcept {
            entity: ElementId(388),
            concept: ConceptId::new(ROOF_SYSTEM),
        }),
    )
    .unwrap();
    assert!(plan.can_commit());
    let plan = publish(&mut world, plan);
    let inspection = plan.content();
    let losses = inspection["context"]["obligations"].as_array().unwrap();
    assert_eq!(losses.len(), 1);
    assert!(losses[0].as_str().unwrap().contains(RAKE_EDGE));
    assert!(losses[0].as_str().unwrap().contains("invalidated"));
    assert_eq!(
        inspection["context"]["assumptions"][0],
        "Reference geometry remains unchanged"
    );
    assert_eq!(
        inspection["context"]["unresolved_decisions"][0],
        "Replacement semantic classification"
    );
    assert!(world.get::<ConceptAssignment>(entity).is_some());
    assert_eq!(world.resource::<History>().revision_token(), base);
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
}
