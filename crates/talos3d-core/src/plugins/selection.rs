use bevy::{
    ecs::{system::SystemParam, world::EntityRef},
    picking::prelude::*,
    prelude::*,
    window::PrimaryWindow,
};

use crate::{
    authored_entity::EntityBounds,
    capability_registry::{CapabilityRegistry, HitCandidate},
    plugins::{
        camera::orbit_modifier_pressed,
        commands::DeleteEntitiesCommand,
        egui_chrome::EguiWantsInput,
        face_edit::FaceEditContext,
        handles::HandleInteractionState,
        identity::ElementId,
        input_ownership::{InputOwnership, InputPhase},
        layers::{LayerAssignment, LayerRegistry, DEFAULT_LAYER_NAME},
        lighting::{SceneLightNode, SceneLightObjectVisibility},
        modeling::{
            csg::CsgNode,
            group::{
                collect_group_members_recursive, find_group_for_member, GroupEditContext,
                GroupEditMuted, GroupMembers,
            },
            occurrence::{GeneratedOccurrencePart, OccurrenceIdentity},
            primitive_trait::Primitive,
            primitives::ShapeRotation,
            profile::ProfileExtrusion,
            profile_feature::{FaceProfileFeature, FeatureOperand},
        },
        tools::ActiveTool,
        transform::{PivotPoint, TransformMode, TransformState, TransformVisualSystems},
        ui::StatusBarData,
    },
};

const SELECTION_HIGHLIGHT_COLOR: Color = Color::srgb(1.0, 0.8, 0.2);
const ACTIVE_TRANSFORM_HIGHLIGHT_COLOR: Color = Color::srgba(1.0, 0.8, 0.2, 0.5);
const BOX_SELECT_WINDOW_COLOR: Color = Color::srgba(0.2, 0.5, 1.0, 0.15);
const BOX_SELECT_CROSSING_COLOR: Color = Color::srgba(0.2, 1.0, 0.5, 0.15);
const BOX_SELECT_WINDOW_BORDER: Color = Color::srgba(0.3, 0.6, 1.0, 0.8);
const BOX_SELECT_CROSSING_BORDER: Color = Color::srgba(0.3, 1.0, 0.6, 0.8);
const BOX_SELECT_DRAG_THRESHOLD: f32 = 5.0;
const SELECTION_CLICK_SLOP_PX: f32 = 6.0;

type SelectedGroupClickQueryItem = (Entity, &'static ElementId, Has<GroupMembers>, Has<CsgNode>);
type BoxSelectEntityQueryItem = (
    Entity,
    &'static ElementId,
    &'static GlobalTransform,
    Option<&'static Visibility>,
    Option<&'static LayerAssignment>,
    Has<GroupMembers>,
    Option<&'static bevy::camera::primitives::Aabb>,
    Has<SceneLightNode>,
    Has<GroupEditMuted>,
);
type GroupMembershipQueryItem = (Entity, &'static ElementId, &'static GroupMembers);

pub struct SelectionPlugin;

impl Plugin for SelectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DoubleClickTracker>()
            .init_resource::<BoxSelectState>()
            .init_resource::<SelectionPressCapture>()
            .init_resource::<PreviousGroupEditContext>()
            .add_systems(
                Update,
                (
                    (
                        handle_box_select,
                        handle_selection_click,
                        draw_box_select_rect,
                    )
                        .chain()
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    handle_group_double_click
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    handle_group_escape
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    handle_delete_shortcut
                        .in_set(InputPhase::ToolInput)
                        .run_if(in_state(ActiveTool::Select)),
                    update_selection_status,
                    update_group_edit_muting,
                    draw_selected_outlines
                        .after(TransformVisualSystems::PreviewUpdate)
                        .before(TransformVisualSystems::PreviewDraw),
                ),
            );
    }
}

#[derive(Resource, Default)]
struct BoxSelectState {
    drag_start: Option<Vec2>,
    is_dragging: bool,
    /// Set on the frame a box-drag completes so the click handler skips.
    just_completed: bool,
}

#[derive(Resource, Default)]
struct SelectionPressCapture {
    entity: Option<Entity>,
    cursor_screen: Option<Vec2>,
    additive: bool,
}

#[derive(Component)]
pub struct Selected;

type MeshSelectableQueryFilter = With<ElementId>;

