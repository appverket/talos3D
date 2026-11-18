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
        layer: None,
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
    measure_bounded_full_candidate_stage(0);
}

#[cfg(feature = "model-api")]
#[test]
#[ignore = "explicit local CPU stage measurement with 32 dependencies; not presented-frame latency"]
fn bounded_full_candidate_stage_benchmark_with_32_harness_dependencies() {
    measure_bounded_full_candidate_stage(32);
}

fn measure_bounded_full_candidate_stage(dependency_count: usize) {
    let mut world = fixture();
    #[cfg(feature = "model-api")]
    if dependency_count > 0 {
        use crate::plugins::{
            authored_edit_plan::dependencies::HarnessPlannerBindings,
            curation::{
                CurationConnection, CurationReleaseStore, ReleaseArtifact, ReleaseManifest,
                RELEASE_SCHEMA,
            },
        };
        let store = CurationReleaseStore::default();
        store.bind(&CurationConnection {
            base_url: "https://curation.example.test/v1/curation".into(),
            trusted_public_keys: vec!["ab".repeat(32)],
        });
        // Explicitly pin all 4096 retained revisions while each batch is
        // current, outside the timer. Unrelated unpinned release history must
        // not consume this cache. The timed path looks up only its 32 pins.
        let mut dependencies = Vec::new();
        for (sequence, start) in (0..4096).step_by(1000).enumerate() {
            let releases = (start..(start + 1000).min(4096))
                .map(|id| ReleaseArtifact {
                    artifact_id: format!("skill:benchmark.{id}"),
                    revision: "r1".into(),
                    remedy_keys: vec![],
                    required_tool_ids: vec![],
                    withdrawn: false,
                    refresh_required: false,
                })
                .collect();
            store
                .accept(ReleaseManifest {
                    schema: RELEASE_SCHEMA.into(),
                    sequence: sequence as u64 + 1,
                    releases,
                })
                .unwrap();
            for id in start..(start + 1000).min(4096) {
                let dependency = store
                    .pin_dependency(&format!("skill:benchmark.{id}"), "r1")
                    .unwrap();
                if (4000..4000 + dependency_count).contains(&id) {
                    dependencies.push(dependency);
                }
            }
        }
        assert_eq!(dependencies.len(), dependency_count);
        let mut bindings = HarnessPlannerBindings::default();
        bindings.bind("core.transform", 1, dependencies).unwrap();
        world.insert_resource(bindings);
        world.insert_resource(store);
    }
    for id in 2..=4352 {
        let snapshot: BoxedEntity = PrimitiveSnapshot {
            element_id: ElementId(id),
            primitive: BoxPrimitive {
                centre: Vec3::new(id as f32, 0., 0.),
                half_extents: Vec3::splat(0.5),
            },
            rotation: ShapeRotation::default(),
            layer: None,
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
        let plan = world
            .resource::<ActiveTransformPreview>()
            .plan
            .as_ref()
            .unwrap();
        assert!(plan.can_commit());
        assert_eq!(plan.context().harness_dependencies.len(), dependency_count);
        if i >= 10 {
            times.push(elapsed);
        }
    }
    times.sort_by(f64::total_cmp);
    let p95 = times[94];
    let worst = times[99];
    eprintln!("candidate CPU stage:256 selected/4096 unrelated/{dependency_count} dependencies/100 samples; p95={p95:.3}ms worst={worst:.3}ms");
    assert!(p95 <= 20., "fixed CPU stage budget exceeded: {p95}ms");
}

#[test]
fn stationary_input_keeps_the_inspectable_candidate_id_stable() {
    let mut world = fixture();
    update_transform_preview(&mut world);
    let original = world
        .resource::<ActiveTransformPreview>()
        .plan
        .clone()
        .unwrap();
    update_transform_preview(&mut world);
    let observed = crate::plugins::ux_harness::observe_ux(&mut world).unwrap();
    assert_eq!(
        observed.active_edit_plan.unwrap().plan_id,
        original.plan_id().0
    );
    assert!(world
        .resource::<AuthoredEditPlanRegistry>()
        .get(original.plan_id())
        .is_some());
}

#[test]
fn inferred_move_is_the_presented_shared_plan_and_undoable_commit() {
    let mut world = fixture();
    {
        let mut state = world.resource_mut::<TransformState>();
        state.axis = AxisConstraint::None;
        state.numeric_buffer = None;
    }
    world.init_resource::<crate::plugins::cursor::DrawingPlane>();
    let raw = Vec3::new(3.0, 0.0, 0.04);
    *world.resource_mut::<SnapResult>() = SnapResult {
        raw_position: Some(raw),
        position: Some(raw),
        kind: crate::plugins::snap::SnapKind::Grid,
        ..Default::default()
    };
    world.resource_mut::<CursorWorldPos>().raw = Some(raw);
    crate::plugins::inference::update_inference(&mut world);
    update_transform_preview(&mut world);
    let presented = world.resource::<ActiveTransformPreview>().snapshots.clone();
    assert_eq!(presented[0].center(), Vec3::X * 3.0);
    let entity = find_entity_by_element_id_readonly(&world, ElementId(1)).unwrap();
    assert_eq!(
        *world.get::<Transform>(entity).unwrap(),
        presented[0].preview_transform().unwrap()
    );
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Enter);
    confirm_transform(&mut world);
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).as_ref(),
        Some(&presented[0])
    );
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).unwrap().center(),
        Vec3::ZERO
    );
}

