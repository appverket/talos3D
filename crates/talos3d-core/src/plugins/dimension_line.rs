use std::any::Any;

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, read_only_property_field,
        scalar_from_json, vec3_from_json, AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo,
        HandleKind, PropertyFieldDef, PropertyValue, PropertyValueKind,
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
        render_pipeline::RenderSettings,
        snap::SnapKind,
        toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
        tools::ActiveTool,
        ui::StatusBarData,
        units::DisplayUnit,
    },
};

const DIMENSION_LINE_COLOR: Color = Color::srgba(0.95, 0.92, 0.42, 0.9);
const DIMENSION_LINE_SELECTED_COLOR: Color = Color::srgba(1.0, 0.98, 0.66, 1.0);
const DIMENSION_LINE_PAPER_COLOR: Color = Color::srgba(0.15, 0.15, 0.18, 0.95);
const DIMENSION_LINE_PAPER_SELECTED_COLOR: Color = Color::srgba(0.08, 0.22, 0.5, 1.0);
const DIMENSION_LINE_HIT_RADIUS: f32 = 0.18;
const DIMENSION_LINE_TICK_HALF: f32 = 0.08;
const DIMENSION_LINE_LABEL_SCREEN_OFFSET: f32 = 18.0;
const DIMENSION_LINE_MIN_MEASURE_LENGTH: f32 = 0.01;
const DIMENSION_LINE_DEFAULT_EXTENSION_MIN: f32 = 0.15;
const DIMENSION_LINE_DEFAULT_EXTENSION_MAX: f32 = 0.4;
const DIMENSION_LINE_DEFAULT_EXTENSION_FACTOR: f32 = 0.12;
const DIMENSION_LINE_DEFAULT_OFFSET_MIN: f32 = 0.2;
const DIMENSION_LINE_DEFAULT_OFFSET_MAX: f32 = 0.8;
const DIMENSION_LINE_DEFAULT_OFFSET_FACTOR: f32 = 0.18;
const DIMENSION_LINE_MIN_OFFSET: f32 = 0.001;

pub struct DimensionLinePlugin;

#[derive(Debug, Clone, PartialEq)]
struct DimensionLabelOverlayInfo {
    pub text: String,
    pub center: Vec2,
    pub selected: bool,
}

#[derive(Resource, Debug, Clone)]
pub struct DimensionLineVisibility {
    pub show_all: bool,
}

impl Default for DimensionLineVisibility {
    fn default() -> Self {
        Self { show_all: true }
    }
}

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct DimensionLineNode {
    pub start: Vec3,
    pub end: Vec3,
    pub line_point: Vec3,
    pub extension: f32,
    pub visible: bool,
    pub label: Option<String>,
    pub display_unit: Option<DisplayUnit>,
    pub precision: Option<u8>,
}