#[derive(SystemParam)]
struct SelectionHitTest<'w, 's> {
    ray_cast: MeshRayCast<'w, 's>,
    mesh_selectable_query: Query<'w, 's, (), MeshSelectableQueryFilter>,
    visibility_query: Query<'w, 's, &'static Visibility>,
    face_profile_feature_query: Query<'w, 's, (), With<FaceProfileFeature>>,
}

fn handle_selection_click(world: &mut World) {
    let ownership = world.resource::<InputOwnership>().clone();
    let egui_ptr = world.resource::<EguiWantsInput>().pointer;
    let handle_captures = world
        .resource::<HandleInteractionState>()
        .captures_pointer();
    let box_dragging = world.resource::<BoxSelectState>().is_dragging;
    let box_just_completed = world.resource::<BoxSelectState>().just_completed;
    let just_pressed = world
        .resource::<ButtonInput<MouseButton>>()
        .just_pressed(MouseButton::Left);
    let just_released = world
        .resource::<ButtonInput<MouseButton>>()
        .just_released(MouseButton::Left);
    let face_editing = world.resource::<FaceEditContext>().is_active();
    let orbit_held = orbit_modifier_pressed(world.resource::<ButtonInput<KeyCode>>());

    if !ownership.is_idle()
        || egui_ptr
        || handle_captures
        || box_dragging
        || box_just_completed
        || face_editing
        || orbit_held
    {
        return;
    }

    let mut window_query = world.query_filtered::<&Window, With<PrimaryWindow>>();
    let Ok(window) = window_query.single(world) else {
        return;
    };

    let Some(cursor_position) = window.cursor_position() else {
        return;
    };

    let mut camera_query = world.query::<(&Camera, &GlobalTransform)>();
    let Some((camera, camera_transform)) = camera_query.iter(world).next() else {
        return;
    };

    let viewport_cursor = match camera.logical_viewport_rect() {
        Some(rect) => cursor_position - rect.min,
        None => cursor_position,
    };

    let Ok(ray) = camera.viewport_to_world(camera_transform, viewport_cursor) else {
        return;
    };

    let hit_entity = selection_hit_entity(world, ray);

    // Skip muted entities (outside active group context)
    let hit_entity = hit_entity.filter(|entity| world.get::<GroupEditMuted>(*entity).is_none());

    // Redirect generated occurrence geometry to its authored occurrence root,
    // then redirect group members relative to the active context.
    let hit_entity = hit_entity.map(|entity| {
        let entity = redirect_generated_occurrence_part_to_owner(world, entity);
        let edit_context = world.resource::<GroupEditContext>();
        if let Some(element_id) = world.get::<ElementId>(entity).copied() {
            redirect_hit_to_context_group(world, entity, element_id, edit_context)
        } else {
            entity
        }
    });

    // Skip entities on locked layers
    let hit_entity =
        hit_entity.filter(|entity| !crate::plugins::layers::entity_on_locked_layer(world, *entity));

    // If inside a group, verify the hit is a direct child of the active context
    let hit_entity = hit_entity.filter(|entity| {
        let edit_context = world.resource::<GroupEditContext>();
        if edit_context.is_root() {
            // At root: allow top-level entities and groups
            true
        } else if let Some(active_group_id) = edit_context.current_group() {
            // Inside group: only allow direct children of the active group
            if let Some(element_id) = world.get::<ElementId>(*entity).copied() {
                is_direct_child_of_group(world, element_id, active_group_id)
            } else {
                false
            }
        } else {
            true
        }
    });

    let additive_pressed = {
        let keys = world.resource::<ButtonInput<KeyCode>>();
        keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
    };
    let mut press_capture = world.resource_mut::<SelectionPressCapture>();
    if just_pressed {
        press_capture.entity = hit_entity;
        press_capture.cursor_screen = Some(cursor_position);
        press_capture.additive = additive_pressed;
    }

    if !just_released {
        return;
    }

    let moved_too_far = press_capture
        .cursor_screen
        .map(|press| press.distance(cursor_position) > SELECTION_CLICK_SLOP_PX)
        .unwrap_or(false);
    let additive_selection = press_capture.additive;
    let hit_entity = press_capture.entity.take();
    press_capture.cursor_screen = None;
    press_capture.additive = false;

    if moved_too_far {
        return;
    }

    let selected_entities: Vec<Entity> = world
        .query_filtered::<Entity, With<Selected>>()
        .iter(world)
        .collect();

    match hit_entity {
        Some(entity) if additive_selection => {
            if selected_entities.contains(&entity) {
                world.entity_mut(entity).remove::<Selected>();
            } else {
                world.entity_mut(entity).insert(Selected);
            }
        }
        Some(entity) => {
            for selected_entity in selected_entities {
                if selected_entity != entity {
                    world.entity_mut(selected_entity).remove::<Selected>();
                }
            }
            world.entity_mut(entity).insert(Selected);
        }
        None if !additive_selection => {
            for selected_entity in selected_entities {
                world.entity_mut(selected_entity).remove::<Selected>();
            }
            world.insert_resource(PivotPoint::default());
            // Exit one level of group editing on empty click
            let edit_context = world.resource::<GroupEditContext>();
            if !edit_context.is_root() {
                let mut ctx = edit_context.clone();
                ctx.exit();
                world.insert_resource(ctx);
            }
        }
        None => {}
    }
}

