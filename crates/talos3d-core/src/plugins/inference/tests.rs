use super::*;
use crate::plugins::modeling::primitives::{BoxPrimitive, ShapeRotation};

fn world_at(raw: Vec3) -> World {
    let mut world = World::new();
    world.init_resource::<InferenceEngine>();
    world.init_resource::<ButtonInput<KeyCode>>();
    world.init_resource::<InputOwnership>();
    world.init_resource::<TransformState>();
    world.init_resource::<DrawingPlane>();
    world.insert_resource(CursorWorldPos {
        raw: Some(raw),
        snapped: Some(raw),
        ..default()
    });
    world.insert_resource(SnapResult {
        raw_position: Some(raw),
        position: Some(raw),
        kind: SnapKind::Grid,
        ..default()
    });
    world
}
fn point(world: &mut World, raw: Vec3) {
    *world.resource_mut::<SnapResult>() = SnapResult {
        raw_position: Some(raw),
        position: Some(raw),
        kind: SnapKind::Grid,
        ..default()
    };
    world.resource_mut::<CursorWorldPos>().raw = Some(raw);
}
#[test]
fn moving_inference_publishes_same_point_for_preview_and_command_consumers() {
    let mut world = world_at(Vec3::new(3.0, 0.0, 0.04));
    let mut state = world.resource_mut::<TransformState>();
    state.mode = TransformMode::Moving;
    state.initial_cursor = Some(Vec3::ZERO);
    update_inference(&mut world);
    assert_eq!(
        world.resource::<SnapResult>().position,
        Some(Vec3::new(3.0, 0.0, 0.0))
    );
    assert_eq!(
        world.resource::<CursorWorldPos>().snapped,
        world.resource::<SnapResult>().position
    );
    assert_eq!(
        world.resource::<InferenceEngine>().best_label(),
        Some("X axis")
    );
}
#[test]
fn shift_holds_direction_but_reprojects_each_frame_and_releases() {
    let mut world = world_at(Vec3::new(3.0, 0.0, 0.04));
    {
        let mut state = world.resource_mut::<TransformState>();
        state.mode = TransformMode::Moving;
        state.initial_cursor = Some(Vec3::ZERO);
    }
    update_inference(&mut world);
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::ShiftLeft);
    point(&mut world, Vec3::new(5.0, 0.0, 2.0));
    update_inference(&mut world);
    assert_eq!(
        world.resource::<SnapResult>().position,
        Some(Vec3::new(5.0, 0.0, 0.0))
    );
    world.resource_mut::<ButtonInput<KeyCode>>().clear();
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .release(KeyCode::ShiftLeft);
    point(&mut world, Vec3::new(5.0, 0.0, 2.0));
    update_inference(&mut world);
    assert!(world.resource::<InferenceEngine>().locked.is_none());
    assert_eq!(
        world.resource::<SnapResult>().position,
        Some(Vec3::new(5.0, 0.0, 2.0))
    );
}
#[test]
fn idle_clears_locked_direction_and_ui_capture_does_not_handle_keys() {
    let mut world = world_at(Vec3::new(3.0, 0.0, 0.04));
    {
        let mut state = world.resource_mut::<TransformState>();
        state.mode = TransformMode::Moving;
        state.initial_cursor = Some(Vec3::ZERO);
    }
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::ArrowRight);
    update_inference(&mut world);
    assert!(world.resource::<InferenceEngine>().locked.is_some());
    world.resource_mut::<ButtonInput<KeyCode>>().clear();
    world.resource_mut::<TransformState>().mode = TransformMode::Idle;
    update_inference(&mut world);
    assert!(world.resource::<InferenceEngine>().locked.is_none());
    *world.resource_mut::<InputOwnership>() = InputOwnership::UiCapture;
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::ArrowRight);
    update_inference(&mut world);
    assert!(world.resource::<InferenceEngine>().locked.is_none());
}
#[test]
fn exact_point_wins_over_unlocked_direction() {
    let mut world = world_at(Vec3::new(3.0, 0.0, 0.04));
    {
        let mut state = world.resource_mut::<TransformState>();
        state.mode = TransformMode::Moving;
        state.initial_cursor = Some(Vec3::ZERO);
    }
    world.resource_mut::<SnapResult>().kind = SnapKind::Endpoint;
    update_inference(&mut world);
    assert_eq!(
        world.resource::<SnapResult>().position,
        Some(Vec3::new(3.0, 0.0, 0.04))
    );
}
#[test]
fn acquisition_requires_dwell_and_is_bounded() {
    let mut engine = InferenceEngine::default();
    for i in 0..12 {
        let reference = Reference {
            point: Vec3::X * i as f32,
            edge: None,
            owner: None,
        };
        engine.acquire(Some(reference.clone()), i as f64);
        engine.acquire(Some(reference), i as f64 + 0.5);
    }
    assert_eq!(engine.references.len(), MAX_REFERENCES);
    engine.acquire(
        Some(Reference {
            point: Vec3::Y,
            edge: None,
            owner: None,
        }),
        20.0,
    );
    engine.acquire(None, 20.1);
    assert!(!engine.references.iter().any(|r| r.point == Vec3::Y));
}
#[test]
fn actual_rotated_topology_supplies_midpoint_and_never_bounds_corner() {
    let mut world = world_at(Vec3::ZERO);
    let rotation = Quat::from_rotation_y(0.5);
    let entity = world
        .spawn((
            ElementId(19),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::ONE,
            },
            ShapeRotation(rotation),
        ))
        .id();
    let cage = build_edge_cage(&world, entity, ElementId(19));
    assert_eq!(cage.edges().len(), 12);
    let edge = &cage.edges()[0];
    let midpoint = (edge.start + edge.end) * 0.5;
    point(&mut world, midpoint + Vec3::splat(0.001));
    world.resource_mut::<CursorWorldPos>().hovered_element_id = Some(ElementId(19));
    update_inference(&mut world);
    assert_eq!(world.resource::<SnapResult>().kind, SnapKind::Midpoint);
    assert!(
        world
            .resource::<SnapResult>()
            .position
            .unwrap()
            .distance(midpoint)
            < 1e-5
    );
}
#[test]
fn degenerate_segments_are_rejected() {
    assert!(segment_point(Vec3::ZERO, Vec3::ZERO, Vec3::ONE, None).is_none());
}
#[test]
fn ctrl_bypasses_without_clearing_arrow_lock() {
    let mut world = world_at(Vec3::new(2.0, 0.0, 0.04));
    {
        let mut state = world.resource_mut::<TransformState>();
        state.mode = TransformMode::Moving;
        state.initial_cursor = Some(Vec3::ZERO);
    }
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::ArrowRight);
    update_inference(&mut world);
    world.resource_mut::<ButtonInput<KeyCode>>().clear();
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::ControlLeft);
    point(&mut world, Vec3::new(4.0, 0.0, 2.0));
    update_inference(&mut world);
    assert_eq!(
        world.resource::<CursorWorldPos>().snapped,
        Some(Vec3::new(4.0, 0.0, 2.0))
    );
    assert!(world.resource::<InferenceEngine>().arrow_guide.is_some());
}
#[test]
fn bounded_hover_resolution_benchmark() {
    let mut world = world_at(Vec3::new(1.0, 1.0, 0.0));
    world.spawn((
        ElementId(19),
        BoxPrimitive {
            centre: Vec3::ZERO,
            half_extents: Vec3::ONE,
        },
        ShapeRotation::default(),
    ));
    world.resource_mut::<CursorWorldPos>().hovered_element_id = Some(ElementId(19));
    update_inference(&mut world);
    let start = std::time::Instant::now();
    for _ in 0..10_000 {
        update_inference(&mut world);
    }
    eprintln!(
        "cached box inference: {:.2} us/frame",
        start.elapsed().as_secs_f64() * 100.0
    );
}