#[test]
fn push_pull_support_edge_inference_matches_live_preview_commit_and_undo() {
    use crate::capability_registry::{FaceId, GeneratedFaceRef};
    use crate::plugins::{
        cursor::DrawingPlane,
        modeling::{
            face_sketch::{shape_profile, FaceSketchShape},
            profile_feature::{
                FaceProfileFeature, FaceProfileFeatureFactory, FaceProfileFeatureSnapshot,
            },
        },
        snap::SnapKind,
    };
    let mut world = fixture();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_factory(FaceProfileFeatureFactory);
    world.init_resource::<DrawingPlane>();
    world.init_resource::<Messages<ApplyEntityChangesCommand>>();
    let original: BoxedEntity = FaceProfileFeatureSnapshot {
        display_overrides: None,
        element_id: ElementId(2),
        feature: FaceProfileFeature {
            parent: ElementId(1),
            anchor_origin: Vec3::X * 0.5,
            depth: 1.5,
            profile: shape_profile(
                FaceSketchShape::Rectangle,
                Vec2::splat(-0.2),
                Vec2::splat(0.2),
            )
            .unwrap(),
            support_face: None,
        },
        rotation: ShapeRotation(Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2)),
    }
    .into();
    original.apply_to(&mut world);
    let entity = find_entity_by_element_id_readonly(&world, ElementId(2)).unwrap();
    world.entity_mut(entity).insert(Selected);
    world.resource_mut::<TransformState>().initial_snapshots = vec![(entity, original.clone())];
    world.resource_mut::<TransformState>().numeric_buffer = None;
    // Deliberately displaced grab point: inference aligns the face, not this point.
    world.resource_mut::<TransformState>().initial_cursor = Some(Vec3::new(8., 3., -4.));
    world.resource_mut::<TransformState>().axis = AxisConstraint::Custom(Vec3::X);
    world.resource_mut::<PushPullContext>().active_face = Some(PushPullFace {
        entity,
        element_id: ElementId(2),
        face_id: FaceId(0),
        generated_face_ref: Some(GeneratedFaceRef::FeatureCap),
        normal: Vec3::X,
        feature_parent: None,
        feature_anchor_origin: None,
        // A tessellated cap centroid may carry rounding residue. The authored
        // anchor and depth must still produce an exactly flush inference.
        drag_plane: DrawingPlane::from_face(Vec3::X * 2.000015, Vec3::Z),
        initial_cursor_screen: None,
        screen_normal: None,
        live_csg: None,
    });
    // The visible combined body belongs to feature 2, but this edge belongs to
    // its fixed, hidden supporting box. The target lies off the extrusion axis.
    let raw = Vec3::new(0.52, 0.5, 0.2);
    *world.resource_mut::<CursorWorldPos>() = CursorWorldPos {
        raw: Some(raw),
        snapped: Some(raw),
        hovered_element_id: Some(ElementId(2)),
        surface_point: Some(Vec3::new(0.5, 0.5, 0.2)),
        surface_normal: Some(Vec3::X),
        ..Default::default()
    };
    *world.resource_mut::<SnapResult>() = SnapResult {
        raw_position: Some(raw),
        position: Some(raw),
        kind: SnapKind::Grid,
        ..Default::default()
    };
    crate::plugins::inference::update_inference(&mut world);
    assert_eq!(world.resource::<SnapResult>().kind, SnapKind::OnEdge);
    assert!((world.resource::<SnapResult>().position.unwrap().x - 0.5).abs() < 1e-6);
    // Imported coplanar boundary vertices can carry small coordinate residue.
    world
        .resource_mut::<SnapResult>()
        .position
        .as_mut()
        .unwrap()
        .x += 0.00002;
    update_transform_preview(&mut world);
    let presented = world.resource::<ActiveTransformPreview>().snapshots[0].clone();
    assert_eq!(world.get::<FaceProfileFeature>(entity).unwrap().depth, 0.0);
    assert_eq!(
        capture_snapshot(&world, ElementId(2)).as_ref(),
        Some(&presented)
    );
    // Repeating frames must not move the fixed target or accumulate depth.
    update_transform_preview(&mut world);
    assert_eq!(world.get::<FaceProfileFeature>(entity).unwrap().depth, 0.0);
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Enter);
    confirm_transform(&mut world);
    let commands: Vec<_> = world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .drain()
        .collect();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].after, vec![presented.clone()]);
    for command in commands {
        crate::plugins::commands::enqueue_apply_entity_changes(&mut world, command);
    }
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(2)).as_ref(),
        Some(&presented)
    );
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(2)).as_ref(),
        Some(&original)
    );
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(2)).as_ref(),
        Some(&presented)
    );
    assert!(world
        .get::<BoxPrimitive>(find_entity_by_element_id_readonly(&world, ElementId(1)).unwrap())
        .is_some());
}