fn redirect_generated_occurrence_part_to_owner(world: &mut World, entity: Entity) -> Entity {
    let Some(generated) = world.get::<GeneratedOccurrencePart>(entity) else {
        return entity;
    };
    let owner = generated.owner;
    let mut query = world.query::<(Entity, &ElementId)>();
    query
        .iter(world)
        .find_map(|(candidate, element_id)| (*element_id == owner).then_some(candidate))
        .unwrap_or(entity)
}

#[derive(Resource, Default)]
struct DoubleClickTracker {
    last_click_time: f64,
    last_click_entity: Option<Entity>,
}

const DOUBLE_CLICK_THRESHOLD_SECONDS: f64 = 0.4;

#[derive(SystemParam)]
struct GroupDoubleClickContext<'w, 's> {
    commands: Commands<'w, 's>,
    mouse_buttons: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    time: Res<'w, Time<Real>>,
    tracker: ResMut<'w, DoubleClickTracker>,
    selected_query: Query<'w, 's, SelectedGroupClickQueryItem, With<Selected>>,
    ownership: Res<'w, InputOwnership>,
    egui_wants_input: Res<'w, EguiWantsInput>,
    edit_context: Res<'w, GroupEditContext>,
    face_edit_context: ResMut<'w, FaceEditContext>,
}

fn handle_group_double_click(mut cx: GroupDoubleClickContext) {
    if !cx.ownership.is_idle() || cx.egui_wants_input.pointer || orbit_modifier_pressed(&cx.keys) {
        return;
    }

    if !cx.mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    // Check if only one entity is selected
    let selected: Vec<_> = cx.selected_query.iter().collect();
    if selected.len() != 1 {
        cx.tracker.last_click_entity = None;
        return;
    }

    let (entity, element_id, is_group, is_csg) = selected[0];

    let now = cx.time.elapsed_secs_f64();
    let is_double_click = cx.tracker.last_click_entity == Some(entity)
        && (now - cx.tracker.last_click_time) < DOUBLE_CLICK_THRESHOLD_SECONDS;

    cx.tracker.last_click_time = now;
    cx.tracker.last_click_entity = Some(entity);

    if !is_double_click {
        return;
    }

    if is_group {
        // Enter group editing
        let mut ctx = cx.edit_context.clone();
        ctx.enter(*element_id);
        cx.commands.insert_resource(ctx);
        cx.commands.entity(entity).remove::<Selected>();
    } else if is_csg {
        // Enter face editing mode on the CsgNode — operand faces are
        // surfaced via csg_face_hit_test in update_hovered_face.
        cx.face_edit_context.enter(entity, *element_id);
        cx.commands.entity(entity).remove::<Selected>();
    } else {
        // Enter face editing mode for non-group entities
        cx.face_edit_context.enter(entity, *element_id);
        cx.commands.entity(entity).remove::<Selected>();
    }
}

fn handle_group_escape(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    edit_context: Res<GroupEditContext>,
    face_edit_context: Res<FaceEditContext>,
    ownership: Res<InputOwnership>,
) {
    if !ownership.is_idle() || edit_context.is_root() || face_edit_context.is_active() {
        return;
    }

    if keys.just_pressed(KeyCode::Escape) {
        let mut ctx = edit_context.clone();
        ctx.exit();
        commands.insert_resource(ctx);
        // Deselect all when exiting group
        // (deselection happens naturally since the next click will re-evaluate)
    }
}

fn handle_delete_shortcut(
    keys: Res<ButtonInput<KeyCode>>,
    selected_query: Query<&ElementId, With<Selected>>,
    ownership: Res<InputOwnership>,
    mut delete_entities_commands: MessageWriter<DeleteEntitiesCommand>,
) {
    if !ownership.is_idle()
        || (!keys.just_pressed(KeyCode::Delete) && !keys.just_pressed(KeyCode::Backspace))
    {
        return;
    }

    let element_ids: Vec<ElementId> = selected_query.iter().copied().collect();
    if element_ids.is_empty() {
        return;
    }

    delete_entities_commands.write(DeleteEntitiesCommand { element_ids });
}