#[test]
fn compound_guides_intersect_in_space_not_just_on_screen() {
    let a = Guide {
        origin: Vec3::ZERO,
        direction: Vec3::X,
        kind: InferenceKind::Axis,
        label: "X".into(),
        owner: None,
    };
    let mut b = Guide {
        origin: Vec3::new(2.0, 0.0, 4.0),
        direction: Vec3::Z,
        kind: InferenceKind::FromPoint,
        label: "From".into(),
        owner: None,
    };
    assert_eq!(guide_intersection(&a, &b), Some(Vec3::X * 2.0));
    b.origin.y = 1.0;
    assert!(guide_intersection(&a, &b).is_none());
}
#[test]
fn tangent_points_are_on_circle_and_perpendicular_to_radius() {
    let center = Vec3::new(1.0, 0.0, 3.0);
    let origin = center + Vec3::X * 4.0;
    let points = circle_tangents(center, Vec3::Y, 2.0, origin);
    assert_eq!(points.len(), 2);
    for point in points {
        assert!((point.distance(center) - 2.0).abs() < 1e-5);
        assert!((point - center).dot(origin - point).abs() < 1e-5);
    }
    assert!(circle_tangents(center, Vec3::Y, 2.0, origin + Vec3::Y).is_empty());
    assert!(circle_tangents(center, Vec3::Y, 2.0, center).is_empty());
}