#[test]
fn boundary_preview_can_collapse_reopen_cancel_commit_and_undo_without_mesh_conversion() {
    use crate::capability_registry::{
        FaceId, GeneratedFaceRef, SelectableSubobjectRef, SubobjectSelection,
    };
    use crate::plugins::{
        cursor::DrawingPlane,
        modeling::{
            boundary_edit::{self, BoundarySession},
            profile::Profile2d,
            profile_feature::{
                make_face_profile_feature_snapshot, FaceProfileFeature, FaceProfileFeatureFactory,
            },
        },
    };
    let mut world = fixture();
    world
        .resource_mut::<CapabilityRegistry>()
        .register_factory(FaceProfileFeatureFactory);
    world.init_resource::<DrawingPlane>();
    world.init_resource::<FaceEditContext>();
    world.init_resource::<SubobjectSelection>();
    let mut feature = make_face_profile_feature_snapshot(
        ElementId(2),
        ElementId(1),
        Profile2d::rectangle(1., 1.),
        Vec3::ZERO,
        Quat::IDENTITY,
        None,
    );
    feature.feature.depth = -0.5;
    let original: BoxedEntity = feature.into();
    original.apply_to(&mut world);
    let reference = SelectableSubobjectRef::Face {
        element_id: 2,
        face: GeneratedFaceRef::FeatureSideSegment(0),
    };
    let binding = boundary_edit::resolve(&world, &reference).unwrap();
    let entity = find_entity_by_element_id_readonly(&world, ElementId(2)).unwrap();
    world.resource_mut::<TransformState>().initial_snapshots = vec![(entity, original.clone())];
    world.resource_mut::<TransformState>().axis = AxisConstraint::Custom(binding.normal);
    world.resource_mut::<PushPullContext>().active_face = Some(PushPullFace {
        entity,
        element_id: ElementId(2),
        face_id: FaceId(2),
        generated_face_ref: Some(GeneratedFaceRef::FeatureSideSegment(0)),
        normal: binding.normal,
        feature_parent: None,
        feature_anchor_origin: None,
        drag_plane: DrawingPlane::from_face(binding.origin, Vec3::Y),
        initial_cursor_screen: None,
        screen_normal: None,
        live_csg: None,
    });
    let gesture = boundary_edit::capture(&world, binding.clone()).unwrap();
    world.insert_resource(BoundarySession(Some(gesture.clone())));
    // A ray can pass through the hole rather than hit the semantic owner.
    // Boundary inference must still use that owner's frozen edges.
    let raw = binding.origin + binding.normal + Vec3::Y * 0.25;
    world.resource_mut::<CursorWorldPos>().raw = Some(raw);
    world.resource_mut::<SnapResult>().raw_position = Some(raw);
    world.resource_mut::<CursorWorldPos>().hovered_element_id = None;
    crate::plugins::inference::update_inference(&mut world);
    assert!(matches!(
        world.resource::<SnapResult>().kind,
        crate::plugins::snap::SnapKind::OnEdge | crate::plugins::snap::SnapKind::Midpoint
    ));
    // Preview passes through zero and can move back without losing the original.
    world.resource_mut::<TransformState>().numeric_buffer = Some("1".into());
    update_transform_preview(&mut world);
    assert!(find_entity_by_element_id_readonly(&world, ElementId(2)).is_none());
    world.resource_mut::<TransformState>().numeric_buffer = Some("0.4".into());
    update_transform_preview(&mut world);
    let reopened = capture_snapshot(&world, ElementId(2)).unwrap();
    assert_eq!(reopened.type_name(), "face_profile_feature");
    assert_ne!(reopened, original);
    world.resource_mut::<TransformState>().numeric_buffer = Some("1".into());
    update_transform_preview(&mut world);
    // Release commits the exact displayed collapse, even if input has changed.
    world.resource_mut::<TransformState>().numeric_buffer = Some("0.1".into());
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Enter);
    confirm_transform(&mut world);
    drain(&mut world);
    assert!(find_entity_by_element_id_readonly(&world, ElementId(2)).is_none());
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(2)),
        Some(original.clone())
    );
    let entity = find_entity_by_element_id_readonly(&world, ElementId(2)).unwrap();
    assert_eq!(world.get::<FaceProfileFeature>(entity).unwrap().depth, -0.5);
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    drain(&mut world);
    assert!(find_entity_by_element_id_readonly(&world, ElementId(2)).is_none());
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    world.insert_resource(BoundarySession(Some(gesture)));
    boundary_edit::preview(&mut world, 1.).unwrap();
    boundary_edit::restore(&mut world);
    assert_eq!(capture_snapshot(&world, ElementId(2)), Some(original));
}