fn update_selection_status(
    selected_query: Query<(), With<Selected>>,
    edit_context: Res<GroupEditContext>,
    group_query: Query<(&ElementId, &GroupMembers)>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    let selection_count = selected_query.iter().count();
    let mut summary = match selection_count {
        0 => String::new(),
        1 => "1 element selected".to_string(),
        count => format!("{count} elements selected"),
    };
    if !edit_context.is_root() {
        let breadcrumb: Vec<&str> = edit_context
            .stack
            .iter()
            .filter_map(|id| {
                group_query
                    .iter()
                    .find(|(eid, _)| *eid == id)
                    .map(|(_, m)| m.name.as_str())
            })
            .collect();
        let breadcrumb = breadcrumb.join(" > ");
        if !breadcrumb.is_empty() {
            summary = if summary.is_empty() {
                format!("Editing: {breadcrumb}")
            } else {
                format!("{summary} | Editing: {breadcrumb}")
            };
        }
    }
    status_bar_data.selection_summary = summary;
}

fn draw_selected_outlines(
    world: &World,
    selected_query: Query<Entity, With<Selected>>,
    registry: Res<CapabilityRegistry>,
    transform_state: Res<TransformState>,
    mut gizmos: Gizmos,
) {
    let active_mode = transform_state.mode;
    let is_active_transform = !transform_state.is_idle();

    for entity in &selected_query {
        if !entity_is_visible(world, entity) {
            continue;
        }
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            continue;
        };
        let Some(factory) = registry.factory_for(snapshot.type_name()) else {
            continue;
        };

        if is_active_transform {
            match active_mode {
                TransformMode::Idle => {}
                TransformMode::Scaling => continue,
                TransformMode::Moving | TransformMode::Rotating => {
                    if snapshot.preview_transform().is_none() {
                        continue;
                    }
                }
            }
        }

        let color = if is_active_transform {
            ACTIVE_TRANSFORM_HIGHLIGHT_COLOR
        } else {
            SELECTION_HIGHLIGHT_COLOR
        };
        if let Some(bounds) = occurrence_generated_part_bounds(world, entity_ref) {
            draw_bounds_wireframe(&mut gizmos, &bounds, color);
        }
        factory.draw_selection(world, entity, &mut gizmos, color);
    }
}

fn occurrence_generated_part_bounds(
    world: &World,
    entity_ref: EntityRef<'_>,
) -> Option<EntityBounds> {
    if entity_ref.get::<OccurrenceIdentity>().is_none() {
        return None;
    }
    let owner = entity_ref.get::<ElementId>().copied()?;
    let mut query = world.try_query::<(
        &GeneratedOccurrencePart,
        &ProfileExtrusion,
        Option<&ShapeRotation>,
    )>()?;
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut any = false;

    for (part, extrusion, rotation) in query.iter(world) {
        if part.owner != owner {
            continue;
        }
        let rotation = rotation
            .map(|rotation| rotation.0)
            .unwrap_or(Quat::IDENTITY);
        if let Some(bounds) = extrusion.bounds(rotation) {
            min = min.min(bounds.min);
            max = max.max(bounds.max);
            any = true;
        }
    }

    any.then_some(EntityBounds { min, max })
}

fn draw_bounds_wireframe(gizmos: &mut Gizmos, bounds: &EntityBounds, color: Color) {
    let corners = bounds.corners();
    for i in 0..4 {
        gizmos.line(corners[i], corners[(i + 1) % 4], color);
        gizmos.line(corners[i + 4], corners[4 + (i + 1) % 4], color);
        gizmos.line(corners[i], corners[i + 4], color);
    }
}

fn entity_is_visible(world: &World, entity: Entity) -> bool {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return false;
    };
    if entity_ref.get::<Visibility>().copied() != Some(Visibility::Hidden) {
        return true;
    }

    entity_ref
        .get::<FeatureOperand>()
        .and_then(|operand| find_entity_by_element_id_ref(world, operand.owner))
        .and_then(|feature_entity| world.get_entity(feature_entity).ok())
        .and_then(|feature_ref| feature_ref.get::<Visibility>().copied())
        != Some(Visibility::Hidden)
}

