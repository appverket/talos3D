use std::any::Any;

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, read_only_property_field,
        vec3_from_json, AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, HandleKind,
        PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityDescriptor, CapabilityDistribution, CapabilityMaturity,
        CapabilityRegistryAppExt, HitCandidate, ModelSummaryAccumulator, SnapPoint,
    },
    plugins::{
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        commands::{despawn_by_element_id, find_entity_by_element_id, CreateEntityCommand},
        cursor::CursorWorldPos,
        document_properties::DocumentProperties,
        egui_chrome::EguiWantsInput,
        identity::{ElementId, ElementIdAllocator},
        snap::SnapKind,
        toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
        tools::ActiveTool,
        ui::StatusBarData,
    },
};

const DIMENSION_LINE_COLOR: Color = Color::srgba(0.95, 0.92, 0.42, 0.9);
const DIMENSION_LINE_SELECTED_COLOR: Color = Color::srgba(1.0, 0.98, 0.66, 1.0);
const DIMENSION_LINE_HIT_RADIUS: f32 = 0.18;
const DIMENSION_LINE_TICK_HALF: f32 = 0.08;
const DIMENSION_LINE_LABEL_OFFSET: f32 = 0.08;
const DIMENSION_LINE_MIN_MEASURE_LENGTH: f32 = 0.01;
const DIMENSION_LINE_MIN_OFFSET_LENGTH: f32 = 0.05;
const DIMENSION_LINE_DEFAULT_OFFSET: f32 = 0.75;

pub struct DimensionLinePlugin;

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct DimensionLineNode {
    pub start: Vec3,
    pub end: Vec3,
    pub offset: Vec3,
    pub visible: bool,
    pub label: Option<String>,
}

impl Default for DimensionLineNode {
    fn default() -> Self {
        Self {
            start: Vec3::ZERO,
            end: Vec3::X,
            offset: Vec3::Z * DIMENSION_LINE_DEFAULT_OFFSET,
            visible: true,
            label: None,
        }
    }
}

#[derive(Resource, Default)]
struct DimensionLineToolState {
    start: Option<Vec3>,
    end: Option<Vec3>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DimensionLineSnapshot {
    pub element_id: ElementId,
    pub start: Vec3,
    pub end: Vec3,
    pub offset: Vec3,
    pub visible: bool,
    pub label: Option<String>,
}

impl DimensionLineSnapshot {
    fn axis(&self) -> Vec3 {
        self.end - self.start
    }

    fn axis_direction(&self) -> Vec3 {
        self.axis().try_normalize().unwrap_or(Vec3::X)
    }

    fn measured_length(&self) -> f32 {
        self.axis().length()
    }

    fn midpoint(&self) -> Vec3 {
        (self.start + self.end) * 0.5
    }

    fn line_points(&self) -> (Vec3, Vec3) {
        (self.start + self.offset, self.end + self.offset)
    }

    fn label_anchor(&self) -> Vec3 {
        let (line_start, line_end) = self.line_points();
        let midpoint = (line_start + line_end) * 0.5;
        midpoint
            + label_offset_direction(self.axis_direction(), self.offset)
                * DIMENSION_LINE_LABEL_OFFSET
    }

    fn display_text(&self, doc_props: &DocumentProperties) -> String {
        let value = doc_props
            .display_unit
            .format_value(self.measured_length(), doc_props.precision);
        match self.label.as_deref() {
            Some(label) if !label.is_empty() => format!("{label}: {value}"),
            _ => value,
        }
    }

    fn display_label(&self) -> String {
        match self.label.as_deref() {
            Some(label) if !label.is_empty() => format!("Dimension {label}"),
            _ => "Dimension".to_string(),
        }
    }