#[test]
fn displayed_move_copy_and_internal_array_keep_source_and_endpoint_through_history() {
    let mut world = fixture();
    world.resource_mut::<ElementIdAllocator>().set_next(10);
    let before = capture_snapshot(&world, ElementId(1)).unwrap();
    copies::toggle(&mut world);
    update_transform_preview(&mut world);
    let shown = world.resource::<ActiveTransformPreview>().snapshots[0].center();
    world.resource_mut::<TransformState>().numeric_buffer = Some("99".into());
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Enter);
    confirm_transform(&mut world);
    drain(&mut world);
    assert_eq!(capture_snapshot(&world, ElementId(1)), Some(before));
    assert_eq!(
        capture_snapshot(&world, ElementId(10)).unwrap().center(),
        shown
    );
    let mut last = world.remove_resource::<copies::LastCopy>().unwrap();
    last.input = "/4".into();
    copies::repeat(&mut world, &last).unwrap();
    drain(&mut world);
    for i in 1..4 {
        assert_eq!(
            capture_snapshot(&world, ElementId(10 + i))
                .unwrap()
                .center(),
            shown * i as f32 / 4.
        );
    }
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    drain(&mut world);
    assert!(capture_snapshot(&world, ElementId(11)).is_none());
    assert_eq!(
        capture_snapshot(&world, ElementId(10)).unwrap().center(),
        shown
    );
}