fn find_entity_by_element_id_ref(world: &World, element_id: ElementId) -> Option<Entity> {
    let mut query = world.try_query::<(Entity, &ElementId)>().unwrap();
    query
        .iter(world)
        .find_map(|(entity, current_id)| (*current_id == element_id).then_some(entity))
}

fn choose_selection_hit(
    custom_hit: Option<HitCandidate>,
    mesh_hit: Option<HitCandidate>,
) -> Option<HitCandidate> {
    match (custom_hit, mesh_hit) {
        (Some(custom_hit), Some(mesh_hit)) => Some(if custom_hit.distance <= mesh_hit.distance {
            custom_hit
        } else {
            mesh_hit
        }),
        (Some(custom_hit), None) => Some(custom_hit),
        (None, Some(mesh_hit)) => Some(mesh_hit),
        (None, None) => None,
    }
}

fn selection_hit_entity(world: &mut World, ray: Ray3d) -> Option<Entity> {
    let custom_hit = world
        .resource::<CapabilityRegistry>()
        .factories()
        .iter()
        .filter_map(|factory| factory.hit_test(world, ray))
        .filter(|hit| entity_is_visible(world, hit.entity))
        .min_by(|left, right| left.distance.total_cmp(&right.distance));
    let mut system_state: bevy::ecs::system::SystemState<SelectionHitTest> =
        bevy::ecs::system::SystemState::new(world);
    let mut hit_test = system_state.get_mut(world);
    let mesh_hit = hit_test
        .ray_cast
        .cast_ray(
            ray,
            &MeshRayCastSettings::default().with_filter(&|entity| {
                hit_test.mesh_selectable_query.contains(entity)
                    && !hit_test.face_profile_feature_query.contains(entity)
                    && hit_test
                        .visibility_query
                        .get(entity)
                        .map_or(true, |visibility| *visibility != Visibility::Hidden)
            }),
        )
        .first()
        .map(|(entity, hit)| HitCandidate {
            entity: *entity,
            distance: hit.distance,
        });
    system_state.apply(world);

    choose_selection_hit(custom_hit, mesh_hit).map(|hit| hit.entity)
}

// --- Box Select ---

#[derive(SystemParam)]
struct BoxSelectContext<'w, 's> {
    commands: Commands<'w, 's>,
    mouse_buttons: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    window_query: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    camera_query: Query<'w, 's, (&'static Camera, &'static GlobalTransform)>,
    box_state: ResMut<'w, BoxSelectState>,
    selected_query: Query<'w, 's, Entity, With<Selected>>,
    ownership: Res<'w, InputOwnership>,
    egui_wants_input: Res<'w, EguiWantsInput>,
    handle_state: Res<'w, HandleInteractionState>,
    face_edit_context: Res<'w, FaceEditContext>,
    layer_registry: Res<'w, LayerRegistry>,
    light_object_visibility: Res<'w, SceneLightObjectVisibility>,
    entity_query: Query<'w, 's, BoxSelectEntityQueryItem>,
    edit_context: Res<'w, GroupEditContext>,
    group_query: Query<'w, 's, GroupMembershipQueryItem>,
}