#[test]
fn drafting_does_not_claim_an_off_plane_point() {
    let mut world = world_at(Vec3::new(2.0, 0.0, 3.0));
    world.insert_resource(State::new(ActiveTool::PlaceDraftLine));
    world.resource_mut::<SnapResult>().position = Some(Vec3::new(2.0, 4.0, 3.0));
    world.resource_mut::<SnapResult>().kind = SnapKind::Endpoint;
    update_inference(&mut world);
    assert_eq!(
        world.resource::<SnapResult>().position,
        Some(Vec3::new(2.0, 0.0, 3.0))
    );
    assert_ne!(world.resource::<SnapResult>().kind, SnapKind::Endpoint);
}
#[test]
fn arrow_locked_height_uses_ray_even_with_ground_cursor() {
    let guide = Guide {
        origin: Vec3::ZERO,
        direction: Vec3::Y,
        kind: InferenceKind::Axis,
        label: "Y".into(),
        owner: None,
    };
    let view = View {
        camera: Camera::default(),
        transform: GlobalTransform::default(),
        cursor: Vec2::ZERO,
        ray: Ray3d::new(Vec3::new(5.0, 3.0, 0.0), Dir3::NEG_X),
    };
    assert_eq!(
        line_point(&guide, Vec3::new(8.0, 0.0, 0.0), Some(&view)),
        Vec3::Y * 3.0
    );
}
#[test]
fn linear_modes_are_visible_and_keep_point_snapping() {
    let mut world = world_at(Vec3::new(3.0, 0.0, 0.04));
    {
        let mut state = world.resource_mut::<TransformState>();
        state.mode = TransformMode::Moving;
        state.initial_cursor = Some(Vec3::ZERO);
    }
    update_inference(&mut world);
    world
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyI);
    update_inference(&mut world);
    assert_eq!(
        world.resource::<InferenceEngine>().linear_mode,
        LinearMode::References
    );
    assert!(world
        .resource::<InferenceEngine>()
        .best_label()
        .unwrap()
        .contains("Edge relations only"));
}

#[test]
fn visible_face_boundary_is_not_rejected_by_its_larger_camera_distance() {
    let eye = Some(Vec3::new(0.0, 3.0, 5.0));
    let surface = Some(Vec3::new(0.0, 1.0, 1.0));
    assert!(visible_near_surface(
        Vec3::new(0.0, 0.0, 1.0),
        surface,
        Some(Vec3::Z),
        eye
    ));
    assert!(!visible_near_surface(
        Vec3::new(0.0, 0.0, -1.0),
        surface,
        Some(Vec3::Z),
        eye
    ));
}