impl Default for DimensionLineNode {
    fn default() -> Self {
        Self {
            start: Vec3::ZERO,
            end: Vec3::X,
            line_point: Vec3::new(0.0, 0.0, DIMENSION_LINE_DEFAULT_OFFSET_MIN),
            extension: DIMENSION_LINE_DEFAULT_EXTENSION_MIN,
            visible: true,
            label: None,
            display_unit: None,
            precision: None,
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
    pub line_point: Vec3,
    pub extension: f32,
    pub visible: bool,
    pub label: Option<String>,
    pub display_unit: Option<DisplayUnit>,
    pub precision: Option<u8>,
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

    fn geometry(&self) -> DimensionGeometry {
        dimension_geometry(self.start, self.end, self.line_point, self.extension)
    }

    fn line_midpoint(&self) -> Vec3 {
        self.geometry().line_midpoint()
    }

    fn offset_distance(&self) -> f32 {
        self.geometry().offset_vec.length()
    }

    fn with_offset_distance(&self, offset: f32) -> Self {
        let mut snapshot = self.clone();
        let geometry = self.geometry();
        snapshot.line_point =
            self.midpoint() + geometry.offset_dir * offset.max(DIMENSION_LINE_MIN_OFFSET);
        snapshot
    }

    fn effective_display_unit(&self, doc_props: &DocumentProperties) -> DisplayUnit {
        self.display_unit.unwrap_or(doc_props.display_unit)
    }

    fn effective_precision(&self, doc_props: &DocumentProperties) -> u8 {
        self.precision.unwrap_or(doc_props.precision)
    }

    fn display_text(&self, doc_props: &DocumentProperties) -> String {
        let unit = self.effective_display_unit(doc_props);
        let precision = self.effective_precision(doc_props);
        let value = unit.format_value(self.measured_length(), precision);
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

    fn display_unit_text(&self) -> String {
        self.display_unit
            .map(|unit| unit.identifier().to_string())
            .unwrap_or_else(|| "document".to_string())
    }

    fn precision_text(&self) -> String {
        self.precision
            .map(|precision| precision.to_string())
            .unwrap_or_else(|| "document".to_string())
    }

    fn entity_bounds(&self) -> EntityBounds {
        let geometry = self.geometry();
        bounds_from_points(&[
            geometry.visible_start,
            self.start,
            self.end,
            geometry.dimension_start,
            geometry.dimension_end,
            geometry.visible_end,
            self.line_midpoint(),
        ])
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
        self.line_midpoint()
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.start += delta;
        snapshot.end += delta;
        snapshot.line_point += delta;
        snapshot.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.start = rotation * snapshot.start;
        snapshot.end = rotation * snapshot.end;
        snapshot.line_point = rotation * snapshot.line_point;
        snapshot.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        let axis_dir = snapshot.axis_direction();
        snapshot.start = center + (snapshot.start - center) * factor;
        snapshot.end = center + (snapshot.end - center) * factor;
        snapshot.line_point = center + (snapshot.line_point - center) * factor;
        snapshot.extension *= directional_scale(axis_dir, factor);
        snapshot.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
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
            property_field_with(
                "line_point",
                "Line Point",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.line_point)),
                true,
            ),
            property_field_with(
                "offset",
                "Offset",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.offset_distance())),
                true,
            ),
            property_field_with(
                "extension",
                "Extension",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.extension)),
                true,
            ),
            property_field(
                "visible",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.visible.to_string())),
            ),
            property_field_with(
                "display_unit",
                "Units",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.display_unit_text())),
                true,
            ),
            property_field_with(
                "precision",
                "Precision",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.precision_text())),
                true,
            ),
            read_only_property_field(
                "length",
                "Length",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.measured_length())),
            ),
            property_field_with(
                "label",
                "Label",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.label.clone().unwrap_or_default())),
                true,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "start" => snapshot.start = vec3_from_json(value)?,
            "end" => snapshot.end = vec3_from_json(value)?,
            "line_point" => snapshot.line_point = vec3_from_json(value)?,
            "offset" => snapshot = snapshot.with_offset_distance(scalar_from_json(value)?),
            "extension" => snapshot.extension = scalar_from_json(value)?,
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
            "display_unit" => snapshot.display_unit = parse_display_unit_override_json(value)?,
            "precision" => snapshot.precision = parse_precision_override_json(value)?,
            "label" => {
                snapshot.label = value
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned);
            }
            _ => {
                return Err(invalid_property_error(
                    "dimension_line",
                    &[
                        "start",
                        "end",
                        "line_point",
                        "offset",
                        "extension",
                        "visible",
                        "display_unit",
                        "precision",
                        "label",
                    ],
                ));
            }
        }
        validate_dimension_geometry(snapshot.start, snapshot.end, snapshot.extension)?;
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
                id: "line_point".to_string(),
                position: self.line_midpoint(),
                kind: HandleKind::Control,
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
            "line_point" => snapshot.line_point = cursor,
            _ => return None,
        }
        validate_dimension_geometry(snapshot.start, snapshot.end, snapshot.extension).ok()?;
        Some(snapshot.into())
    }

    fn to_json(&self) -> Value {
        json!({
            "element_id": self.element_id,
            "start": self.start.to_array(),
            "end": self.end.to_array(),
            "line_point": self.line_point.to_array(),
            "offset": self.offset_distance(),
            "extension": self.extension,
            "visible": self.visible,
            "label": self.label,
            "display_unit": self.display_unit.map(|unit| unit.identifier()),
            "precision": self.precision,
            "length": self.measured_length(),
        })
    }

    fn apply_to(&self, world: &mut World) {
        let node = DimensionLineNode {
            start: self.start,
            end: self.end,
            line_point: self.line_point,
            extension: self.extension,
            visible: self.visible,
            label: self.label.clone(),
            display_unit: self.display_unit,
            precision: self.precision,
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
        draw_dimension_line_gizmo(
            gizmos,
            self.start,
            self.end,
            self.line_point,
            self.extension,
            color,
        );
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
                line_point: node.line_point,
                extension: node.extension,
                visible: node.visible,
                label: node.label.clone(),
                display_unit: node.display_unit,
                precision: node.precision,
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
        let line_point = parse_dimension_line_point_json(data, start, end)?;
        let extension = data
            .get("extension")
            .map(scalar_from_json)
            .transpose()?
            .unwrap_or_else(|| default_dimension_extension(start, end));
        let visible = data.get("visible").and_then(Value::as_bool).unwrap_or(true);
        let label = data.get("label").and_then(Value::as_str).map(String::from);
        let display_unit = data
            .get("display_unit")
            .filter(|value| !value.is_null())
            .map(parse_display_unit_override_json)
            .transpose()?
            .flatten();
        let precision = data
            .get("precision")
            .filter(|value| !value.is_null())
            .map(parse_precision_override_json)
            .transpose()?
            .flatten();
        validate_dimension_geometry(start, end, extension)?;
        Ok(DimensionLineSnapshot {
            element_id,
            start,
            end,
            line_point,
            extension,
            visible,
            label,
            display_unit,
            precision,
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
        let line_point = parse_dimension_line_point_json(request, start, end)?;
        let extension = object
            .get("extension")
            .map(scalar_from_json)
            .transpose()?
            .unwrap_or_else(|| default_dimension_extension(start, end));
        let visible = object
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let label = object
            .get("label")
            .or_else(|| object.get("name"))
            .and_then(Value::as_str)
            .map(String::from);
        let display_unit = object
            .get("display_unit")
            .or_else(|| object.get("unit"))
            .filter(|value| !value.is_null())
            .map(parse_display_unit_override_json)
            .transpose()?
            .flatten();
        let precision = object
            .get("precision")
            .filter(|value| !value.is_null())
            .map(parse_precision_override_json)
            .transpose()?
            .flatten();

        validate_dimension_geometry(start, end, extension)?;
        Ok(DimensionLineSnapshot {
            element_id,
            start,
            end,
            line_point,
            extension,
            visible,
            label,
            display_unit,
            precision,
        }
        .into())
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        let visibility = world.get_resource::<DimensionLineVisibility>()?;
        if !visibility.show_all {
            return None;
        }
        let mut query = world.try_query::<(Entity, &DimensionLineNode)>()?;
        query
            .iter(world)
            .filter(|(_, node)| node.visible)
            .filter_map(|(entity, node)| {
                let segments =
                    dimension_segments(node.start, node.end, node.line_point, node.extension);
                let best_distance = segments
                    .into_iter()
                    .filter_map(|(start, end)| {
                        ray_segment_distance(ray.origin, ray.direction.into(), start, end)
                    })
                    .min_by(|left, right| left.total_cmp(right))?;
                if best_distance > DIMENSION_LINE_HIT_RADIUS {
                    return None;
                }
                Some(HitCandidate {
                    entity,
                    distance: ray.origin.distance((node.start + node.end) * 0.5),
                })
            })
            .min_by(|left, right| left.distance.total_cmp(&right.distance))
    }

    fn collect_snap_points(&self, world: &World, out: &mut Vec<SnapPoint>) {
        let Some(visibility) = world.get_resource::<DimensionLineVisibility>() else {
            return;
        };
        if !visibility.show_all {
            return;
        }
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
                position: dimension_geometry(node.start, node.end, node.line_point, node.extension)
                    .line_midpoint(),
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
    line_point: Vec3,
    extension: f32,
    color: Color,
) {
    for (segment_start, segment_end) in dimension_segments(start, end, line_point, extension) {
        gizmos.line(segment_start, segment_end, color);
    }
}

fn draw_dimension_line_visuals(
    visibility: Res<DimensionLineVisibility>,
    render_settings: Res<RenderSettings>,
    dimensions: Query<(Entity, &DimensionLineNode), Without<crate::plugins::selection::Selected>>,
    selected_dimensions: Query<
        (Entity, &DimensionLineNode),
        With<crate::plugins::selection::Selected>,
    >,
    mut gizmos: Gizmos,
) {
    if !visibility.show_all {
        return;
    }
    let regular_color = dimension_line_color(Some(&render_settings), false);
    let selected_color = dimension_line_color(Some(&render_settings), true);
    for (_entity, node) in &dimensions {
        if !node.visible {
            continue;
        }
        draw_dimension_line_gizmo(
            &mut gizmos,
            node.start,
            node.end,
            node.line_point,
            node.extension,
            regular_color,
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
            node.line_point,
            node.extension,
            selected_color,
        );
    }
}

fn draw_dimension_line_labels(
    mut contexts: EguiContexts,
    doc_props: Res<DocumentProperties>,
    visibility: Res<DimensionLineVisibility>,
    render_settings: Res<RenderSettings>,
    camera_query: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    window_query: Query<&Window, With<PrimaryWindow>>,
    dimensions: Query<(
        &DimensionLineNode,
        Option<&crate::plugins::selection::Selected>,
    )>,
) {
    if !visibility.show_all {
        return;
    }
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

    let overlays = collect_dimension_label_overlays_from_iter(
        &doc_props,
        &visibility,
        camera,
        camera_transform,
        window,
        dimensions
            .iter()
            .map(|(node, selected)| (node, selected.is_some())),
    );
    if overlays.is_empty() {
        return;
    }
    let paper_style = drawing_annotation_paper_style(&render_settings);

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("dimension_line_labels"),
    ));
    for overlay in overlays {
        let pos = egui::pos2(overlay.center.x, overlay.center.y);
        let rect = egui::Rect::from_center_size(
            pos,
            egui::vec2(overlay.text.chars().count() as f32 * 7.5 + 14.0, 22.0),
        );
        let (background, border, foreground) = label_colors(overlay.selected, paper_style);
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
            overlay.text,
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

fn execute_toggle_dimension_line_visibility(
    world: &mut World,
    _params: &Value,
) -> Result<CommandResult, String> {
    let mut visibility = world.resource_mut::<DimensionLineVisibility>();
    visibility.show_all = !visibility.show_all;
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

    match tool_state.start {
        None => {
            tool_state.start = Some(cursor_position);
            tool_state.end = None;
            status_bar_data.hint =
                "Click the end point, then drag away from the edge to place the dimension line"
                    .to_string();
        }
        Some(start) if tool_state.end.is_none() => {
            if start.distance(cursor_position) < DIMENSION_LINE_MIN_MEASURE_LENGTH {
                return;
            }

            tool_state.end = Some(cursor_position);
            status_bar_data.hint =
                "Click to place the offset dimension line outside the geometry".to_string();
        }
        Some(start) => {
            let end = tool_state
                .end
                .expect("dimension tool end point should exist in offset placement stage");
            let snapshot = DimensionLineSnapshot {
                element_id: allocator.next_id(),
                start,
                end,
                line_point: cursor_position,
                extension: default_dimension_extension(start, end),
                visible: true,
                label: None,
                display_unit: None,
                precision: None,
            };
            create_entity.write(CreateEntityCommand {
                snapshot: snapshot.into(),
            });
            tool_state.start = None;
            tool_state.end = None;
            next_active_tool.set(ActiveTool::PlaceDimensionLine);
            status_bar_data.hint =
                "Click a start point, then click an end point, then click to place the offset"
                    .to_string();
        }
    }
}

fn draw_dimension_line_tool_preview(
    cursor_world_pos: Res<CursorWorldPos>,
    tool_state: Option<Res<DimensionLineToolState>>,
    render_settings: Res<RenderSettings>,
    mut gizmos: Gizmos,
) {
    let Some(tool_state) = tool_state else {
        return;
    };
    let Some(start) = tool_state.start else {
        return;
    };
    let Some(cursor_position) = cursor_world_pos.snapped else {
        return;
    };
    let preview_color = dimension_line_color(Some(&render_settings), true);
    match tool_state.end {
        None => {
            if start.distance(cursor_position) < DIMENSION_LINE_MIN_MEASURE_LENGTH {
                return;
            }
            let end = cursor_position;
            draw_dimension_line_gizmo(
                &mut gizmos,
                start,
                end,
                default_dimension_line_point(start, end),
                default_dimension_extension(start, end),
                preview_color,
            );
        }
        Some(end) => {
            draw_dimension_line_gizmo(
                &mut gizmos,
                start,
                end,
                cursor_position,
                default_dimension_extension(start, end),
                preview_color,
            );
        }
    }
}

impl Plugin for DimensionLinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DimensionLineVisibility>()
            .register_authored_entity_factory(DimensionLineFactory)
            .register_capability(CapabilityDescriptor {
                id: "dimensions".to_string(),
                name: "Dimensions".to_string(),
                version: 1,
                api_version: crate::capability_registry::CAPABILITY_API_VERSION,
                description: Some(
                    "Authored drafting dimensions with witness lines, dragged offset placement, configurable units, and measured labels."
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
                    description: "Activate dimension placement".to_string(),
                    category: CommandCategory::Create,
                    parameters: None,
                    default_shortcut: Some("D".to_string()),
                    icon: None,
                    hint: Some(
                        "Click a start point, click an end point, then click to place the offset dimension line."
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
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_dimensions".to_string(),
                    label: "Dimensions".to_string(),
                    description: "Show or hide all dimensions".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Toggle visibility of dimension annotations".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("dimensions".to_string()),
                },
                execute_toggle_dimension_line_visibility,
            )
            .register_toolbar(ToolbarDescriptor {
                id: "dimensions".to_string(),
                label: "Dimensions".to_string(),
                default_dock: ToolbarDock::Left,
                default_visible: true,
                sections: vec![ToolbarSection {
                    label: "Annotate".to_string(),
                    command_ids: vec![
                        "dimensions.place".to_string(),
                        "view.toggle_dimensions".to_string(),
                    ],
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
                    draw_dimension_line_labels,
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

fn validate_dimension_geometry(start: Vec3, end: Vec3, extension: f32) -> Result<(), String> {
    if start.distance(end) < DIMENSION_LINE_MIN_MEASURE_LENGTH {
        return Err("Dimension start and end points must be distinct".to_string());
    }
    if extension < 0.0 {
        return Err("Dimension extension must be zero or positive".to_string());
    }
    Ok(())
}

fn default_dimension_extension(start: Vec3, end: Vec3) -> f32 {
    let measured_length = start.distance(end);
    (measured_length * DIMENSION_LINE_DEFAULT_EXTENSION_FACTOR).clamp(
        DIMENSION_LINE_DEFAULT_EXTENSION_MIN,
        DIMENSION_LINE_DEFAULT_EXTENSION_MAX,
    )
}

fn default_dimension_offset(start: Vec3, end: Vec3) -> f32 {
    let measured_length = start.distance(end);
    (measured_length * DIMENSION_LINE_DEFAULT_OFFSET_FACTOR).clamp(
        DIMENSION_LINE_DEFAULT_OFFSET_MIN,
        DIMENSION_LINE_DEFAULT_OFFSET_MAX,
    )
}

fn stable_offset_direction(axis_dir: Vec3) -> Vec3 {
    for candidate in [Vec3::Z, Vec3::Y, Vec3::X] {
        if let Some(direction) = axis_dir.cross(candidate).try_normalize() {
            return direction;
        }
    }
    perpendicular_pair(axis_dir).0
}

fn default_dimension_line_point(start: Vec3, end: Vec3) -> Vec3 {
    let midpoint = (start + end) * 0.5;
    midpoint
        + stable_offset_direction((end - start).try_normalize().unwrap_or(Vec3::X))
            * default_dimension_offset(start, end)
}

fn directional_scale(axis_dir: Vec3, factor: Vec3) -> f32 {
    Vec3::new(
        axis_dir.x * factor.x.abs(),
        axis_dir.y * factor.y.abs(),
        axis_dir.z * factor.z.abs(),
    )
    .length()
    .max(1e-4)
}

fn tick_direction(axis_dir: Vec3) -> Vec3 {
    let perpendicular = perpendicular_pair(axis_dir).0;
    (axis_dir + perpendicular)
        .try_normalize()
        .unwrap_or(perpendicular)
}

fn perpendicular_pair(dir: Vec3) -> (Vec3, Vec3) {
    let candidate = if dir.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let perp1 = dir.cross(candidate).normalize_or_zero();
    let perp2 = dir.cross(perp1).normalize_or_zero();
    (perp1, perp2)
}

fn parse_display_unit_override_json(value: &Value) -> Result<Option<DisplayUnit>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_str() else {
        return Err(
            "display_unit must be a string such as mm, cm, m, ft, in, or document".to_string(),
        );
    };
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("document") {
        return Ok(None);
    }
    DisplayUnit::parse(text)
        .map(Some)
        .ok_or_else(|| "display_unit must be one of: document, mm, cm, m, ft, in".to_string())
}

fn parse_precision_override_json(value: &Value) -> Result<Option<u8>, String> {
    if value.is_null() {
        return Ok(None);
    }
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("document") {
            return Ok(None);
        }
        return text
            .parse::<u8>()
            .map(Some)
            .map_err(|_| "precision must be a whole number or 'document'".to_string());
    }
    if let Some(number) = value.as_u64() {
        return u8::try_from(number)
            .map(Some)
            .map_err(|_| "precision is out of range for u8".to_string());
    }
    if let Some(number) = value.as_f64() {
        if number.fract().abs() > f64::EPSILON || number < 0.0 || number > u8::MAX as f64 {
            return Err("precision must be a whole number between 0 and 255".to_string());
        }
        return Ok(Some(number as u8));
    }
    Err("precision must be a whole number or 'document'".to_string())
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct DimensionGeometry {
    axis_dir: Vec3,
    offset_dir: Vec3,
    offset_vec: Vec3,
    dimension_start: Vec3,
    dimension_end: Vec3,
    visible_start: Vec3,
    visible_end: Vec3,
}

impl DimensionGeometry {
    fn line_midpoint(&self) -> Vec3 {
        (self.dimension_start + self.dimension_end) * 0.5
    }
}

fn dimension_geometry(
    start: Vec3,
    end: Vec3,
    line_point: Vec3,
    extension: f32,
) -> DimensionGeometry {
    let axis_dir = (end - start).try_normalize().unwrap_or(Vec3::X);
    let midpoint = (start + end) * 0.5;
    let raw_offset_vec = line_point - midpoint;
    let projected_offset_vec = raw_offset_vec - axis_dir * raw_offset_vec.dot(axis_dir);
    let (offset_dir, offset_vec) = if let Some(direction) =
        projected_offset_vec.try_normalize().filter(|_| {
            projected_offset_vec.length_squared()
                >= DIMENSION_LINE_MIN_OFFSET * DIMENSION_LINE_MIN_OFFSET
        }) {
        (direction, projected_offset_vec)
    } else {
        let direction = stable_offset_direction(axis_dir);
        (
            direction,
            direction * default_dimension_offset(start, end).max(DIMENSION_LINE_MIN_OFFSET),
        )
    };
    let dimension_start = start + offset_vec;
    let dimension_end = end + offset_vec;
    let visible_start = dimension_start - axis_dir * extension;
    let visible_end = dimension_end + axis_dir * extension;

    DimensionGeometry {
        axis_dir,
        offset_dir,
        offset_vec,
        dimension_start,
        dimension_end,
        visible_start,
        visible_end,
    }
}

fn parse_dimension_line_point_json(value: &Value, start: Vec3, end: Vec3) -> Result<Vec3, String> {
    let Some(object) = value.as_object() else {
        return Ok(default_dimension_line_point(start, end));
    };
    if let Some(line_point) = object.get("line_point").filter(|value| !value.is_null()) {
        return vec3_from_json(line_point);
    }
    if let Some(offset) = object.get("offset").filter(|value| !value.is_null()) {
        let midpoint = (start + end) * 0.5;
        let offset = scalar_from_json(offset)?.max(DIMENSION_LINE_MIN_OFFSET);
        return Ok(midpoint
            + stable_offset_direction((end - start).try_normalize().unwrap_or(Vec3::X)) * offset);
    }
    Ok(default_dimension_line_point(start, end))
}

fn dimension_segments(
    start: Vec3,
    end: Vec3,
    line_point: Vec3,
    extension: f32,
) -> [(Vec3, Vec3); 5] {
    let geometry = dimension_geometry(start, end, line_point, extension);
    let tick_dir = tick_direction(geometry.axis_dir) * DIMENSION_LINE_TICK_HALF;
    [
        (geometry.visible_start, geometry.visible_end),
        (start, geometry.dimension_start),
        (end, geometry.dimension_end),
        (
            geometry.dimension_start - tick_dir,
            geometry.dimension_start + tick_dir,
        ),
        (
            geometry.dimension_end - tick_dir,
            geometry.dimension_end + tick_dir,
        ),
    ]
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

fn snapshot_from_node(node: &DimensionLineNode) -> DimensionLineSnapshot {
    DimensionLineSnapshot {
        element_id: ElementId(u64::MAX),
        start: node.start,
        end: node.end,
        line_point: node.line_point,
        extension: node.extension,
        visible: node.visible,
        label: node.label.clone(),
        display_unit: node.display_unit,
        precision: node.precision,
    }
}

fn drawing_annotation_paper_style(render_settings: &RenderSettings) -> bool {
    render_settings.paper_fill_enabled
        || (render_settings.background_rgb[0] * 0.2126
            + render_settings.background_rgb[1] * 0.7152
            + render_settings.background_rgb[2] * 0.0722)
            >= 0.8
}

fn dimension_line_color(render_settings: Option<&RenderSettings>, selected: bool) -> Color {
    if render_settings.is_some_and(drawing_annotation_paper_style) {
        if selected {
            DIMENSION_LINE_PAPER_SELECTED_COLOR
        } else {
            DIMENSION_LINE_PAPER_COLOR
        }
    } else if selected {
        DIMENSION_LINE_SELECTED_COLOR
    } else {
        DIMENSION_LINE_COLOR
    }
}

fn label_colors(
    selected: bool,
    paper_style: bool,
) -> (egui::Color32, egui::Color32, egui::Color32) {
    match (paper_style, selected) {
        (true, true) => (
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 242),
            egui::Color32::from_rgb(54, 103, 191),
            egui::Color32::from_rgb(22, 35, 64),
        ),
        (true, false) => (
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 236),
            egui::Color32::from_rgb(54, 54, 64),
            egui::Color32::from_rgb(28, 28, 34),
        ),
        (false, true) => (
            egui::Color32::from_rgba_unmultiplied(64, 60, 24, 220),
            egui::Color32::from_rgb(255, 232, 120),
            egui::Color32::from_rgb(255, 246, 190),
        ),
        (false, false) => (
            egui::Color32::from_rgba_unmultiplied(26, 26, 26, 200),
            egui::Color32::from_rgb(222, 210, 110),
            egui::Color32::from_rgb(245, 236, 180),
        ),
    }
}

fn collect_dimension_label_overlays_from_iter<'a>(
    doc_props: &DocumentProperties,
    visibility: &DimensionLineVisibility,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    window: &Window,
    dimensions: impl Iterator<Item = (&'a DimensionLineNode, bool)>,
) -> Vec<DimensionLabelOverlayInfo> {
    if !visibility.show_all {
        return Vec::new();
    }

    let mut overlays = Vec::new();
    for (node, selected) in dimensions {
        if !node.visible {
            continue;
        }
        let snapshot = snapshot_from_node(node);
        if snapshot.measured_length() < DIMENSION_LINE_MIN_MEASURE_LENGTH {
            continue;
        }
        let geometry = snapshot.geometry();
        let line_midpoint = geometry.line_midpoint();
        let Ok(screen_pos) = camera.world_to_viewport(camera_transform, line_midpoint) else {
            continue;
        };
        if screen_pos.x < 0.0
            || screen_pos.y < 0.0
            || screen_pos.x > window.width()
            || screen_pos.y > window.height()
        {
            continue;
        }
        let label_center = camera
            .world_to_viewport(
                camera_transform,
                line_midpoint
                    + geometry.offset_dir * 0.15_f32.max(snapshot.offset_distance() * 0.15),
            )
            .ok()
            .and_then(|probe| (probe - screen_pos).try_normalize())
            .map(|direction| screen_pos + direction * DIMENSION_LINE_LABEL_SCREEN_OFFSET)
            .unwrap_or(screen_pos);
        overlays.push(DimensionLabelOverlayInfo {
            text: snapshot.display_text(doc_props),
            center: label_center,
            selected,
        });
    }
    overlays
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::AuthoredEntityFactory;

    #[test]
    fn create_request_defaults_visible_extension() {
        let mut world = World::new();
        world.insert_resource(ElementIdAllocator::default());

        let snapshot = DimensionLineFactory
            .from_create_request(
                &world,
                &json!({
                    "type": "dimension_line",
                    "start": [0.0, 0.0, 0.0],
                    "end": [2.0, 0.0, 0.0],
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
        assert_eq!(snapshot.line_point, Vec3::new(1.0, -0.36, 0.0));
        assert_eq!(snapshot.extension, 0.24);
        assert_eq!(snapshot.label.as_deref(), Some("Width"));
    }

    #[test]
    fn measured_length_can_override_units_and_precision() {
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(7),
            start: Vec3::ZERO,
            end: Vec3::new(2.5, 0.0, 0.0),
            line_point: Vec3::new(1.25, -0.45, 0.0),
            extension: 0.25,
            visible: true,
            label: Some("Overall".to_string()),
            display_unit: Some(DisplayUnit::Feet),
            precision: Some(2),
        };
        let doc_props = DocumentProperties::default();

        assert_eq!(snapshot.display_text(&doc_props), "Overall: 8.20ft");
    }

    #[test]
    fn display_unit_and_precision_accept_document_keyword() {
        assert_eq!(
            parse_display_unit_override_json(&json!("document")).expect("unit should parse"),
            None
        );
        assert_eq!(
            parse_precision_override_json(&json!("document")).expect("precision should parse"),
            None
        );
    }

    #[test]
    fn explicit_offset_creates_dimension_line_point() {
        let start = Vec3::ZERO;
        let end = Vec3::new(2.0, 0.0, 0.0);
        let line_point = parse_dimension_line_point_json(&json!({ "offset": 0.5 }), start, end)
            .expect("offset should parse");

        assert_eq!(line_point, Vec3::new(1.0, -0.5, 0.0));
    }

    #[test]
    fn dimension_geometry_offsets_line_outside_measurement() {
        let geometry = dimension_geometry(
            Vec3::ZERO,
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(1.0, -0.5, 0.0),
            0.25,
        );

        assert_eq!(geometry.dimension_start, Vec3::new(0.0, -0.5, 0.0));
        assert_eq!(geometry.dimension_end, Vec3::new(2.0, -0.5, 0.0));
        assert_eq!(geometry.visible_start, Vec3::new(-0.25, -0.5, 0.0));
        assert_eq!(geometry.visible_end, Vec3::new(2.25, -0.5, 0.0));
    }

    #[test]
    fn dimension_snap_points_respect_global_visibility_toggle() {
        let mut world = World::new();
        world.insert_resource(DimensionLineVisibility { show_all: false });
        world.spawn((
            ElementId(17),
            DimensionLineNode {
                start: Vec3::ZERO,
                end: Vec3::X,
                line_point: Vec3::new(0.5, -0.2, 0.0),
                extension: 0.2,
                visible: true,
                label: None,
                display_unit: None,
                precision: None,
            },
        ));

        let mut snap_points = Vec::new();
        DimensionLineFactory.collect_snap_points(&world, &mut snap_points);

        assert!(snap_points.is_empty());
    }
}