fn handle_box_select(mut cx: BoxSelectContext) {
    cx.box_state.just_completed = false;

    if !cx.ownership.is_idle()
        || cx.egui_wants_input.pointer
        || cx.face_edit_context.is_active()
        || orbit_modifier_pressed(&cx.keys)
    {
        cx.box_state.drag_start = None;
        cx.box_state.is_dragging = false;
        return;
    }

    let Ok(window) = cx.window_query.single() else {
        return;
    };
    let Some(cursor_position) = window.cursor_position() else {
        return;
    };

    if cx.mouse_buttons.just_pressed(MouseButton::Left) && !cx.handle_state.captures_pointer() {
        cx.box_state.drag_start = Some(cursor_position);
        cx.box_state.is_dragging = false;
    }

    if let Some(start) = cx.box_state.drag_start {
        if cx.mouse_buttons.pressed(MouseButton::Left) {
            let distance = (cursor_position - start).length();
            if distance > BOX_SELECT_DRAG_THRESHOLD {
                cx.box_state.is_dragging = true;
            }
        }

        if cx.mouse_buttons.just_released(MouseButton::Left) && cx.box_state.is_dragging {
            let end = cursor_position;
            let is_window_select = end.x >= start.x;

            let Some((camera, camera_transform)) = cx.camera_query.iter().next() else {
                cx.box_state.drag_start = None;
                cx.box_state.is_dragging = false;
                return;
            };

            // Adjust window coordinates to viewport coordinates so they
            // match the coordinate space returned by world_to_viewport.
            let viewport_offset = camera
                .logical_viewport_rect()
                .map(|rect| rect.min)
                .unwrap_or(Vec2::ZERO);
            let rect_min = start.min(end) - viewport_offset;
            let rect_max = start.max(end) - viewport_offset;

            let additive =
                cx.keys.pressed(KeyCode::ShiftLeft) || cx.keys.pressed(KeyCode::ShiftRight);
            if !additive {
                for entity in &cx.selected_query {
                    cx.commands.entity(entity).remove::<Selected>();
                }
            }

            for (
                entity,
                element_id,
                global_transform,
                visibility,
                layer_assignment,
                has_group_members,
                aabb,
                is_scene_light,
                is_muted,
            ) in &cx.entity_query
            {
                if visibility.copied() == Some(Visibility::Hidden) {
                    continue;
                }

                if is_scene_light && !cx.light_object_visibility.visible {
                    continue;
                }

                if is_muted {
                    continue;
                }

                let layer_name = layer_assignment
                    .map(|a| a.layer.as_str())
                    .unwrap_or(DEFAULT_LAYER_NAME);
                if !cx.layer_registry.is_visible(layer_name)
                    || cx.layer_registry.is_locked(layer_name)
                {
                    continue;
                }

                if has_group_members {
                    continue;
                }

                let Some(aabb) = aabb else {
                    continue;
                };

                let bounds = aabb_to_world_bounds(aabb, global_transform);

                if entity_screen_bounds_match(
                    &bounds,
                    camera,
                    camera_transform,
                    rect_min,
                    rect_max,
                    is_window_select,
                ) {
                    let target = redirect_to_group_query(
                        entity,
                        *element_id,
                        &cx.edit_context,
                        &cx.group_query,
                    );
                    cx.commands.entity(target).insert(Selected);
                }
            }

            cx.box_state.drag_start = None;
            cx.box_state.is_dragging = false;
            cx.box_state.just_completed = true;
        } else if !cx.mouse_buttons.pressed(MouseButton::Left) {
            cx.box_state.drag_start = None;
            cx.box_state.is_dragging = false;
        }
    }
}

fn aabb_to_world_bounds(
    aabb: &bevy::camera::primitives::Aabb,
    transform: &GlobalTransform,
) -> EntityBounds {
    let center = Vec3::from(aabb.center);
    let half = Vec3::from(aabb.half_extents);
    let local_corners = [
        center + Vec3::new(-half.x, -half.y, -half.z),
        center + Vec3::new(-half.x, -half.y, half.z),
        center + Vec3::new(half.x, -half.y, half.z),
        center + Vec3::new(half.x, -half.y, -half.z),
        center + Vec3::new(-half.x, half.y, -half.z),
        center + Vec3::new(-half.x, half.y, half.z),
        center + Vec3::new(half.x, half.y, half.z),
        center + Vec3::new(half.x, half.y, -half.z),
    ];
    let mut world_min = Vec3::splat(f32::MAX);
    let mut world_max = Vec3::splat(f32::MIN);
    for corner in &local_corners {
        let world_corner = transform.transform_point(*corner);
        world_min = world_min.min(world_corner);
        world_max = world_max.max(world_corner);
    }
    EntityBounds {
        min: world_min,
        max: world_max,
    }
}

fn entity_screen_bounds_match(
    bounds: &EntityBounds,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    rect_min: Vec2,
    rect_max: Vec2,
    window_select: bool,
) -> bool {
    let corners = bounds.corners();
    let mut screen_min = Vec2::splat(f32::MAX);
    let mut screen_max = Vec2::splat(f32::MIN);
    let mut any_visible = false;

    for corner in &corners {
        if let Ok(screen_pos) = camera.world_to_viewport(camera_transform, *corner) {
            screen_min = screen_min.min(screen_pos);
            screen_max = screen_max.max(screen_pos);
            any_visible = true;
        }
    }

    if !any_visible {
        return false;
    }

    if window_select {
        // Window select: entity must be fully inside
        screen_min.x >= rect_min.x
            && screen_min.y >= rect_min.y
            && screen_max.x <= rect_max.x
            && screen_max.y <= rect_max.y
    } else {
        // Crossing select: entity must overlap
        screen_min.x <= rect_max.x
            && screen_max.x >= rect_min.x
            && screen_min.y <= rect_max.y
            && screen_max.y >= rect_min.y
    }
}

