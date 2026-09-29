use super::*;
use crate::plugins::{
    authored_edit_plan::capture_snapshot,
    history::{
        apply_pending_history_commands_for_test as drain, PendingCommandQueue, SemanticEnforcement,
    },
    identity::ElementIdAllocator,
    modeling::{
        generic_factory::PrimitiveFactory,
        primitives::{BoxPrimitive, ShapeRotation},
    },
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Resource, Default)]
struct Calls(AtomicUsize);
fn fixture() -> World {
    let mut world = World::new();
    let mut registry = CapabilityRegistry::default();
    registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
    world.insert_resource(registry);
    world.init_resource::<History>();
    world.init_resource::<AuthoredEditPlanRegistry>();
    world.init_resource::<PendingCommandQueue>();
    world.init_resource::<SemanticEnforcement>();
    world.init_resource::<ElementIdAllocator>();
    world.init_resource::<ActiveTransformPreview>();
    world.init_resource::<TransformState>();
    world.init_resource::<PushPullContext>();
    world.init_resource::<PivotPoint>();
    world.init_resource::<SnapResult>();
    world.insert_resource(CursorWorldPos {
        raw: Some(Vec3::X * 2.),
        ..Default::default()
    });
    world.init_resource::<InferenceEngine>();
    world.init_resource::<StatusBarData>();
    world.init_resource::<ButtonInput<KeyCode>>();
    world.init_resource::<ButtonInput<MouseButton>>();
    world.insert_resource(InputOwnership::Modal(ModalKind::Transform));
    world.init_resource::<Calls>();
    #[cfg(feature = "perf-stats")]
    world.init_resource::<PerfStats>();
    let before: BoxedEntity = PrimitiveSnapshot {
        element_id: ElementId(1),
        primitive: BoxPrimitive {
            centre: Vec3::ZERO,
            half_extents: Vec3::splat(0.5),
        },
        rotation: ShapeRotation::default(),
        material_assignment: None,
        opening_context: None,
        subobject_display_overrides: None,
    }
    .into();
    before.apply_to(&mut world);
    let entity = find_entity_by_element_id_readonly(&world, ElementId(1)).unwrap();
    world.insert_resource(TransformState {
        mode: TransformMode::Moving,
        axis: AxisConstraint::X,
        initial_cursor: Some(Vec3::ZERO),
        numeric_buffer: Some("2".into()),
        initial_snapshots: vec![(entity, before)],
        ..Default::default()
    });
    let mut modifiers = TransformPreviewModifiers::default();
    modifiers.register(|world, _, _| {
        world.resource::<Calls>().0.fetch_add(1, Ordering::Relaxed);
    });
    world.insert_resource(modifiers);
    world
}

#[test]
fn release_uses_presented_id_without_reading_new_input_or_rerunning_modifiers() {
    let mut world = fixture();
    let before = capture_snapshot(&world, ElementId(1)).unwrap();
    update_transform_preview(&mut world);
    let plan = world
        .resource::<ActiveTransformPreview>()
        .plan
        .clone()
        .unwrap();
    let presented = world.resource::<ActiveTransformPreview>().snapshots.clone();
    assert_eq!(plan.after_snapshots(), presented);
    let entity = find_entity_by_element_id_readonly(&world, ElementId(1)).unwrap();
    assert_eq!(
        *world.get::<Transform>(entity).unwrap(),
        presented[0].preview_transform().unwrap()
    );
    // An input arriving at release must not silently replace the displayed candidate.
    world.resource_mut::<TransformState>().numeric_buffer = Some("99".into());
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Enter);
    confirm_transform(&mut world);
    assert_eq!(world.resource::<Calls>().0.load(Ordering::Relaxed), 1);
    assert!(world
        .resource::<AuthoredEditPlanRegistry>()
        .get(plan.plan_id())
        .is_none());
    drain(&mut world);
    assert_eq!(world.resource::<History>().undo_stack_len(), 1);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).as_ref(),
        Some(&presented[0])
    );
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).as_ref(),
        Some(&before)
    );
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).as_ref(),
        Some(&presented[0])
    );
}