    fn entity_bounds(&self) -> EntityBounds {
        let (line_start, line_end) = self.line_points();
        let label_anchor = self.label_anchor();
        bounds_from_points(&[self.start, self.end, line_start, line_end, label_anchor])
    }
}

impl AuthoredEntity for DimensionLineSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "dimension_line"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.display_label()
    }

    fn center(&self) -> Vec3 {
        self.midpoint() + self.offset
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.start += delta;
        snapshot.end += delta;
        snapshot.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.start = rotation * snapshot.start;
        snapshot.end = rotation * snapshot.end;
        snapshot.offset = rotation * snapshot.offset;
        snapshot.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.start = center + (snapshot.start - center) * factor;
        snapshot.end = center + (snapshot.end - center) * factor;
        snapshot.offset *= factor;
        snapshot.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let mut fields = vec![
            property_field(
                "start",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.start)),
            ),
            property_field(
                "end",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.end)),
            ),
            property_field(
                "offset",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.offset)),
            ),
            property_field(
                "visible",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.visible.to_string())),
            ),
            read_only_property_field(
                "length",
                "Length",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.measured_length())),
            ),
        ];
        if let Some(label) = &self.label {
            fields.push(property_field_with(
                "label",
                "Label",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(label.clone())),
                true,
            ));
        }
        fields
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "start" => snapshot.start = vec3_from_json(value)?,
            "end" => snapshot.end = vec3_from_json(value)?,
            "offset" => snapshot.offset = vec3_from_json(value)?,
            "visible" => {
                snapshot.visible = match value.as_bool() {
                    Some(value) => value,
                    None => match value.as_str() {
                        Some("true") | Some("1") => true,
                        Some("false") | Some("0") => false,
                        _ => return Err("visible must be true or false".to_string()),
                    },
                };
            }
            "label" => {
                snapshot.label = value
                    .as_str()
                    .map(|text| {
                        if text.is_empty() {
                            None
                        } else {
                            Some(text.to_string())
                        }
                    })
                    .unwrap_or(None);
            }
            _ => {
                return Err(invalid_property_error(
                    "dimension_line",
                    &["start", "end", "offset", "visible", "label"],
                ));
            }
        }
        validate_dimension_geometry(snapshot.start, snapshot.end, snapshot.offset)?;
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![
            HandleInfo {
                id: "start".to_string(),
                position: self.start,
                kind: HandleKind::Vertex,
                label: "Start".to_string(),
            },
            HandleInfo {
                id: "end".to_string(),
                position: self.end,
                kind: HandleKind::Vertex,
                label: "End".to_string(),
            },
            HandleInfo {
                id: "offset".to_string(),
                position: self.midpoint() + self.offset,
                kind: HandleKind::Parameter,
                label: "Offset".to_string(),
            },
        ]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        Some(self.entity_bounds())
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        let mut snapshot = self.clone();
        match handle_id {
            "start" => snapshot.start = cursor,
            "end" => snapshot.end = cursor,
            "offset" => {
                snapshot.offset = offset_from_line_point(snapshot.start, snapshot.end, cursor)
            }
            _ => return None,
        }
        validate_dimension_geometry(snapshot.start, snapshot.end, snapshot.offset).ok()?;
        Some(snapshot.into())
    }

    fn to_json(&self) -> Value {
        json!({
            "element_id": self.element_id,
            "start": self.start.to_array(),
            "end": self.end.to_array(),
            "offset": self.offset.to_array(),
            "visible": self.visible,
            "label": self.label,
            "length": self.measured_length(),
        })
    }

    fn apply_to(&self, world: &mut World) {
        let node = DimensionLineNode {
            start: self.start,
            end: self.end,
            offset: self.offset,
            visible: self.visible,
            label: self.label.clone(),
        };
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert(node);
        } else {
            world.spawn((self.element_id, node));
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        draw_dimension_line_gizmo(gizmos, self.start, self.end, self.offset, color);
    }

    fn preview_line_count(&self) -> usize {
        5
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == "dimension_line" && other.to_json() == self.to_json()
    }
}