fn redirect_to_group_query(
    entity: Entity,
    element_id: ElementId,
    edit_context: &GroupEditContext,
    group_query: &Query<(Entity, &ElementId, &GroupMembers)>,
) -> Entity {
    // Find the owning group whose parent context matches the active editing context
    for (group_entity, group_eid, members) in group_query.iter() {
        if members.member_ids.contains(&element_id) {
            if edit_context.is_root() {
                // At root: redirect to the top-level group
                if !is_member_of_any_group_query(*group_eid, group_query) {
                    return group_entity;
                }
            } else if edit_context.current_group() == Some(*group_eid) {
                // Inside a group: entity is a direct child, no redirect needed
                return entity;
            } else {
                // Entity belongs to a sub-group of the active context; redirect to that sub-group
                return group_entity;
            }
        }
    }
    entity
}

fn is_member_of_any_group_query(
    element_id: ElementId,
    group_query: &Query<(Entity, &ElementId, &GroupMembers)>,
) -> bool {
    group_query
        .iter()
        .any(|(_, _, members)| members.member_ids.contains(&element_id))
}

/// Redirect a hit entity to its owning group if appropriate for the current editing context.
fn redirect_hit_to_context_group(
    world: &World,
    entity: Entity,
    element_id: ElementId,
    edit_context: &GroupEditContext,
) -> Entity {
    if let Some(active_group_id) = edit_context.current_group() {
        // Inside a group: if entity is a direct child, keep it; if it belongs to a sub-group, redirect
        if is_direct_child_of_group(world, element_id, active_group_id) {
            // Check if this child is itself a group — if so, it's already the right target
            return entity;
        }
        // Check if it's a descendant of a direct child sub-group
        let mut q = world.try_query::<EntityRef>().unwrap();
        for entity_ref in q.iter(world) {
            let Some(group_eid) = entity_ref.get::<ElementId>() else {
                continue;
            };
            if entity_ref.get::<GroupMembers>().is_none() {
                continue;
            }
            if is_direct_child_of_group(world, *group_eid, active_group_id) {
                // This is a sub-group that's a direct child of the active group
                let sub_members = collect_group_members_recursive(world, *group_eid);
                if sub_members.contains(&element_id) {
                    return entity_ref.id();
                }
            }
        }
        entity
    } else {
        // Root level: redirect to top-level group
        if let Some(group_id) = find_top_level_group_for(world, element_id) {
            let mut q = world.try_query::<EntityRef>().unwrap();
            if let Some(group_entity) = q
                .iter(world)
                .find(|e| e.get::<ElementId>().copied() == Some(group_id))
            {
                return group_entity.id();
            }
        }
        entity
    }
}

/// Check if an element is a direct child of the given group.
fn is_direct_child_of_group(world: &World, element_id: ElementId, group_id: ElementId) -> bool {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world).any(|e| {
        e.get::<ElementId>().copied() == Some(group_id)
            && e.get::<GroupMembers>()
                .is_some_and(|m| m.member_ids.contains(&element_id))
    })
}

/// Find the top-level group that (transitively) owns a given element.
///
/// Defends against cycles in the group-membership graph (a group whose
/// member chain eventually points back at itself). Cycles are not
/// supposed to exist, but a malformed import or a bug in group editing
/// could produce one, and the loop must not hang the app.
fn find_top_level_group_for(world: &World, element_id: ElementId) -> Option<ElementId> {
    use std::collections::HashSet;

    let mut current = element_id;
    let mut visited: HashSet<ElementId> = HashSet::new();
    visited.insert(current);
    loop {
        match find_group_for_member(world, current) {
            Some(parent_id) => {
                if !visited.insert(parent_id) {
                    // Cycle detected — bail out at the deepest unique
                    // ancestor we reached. Returning the last `current`
                    // matches the documented contract (a top-level
                    // group, even if the graph is malformed).
                    if current == element_id {
                        return None;
                    }
                    return Some(current);
                }
                current = parent_id;
            }
            None => {
                // current is not a member of any group
                if current == element_id {
                    return None; // The original element is top-level, no group
                }
                return Some(current);
            }
        }
    }
}