#[test]
fn next_frame_supersedes_candidate_and_cancel_restores_without_history() {
    let mut world = fixture();
    let before = capture_snapshot(&world, ElementId(1)).unwrap();
    update_transform_preview(&mut world);
    let first = world
        .resource::<ActiveTransformPreview>()
        .plan
        .clone()
        .unwrap();
    world.resource_mut::<TransformState>().numeric_buffer = Some("3".into());
    update_transform_preview(&mut world);
    let second = world
        .resource::<ActiveTransformPreview>()
        .plan
        .clone()
        .unwrap();
    assert_eq!(first.interaction_id(), second.interaction_id());
    assert_ne!(first.plan_id(), second.plan_id());
    assert_eq!(first.before_snapshots(), second.before_snapshots());
    assert!(world
        .resource::<AuthoredEditPlanRegistry>()
        .get(first.plan_id())
        .is_none());
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Escape);
    cancel_transform(&mut world);
    assert!(world
        .resource::<AuthoredEditPlanRegistry>()
        .get(second.plan_id())
        .is_none());
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).as_ref(),
        Some(&before)
    );
    let entity = find_entity_by_element_id_readonly(&world, ElementId(1)).unwrap();
    assert_eq!(
        *world.get::<Transform>(entity).unwrap(),
        before.preview_transform().unwrap()
    );
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
    assert!(world.resource::<TransformState>().is_idle());
}

#[test]
fn legacy_external_write_is_preserved_and_refuses_the_captured_gesture() {
    let mut world = fixture();
    update_transform_preview(&mut world);
    let external = capture_snapshot(&world, ElementId(1))
        .unwrap()
        .translate_by(Vec3::Y * 8.);
    external.apply_to(&mut world); // deliberately no History revision increment
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Enter);
    confirm_transform(&mut world);
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).as_ref(),
        Some(&external)
    );
    assert_eq!(world.resource::<History>().undo_stack_len(), 0);
    assert!(world.resource::<StatusBarData>().hint.contains("refused"));
}

#[test]
fn native_history_action_cancels_presentation_before_the_action_runs() {
    let mut world = fixture();
    let before = capture_snapshot(&world, ElementId(1)).unwrap();
    update_transform_preview(&mut world);
    let id = world
        .resource::<ActiveTransformPreview>()
        .plan
        .as_ref()
        .unwrap()
        .plan_id()
        .clone();
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    assert!(world.resource::<TransformState>().is_idle());
    assert!(world
        .resource::<AuthoredEditPlanRegistry>()
        .get(&id)
        .is_none());
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).as_ref(),
        Some(&before)
    );
}

#[test]
#[ignore = "explicit local CPU stage measurement; not a presented-frame latency claim"]
fn bounded_full_candidate_stage_benchmark() {
    let mut world = fixture();
    for id in 2..=4352 {
        let snapshot: BoxedEntity = PrimitiveSnapshot {
            element_id: ElementId(id),
            primitive: BoxPrimitive {
                centre: Vec3::new(id as f32, 0., 0.),
                half_extents: Vec3::splat(0.5),
            },
            rotation: ShapeRotation::default(),
            material_assignment: None,
            opening_context: None,
            subobject_display_overrides: None,
        }
        .into();
        snapshot.apply_to(&mut world);
        if id <= 256 {
            let entity = find_entity_by_element_id_readonly(&world, ElementId(id)).unwrap();
            world
                .resource_mut::<TransformState>()
                .initial_snapshots
                .push((entity, snapshot));
        }
    }
    let mut times = Vec::new();
    for i in 0..110 {
        world.resource_mut::<TransformState>().numeric_buffer =
            Some((2. + i as f32 * 0.01).to_string());
        let start = std::time::Instant::now();
        update_transform_preview(&mut world);
        let elapsed = start.elapsed().as_secs_f64() * 1000.;
        assert!(world.resource::<ActiveTransformPreview>().plan.is_some());
        if i >= 10 {
            times.push(elapsed);
        }
    }
    times.sort_by(f64::total_cmp);
    let p95 = times[94];
    eprintln!("candidate CPU stage:256 selected/4096 unrelated/100 samples; p95={p95:.3}ms");
    assert!(p95 <= 20., "fixed CPU stage budget exceeded: {p95}ms");
}