#[cfg(feature = "model-api")]
#[test]
fn repeated_copy_inherits_harness_dependency_at_queue_and_execution() {
    use crate::plugins::{
        authored_edit_plan::dependencies::HarnessPlannerBindings,
        curation::{
            CurationConnection, CurationReleaseStore, ReleaseArtifact, ReleaseManifest,
            RELEASE_SCHEMA,
        },
    };
    for withdraw_before_queue in [true, false] {
        let mut world = fixture();
        world.resource_mut::<ElementIdAllocator>().set_next(10);
        let store = CurationReleaseStore::default();
        store.bind(&CurationConnection {
            base_url: "https://curation.example.test/v1/curation".into(),
            trusted_public_keys: vec!["ab".repeat(32)],
        });
        let manifest = |sequence, withdrawn| ReleaseManifest {
            schema: RELEASE_SCHEMA.into(),
            sequence,
            releases: vec![ReleaseArtifact {
                artifact_id: "skill:test.copy".into(),
                revision: "r1".into(),
                remedy_keys: vec![],
                required_tool_ids: vec![],
                withdrawn,
                refresh_required: withdrawn,
            }],
        };
        store.accept(manifest(1, false)).unwrap();
        let dependency = store.pin_dependency("skill:test.copy", "r1").unwrap();
        let mut bindings = HarnessPlannerBindings::default();
        // Intentionally do not bind core.transform.copy: the derived repeated
        // command must inherit what the original displayed planner actually used.
        bindings
            .bind("core.transform", 1, vec![dependency.clone()])
            .unwrap();
        world.insert_resource(bindings);
        world.insert_resource(store.clone());
        let source = capture_snapshot(&world, ElementId(1)).unwrap();
        copies::toggle(&mut world);
        update_transform_preview(&mut world);
        let first = world
            .resource::<ActiveTransformPreview>()
            .plan
            .clone()
            .unwrap();
        assert_eq!(first.context().harness_dependencies, vec![dependency]);
        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Enter);
        confirm_transform(&mut world);
        drain(&mut world);
        let endpoint = capture_snapshot(&world, ElementId(10)).unwrap();
        assert_eq!(endpoint.center(), Vec3::X * 2.);
        let revision = world.resource::<History>().revision_token();
        let depth = world.resource::<History>().undo_stack_len();
        let mut last = world.remove_resource::<copies::LastCopy>().unwrap();
        last.input = "/4".into();
        if withdraw_before_queue {
            store.accept(manifest(2, true)).unwrap();
            let error = copies::repeat(&mut world, &last).unwrap_err();
            assert!(error.to_lowercase().contains("withdraw"), "{error}");
            assert!(world.resource::<PendingCommandQueue>().is_empty());
        } else {
            copies::repeat(&mut world, &last).unwrap();
            assert!(!world.resource::<PendingCommandQueue>().is_empty());
            store.accept(manifest(2, true)).unwrap();
        }
        drain(&mut world);
        assert_eq!(world.resource::<History>().revision_token(), revision);
        assert_eq!(world.resource::<History>().undo_stack_len(), depth);
        assert_eq!(capture_snapshot(&world, ElementId(1)), Some(source));
        assert_eq!(capture_snapshot(&world, ElementId(10)), Some(endpoint));
        assert_eq!(world.query::<&ElementId>().iter(&world).count(), 2);
        for id in 11..14 {
            assert!(capture_snapshot(&world, ElementId(id)).is_none());
        }
    }
}

#[test]
fn free_move_places_raised_edge_anchor_at_the_actual_lower_guide_point() {
    let mut world = fixture();
    let anchor = Vec3::new(0.25, 1.375, 0.5);
    let target = Vec3::new(1., 0., -2.);
    let mut state = world.resource_mut::<TransformState>();
    state.axis = AxisConstraint::None;
    state.numeric_buffer = None;
    state.initial_cursor = Some(anchor);
    world.resource_mut::<SnapResult>().target = Some(target);
    update_transform_preview(&mut world);
    let pose = world
        .resource::<ActiveTransformPreview>()
        .plan
        .as_ref()
        .unwrap()
        .after_snapshots()[0]
        .center();
    assert!(pose.abs_diff_eq(target - anchor, 1e-5));
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Enter);
    confirm_transform(&mut world);
    drain(&mut world);
    assert_eq!(
        capture_snapshot(&world, ElementId(1)).unwrap().center(),
        pose
    );
}