fn draw_box_select_rect(
    box_state: Res<BoxSelectState>,
    window_query: Query<&Window, With<PrimaryWindow>>,
    camera_query: Query<(&Camera, &GlobalTransform)>,
    mut gizmos: Gizmos,
) {
    if !box_state.is_dragging {
        return;
    }
    let Some(start) = box_state.drag_start else {
        return;
    };
    let Ok(window) = window_query.single() else {
        return;
    };
    let Some(cursor_position) = window.cursor_position() else {
        return;
    };
    let Some((camera, camera_transform)) = camera_query.iter().next() else {
        return;
    };

    let end = cursor_position;
    let is_window = end.x >= start.x;
    let border_color = if is_window {
        BOX_SELECT_WINDOW_BORDER
    } else {
        BOX_SELECT_CROSSING_BORDER
    };
    let _fill_color = if is_window {
        BOX_SELECT_WINDOW_COLOR
    } else {
        BOX_SELECT_CROSSING_COLOR
    };

    // Draw rectangle border as 4 lines in world space on the near plane
    let corners_2d = [
        Vec2::new(start.x, start.y),
        Vec2::new(end.x, start.y),
        Vec2::new(end.x, end.y),
        Vec2::new(start.x, end.y),
    ];

    let near_distance = 0.5;
    let world_corners: Vec<Vec3> = corners_2d
        .iter()
        .filter_map(|screen_pos| {
            let viewport_pos = match camera.logical_viewport_rect() {
                Some(rect) => *screen_pos - rect.min,
                None => *screen_pos,
            };
            camera
                .viewport_to_world(camera_transform, viewport_pos)
                .ok()
                .map(|ray| ray.origin + ray.direction * near_distance)
        })
        .collect();

    if world_corners.len() == 4 {
        for i in 0..4 {
            gizmos.line(world_corners[i], world_corners[(i + 1) % 4], border_color);
        }
    }
}

// --- Group edit muting ---

#[derive(Resource, Default)]
struct PreviousGroupEditContext {
    stack: Vec<ElementId>,
}

const MUTED_ALPHA: f32 = 0.15;

fn update_group_edit_muting(
    mut commands: Commands,
    edit_context: Res<GroupEditContext>,
    mut previous: ResMut<PreviousGroupEditContext>,
    entity_query: Query<(Entity, &ElementId, Has<GroupEditMuted>)>,
    group_query: Query<(&ElementId, &GroupMembers)>,
    material_query: Query<(Entity, &ElementId, &MeshMaterial3d<StandardMaterial>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if edit_context.stack == previous.stack {
        return;
    }
    previous.stack = edit_context.stack.clone();

    if edit_context.is_root() {
        for (entity, _, is_muted) in &entity_query {
            if is_muted {
                if let Ok(mut ec) = commands.get_entity(entity) {
                    ec.remove::<GroupEditMuted>();
                }
            }
        }
        for (_, _, mat_handle) in &material_query {
            if let Some(mat) = materials.get_mut(mat_handle) {
                if mat.base_color.alpha() < 1.0 {
                    mat.base_color.set_alpha(1.0);
                    mat.alpha_mode = AlphaMode::Opaque;
                }
            }
        }
    } else {
        let active_group_id = edit_context.current_group().unwrap();
        let active_members = collect_members_recursive(&group_query, active_group_id);

        for (entity, element_id, is_muted) in &entity_query {
            let is_active_member =
                active_members.contains(element_id) || *element_id == active_group_id;

            if is_active_member && is_muted {
                if let Ok(mut ec) = commands.get_entity(entity) {
                    ec.remove::<GroupEditMuted>();
                }
            } else if !is_active_member && !is_muted {
                if let Ok(mut ec) = commands.get_entity(entity) {
                    ec.insert(GroupEditMuted);
                }
            }
        }

        for (_, element_id, mat_handle) in &material_query {
            let is_muted = !active_members.contains(element_id) && *element_id != active_group_id;

            if let Some(mat) = materials.get_mut(mat_handle) {
                if is_muted {
                    mat.base_color.set_alpha(MUTED_ALPHA);
                    mat.alpha_mode = AlphaMode::Blend;
                } else {
                    mat.base_color.set_alpha(1.0);
                    mat.alpha_mode = AlphaMode::Opaque;
                }
            }
        }
    }
}

fn collect_members_recursive(
    group_query: &Query<(&ElementId, &GroupMembers)>,
    group_id: ElementId,
) -> Vec<ElementId> {
    let mut result = Vec::new();
    let mut stack = vec![group_id];
    while let Some(id) = stack.pop() {
        for (eid, members) in group_query.iter() {
            if *eid == id {
                for member_id in &members.member_ids {
                    result.push(*member_id);
                    stack.push(*member_id);
                }
            }
        }
    }
    result
}