impl From<DimensionLineSnapshot> for BoxedEntity {
    fn from(snapshot: DimensionLineSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

pub struct DimensionLineFactory;

impl AuthoredEntityFactory for DimensionLineFactory {
    fn type_name(&self) -> &'static str {
        "dimension_line"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let node = entity_ref.get::<DimensionLineNode>()?;
        Some(
            DimensionLineSnapshot {
                element_id,
                start: node.start,
                end: node.end,
                offset: node.offset,
                visible: node.visible,
                label: node.label.clone(),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id: ElementId = serde_json::from_value(
            data.get("element_id")
                .cloned()
                .ok_or_else(|| "Missing element_id".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let start = data
            .get("start")
            .map(vec3_from_json)
            .transpose()?
            .ok_or_else(|| "Missing start".to_string())?;
        let end = data
            .get("end")
            .map(vec3_from_json)
            .transpose()?
            .ok_or_else(|| "Missing end".to_string())?;
        let offset = data
            .get("offset")
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or_else(|| default_dimension_offset(start, end));
        let visible = data.get("visible").and_then(Value::as_bool).unwrap_or(true);
        let label = data.get("label").and_then(Value::as_str).map(String::from);
        validate_dimension_geometry(start, end, offset)?;
        Ok(DimensionLineSnapshot {
            element_id,
            start,
            end,
            offset,
            visible,
            label,
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let element_id = world
            .get_resource::<ElementIdAllocator>()
            .ok_or_else(|| "ElementIdAllocator not available".to_string())?
            .next_id();
        let start = object
            .get("start")
            .or_else(|| object.get("from"))
            .map(vec3_from_json)
            .transpose()?
            .ok_or_else(|| "dimension_line requires start".to_string())?;
        let end = object
            .get("end")
            .or_else(|| object.get("to"))
            .map(vec3_from_json)
            .transpose()?
            .ok_or_else(|| "dimension_line requires end".to_string())?;
        let offset = if let Some(offset) = object.get("offset").filter(|value| !value.is_null()) {
            vec3_from_json(offset)?
        } else if let Some(line_point) = object
            .get("line_point")
            .filter(|value| !value.is_null())
            .or_else(|| object.get("placement_point"))
            .filter(|value| !value.is_null())
            .or_else(|| object.get("offset_point"))
            .filter(|value| !value.is_null())
        {
            offset_from_line_point(start, end, vec3_from_json(line_point)?)
        } else {
            default_dimension_offset(start, end)
        };
        let visible = object
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let label = object
            .get("label")
            .or_else(|| object.get("name"))
            .and_then(Value::as_str)
            .map(String::from);

        validate_dimension_geometry(start, end, offset)?;
        Ok(DimensionLineSnapshot {
            element_id,
            start,
            end,
            offset,
            visible,
            label,
        }
        .into())
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let mut query = world.try_query::<(Entity, &DimensionLineNode)>()?;
        query
            .iter(world)
            .filter(|(_, node)| node.visible)
            .filter_map(|(entity, node)| {
                let segments = dimension_segments(node.start, node.end, node.offset);
                let best_distance = segments
                    .into_iter()
                    .filter_map(|(start, end)| {
                        ray_segment_distance(ray.origin, ray.direction.into(), start, end)
                    })
                    .min_by(|left, right| left.total_cmp(right))?;
                if best_distance > DIMENSION_LINE_HIT_RADIUS {
                    return None;
                }
                let distance = (node.start + node.end).length() * 0.5;
                Some(HitCandidate { entity, distance })
            })
            .min_by(|left, right| left.distance.total_cmp(&right.distance))
    }

    fn collect_snap_points(&self, world: &World, out: &mut Vec<SnapPoint>) {
        let Some(mut query) = world.try_query::<&DimensionLineNode>() else {
            return;
        };
        for node in query.iter(world) {
            if !node.visible {
                continue;
            }
            out.push(SnapPoint {
                position: node.start,
                kind: SnapKind::Endpoint,
            });
            out.push(SnapPoint {
                position: node.end,
                kind: SnapKind::Endpoint,
            });
            out.push(SnapPoint {
                position: (node.start + node.end) * 0.5 + node.offset,
                kind: SnapKind::Control,
            });
        }
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let Some(mut query) = world.try_query::<&DimensionLineNode>() else {
            return;
        };
        for _ in query.iter(world) {
            *summary
                .entity_counts
                .entry("dimension_line".to_string())
                .or_default() += 1;
        }
    }
}

fn draw_dimension_line_gizmo(
    gizmos: &mut Gizmos,
    start: Vec3,
    end: Vec3,
    offset: Vec3,
    color: Color,
) {
    let (line_start, line_end) = (start + offset, end + offset);
    gizmos.line(start, line_start, color);
    gizmos.line(end, line_end, color);
    gizmos.line(line_start, line_end, color);

    let tick_dir = tick_direction(end - start, offset);
    gizmos.line(
        line_start - tick_dir * DIMENSION_LINE_TICK_HALF,
        line_start + tick_dir * DIMENSION_LINE_TICK_HALF,
        color,
    );
    gizmos.line(
        line_end - tick_dir * DIMENSION_LINE_TICK_HALF,
        line_end + tick_dir * DIMENSION_LINE_TICK_HALF,
        color,
    );
}

fn draw_dimension_line_visuals(
    dimensions: Query<(Entity, &DimensionLineNode), Without<crate::plugins::selection::Selected>>,
    selected_dimensions: Query<
        (Entity, &DimensionLineNode),
        With<crate::plugins::selection::Selected>,
    >,
    mut gizmos: Gizmos,
) {
    for (_entity, node) in &dimensions {
        if !node.visible {
            continue;
        }
        draw_dimension_line_gizmo(
            &mut gizmos,
            node.start,
            node.end,
            node.offset,
            DIMENSION_LINE_COLOR,
        );
    }

    for (_entity, node) in &selected_dimensions {
        if !node.visible {
            continue;
        }
        draw_dimension_line_gizmo(
            &mut gizmos,
            node.start,
            node.end,
            node.offset,
            DIMENSION_LINE_SELECTED_COLOR,
        );
    }
}

fn draw_dimension_line_labels(
    mut contexts: EguiContexts,
    doc_props: Res<DocumentProperties>,
    camera_query: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    window_query: Query<&Window, With<PrimaryWindow>>,
    dimensions: Query<(
        &DimensionLineNode,
        Option<&crate::plugins::selection::Selected>,
    )>,
) {
    let Ok(ctx_ref) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx_ref.clone();
    let Ok((camera, camera_transform)) = camera_query.single() else {
        return;
    };
    let Ok(window) = window_query.single() else {
        return;
    };

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("dimension_line_labels"),
    ));

    for (node, selected) in &dimensions {
        if !node.visible {
            continue;
        }
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(u64::MAX),
            start: node.start,
            end: node.end,
            offset: node.offset,
            visible: node.visible,
            label: node.label.clone(),
        };
        if snapshot.measured_length() < DIMENSION_LINE_MIN_MEASURE_LENGTH {
            continue;
        }
        let Ok(screen_pos) = camera.world_to_viewport(camera_transform, snapshot.label_anchor())
        else {
            continue;
        };
        if screen_pos.x < 0.0
            || screen_pos.y < 0.0
            || screen_pos.x > window.width()
            || screen_pos.y > window.height()
        {
            continue;
        }

        let text = snapshot.display_text(&doc_props);
        let pos = egui::pos2(screen_pos.x, screen_pos.y);
        let rect =
            egui::Rect::from_center_size(pos, egui::vec2(text.len() as f32 * 7.5 + 14.0, 22.0));
        let selected = selected.is_some();
        let background = if selected {
            egui::Color32::from_rgba_unmultiplied(64, 60, 24, 220)
        } else {
            egui::Color32::from_rgba_unmultiplied(26, 26, 26, 200)
        };
        let border = if selected {
            egui::Color32::from_rgb(255, 232, 120)
        } else {
            egui::Color32::from_rgb(222, 210, 110)
        };
        let foreground = if selected {
            egui::Color32::from_rgb(255, 246, 190)
        } else {
            egui::Color32::from_rgb(245, 236, 180)
        };

        painter.rect_filled(rect, 4.0, background);
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Outside,
        );
        painter.text(
            pos,
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(13.0),
            foreground,
        );
    }
}

fn execute_place_dimension_line(
    world: &mut World,
    _params: &Value,
) -> Result<CommandResult, String> {
    world
        .resource_mut::<NextState<ActiveTool>>()
        .set(ActiveTool::PlaceDimensionLine);
    Ok(CommandResult::empty())
}

fn initialize_dimension_line_tool(mut commands: Commands) {
    commands.insert_resource(DimensionLineToolState::default());
}

fn cleanup_dimension_line_tool(mut commands: Commands) {
    commands.remove_resource::<DimensionLineToolState>();
}

fn cancel_dimension_line_tool(
    keys: Res<ButtonInput<KeyCode>>,
    egui_wants_input: Res<EguiWantsInput>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if egui_wants_input.keyboard || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    next_active_tool.set(ActiveTool::Select);
    status_bar_data.hint.clear();
}

fn handle_dimension_line_clicks(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    egui_wants_input: Res<EguiWantsInput>,
    cursor_world_pos: Res<CursorWorldPos>,
    mut tool_state: ResMut<DimensionLineToolState>,
    allocator: Res<ElementIdAllocator>,
    mut create_entity: MessageWriter<CreateEntityCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if egui_wants_input.pointer || !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match (tool_state.start, tool_state.end) {
        (None, _) => {
            tool_state.start = Some(cursor_position);
            status_bar_data.hint = "Click second witness point".to_string();
        }
        (Some(start), None) => {
            if start.distance(cursor_position) < DIMENSION_LINE_MIN_MEASURE_LENGTH {
                return;
            }
            tool_state.end = Some(cursor_position);
            status_bar_data.hint = "Click to place dimension line offset".to_string();
        }
        (Some(start), Some(end)) => {
            let offset = offset_from_line_point(start, end, cursor_position);
            if offset.length() < DIMENSION_LINE_MIN_OFFSET_LENGTH {
                return;
            }

            let snapshot = DimensionLineSnapshot {
                element_id: allocator.next_id(),
                start,
                end,
                offset,
                visible: true,
                label: None,
            };
            create_entity.write(CreateEntityCommand {
                snapshot: snapshot.into(),
            });
            tool_state.start = None;
            tool_state.end = None;
            next_active_tool.set(ActiveTool::Select);
            status_bar_data.hint.clear();
        }
    }
}

fn draw_dimension_line_tool_preview(
    cursor_world_pos: Res<CursorWorldPos>,
    tool_state: Option<Res<DimensionLineToolState>>,
    mut gizmos: Gizmos,
) {
    let Some(tool_state) = tool_state else {
        return;
    };
    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };

    match (tool_state.start, tool_state.end) {
        (Some(start), None) => {
            gizmos.line(start, cursor_position, DIMENSION_LINE_SELECTED_COLOR);
        }
        (Some(start), Some(end)) => {
            let offset = offset_from_line_point(start, end, cursor_position);
            if offset.length() < DIMENSION_LINE_MIN_OFFSET_LENGTH {
                return;
            }
            draw_dimension_line_gizmo(
                &mut gizmos,
                start,
                end,
                offset,
                DIMENSION_LINE_SELECTED_COLOR,
            );
        }
        _ => {}
    }
}

impl Plugin for DimensionLinePlugin {
    fn build(&self, app: &mut App) {
        app.register_authored_entity_factory(DimensionLineFactory)
            .register_capability(CapabilityDescriptor {
                id: "dimensions".to_string(),
                name: "Dimensions".to_string(),
                version: 1,
                api_version: crate::capability_registry::CAPABILITY_API_VERSION,
                description: Some(
                    "Authored dimension annotations with witness points, offset placement, and measured labels."
                        .to_string(),
                ),
                dependencies: vec![],
                optional_dependencies: vec![],
                conflicts: vec![],
                maturity: CapabilityMaturity::Stable,
                distribution: CapabilityDistribution::Bundled,
                license: None,
                repository: None,
            })
            .register_command(
                CommandDescriptor {
                    id: "dimensions.place".to_string(),
                    label: "Dimension".to_string(),
                    description: "Activate dimension line placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: None,
                    default_shortcut: Some("D".to_string()),
                    icon: None,
                    hint: Some(
                        "Click first witness point, click second witness point, then click to place the dimension line."
                            .to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlaceDimensionLine".to_string()),
                    capability_id: Some("dimensions".to_string()),
                },
                execute_place_dimension_line,
            )
            .register_toolbar(ToolbarDescriptor {
                id: "dimensions".to_string(),
                label: "Dimensions".to_string(),
                default_dock: ToolbarDock::Left,
                default_visible: true,
                sections: vec![ToolbarSection {
                    label: "Annotate".to_string(),
                    command_ids: vec!["dimensions.place".to_string()],
                }],
            })
            .add_systems(
                OnEnter(ActiveTool::PlaceDimensionLine),
                initialize_dimension_line_tool,
            )
            .add_systems(
                OnExit(ActiveTool::PlaceDimensionLine),
                cleanup_dimension_line_tool,
            )
            .add_systems(
                Update,
                (
                    cancel_dimension_line_tool,
                    handle_dimension_line_clicks,
                    draw_dimension_line_tool_preview,
                    draw_dimension_line_visuals,
                )
                    .run_if(in_state(ActiveTool::PlaceDimensionLine)),
            )
            .add_systems(
                Update,
                (
                    draw_dimension_line_visuals
                        .run_if(not(in_state(ActiveTool::PlaceDimensionLine))),
                    draw_dimension_line_labels,
                ),
            );
    }
}

fn validate_dimension_geometry(start: Vec3, end: Vec3, offset: Vec3) -> Result<(), String> {
    if start.distance(end) < DIMENSION_LINE_MIN_MEASURE_LENGTH {
        return Err("Dimension start and end points must be distinct".to_string());
    }
    if offset.length() < DIMENSION_LINE_MIN_OFFSET_LENGTH {
        return Err("Dimension offset must move the line away from the measured axis".to_string());
    }
    Ok(())
}

fn offset_from_line_point(start: Vec3, end: Vec3, line_point: Vec3) -> Vec3 {
    let axis = end - start;
    let Some(axis_dir) = axis.try_normalize() else {
        return Vec3::ZERO;
    };
    let projected = axis_dir * (line_point - start).dot(axis_dir);
    line_point - (start + projected)
}

fn default_dimension_offset(start: Vec3, end: Vec3) -> Vec3 {
    let axis = end - start;
    let Some(axis_dir) = axis.try_normalize() else {
        return Vec3::Z * DIMENSION_LINE_DEFAULT_OFFSET;
    };

    for candidate in [Vec3::Y, Vec3::Z, Vec3::X] {
        let offset = orthogonal_component(candidate * DIMENSION_LINE_DEFAULT_OFFSET, axis_dir);
        if offset.length() >= DIMENSION_LINE_MIN_OFFSET_LENGTH {
            return offset;
        }
    }

    Vec3::Z * DIMENSION_LINE_DEFAULT_OFFSET
}

fn orthogonal_component(vector: Vec3, axis_dir: Vec3) -> Vec3 {
    vector - axis_dir * vector.dot(axis_dir)
}

fn tick_direction(axis: Vec3, offset: Vec3) -> Vec3 {
    let axis_dir = axis.try_normalize().unwrap_or(Vec3::X);
    let offset_dir = if offset.length_squared() > f32::EPSILON {
        offset.normalize()
    } else {
        perpendicular_pair(axis_dir).0
    };
    (axis_dir + offset_dir)
        .try_normalize()
        .unwrap_or(offset_dir)
}

fn label_offset_direction(axis_dir: Vec3, offset: Vec3) -> Vec3 {
    if offset.length_squared() > f32::EPSILON {
        offset.normalize()
    } else {
        perpendicular_pair(axis_dir).0
    }
}

fn perpendicular_pair(dir: Vec3) -> (Vec3, Vec3) {
    let candidate = if dir.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let perp1 = dir.cross(candidate).normalize_or_zero();
    let perp2 = dir.cross(perp1).normalize_or_zero();
    (perp1, perp2)
}

fn ray_segment_distance(
    ray_origin: Vec3,
    ray_dir: Vec3,
    seg_start: Vec3,
    seg_end: Vec3,
) -> Option<f32> {
    let d = seg_end - seg_start;
    let w = ray_origin - seg_start;
    let a = ray_dir.dot(ray_dir);
    let b = ray_dir.dot(d);
    let c = d.dot(d);
    let denom = a * c - b * b;
    if denom.abs() < 1e-10 {
        return None;
    }
    let e_val = ray_dir.dot(w);
    let f_val = d.dot(w);
    let mut s = (b * f_val - c * e_val) / denom;
    let mut t = (a * f_val - b * e_val) / denom;
    s = s.max(0.0);
    t = t.clamp(0.0, 1.0);
    let closest_on_ray = ray_origin + ray_dir * s;
    let closest_on_seg = seg_start + d * t;
    Some(closest_on_ray.distance(closest_on_seg))
}

fn dimension_segments(start: Vec3, end: Vec3, offset: Vec3) -> [(Vec3, Vec3); 3] {
    let line_start = start + offset;
    let line_end = end + offset;
    [(start, line_start), (end, line_end), (line_start, line_end)]
}

fn bounds_from_points(points: &[Vec3]) -> EntityBounds {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for point in points {
        min = min.min(*point);
        max = max.max(*point);
    }
    EntityBounds { min, max }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::AuthoredEntityFactory;

    #[test]
    fn create_request_projects_line_point_to_orthogonal_offset() {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());

        let snapshot = DimensionLineFactory
            .from_create_request(
                &world,
                &json!({
                    "type": "dimension_line",
                    "start": [0.0, 0.0, 0.0],
                    "end": [2.0, 0.0, 0.0],
                    "line_point": [0.5, 0.0, 1.25],
                    "label": "Width"
                }),
            )
            .expect("dimension request should parse");

        let snapshot = snapshot
            .0
            .as_any()
            .downcast_ref::<DimensionLineSnapshot>()
            .expect("snapshot should downcast");
        assert_eq!(snapshot.start, Vec3::ZERO);
        assert_eq!(snapshot.end, Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(snapshot.offset, Vec3::new(0.0, 0.0, 1.25));
        assert_eq!(snapshot.label.as_deref(), Some("Width"));
    }

    #[test]
    fn measured_length_uses_document_display_units() {
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(7),
            start: Vec3::ZERO,
            end: Vec3::new(2.5, 0.0, 0.0),
            offset: Vec3::Z,
            visible: true,
            label: Some("Overall".to_string()),
        };
        let doc_props = DocumentProperties::default();

        assert_eq!(snapshot.display_text(&doc_props), "Overall: 2500mm");
    }
}
