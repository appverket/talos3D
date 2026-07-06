use std::{any::Any, collections::HashMap};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, read_only_property_field,
        scalar_from_json, vec3_from_json, AuthoredEntity, BoxedEntity, EntityBounds, EntityScope,
        HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{
        AuthoredEntityFactory, CapabilityDescriptor, CapabilityDistribution, CapabilityMaturity,
        CapabilityRegistryAppExt, HitCandidate, ModelSummaryAccumulator, SnapPoint,
    },
    plugins::{
        camera::OrbitCamera,
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        commands::{despawn_by_element_id, find_entity_by_element_id, CreateEntityCommand},
        cursor::{dimension_annotation_plane, CursorWorldPos, DrawingPlane},
        document_properties::DocumentProperties,
        drafting::{
            kind::DimensionKind as DraftDimensionKind,
            render_dimension,
            style::{DimensionStyle, TextPlacement},
            DimPrimitive, DimensionInput,
        },
        identity::{ElementId, ElementIdAllocator},
        input_ownership::{InputOwnership, InputPhase},
        layers::{LayerAssignment, LayerRegistry},
        render_pipeline::RenderSettings,
        snap::{SnapKind, SnapResult, SnapSystems},
        toolbar::{ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection},
        tools::ActiveTool,
        ui::StatusBarData,
        units::DisplayUnit,
    },
};

const DIMENSION_LINE_HIT_RADIUS: f32 = 0.18;
const DIMENSION_LINE_TICK_HALF: f32 = 0.08;
const DIMENSION_LINE_MIN_MEASURE_LENGTH: f32 = 0.01;
const DIMENSION_LINE_DEFAULT_EXTENSION_MIN: f32 = 0.15;
const DIMENSION_LINE_DEFAULT_EXTENSION_MAX: f32 = 0.4;
const DIMENSION_LINE_DEFAULT_EXTENSION_FACTOR: f32 = 0.12;
const DIMENSION_LINE_DEFAULT_OFFSET_MIN: f32 = 0.2;
const DIMENSION_LINE_DEFAULT_OFFSET_MAX: f32 = 0.8;
const DIMENSION_LINE_DEFAULT_OFFSET_FACTOR: f32 = 0.18;
const DIMENSION_LINE_MIN_OFFSET: f32 = 0.001;
const DIMENSION_LINE_OFFSET_DRAG_COMMIT_DISTANCE: f32 = 0.02;
const DIMENSION_FACE_PLANE_TOLERANCE: f32 = 0.03;
const DIMENSION_FACE_NORMAL_ALIGNMENT: f32 = 0.98;
pub(crate) const DIMENSION_RENDER_PAPER_MM_PER_WORLD_M: f32 = 20.0;
const DIMENSION_VIEWPORT_MIN_TEXT_PX: f32 = 13.0;
const DIMENSION_VIEWPORT_MAX_TEXT_PX: f32 = 28.0;
const DIMENSION_WORLD_TEXT_GAP_FACTOR: f32 = 0.35;
const DRAFTING_TEXT_CHAR_SPACING: f32 = 0.18;
pub const DIMENSION_ANNOTATIONS_KEY: &str = "dimension_annotations";
pub const DIMENSION_LAYER_NAME: &str = "Dimensions";

pub struct DimensionLinePlugin;

#[derive(Resource, Debug, Clone)]
pub struct DimensionLineVisibility {
    pub show_all: bool,
}

impl Default for DimensionLineVisibility {
    fn default() -> Self {
        Self { show_all: true }
    }
}

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    stage: DimensionLinePlacementStage,
}

#[derive(Debug, Clone, Copy, Default)]
enum DimensionLinePlacementStage {
    #[default]
    AwaitingStart,
    AwaitingEnd {
        start: Vec3,
        host_element_id: ElementId,
        face: DimensionFacePick,
    },
    PlacingOffset {
        start: Vec3,
        end: Vec3,
        host_element_id: ElementId,
        face: DimensionFacePick,
        preferred_offset_dir: Vec3,
        drag_origin: Option<Vec3>,
        dragging: bool,
    },
}

#[derive(Resource, Default)]
struct DimensionAnnotationSyncState {
    last_serialized: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        unit.format_value(self.measured_length(), precision)
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

    fn scope(&self) -> EntityScope {
        EntityScope::DrawingMetadata
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        self.clone().into()
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        self.clone().into()
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        self.clone().into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            read_only_property_field(
                "start",
                "start",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.start)),
            ),
            read_only_property_field(
                "end",
                "end",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.end)),
            ),
            property_field_with(
                "line_point",
                "Line Point",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.line_point)),
                false,
            ),
            property_field_with(
                "offset",
                "Offset",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.offset_distance())),
                false,
            ),
            property_field_with(
                "extension",
                "Extension",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.extension)),
                false,
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
        Vec::new()
    }

    fn bounds(&self) -> Option<EntityBounds> {
        Some(self.entity_bounds())
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
        ensure_dimension_layer(world);
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
            world
                .entity_mut(entity)
                .insert((node, LayerAssignment::new(DIMENSION_LAYER_NAME)));
        } else {
            world.spawn((
                self.element_id,
                node,
                LayerAssignment::new(DIMENSION_LAYER_NAME),
            ));
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        for (segment_start, segment_end) in
            dimension_segments(self.start, self.end, self.line_point, self.extension)
        {
            gizmos.line(segment_start, segment_end, color);
        }
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
        let explicit_line_point = object
            .get("line_point")
            .is_some_and(|value| !value.is_null());
        let line_point = parse_dimension_line_point_json(request, start, end)?;
        let line_point = if explicit_line_point {
            line_point
        } else {
            current_dimension_plane(world)
                .map(|plane| resolved_dimension_line_point(start, end, line_point, &plane, None))
                .unwrap_or(line_point)
        };
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
                element_id: None,
                label: None,
            });
            out.push(SnapPoint {
                position: node.end,
                kind: SnapKind::Endpoint,
                element_id: None,
                label: None,
            });
            out.push(SnapPoint {
                position: dimension_geometry(node.start, node.end, node.line_point, node.extension)
                    .line_midpoint(),
                kind: SnapKind::Control,
                element_id: None,
                label: None,
            });
        }
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let _ = (world, summary);
    }
}

fn draw_dimension_line_overlay(
    mut gizmos: Gizmos,
    doc_props: Res<DocumentProperties>,
    visibility: Res<DimensionLineVisibility>,
    viewport_export_state: Res<crate::plugins::drawing_export::ViewportExportState>,
    render_settings: Res<RenderSettings>,
    camera_query: Query<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    dimensions: Query<(
        &DimensionLineNode,
        Option<&crate::plugins::selection::Selected>,
    )>,
) {
    if viewport_export_state.annotation_overlays_suppressed() || !visibility.show_all {
        return;
    }
    let Some((camera, camera_transform)) = active_dimension_camera(camera_query.iter()) else {
        return;
    };
    let paper_style = drawing_annotation_paper_style(&render_settings);
    for (node, selected) in &dimensions {
        if !node.visible {
            continue;
        }
        let color = dimension_overlay_color(selected.is_some(), paper_style);
        draw_dimension_world_lines(&mut gizmos, node, color);
        draw_dimension_world_text(
            &mut gizmos,
            node,
            &doc_props,
            camera,
            camera_transform,
            color,
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
    ownership: Res<InputOwnership>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
    mut status_bar_data: ResMut<StatusBarData>,
) {
    if !ownership.is_idle() || !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    next_active_tool.set(ActiveTool::Select);
    status_bar_data.hint.clear();
}

fn handle_dimension_line_clicks(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    ownership: Res<InputOwnership>,
    cursor_world_pos: Res<CursorWorldPos>,
    snap_result: Res<SnapResult>,
    mut tool_state: ResMut<DimensionLineToolState>,
    allocator: Res<ElementIdAllocator>,
    drawing_plane: Res<DrawingPlane>,
    camera_query: Query<(&GlobalTransform, &OrbitCamera), With<Camera3d>>,
    host_bounds_query: Query<(
        &ElementId,
        Option<&bevy::camera::primitives::Aabb>,
        Option<&GlobalTransform>,
    )>,
    mut create_entity: MessageWriter<CreateEntityCommand>,
    mut status_bar_data: ResMut<StatusBarData>,
    mut next_active_tool: ResMut<NextState<ActiveTool>>,
) {
    if !ownership.is_idle() {
        return;
    }

    if mouse_buttons.just_released(MouseButton::Left) {
        let DimensionLinePlacementStage::PlacingOffset {
            start,
            end,
            host_element_id,
            face,
            preferred_offset_dir,
            drag_origin: Some(drag_origin),
            dragging: true,
        } = tool_state.stage
        else {
            return;
        };
        let Some(offset_position) =
            dimension_tool_offset_cursor_position(&cursor_world_pos, &snap_result)
        else {
            return;
        };
        if offset_position.distance(drag_origin) < DIMENSION_LINE_OFFSET_DRAG_COMMIT_DISTANCE {
            tool_state.stage = DimensionLinePlacementStage::PlacingOffset {
                start,
                end,
                host_element_id,
                face,
                preferred_offset_dir,
                drag_origin: None,
                dragging: false,
            };
            status_bar_data.hint =
                "Drag away from the measured line, or click to place the dimension line"
                    .to_string();
            return;
        }
        commit_dimension_line(
            start,
            end,
            guided_face_dimension_line_request(
                start,
                end,
                offset_position,
                face,
                preferred_offset_dir,
            ),
            Some(host_element_id),
            Some(face),
            &allocator,
            &drawing_plane,
            &camera_query,
            &host_bounds_query,
            &mut create_entity,
        );
        tool_state.stage = DimensionLinePlacementStage::AwaitingStart;
        next_active_tool.set(ActiveTool::PlaceDimensionLine);
        status_bar_data.hint =
            "Click a start anchor, then click an end anchor and drag out the dimension line"
                .to_string();
        return;
    }

    if !mouse_buttons.just_pressed(MouseButton::Left) {
        return;
    }

    match tool_state.stage {
        DimensionLinePlacementStage::AwaitingStart => {
            let Some(anchor) = dimension_tool_anchor_pick(
                &cursor_world_pos,
                &snap_result,
                Some(&host_bounds_query),
            ) else {
                status_bar_data.hint =
                    "Click a visible face edge endpoint to start a dimension".to_string();
                return;
            };
            tool_state.stage = DimensionLinePlacementStage::AwaitingEnd {
                start: anchor.position,
                host_element_id: anchor.host_element_id,
                face: anchor.face,
            };
            status_bar_data.hint = "Click another endpoint on the same face edge".to_string();
        }
        DimensionLinePlacementStage::AwaitingEnd {
            start,
            host_element_id,
            face,
        } => {
            let Some(anchor) = dimension_tool_anchor_pick_on_face(
                &cursor_world_pos,
                &snap_result,
                host_element_id,
                face,
                Some(&host_bounds_query),
            ) else {
                status_bar_data.hint =
                    "Dimension endpoints must be visible face edge endpoints".to_string();
                return;
            };
            let cursor_position = anchor.position;
            if start.distance(cursor_position) < DIMENSION_LINE_MIN_MEASURE_LENGTH {
                return;
            }

            let Some(edge_constraint) = dimension_face_edge_constraint(
                start,
                anchor,
                host_element_id,
                face,
                &host_bounds_query,
            ) else {
                status_bar_data.hint =
                    "Pick two endpoints of the same visible face edge".to_string();
                return;
            };
            let camera_position = active_dimension_tool_transform(camera_query.iter())
                .map(|transform| transform.translation());
            let preferred_offset_dir = preferred_face_dimension_offset_dir(
                start,
                cursor_position,
                edge_constraint.face,
                edge_constraint.host_bounds,
                camera_position,
            )
            .unwrap_or(edge_constraint.preferred_offset_dir);
            tool_state.stage = DimensionLinePlacementStage::PlacingOffset {
                start,
                end: cursor_position,
                host_element_id: edge_constraint.host_element_id,
                face: edge_constraint.face,
                preferred_offset_dir,
                drag_origin: Some(cursor_position),
                dragging: true,
            };
            status_bar_data.hint =
                "Drag perpendicular from the measured line and release to place".to_string();
        }
        DimensionLinePlacementStage::PlacingOffset {
            start,
            end,
            host_element_id,
            face,
            preferred_offset_dir,
            ..
        } => {
            let Some(offset_position) =
                dimension_tool_offset_cursor_position(&cursor_world_pos, &snap_result)
            else {
                return;
            };
            commit_dimension_line(
                start,
                end,
                guided_face_dimension_line_request(
                    start,
                    end,
                    offset_position,
                    face,
                    preferred_offset_dir,
                ),
                Some(host_element_id),
                Some(face),
                &allocator,
                &drawing_plane,
                &camera_query,
                &host_bounds_query,
                &mut create_entity,
            );
            tool_state.stage = DimensionLinePlacementStage::AwaitingStart;
            next_active_tool.set(ActiveTool::PlaceDimensionLine);
            status_bar_data.hint =
                "Click a start anchor, then click an end anchor and drag out the dimension line"
                    .to_string();
        }
    }
}

fn commit_dimension_line(
    start: Vec3,
    end: Vec3,
    offset_position: Vec3,
    host_element_id: Option<ElementId>,
    face: Option<DimensionFacePick>,
    allocator: &ElementIdAllocator,
    drawing_plane: &DrawingPlane,
    camera_query: &Query<(&GlobalTransform, &OrbitCamera), With<Camera3d>>,
    host_bounds_query: &Query<(
        &ElementId,
        Option<&bevy::camera::primitives::Aabb>,
        Option<&GlobalTransform>,
    )>,
    create_entity: &mut MessageWriter<CreateEntityCommand>,
) {
    let active_plane = camera_query
        .single()
        .ok()
        .map(|(camera_transform, orbit)| {
            dimension_annotation_plane(drawing_plane, Some(orbit), Some(camera_transform))
        })
        .unwrap_or_else(|| drawing_plane.clone());
    let active_plane = face.map(face_dimension_plane).unwrap_or(active_plane);
    let host_reference =
        host_entity_dimension_reference(host_element_id, &active_plane, host_bounds_query);
    let snapshot = DimensionLineSnapshot {
        element_id: allocator.next_id(),
        start,
        end,
        line_point: resolved_dimension_line_point(
            start,
            end,
            offset_position,
            &active_plane,
            host_reference,
        ),
        extension: default_dimension_extension(start, end),
        visible: true,
        label: None,
        display_unit: None,
        precision: None,
    };
    create_entity.write(CreateEntityCommand {
        snapshot: snapshot.into(),
    });
}

fn draw_dimension_line_tool_preview(
    mut gizmos: Gizmos,
    doc_props: Res<DocumentProperties>,
    viewport_export_state: Res<crate::plugins::drawing_export::ViewportExportState>,
    cursor_world_pos: Res<CursorWorldPos>,
    snap_result: Res<SnapResult>,
    tool_state: Option<Res<DimensionLineToolState>>,
    drawing_plane: Res<DrawingPlane>,
    camera_query: Query<(&Camera, &GlobalTransform, &OrbitCamera)>,
    host_bounds_query: Query<(
        &ElementId,
        Option<&bevy::camera::primitives::Aabb>,
        Option<&GlobalTransform>,
    )>,
    render_settings: Res<RenderSettings>,
) {
    if viewport_export_state.annotation_overlays_suppressed() {
        return;
    }
    let Some(tool_state) = tool_state else {
        return;
    };
    let Some((camera, camera_transform, orbit)) = active_dimension_tool_camera(camera_query.iter())
    else {
        return;
    };
    let Some(cursor_position) = dimension_tool_preview_cursor_position(
        &tool_state,
        &cursor_world_pos,
        &snap_result,
        Some(&host_bounds_query),
    ) else {
        return;
    };
    let Some(preview_node) = preview_dimension_line_node(
        &tool_state,
        cursor_position,
        &drawing_plane,
        Some(orbit),
        camera_transform,
        &host_bounds_query,
    ) else {
        return;
    };
    let color = dimension_overlay_color(true, drawing_annotation_paper_style(&render_settings));
    draw_dimension_world_lines(&mut gizmos, &preview_node, color);
    draw_dimension_world_text(
        &mut gizmos,
        &preview_node,
        &doc_props,
        camera,
        camera_transform,
        color,
    );
}

fn draw_dimension_world_lines(gizmos: &mut Gizmos, node: &DimensionLineNode, color: Color) {
    for (segment_start, segment_end) in
        dimension_segments(node.start, node.end, node.line_point, node.extension)
    {
        gizmos.line(segment_start, segment_end, color);
    }
}

fn current_dimension_plane(world: &World) -> Option<DrawingPlane> {
    let fallback = world
        .get_resource::<DrawingPlane>()
        .cloned()
        .unwrap_or_default();
    let mut query = world.try_query::<(&GlobalTransform, &OrbitCamera)>()?;
    query
        .single(world)
        .ok()
        .map(|(camera_transform, orbit)| {
            dimension_annotation_plane(&fallback, Some(orbit), Some(camera_transform))
        })
        .or(Some(fallback))
}

fn sync_dimension_annotations(world: &mut World) {
    let saved = {
        let doc_props = world.resource::<DocumentProperties>();
        doc_props
            .domain_defaults
            .get(DIMENSION_ANNOTATIONS_KEY)
            .cloned()
    };
    let saved_changed = {
        let sync_state = world.resource::<DimensionAnnotationSyncState>();
        saved != sync_state.last_serialized
    };

    if saved_changed {
        match saved.as_ref() {
            Some(value) => {
                let Some(snapshots) = deserialize_dimension_annotations(value) else {
                    world
                        .resource_mut::<DimensionAnnotationSyncState>()
                        .last_serialized = saved.clone();
                    return;
                };
                apply_dimension_annotations_to_world(world, &snapshots);
            }
            None => apply_dimension_annotations_to_world(world, &[]),
        }
        world
            .resource_mut::<DimensionAnnotationSyncState>()
            .last_serialized = saved.clone();
    }

    let serialized = serialize_dimension_annotations_from_world(world);
    {
        let mut doc_props = world.resource_mut::<DocumentProperties>();
        match &serialized {
            Some(value) => {
                if doc_props.domain_defaults.get(DIMENSION_ANNOTATIONS_KEY) != Some(value) {
                    doc_props
                        .domain_defaults
                        .insert(DIMENSION_ANNOTATIONS_KEY.to_string(), value.clone());
                }
            }
            None => {
                doc_props.domain_defaults.remove(DIMENSION_ANNOTATIONS_KEY);
            }
        }
    }
    world
        .resource_mut::<DimensionAnnotationSyncState>()
        .last_serialized = serialized;
}

fn serialize_dimension_annotations_from_world(world: &mut World) -> Option<Value> {
    let mut query = world.query::<(&ElementId, &DimensionLineNode)>();
    let mut annotations = query
        .iter(world)
        .map(|(element_id, node)| DimensionLineSnapshot {
            element_id: *element_id,
            start: node.start,
            end: node.end,
            line_point: node.line_point,
            extension: node.extension,
            visible: node.visible,
            label: node.label.clone(),
            display_unit: node.display_unit,
            precision: node.precision,
        })
        .collect::<Vec<_>>();
    if annotations.is_empty() {
        return None;
    }
    annotations.sort_by_key(|snapshot| snapshot.element_id.0);
    serde_json::to_value(annotations).ok()
}

fn deserialize_dimension_annotations(value: &Value) -> Option<Vec<DimensionLineSnapshot>> {
    let mut snapshots: Vec<DimensionLineSnapshot> = serde_json::from_value(value.clone()).ok()?;
    snapshots.sort_by_key(|snapshot| snapshot.element_id.0);
    Some(snapshots)
}

fn apply_dimension_annotations_to_world(world: &mut World, snapshots: &[DimensionLineSnapshot]) {
    ensure_dimension_layer(world);
    let mut existing_query = world.query::<(Entity, &ElementId, &DimensionLineNode)>();
    let mut existing = existing_query
        .iter(world)
        .map(|(entity, element_id, node)| (element_id.0, (entity, node.clone())))
        .collect::<HashMap<_, _>>();

    for snapshot in snapshots {
        let node = DimensionLineNode {
            start: snapshot.start,
            end: snapshot.end,
            line_point: snapshot.line_point,
            extension: snapshot.extension,
            visible: snapshot.visible,
            label: snapshot.label.clone(),
            display_unit: snapshot.display_unit,
            precision: snapshot.precision,
        };
        if let Some((entity, existing_node)) = existing.remove(&snapshot.element_id.0) {
            if existing_node != node {
                world.entity_mut(entity).insert(node);
            }
            world
                .entity_mut(entity)
                .insert(LayerAssignment::new(DIMENSION_LAYER_NAME));
        } else {
            world.spawn((
                snapshot.element_id,
                node,
                LayerAssignment::new(DIMENSION_LAYER_NAME),
            ));
        }
    }

    for (_, (entity, _)) in existing {
        let _ = world.despawn(entity);
    }
}

fn ensure_dimension_layer(world: &mut World) {
    if let Some(mut registry) = world.get_resource_mut::<LayerRegistry>() {
        registry.ensure_layer(DIMENSION_LAYER_NAME);
    }
}

impl Plugin for DimensionLinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DimensionLineVisibility>()
            .init_resource::<DimensionAnnotationSyncState>()
            .register_authored_entity_factory(DimensionLineFactory)
            .register_capability(CapabilityDescriptor {
                id: "dimensions".to_string(),
                name: "Dimensions".to_string(),
                version: 1,
                api_version: crate::capability_registry::CAPABILITY_API_VERSION,
                description: Some(
                    "Drawing metadata for orthographic dimensions with witness lines, offset placement, configurable units, and measured labels."
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
                    icon: Some("icon.dimension".to_string()),
                    hint: Some(
                        "Click a start anchor, then click an end anchor and drag perpendicular to place the dimension line."
                            .to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: Some("PlaceDimensionLine".to_string()),
                    capability_id: Some(crate::plugins::drawing_export::DRAFTING_CAPABILITY_ID.to_string()),
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
                    icon: Some("icon.dimensions".to_string()),
                    hint: Some("Toggle visibility of dimension annotations".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(crate::plugins::drawing_export::DRAFTING_CAPABILITY_ID.to_string()),
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
                sync_dimension_annotations,
            )
            .add_systems(
                Update,
                (
                    cancel_dimension_line_tool,
                    handle_dimension_line_clicks,
                    draw_dimension_line_overlay,
                    draw_dimension_line_tool_preview,
                )
                    .in_set(InputPhase::ToolInput)
                    .after(SnapSystems::Resolve)
                    .run_if(in_state(ActiveTool::PlaceDimensionLine)),
            )
            .add_systems(
                Update,
                draw_dimension_line_overlay
                    .after(SnapSystems::Resolve)
                    .run_if(not(in_state(ActiveTool::PlaceDimensionLine))),
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct ProjectedBounds2d {
    min: Vec2,
    max: Vec2,
}

impl ProjectedBounds2d {
    fn max_along(self, axis: Vec2) -> f32 {
        [
            Vec2::new(self.min.x, self.min.y),
            Vec2::new(self.min.x, self.max.y),
            Vec2::new(self.max.x, self.max.y),
            Vec2::new(self.max.x, self.min.y),
        ]
        .into_iter()
        .map(|corner| corner.dot(axis))
        .fold(f32::NEG_INFINITY, f32::max)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HostDimensionReference {
    world_bounds: EntityBounds,
    projected_bounds: ProjectedBounds2d,
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

fn resolved_dimension_line_point(
    start: Vec3,
    end: Vec3,
    requested_line_point: Vec3,
    plane: &DrawingPlane,
    host_reference: Option<HostDimensionReference>,
) -> Vec3 {
    if let Some(line_point) = host_reference.and_then(|reference| {
        constrained_box_edge_dimension_line_point(
            start,
            end,
            requested_line_point,
            reference.world_bounds,
        )
    }) {
        return line_point;
    }

    let start_2d = plane.project_to_2d(start);
    let end_2d = plane.project_to_2d(end);
    let request_2d = plane.project_to_2d(requested_line_point);
    let axis_2d = end_2d - start_2d;
    let Some(axis_dir_2d) = axis_2d.try_normalize() else {
        return requested_line_point;
    };
    let midpoint_2d = (start_2d + end_2d) * 0.5;
    let mut offset_axis = Vec2::new(-axis_dir_2d.y, axis_dir_2d.x);
    let requested_offset = request_2d - midpoint_2d;
    if requested_offset.dot(offset_axis) < 0.0 {
        offset_axis = -offset_axis;
    }

    let midpoint_projection = midpoint_2d.dot(offset_axis);
    let requested_projection = request_2d.dot(offset_axis);
    let minimum_projection = host_reference
        .map(|reference| reference.projected_bounds.max_along(offset_axis))
        .unwrap_or(midpoint_projection)
        + default_dimension_offset(start, end);
    let final_projection = requested_projection.max(minimum_projection);
    let resolved_midpoint = midpoint_2d + offset_axis * (final_projection - midpoint_projection);
    plane.to_world(resolved_midpoint)
}

pub(crate) fn constrained_box_edge_dimension_line_point(
    start: Vec3,
    end: Vec3,
    requested_line_point: Vec3,
    bounds: EntityBounds,
) -> Option<Vec3> {
    let edge = box_edge_reference(start, end, bounds)?;
    let midpoint = (start + end) * 0.5;
    let requested_offset = requested_line_point - midpoint;
    let first_score = requested_offset.dot(edge.candidate_dirs[0]);
    let second_score = requested_offset.dot(edge.candidate_dirs[1]);
    let selected_dir = if first_score >= second_score {
        edge.candidate_dirs[0]
    } else {
        edge.candidate_dirs[1]
    };
    let requested_distance = requested_offset.dot(selected_dir);
    let selected_extent = bounds_max_along(bounds, selected_dir);
    let midpoint_projection = midpoint.dot(selected_dir);
    let minimum_distance =
        selected_extent - midpoint_projection + default_dimension_offset(start, end);
    Some(midpoint + selected_dir * requested_distance.max(minimum_distance))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BoxEdgeReference {
    candidate_dirs: [Vec3; 2],
}

fn box_edge_reference(start: Vec3, end: Vec3, bounds: EntityBounds) -> Option<BoxEdgeReference> {
    let start_coords = [start.x, start.y, start.z];
    let end_coords = [end.x, end.y, end.z];
    let min_coords = [bounds.min.x, bounds.min.y, bounds.min.z];
    let max_coords = [bounds.max.x, bounds.max.y, bounds.max.z];
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    let tolerance = box_edge_tolerance(bounds);

    let mut measured_axis = None;
    let mut candidate_dirs = Vec::new();
    for axis in 0..3 {
        let start_side = box_bound_side(
            start_coords[axis],
            min_coords[axis],
            max_coords[axis],
            tolerance,
        )?;
        let end_side = box_bound_side(
            end_coords[axis],
            min_coords[axis],
            max_coords[axis],
            tolerance,
        )?;
        if start_side == end_side {
            let dir = match start_side {
                BoundSide::Min => -axes[axis],
                BoundSide::Max => axes[axis],
            };
            candidate_dirs.push(dir);
        } else if measured_axis.replace(axis).is_some() {
            return None;
        }
    }

    measured_axis?;
    if candidate_dirs.len() != 2 {
        return None;
    }
    Some(BoxEdgeReference {
        candidate_dirs: [candidate_dirs[0], candidate_dirs[1]],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundSide {
    Min,
    Max,
}

fn box_bound_side(value: f32, min: f32, max: f32, tolerance: f32) -> Option<BoundSide> {
    if (value - min).abs() <= tolerance {
        Some(BoundSide::Min)
    } else if (value - max).abs() <= tolerance {
        Some(BoundSide::Max)
    } else {
        None
    }
}

fn box_edge_tolerance(bounds: EntityBounds) -> f32 {
    ((bounds.max - bounds.min).length() * 1e-4).max(1e-3)
}

fn bounds_max_along(bounds: EntityBounds, axis: Vec3) -> f32 {
    bounds
        .corners()
        .into_iter()
        .map(|corner| corner.dot(axis))
        .fold(f32::NEG_INFINITY, f32::max)
}

fn host_entity_dimension_reference(
    host_element_id: Option<ElementId>,
    plane: &DrawingPlane,
    query: &Query<(
        &ElementId,
        Option<&bevy::camera::primitives::Aabb>,
        Option<&GlobalTransform>,
    )>,
) -> Option<HostDimensionReference> {
    let bounds = host_entity_world_bounds(host_element_id, query)?;
    Some(HostDimensionReference {
        world_bounds: bounds,
        projected_bounds: projected_bounds_from_world_bounds(bounds, plane),
    })
}

fn host_entity_world_bounds(
    host_element_id: Option<ElementId>,
    query: &Query<(
        &ElementId,
        Option<&bevy::camera::primitives::Aabb>,
        Option<&GlobalTransform>,
    )>,
) -> Option<EntityBounds> {
    let host_element_id = host_element_id?;
    let (_, aabb, transform) = query
        .iter()
        .find(|(element_id, _, _)| **element_id == host_element_id)?;
    Some(aabb_to_world_bounds(aabb?, transform?))
}

fn projected_bounds_from_world_bounds(
    bounds: EntityBounds,
    plane: &DrawingPlane,
) -> ProjectedBounds2d {
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    for corner in bounds.corners() {
        let projected = plane.project_to_2d(corner);
        min = min.min(projected);
        max = max.max(projected);
    }
    ProjectedBounds2d { min, max }
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
    bounds_from_points(
        &local_corners
            .into_iter()
            .map(|corner| transform.transform_point(corner))
            .collect::<Vec<_>>(),
    )
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

pub(crate) fn dimension_line_midpoint(node: &DimensionLineNode) -> Vec3 {
    snapshot_from_node(node).midpoint()
}

pub(crate) fn dimension_line_offset_vector(node: &DimensionLineNode) -> Vec3 {
    snapshot_from_node(node).geometry().offset_vec
}

pub(crate) fn render_dimension_line_projected_primitives(
    node: &DimensionLineNode,
    doc_props: &DocumentProperties,
    start: Vec2,
    end: Vec2,
    offset: Vec2,
    units_per_paper_mm: f32,
) -> Vec<DimPrimitive> {
    let snapshot = snapshot_from_node(node);
    if snapshot.measured_length() < DIMENSION_LINE_MIN_MEASURE_LENGTH {
        return Vec::new();
    }
    let mut style = legacy_dimension_style(snapshot.effective_display_unit(doc_props));
    scale_dimension_style_in_place(&mut style, units_per_paper_mm);
    render_dimension(
        &DimensionInput {
            kind: DraftDimensionKind::Aligned,
            a: start.extend(0.0),
            b: end.extend(0.0),
            offset: offset.extend(0.0),
            text_override: Some(snapshot.display_text(doc_props)),
        },
        &style,
        1.0,
    )
}

fn legacy_dimension_style(display_unit: DisplayUnit) -> DimensionStyle {
    let mut style = match display_unit {
        DisplayUnit::Feet | DisplayUnit::Inches => DimensionStyle::architectural_imperial(),
        _ => DimensionStyle::architectural_metric(),
    };
    style.text_placement = TextPlacement::Centered {
        break_line: false,
        gap_mm: 0.0,
    };
    style.text_color_hex = "4A4A4E".to_string();
    style
}

fn scale_dimension_style_in_place(style: &mut DimensionStyle, factor: f32) {
    style.terminator_size_mm *= factor;
    style.dim_line_extend_past_tick_mm *= factor;
    style.extension_gap_mm *= factor;
    style.extension_past_mm *= factor;
    style.extension_stroke_mm *= factor;
    style.dim_line_stroke_mm *= factor;
    style.first_offset_mm *= factor;
    style.stack_spacing_mm *= factor;
    style.text_height_mm *= factor;
    style.text_placement = match style.text_placement {
        TextPlacement::Above { gap_mm } => TextPlacement::Above {
            gap_mm: gap_mm * factor,
        },
        TextPlacement::Centered { break_line, gap_mm } => TextPlacement::Centered {
            break_line,
            gap_mm: gap_mm * factor,
        },
        TextPlacement::Horizontal { gap_mm } => TextPlacement::Horizontal {
            gap_mm: gap_mm * factor,
        },
    };
}

fn viewport_dimension_style_scale(display_unit: DisplayUnit, scale: f32) -> f32 {
    let text_height = legacy_dimension_style(display_unit).text_height_mm;
    let min_scale = DIMENSION_VIEWPORT_MIN_TEXT_PX / text_height;
    let max_scale = DIMENSION_VIEWPORT_MAX_TEXT_PX / text_height;
    scale.clamp(min_scale, max_scale)
}

fn viewport_pixels_per_world_m(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    anchor: Vec3,
) -> Option<f32> {
    let anchor_px = camera.world_to_viewport(camera_transform, anchor).ok()?;
    let right_px = camera
        .world_to_viewport(
            camera_transform,
            anchor + camera_transform.right().as_vec3(),
        )
        .ok()
        .map(|px| (px - anchor_px).length());
    let up_px = camera
        .world_to_viewport(camera_transform, anchor + camera_transform.up().as_vec3())
        .ok()
        .map(|px| (px - anchor_px).length());
    right_px
        .into_iter()
        .chain(up_px)
        .reduce(f32::max)
        .filter(|length| *length > 0.0)
}

fn draw_dimension_world_text(
    gizmos: &mut Gizmos,
    node: &DimensionLineNode,
    doc_props: &DocumentProperties,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    color: Color,
) {
    let snapshot = snapshot_from_node(node);
    if snapshot.measured_length() < DIMENSION_LINE_MIN_MEASURE_LENGTH {
        return;
    }
    let geometry = snapshot.geometry();
    let Some(pixels_per_world_m) =
        viewport_pixels_per_world_m(camera, camera_transform, geometry.line_midpoint())
    else {
        return;
    };
    let display_unit = snapshot.effective_display_unit(doc_props);
    let text_height_px = (legacy_dimension_style(display_unit).text_height_mm
        * viewport_dimension_style_scale(
            display_unit,
            pixels_per_world_m / DIMENSION_RENDER_PAPER_MM_PER_WORLD_M,
        ))
    .clamp(
        DIMENSION_VIEWPORT_MIN_TEXT_PX,
        DIMENSION_VIEWPORT_MAX_TEXT_PX,
    );
    let text_height_world = text_height_px / pixels_per_world_m;
    let text = snapshot.display_text(doc_props);
    let anchor = geometry.line_midpoint();
    let mut right = geometry.axis_dir;
    let mut glyph_up =
        if projected_world_delta_px(camera, camera_transform, anchor, geometry.offset_dir)
            .is_some_and(|length| length > 1.0)
        {
            geometry.offset_dir
        } else {
            camera_readable_text_up(camera_transform, right).unwrap_or(geometry.offset_dir)
        };
    if let (Ok(anchor_px), Ok(right_px)) = (
        camera.world_to_viewport(camera_transform, anchor),
        camera.world_to_viewport(camera_transform, anchor + right),
    ) {
        let projected_right = right_px - anchor_px;
        if projected_right.x < -1.0 || (projected_right.x.abs() <= 1.0 && projected_right.y > 0.0) {
            right = -right;
            glyph_up = -glyph_up;
        }
    }
    if let (Ok(anchor_px), Ok(up_px)) = (
        camera.world_to_viewport(camera_transform, anchor),
        camera.world_to_viewport(camera_transform, anchor + glyph_up),
    ) {
        if (up_px - anchor_px).y > 0.0 {
            glyph_up = -glyph_up;
        }
    }
    let text_width_world = drafting_text_width(&text) * text_height_world;
    let text_center =
        anchor + glyph_up * (text_height_world * (DIMENSION_WORLD_TEXT_GAP_FACTOR + 0.5));
    let origin =
        text_center - right * (text_width_world * 0.5) - glyph_up * (text_height_world * 0.5);
    draw_drafting_text_glyphs(
        gizmos,
        &text,
        origin,
        right,
        glyph_up,
        text_height_world,
        color,
    );
}

fn projected_world_delta_px(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    anchor: Vec3,
    delta: Vec3,
) -> Option<f32> {
    let anchor_px = camera.world_to_viewport(camera_transform, anchor).ok()?;
    let delta_px = camera
        .world_to_viewport(camera_transform, anchor + delta)
        .ok()?;
    Some((delta_px - anchor_px).length())
}

fn camera_readable_text_up(camera_transform: &GlobalTransform, text_right: Vec3) -> Option<Vec3> {
    let right = text_right.normalize_or_zero();
    for candidate in [
        camera_transform.up().as_vec3(),
        camera_transform.right().as_vec3(),
    ] {
        let rejected = candidate - right * candidate.dot(right);
        if rejected.length_squared() > 1e-6 {
            return Some(rejected.normalize());
        }
    }
    None
}

fn active_dimension_camera<'a>(
    mut cameras: impl Iterator<Item = (&'a Camera, &'a GlobalTransform)>,
) -> Option<(&'a Camera, &'a GlobalTransform)> {
    let first = cameras.next()?;
    if first.0.is_active {
        return Some(first);
    }
    cameras.find(|(camera, _)| camera.is_active).or(Some(first))
}

fn active_dimension_tool_camera<'a>(
    mut cameras: impl Iterator<Item = (&'a Camera, &'a GlobalTransform, &'a OrbitCamera)>,
) -> Option<(&'a Camera, &'a GlobalTransform, &'a OrbitCamera)> {
    let first = cameras.next()?;
    if first.0.is_active {
        return Some(first);
    }
    cameras
        .find(|(camera, _, _)| camera.is_active)
        .or(Some(first))
}

fn active_dimension_tool_transform<'a>(
    cameras: impl Iterator<Item = (&'a GlobalTransform, &'a OrbitCamera)>,
) -> Option<&'a GlobalTransform> {
    cameras.into_iter().next().map(|(transform, _)| transform)
}

fn dimension_overlay_color(selected: bool, paper_style: bool) -> Color {
    match (paper_style, selected) {
        (true, true) => Color::srgb_u8(32, 78, 173),
        (true, false) => Color::srgb_u8(72, 72, 78),
        (false, true) => Color::srgb_u8(106, 152, 255),
        (false, false) => Color::srgb_u8(238, 245, 252),
    }
}

#[derive(Clone, Copy)]
struct DraftingGlyph {
    advance: f32,
    segments: &'static [(Vec2, Vec2)],
}

fn draw_drafting_text_glyphs(
    gizmos: &mut Gizmos,
    text: &str,
    origin: Vec3,
    right: Vec3,
    up: Vec3,
    height: f32,
    color: Color,
) {
    let mut cursor = 0.0;
    for ch in text.chars() {
        let glyph = drafting_glyph(ch);
        for (from, to) in glyph.segments {
            gizmos.line(
                origin + right * ((cursor + from.x) * height) + up * (from.y * height),
                origin + right * ((cursor + to.x) * height) + up * (to.y * height),
                color,
            );
        }
        cursor += glyph.advance + DRAFTING_TEXT_CHAR_SPACING;
    }
}

fn drafting_text_width(text: &str) -> f32 {
    text.chars()
        .map(|ch| drafting_glyph(ch).advance + DRAFTING_TEXT_CHAR_SPACING)
        .sum::<f32>()
        .saturating_sub(DRAFTING_TEXT_CHAR_SPACING)
}

trait SaturatingSubF32 {
    fn saturating_sub(self, rhs: f32) -> f32;
}

impl SaturatingSubF32 for f32 {
    fn saturating_sub(self, rhs: f32) -> f32 {
        (self - rhs).max(0.0)
    }
}

fn drafting_glyph(ch: char) -> DraftingGlyph {
    match ch.to_ascii_lowercase() {
        '0' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_0,
        },
        '1' => DraftingGlyph {
            advance: 0.48,
            segments: GLYPH_1,
        },
        '2' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_2,
        },
        '3' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_3,
        },
        '4' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_4,
        },
        '5' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_5,
        },
        '6' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_6,
        },
        '7' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_7,
        },
        '8' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_8,
        },
        '9' => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_9,
        },
        '.' | ',' => DraftingGlyph {
            advance: 0.22,
            segments: GLYPH_DOT,
        },
        '-' => DraftingGlyph {
            advance: 0.5,
            segments: GLYPH_DASH,
        },
        'm' => DraftingGlyph {
            advance: 0.76,
            segments: GLYPH_M,
        },
        'c' => DraftingGlyph {
            advance: 0.7,
            segments: GLYPH_C,
        },
        'f' => DraftingGlyph {
            advance: 0.62,
            segments: GLYPH_F,
        },
        't' => DraftingGlyph {
            advance: 0.64,
            segments: GLYPH_T,
        },
        'i' => DraftingGlyph {
            advance: 0.22,
            segments: GLYPH_I,
        },
        'n' => DraftingGlyph {
            advance: 0.78,
            segments: GLYPH_N,
        },
        '\'' => DraftingGlyph {
            advance: 0.22,
            segments: GLYPH_APOSTROPHE,
        },
        '"' => DraftingGlyph {
            advance: 0.45,
            segments: GLYPH_QUOTE,
        },
        ' ' => DraftingGlyph {
            advance: 0.45,
            segments: &[],
        },
        _ => DraftingGlyph {
            advance: 0.72,
            segments: GLYPH_BOX,
        },
    }
}

const GLYPH_0: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 0.0), Vec2::new(0.64, 0.0)),
    (Vec2::new(0.64, 0.0), Vec2::new(0.64, 1.0)),
    (Vec2::new(0.64, 1.0), Vec2::new(0.08, 1.0)),
    (Vec2::new(0.08, 1.0), Vec2::new(0.08, 0.0)),
    (Vec2::new(0.12, 0.08), Vec2::new(0.6, 0.92)),
];
const GLYPH_1: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.24, 0.0), Vec2::new(0.24, 1.0)),
    (Vec2::new(0.08, 0.82), Vec2::new(0.24, 1.0)),
    (Vec2::new(0.08, 0.0), Vec2::new(0.42, 0.0)),
];
const GLYPH_2: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 0.82), Vec2::new(0.22, 1.0)),
    (Vec2::new(0.22, 1.0), Vec2::new(0.64, 1.0)),
    (Vec2::new(0.64, 1.0), Vec2::new(0.64, 0.58)),
    (Vec2::new(0.64, 0.58), Vec2::new(0.08, 0.0)),
    (Vec2::new(0.08, 0.0), Vec2::new(0.66, 0.0)),
];
const GLYPH_3: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 1.0), Vec2::new(0.64, 1.0)),
    (Vec2::new(0.64, 1.0), Vec2::new(0.64, 0.0)),
    (Vec2::new(0.08, 0.5), Vec2::new(0.64, 0.5)),
    (Vec2::new(0.08, 0.0), Vec2::new(0.64, 0.0)),
];
const GLYPH_4: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.1, 1.0), Vec2::new(0.1, 0.5)),
    (Vec2::new(0.1, 0.5), Vec2::new(0.66, 0.5)),
    (Vec2::new(0.66, 1.0), Vec2::new(0.66, 0.0)),
];
const GLYPH_5: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.64, 1.0), Vec2::new(0.08, 1.0)),
    (Vec2::new(0.08, 1.0), Vec2::new(0.08, 0.5)),
    (Vec2::new(0.08, 0.5), Vec2::new(0.62, 0.5)),
    (Vec2::new(0.62, 0.5), Vec2::new(0.62, 0.0)),
    (Vec2::new(0.62, 0.0), Vec2::new(0.08, 0.0)),
];
const GLYPH_6: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.62, 1.0), Vec2::new(0.08, 0.5)),
    (Vec2::new(0.08, 0.5), Vec2::new(0.08, 0.0)),
    (Vec2::new(0.08, 0.0), Vec2::new(0.62, 0.0)),
    (Vec2::new(0.62, 0.0), Vec2::new(0.62, 0.5)),
    (Vec2::new(0.62, 0.5), Vec2::new(0.08, 0.5)),
];
const GLYPH_7: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 1.0), Vec2::new(0.66, 1.0)),
    (Vec2::new(0.66, 1.0), Vec2::new(0.2, 0.0)),
];
const GLYPH_8: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 0.0), Vec2::new(0.64, 0.0)),
    (Vec2::new(0.64, 0.0), Vec2::new(0.64, 1.0)),
    (Vec2::new(0.64, 1.0), Vec2::new(0.08, 1.0)),
    (Vec2::new(0.08, 1.0), Vec2::new(0.08, 0.0)),
    (Vec2::new(0.08, 0.5), Vec2::new(0.64, 0.5)),
];
const GLYPH_9: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.62, 0.5), Vec2::new(0.08, 0.5)),
    (Vec2::new(0.08, 0.5), Vec2::new(0.08, 1.0)),
    (Vec2::new(0.08, 1.0), Vec2::new(0.62, 1.0)),
    (Vec2::new(0.62, 1.0), Vec2::new(0.62, 0.0)),
    (Vec2::new(0.62, 0.0), Vec2::new(0.08, 0.0)),
];
const GLYPH_DOT: &[(Vec2, Vec2)] = &[(Vec2::new(0.1, 0.0), Vec2::new(0.1, 0.08))];
const GLYPH_DASH: &[(Vec2, Vec2)] = &[(Vec2::new(0.05, 0.5), Vec2::new(0.45, 0.5))];
const GLYPH_M: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 0.0), Vec2::new(0.08, 0.68)),
    (Vec2::new(0.08, 0.68), Vec2::new(0.32, 0.68)),
    (Vec2::new(0.32, 0.68), Vec2::new(0.32, 0.0)),
    (Vec2::new(0.32, 0.68), Vec2::new(0.56, 0.68)),
    (Vec2::new(0.56, 0.68), Vec2::new(0.56, 0.0)),
];
const GLYPH_C: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.62, 0.7), Vec2::new(0.12, 0.7)),
    (Vec2::new(0.12, 0.7), Vec2::new(0.12, 0.0)),
    (Vec2::new(0.12, 0.0), Vec2::new(0.62, 0.0)),
];
const GLYPH_F: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.12, 0.0), Vec2::new(0.12, 1.0)),
    (Vec2::new(0.12, 1.0), Vec2::new(0.58, 1.0)),
    (Vec2::new(0.12, 0.52), Vec2::new(0.48, 0.52)),
];
const GLYPH_T: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.32, 1.0), Vec2::new(0.32, 0.0)),
    (Vec2::new(0.08, 0.7), Vec2::new(0.58, 0.7)),
];
const GLYPH_I: &[(Vec2, Vec2)] = &[(Vec2::new(0.11, 0.0), Vec2::new(0.11, 0.72))];
const GLYPH_N: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 0.0), Vec2::new(0.08, 0.7)),
    (Vec2::new(0.08, 0.7), Vec2::new(0.68, 0.0)),
    (Vec2::new(0.68, 0.0), Vec2::new(0.68, 0.7)),
];
const GLYPH_APOSTROPHE: &[(Vec2, Vec2)] = &[(Vec2::new(0.1, 1.0), Vec2::new(0.04, 0.72))];
const GLYPH_QUOTE: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 1.0), Vec2::new(0.04, 0.72)),
    (Vec2::new(0.32, 1.0), Vec2::new(0.28, 0.72)),
];
const GLYPH_BOX: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.08, 0.0), Vec2::new(0.64, 0.0)),
    (Vec2::new(0.64, 0.0), Vec2::new(0.64, 1.0)),
    (Vec2::new(0.64, 1.0), Vec2::new(0.08, 1.0)),
    (Vec2::new(0.08, 1.0), Vec2::new(0.08, 0.0)),
];

#[derive(Debug, Clone, Copy)]
struct DimensionFacePick {
    point: Vec3,
    normal: Vec3,
}

#[derive(Debug, Clone, Copy)]
struct DimensionAnchorPick {
    position: Vec3,
    host_element_id: ElementId,
    face: DimensionFacePick,
}

#[derive(Debug, Clone, Copy)]
struct DimensionFaceEdgeConstraint {
    host_element_id: ElementId,
    face: DimensionFacePick,
    host_bounds: EntityBounds,
    preferred_offset_dir: Vec3,
}

fn dimension_tool_anchor_pick_on_face(
    cursor_world_pos: &CursorWorldPos,
    snap_result: &SnapResult,
    host_element_id: ElementId,
    required_face: DimensionFacePick,
    host_bounds_query: Option<
        &Query<(
            &ElementId,
            Option<&bevy::camera::primitives::Aabb>,
            Option<&GlobalTransform>,
        )>,
    >,
) -> Option<DimensionAnchorPick> {
    if snap_result.kind != SnapKind::Endpoint {
        return None;
    }
    let visible_face = DimensionFacePick {
        point: cursor_world_pos.surface_point.or(cursor_world_pos.raw)?,
        normal: cursor_world_pos.surface_normal?.try_normalize()?,
    };
    let host_bounds =
        host_bounds_query.and_then(|query| host_entity_world_bounds(Some(host_element_id), query));
    for candidate in &snap_result.candidates {
        if candidate.kind != SnapKind::Endpoint || candidate.element_id != Some(host_element_id) {
            continue;
        }
        if let Some(face) =
            dimension_anchor_face_pick(candidate.position, visible_face, host_bounds)
        {
            if same_dimension_face(required_face, face) {
                return Some(DimensionAnchorPick {
                    position: candidate.position,
                    host_element_id,
                    face,
                });
            }
        }
    }

    let anchor = dimension_tool_anchor_pick(cursor_world_pos, snap_result, host_bounds_query)?;
    (anchor.host_element_id == host_element_id && same_dimension_face(required_face, anchor.face))
        .then_some(anchor)
}

fn dimension_tool_anchor_pick(
    cursor_world_pos: &CursorWorldPos,
    snap_result: &SnapResult,
    host_bounds_query: Option<
        &Query<(
            &ElementId,
            Option<&bevy::camera::primitives::Aabb>,
            Option<&GlobalTransform>,
        )>,
    >,
) -> Option<DimensionAnchorPick> {
    if snap_result.kind != SnapKind::Endpoint {
        return None;
    }
    let position = snap_result.position?;
    let host_element_id = cursor_world_pos.hovered_element_id?;
    let visible_face = DimensionFacePick {
        point: cursor_world_pos.surface_point.or(cursor_world_pos.raw)?,
        normal: cursor_world_pos.surface_normal?.try_normalize()?,
    };
    let host_bounds =
        host_bounds_query.and_then(|query| host_entity_world_bounds(Some(host_element_id), query));
    let face = dimension_anchor_face_pick(position, visible_face, host_bounds)?;
    Some(DimensionAnchorPick {
        position,
        host_element_id,
        face,
    })
}

fn dimension_tool_anchor_position(
    cursor_world_pos: &CursorWorldPos,
    snap_result: &SnapResult,
    host_bounds_query: Option<
        &Query<(
            &ElementId,
            Option<&bevy::camera::primitives::Aabb>,
            Option<&GlobalTransform>,
        )>,
    >,
) -> Option<Vec3> {
    dimension_tool_anchor_pick(cursor_world_pos, snap_result, host_bounds_query)
        .map(|pick| pick.position)
}

fn dimension_tool_offset_cursor_position(
    cursor_world_pos: &CursorWorldPos,
    snap_result: &SnapResult,
) -> Option<Vec3> {
    snap_result
        .raw_position
        .or(cursor_world_pos.raw)
        .or(cursor_world_pos.snapped)
        .or(snap_result.position)
}

fn dimension_tool_preview_cursor_position(
    tool_state: &DimensionLineToolState,
    cursor_world_pos: &CursorWorldPos,
    snap_result: &SnapResult,
    host_bounds_query: Option<
        &Query<(
            &ElementId,
            Option<&bevy::camera::primitives::Aabb>,
            Option<&GlobalTransform>,
        )>,
    >,
) -> Option<Vec3> {
    match tool_state.stage {
        DimensionLinePlacementStage::AwaitingStart
        | DimensionLinePlacementStage::AwaitingEnd { .. } => {
            dimension_tool_anchor_position(cursor_world_pos, snap_result, host_bounds_query)
        }
        DimensionLinePlacementStage::PlacingOffset { .. } => {
            dimension_tool_offset_cursor_position(cursor_world_pos, snap_result)
        }
    }
}

fn dimension_anchor_face_pick(
    position: Vec3,
    visible_face: DimensionFacePick,
    host_bounds: Option<EntityBounds>,
) -> Option<DimensionFacePick> {
    if point_lies_on_face_plane(position, visible_face) {
        return Some(visible_face);
    }
    host_bounds.and_then(|bounds| bounds_face_pick_for_corner(position, visible_face, bounds))
}

fn bounds_face_pick_for_corner(
    position: Vec3,
    visible_face: DimensionFacePick,
    bounds: EntityBounds,
) -> Option<DimensionFacePick> {
    let visible_normal = visible_face.normal.try_normalize()?;
    let tolerance = DIMENSION_FACE_PLANE_TOLERANCE.max(box_edge_tolerance(bounds) * 4.0);
    let coords = [position.x, position.y, position.z];
    let face_coords = [
        visible_face.point.x,
        visible_face.point.y,
        visible_face.point.z,
    ];
    let min_coords = [bounds.min.x, bounds.min.y, bounds.min.z];
    let max_coords = [bounds.max.x, bounds.max.y, bounds.max.z];
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];

    let mut best: Option<(f32, DimensionFacePick)> = None;
    for axis in 0..3 {
        let Some(side) =
            box_bound_side(coords[axis], min_coords[axis], max_coords[axis], tolerance)
        else {
            continue;
        };
        let plane_coord = match side {
            BoundSide::Min => min_coords[axis],
            BoundSide::Max => max_coords[axis],
        };
        let face_distance = (face_coords[axis] - plane_coord).abs();
        if face_distance > tolerance && visible_normal.dot(axes[axis]).abs() < 0.7 {
            continue;
        }
        let normal = match side {
            BoundSide::Min => -axes[axis],
            BoundSide::Max => axes[axis],
        };
        let score = visible_normal.dot(normal).abs() - face_distance * 0.01;
        let point = match axis {
            0 => Vec3::new(plane_coord, visible_face.point.y, visible_face.point.z),
            1 => Vec3::new(visible_face.point.x, plane_coord, visible_face.point.z),
            _ => Vec3::new(visible_face.point.x, visible_face.point.y, plane_coord),
        };
        let pick = DimensionFacePick { point, normal };
        if best.is_none_or(|(best_score, _)| score > best_score) {
            best = Some((score, pick));
        }
    }
    best.map(|(_, pick)| pick)
}

fn dimension_face_edge_constraint(
    start: Vec3,
    end: DimensionAnchorPick,
    host_element_id: ElementId,
    start_face: DimensionFacePick,
    host_bounds_query: &Query<(
        &ElementId,
        Option<&bevy::camera::primitives::Aabb>,
        Option<&GlobalTransform>,
    )>,
) -> Option<DimensionFaceEdgeConstraint> {
    if end.host_element_id != host_element_id || !same_dimension_face(start_face, end.face) {
        return None;
    }
    let host_bounds = host_entity_world_bounds(Some(host_element_id), host_bounds_query)?;
    box_edge_reference(start, end.position, host_bounds)?;
    let preferred_offset_dir =
        inward_face_edge_offset_dir(start, end.position, start_face, host_bounds)?;
    Some(DimensionFaceEdgeConstraint {
        host_element_id,
        face: start_face,
        host_bounds,
        preferred_offset_dir,
    })
}

fn point_lies_on_face_plane(point: Vec3, face: DimensionFacePick) -> bool {
    (point - face.point).dot(face.normal).abs() <= DIMENSION_FACE_PLANE_TOLERANCE
}

fn same_dimension_face(left: DimensionFacePick, right: DimensionFacePick) -> bool {
    left.normal.dot(right.normal).abs() >= DIMENSION_FACE_NORMAL_ALIGNMENT
        && point_lies_on_face_plane(right.point, left)
}

fn face_dimension_plane(face: DimensionFacePick) -> DrawingPlane {
    DrawingPlane::from_face(face.point, face.normal)
}

fn project_point_to_face_plane(point: Vec3, face: DimensionFacePick) -> Vec3 {
    point - face.normal * (point - face.point).dot(face.normal)
}

fn inward_face_edge_offset_dir(
    start: Vec3,
    end: Vec3,
    face: DimensionFacePick,
    bounds: EntityBounds,
) -> Option<Vec3> {
    let axis_dir = (end - start).try_normalize()?;
    let midpoint = (start + end) * 0.5;
    let toward_center = bounds.center() - midpoint;
    let in_face = toward_center - face.normal * toward_center.dot(face.normal);
    let perpendicular = in_face - axis_dir * in_face.dot(axis_dir);
    perpendicular.try_normalize().or_else(|| {
        face.normal
            .cross(axis_dir)
            .try_normalize()
            .or_else(|| axis_dir.cross(face.normal).try_normalize())
    })
}

fn preferred_face_dimension_offset_dir(
    start: Vec3,
    end: Vec3,
    face: DimensionFacePick,
    bounds: EntityBounds,
    camera_position: Option<Vec3>,
) -> Option<Vec3> {
    let fallback = inward_face_edge_offset_dir(start, end, face, bounds)?;
    let axis_dir = (end - start).try_normalize()?;
    let midpoint = (start + end) * 0.5;
    let camera_side = camera_position.and_then(|position| {
        let projected = position - midpoint;
        let in_face = projected - face.normal * projected.dot(face.normal);
        (in_face - axis_dir * in_face.dot(axis_dir)).try_normalize()
    });
    Some(match camera_side {
        Some(camera_side) if fallback.dot(camera_side) < 0.0 => -fallback,
        _ => fallback,
    })
}

fn guided_face_dimension_line_request(
    start: Vec3,
    end: Vec3,
    requested_line_point: Vec3,
    face: DimensionFacePick,
    preferred_offset_dir: Vec3,
) -> Vec3 {
    let Some(preferred_offset_dir) = preferred_offset_dir.try_normalize() else {
        return project_point_to_face_plane(requested_line_point, face);
    };
    let Some(axis_dir) = (end - start).try_normalize() else {
        return project_point_to_face_plane(requested_line_point, face);
    };
    let midpoint = (start + end) * 0.5;
    let requested_on_face = project_point_to_face_plane(requested_line_point, face);
    let requested_offset = requested_on_face - midpoint;
    let perpendicular_offset = requested_offset - axis_dir * requested_offset.dot(axis_dir);
    let offset_dir = if perpendicular_offset.dot(preferred_offset_dir) < 0.0 {
        -preferred_offset_dir
    } else {
        preferred_offset_dir
    };
    let distance = perpendicular_offset
        .length()
        .max(default_dimension_offset(start, end));
    project_point_to_face_plane(midpoint + offset_dir * distance, face)
}

fn preview_dimension_line_node(
    tool_state: &DimensionLineToolState,
    cursor_position: Vec3,
    _drawing_plane: &DrawingPlane,
    _orbit: Option<&OrbitCamera>,
    _camera_transform: &GlobalTransform,
    host_bounds_query: &Query<(
        &ElementId,
        Option<&bevy::camera::primitives::Aabb>,
        Option<&GlobalTransform>,
    )>,
) -> Option<DimensionLineNode> {
    match tool_state.stage {
        DimensionLinePlacementStage::AwaitingStart => None,
        DimensionLinePlacementStage::AwaitingEnd { start, .. } => {
            if start.distance(cursor_position) < DIMENSION_LINE_MIN_MEASURE_LENGTH {
                return None;
            }
            let end = cursor_position;
            Some(DimensionLineNode {
                start,
                end,
                line_point: default_dimension_line_point(start, end),
                extension: default_dimension_extension(start, end),
                visible: true,
                label: None,
                display_unit: None,
                precision: None,
            })
        }
        DimensionLinePlacementStage::PlacingOffset {
            start,
            end,
            host_element_id,
            face,
            preferred_offset_dir,
            ..
        } => {
            let active_plane = face_dimension_plane(face);
            let host_reference = host_entity_dimension_reference(
                Some(host_element_id),
                &active_plane,
                host_bounds_query,
            );
            let requested_line_point = guided_face_dimension_line_request(
                start,
                end,
                cursor_position,
                face,
                preferred_offset_dir,
            );
            Some(DimensionLineNode {
                start,
                end,
                line_point: resolved_dimension_line_point(
                    start,
                    end,
                    requested_line_point,
                    &active_plane,
                    host_reference,
                ),
                extension: default_dimension_extension(start, end),
                visible: true,
                label: None,
                display_unit: None,
                precision: None,
            })
        }
    }
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

        assert_eq!(snapshot.display_text(&doc_props), "8.20ft");
    }

    #[test]
    fn dimension_display_text_treats_label_as_metadata_only() {
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(8),
            start: Vec3::ZERO,
            end: Vec3::new(7.2277, 0.0, 0.0),
            line_point: Vec3::new(3.6, -0.5, 0.0),
            extension: 0.25,
            visible: true,
            label: Some("Foundation length".to_string()),
            display_unit: Some(DisplayUnit::Metres),
            precision: Some(2),
        };

        assert_eq!(
            snapshot.display_text(&DocumentProperties::default()),
            "7.23m"
        );
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
    fn resolved_dimension_line_point_pushes_dimension_outside_host_projection() {
        let plane = DrawingPlane::ground();
        let resolved = resolved_dimension_line_point(
            Vec3::ZERO,
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, -0.05),
            &plane,
            Some(HostDimensionReference {
                world_bounds: EntityBounds {
                    min: Vec3::new(-1.0, 0.0, -0.4),
                    max: Vec3::new(3.0, 1.0, 0.4),
                },
                projected_bounds: ProjectedBounds2d {
                    min: Vec2::new(0.0, -0.4),
                    max: Vec2::new(2.0, 0.4),
                },
            }),
        );

        assert!(
            (resolved.z + 0.76).abs() < 1e-3,
            "resolved point was {resolved:?}"
        );
    }

    #[test]
    fn resolved_dimension_line_point_projects_drag_perpendicular_to_baseline() {
        let plane = DrawingPlane::ground();
        let start = Vec3::ZERO;
        let end = Vec3::new(2.0, 0.0, 0.0);
        let requested = Vec3::new(1.7, 0.0, -0.5);

        let resolved = resolved_dimension_line_point(start, end, requested, &plane, None);
        let offset = resolved - (start + end) * 0.5;

        assert!(
            offset.dot((end - start).normalize()).abs() < 1e-5,
            "offset should be perpendicular to baseline: {offset:?}"
        );
        assert!((resolved.z + 0.5).abs() < 1e-5);
    }

    #[test]
    fn face_edge_dimension_request_stays_on_face_plane_and_defaults_toward_face_interior() {
        let bounds = EntityBounds {
            min: Vec3::new(0.0, 0.0, -0.2),
            max: Vec3::new(10.0, 3.0, 0.2),
        };
        let face = DimensionFacePick {
            point: Vec3::new(0.0, 1.0, -0.2),
            normal: Vec3::NEG_Z,
        };
        let start = Vec3::new(0.0, 3.0, -0.2);
        let end = Vec3::new(10.0, 3.0, -0.2);

        let offset_dir =
            inward_face_edge_offset_dir(start, end, face, bounds).expect("face edge should guide");
        let requested = guided_face_dimension_line_request(
            start,
            end,
            Vec3::new(5.0, 3.0, 8.0),
            face,
            offset_dir,
        );

        assert!(
            offset_dir.dot(Vec3::NEG_Y) > 0.99,
            "top wall edge should offset toward the face interior: {offset_dir:?}"
        );
        assert!(
            (requested.z - face.point.z).abs() < 1e-5,
            "dimension request must stay on the picked face plane: {requested:?}"
        );
        assert!(
            requested.y < start.y,
            "dimension should extend along the face, not away from it: {requested:?}"
        );
    }

    #[test]
    fn dimension_anchor_requires_visible_face_endpoint_snap() {
        let cursor_world_pos = CursorWorldPos {
            raw: Some(Vec3::new(0.0, 3.0, -0.2)),
            snapped: Some(Vec3::new(0.0, 3.0, -0.2)),
            hovered_element_id: Some(ElementId(42)),
            surface_point: Some(Vec3::new(0.0, 1.5, -0.2)),
            surface_normal: Some(Vec3::NEG_Z),
        };
        let grid_snap = SnapResult {
            raw_position: Some(Vec3::new(0.0, 3.0, -0.2)),
            position: Some(Vec3::new(0.0, 3.0, -0.2)),
            target: Some(Vec3::new(0.0, 3.0, -0.2)),
            kind: SnapKind::Grid,
            candidates: Vec::new(),
            active_candidate: 0,
        };
        let endpoint_snap = SnapResult {
            kind: SnapKind::Endpoint,
            ..grid_snap.clone()
        };

        assert!(
            dimension_tool_anchor_pick(&cursor_world_pos, &grid_snap, None).is_none(),
            "dimensions must not start from raw/grid cursor hits"
        );
        assert!(
            dimension_tool_anchor_pick(&cursor_world_pos, &endpoint_snap, None).is_some(),
            "visible endpoint on the hovered face should be accepted"
        );
    }

    #[test]
    fn dimension_anchor_accepts_bounds_corner_when_surface_hit_is_slightly_offset() {
        let visible_face = DimensionFacePick {
            point: Vec3::new(5.0, 1.5, -0.12),
            normal: Vec3::NEG_Z,
        };
        let bounds = EntityBounds {
            min: Vec3::new(0.0, 0.0, -0.2),
            max: Vec3::new(10.0, 3.0, 0.2),
        };
        let position = Vec3::new(0.0, 3.0, -0.2);

        let face = dimension_anchor_face_pick(position, visible_face, Some(bounds))
            .expect("bounds corner should infer the visible bounds face");

        assert!(
            (face.point.z - bounds.min.z).abs() < 1e-5,
            "face should be corrected onto the measured bounds plane: {face:?}"
        );
        assert!(face.normal.dot(Vec3::NEG_Z) > 0.99);
    }

    #[test]
    fn second_dimension_anchor_prefers_candidate_on_first_selected_face() {
        let host = ElementId(42);
        let required_face = DimensionFacePick {
            point: Vec3::new(0.0, 0.0, -0.2),
            normal: Vec3::NEG_Z,
        };
        let cursor_world_pos = CursorWorldPos {
            raw: Some(Vec3::new(5.0, 1.5, -0.2)),
            snapped: Some(Vec3::new(0.0, 3.0, 0.2)),
            hovered_element_id: Some(host),
            surface_point: Some(Vec3::new(5.0, 1.5, -0.2)),
            surface_normal: Some(Vec3::NEG_Z),
        };
        let snap_result = SnapResult {
            raw_position: cursor_world_pos.raw,
            position: Some(Vec3::new(0.0, 3.0, 0.2)),
            target: Some(Vec3::new(0.0, 3.0, 0.2)),
            kind: SnapKind::Endpoint,
            candidates: vec![
                crate::plugins::snap::SnapCandidate {
                    position: Vec3::new(0.0, 3.0, 0.2),
                    kind: SnapKind::Endpoint,
                    element_id: Some(host),
                    label: Some("wrong_face".to_string()),
                },
                crate::plugins::snap::SnapCandidate {
                    position: Vec3::new(10.0, 3.0, -0.2),
                    kind: SnapKind::Endpoint,
                    element_id: Some(host),
                    label: Some("same_face".to_string()),
                },
            ],
            active_candidate: 0,
        };

        let anchor = dimension_tool_anchor_pick_on_face(
            &cursor_world_pos,
            &snap_result,
            host,
            required_face,
            None,
        )
        .expect("same-face candidate should be inferred from overlapping corners");

        assert_eq!(anchor.position, Vec3::new(10.0, 3.0, -0.2));
    }

    #[test]
    fn diagonal_face_points_are_not_valid_dimension_edges() {
        let bounds = EntityBounds {
            min: Vec3::new(0.0, 0.0, -0.2),
            max: Vec3::new(10.0, 3.0, 0.2),
        };

        assert!(
            box_edge_reference(
                Vec3::new(0.0, 3.0, -0.2),
                Vec3::new(10.0, 0.0, -0.2),
                bounds,
            )
            .is_none(),
            "diagonal points across a face must not produce a free-space dimension"
        );
    }

    #[test]
    fn box_edge_dimension_line_point_offsets_from_nearest_box_side() {
        let bounds = EntityBounds {
            min: Vec3::new(0.0, 0.0, -0.2),
            max: Vec3::new(10.0, 3.0, 0.2),
        };
        let start = Vec3::new(0.0, 3.0, -0.2);
        let end = Vec3::new(10.0, 3.0, -0.2);

        let outside = constrained_box_edge_dimension_line_point(
            start,
            end,
            Vec3::new(5.0, 3.0, -0.8),
            bounds,
        )
        .expect("wall top exterior edge should constrain");

        assert!(
            outside.z < bounds.min.z,
            "dimension should extend outward from the picked exterior wall side: {outside:?}"
        );
        assert!(
            (outside.y - bounds.max.y).abs() < 1e-5,
            "dimension should stay straight off the measured wall edge: {outside:?}"
        );
    }

    #[test]
    fn box_edge_dimension_line_point_uses_only_adjacent_edge_directions() {
        let bounds = EntityBounds {
            min: Vec3::ZERO,
            max: Vec3::new(4.0, 3.0, 2.0),
        };
        let start = Vec3::ZERO;
        let end = Vec3::new(4.0, 0.0, 0.0);

        let toward_y = constrained_box_edge_dimension_line_point(
            start,
            end,
            Vec3::new(2.0, -1.2, 0.2),
            bounds,
        )
        .expect("box edge should constrain");
        let toward_z = constrained_box_edge_dimension_line_point(
            start,
            end,
            Vec3::new(2.0, 0.2, -1.2),
            bounds,
        )
        .expect("box edge should constrain");

        assert!(
            (toward_y.z - 0.0).abs() < 1e-5,
            "Y-side dimension must not drift diagonally: {toward_y:?}"
        );
        assert!(
            (toward_z.y - 0.0).abs() < 1e-5,
            "Z-side dimension must not drift diagonally: {toward_z:?}"
        );
        assert!(
            toward_y.y < bounds.min.y && toward_z.z < bounds.min.z,
            "dimension line should be pushed outside the chosen box side"
        );
    }

    #[test]
    fn non_box_edge_dimension_line_point_keeps_general_aligned_behavior() {
        let bounds = EntityBounds {
            min: Vec3::ZERO,
            max: Vec3::new(4.0, 3.0, 2.0),
        };

        assert!(constrained_box_edge_dimension_line_point(
            Vec3::new(0.5, 0.0, 0.0),
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(2.0, 1.0, 1.0),
            bounds,
        )
        .is_none());
    }

    #[test]
    fn dimension_offset_preview_uses_raw_cursor_instead_of_snapped_anchor() {
        let tool_state = DimensionLineToolState {
            stage: DimensionLinePlacementStage::PlacingOffset {
                start: Vec3::ZERO,
                end: Vec3::X,
                host_element_id: ElementId(1),
                face: DimensionFacePick {
                    point: Vec3::ZERO,
                    normal: Vec3::Y,
                },
                preferred_offset_dir: Vec3::Z,
                drag_origin: Some(Vec3::X),
                dragging: true,
            },
        };
        let cursor_world_pos = CursorWorldPos {
            raw: Some(Vec3::new(0.5, 0.0, -0.8)),
            snapped: Some(Vec3::new(10.0, 0.0, 10.0)),
            hovered_element_id: None,
            surface_point: None,
            surface_normal: None,
        };
        let snap_result = SnapResult {
            raw_position: Some(Vec3::new(0.5, 0.0, -0.8)),
            position: Some(Vec3::new(10.0, 0.0, 10.0)),
            target: Some(Vec3::new(10.0, 0.0, 10.0)),
            kind: SnapKind::Endpoint,
            candidates: Vec::new(),
            active_candidate: 0,
        };

        assert_eq!(
            dimension_tool_preview_cursor_position(
                &tool_state,
                &cursor_world_pos,
                &snap_result,
                None
            ),
            Some(Vec3::new(0.5, 0.0, -0.8))
        );
    }

    #[test]
    fn dimension_overlay_camera_selection_ignores_inactive_auxiliary_cameras() {
        let inactive = Camera {
            is_active: false,
            ..default()
        };
        let active = Camera {
            is_active: true,
            ..default()
        };
        let inactive_transform = GlobalTransform::from_xyz(1.0, 0.0, 0.0);
        let active_transform = GlobalTransform::from_xyz(2.0, 0.0, 0.0);

        let (_, selected_transform) = active_dimension_camera(
            vec![
                (&inactive, &inactive_transform),
                (&active, &active_transform),
            ]
            .into_iter(),
        )
        .expect("an active camera should be selected");

        assert_eq!(selected_transform.translation(), Vec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn viewport_dimension_style_scale_keeps_labels_readable() {
        let style = legacy_dimension_style(DisplayUnit::Metres);
        let min_scale = DIMENSION_VIEWPORT_MIN_TEXT_PX / style.text_height_mm;
        let max_scale = DIMENSION_VIEWPORT_MAX_TEXT_PX / style.text_height_mm;

        assert_eq!(
            viewport_dimension_style_scale(DisplayUnit::Metres, 0.1),
            min_scale
        );
        assert_eq!(
            viewport_dimension_style_scale(DisplayUnit::Metres, 10_000.0),
            max_scale
        );
        assert_eq!(
            viewport_dimension_style_scale(DisplayUnit::Metres, (min_scale + max_scale) * 0.5),
            (min_scale + max_scale) * 0.5
        );
    }

    #[test]
    fn dimension_annotations_restore_from_document_metadata() {
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(42),
            start: Vec3::ZERO,
            end: Vec3::new(2.0, 0.0, 0.0),
            line_point: Vec3::new(1.0, 0.0, -0.5),
            extension: 0.24,
            visible: true,
            label: Some("Width".to_string()),
            display_unit: Some(DisplayUnit::Centimetres),
            precision: Some(1),
        };
        let serialized = serde_json::to_value(vec![snapshot.clone()]).expect("serialize snapshot");

        let mut app = App::new();
        let mut doc_props = DocumentProperties::default();
        doc_props
            .domain_defaults
            .insert(DIMENSION_ANNOTATIONS_KEY.to_string(), serialized);
        app.insert_resource(doc_props)
            .init_resource::<DimensionAnnotationSyncState>()
            .init_resource::<LayerRegistry>()
            .add_systems(Update, sync_dimension_annotations);

        app.update();

        let world = app.world_mut();
        let mut query = world.query::<(&ElementId, &DimensionLineNode, &LayerAssignment)>();
        let restored = query
            .iter(world)
            .next()
            .expect("dimension should be restored from metadata");
        assert_eq!(*restored.0, ElementId(42));
        assert_eq!(restored.1.label.as_deref(), Some("Width"));
        assert_eq!(restored.1.display_unit, Some(DisplayUnit::Centimetres));
        assert_eq!(restored.1.precision, Some(1));
        assert_eq!(restored.2.layer, DIMENSION_LAYER_NAME);
        assert!(world
            .resource::<LayerRegistry>()
            .layers
            .contains_key(DIMENSION_LAYER_NAME));
    }

    #[test]
    fn dimension_snapshot_apply_assigns_dimensions_layer() {
        let mut world = World::new();
        world.insert_resource(LayerRegistry::default());
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(61),
            start: Vec3::ZERO,
            end: Vec3::new(2.0, 0.0, 0.0),
            line_point: Vec3::new(1.0, 0.0, -0.5),
            extension: 0.24,
            visible: true,
            label: None,
            display_unit: None,
            precision: None,
        };

        snapshot.apply_to(&mut world);

        let mut query = world.query::<(&ElementId, &LayerAssignment)>();
        let (_, assignment) = query
            .iter(&world)
            .find(|(element_id, _)| **element_id == ElementId(61))
            .expect("dimension entity should exist");
        assert_eq!(assignment.layer, DIMENSION_LAYER_NAME);
        assert!(world
            .resource::<LayerRegistry>()
            .layers
            .contains_key(DIMENSION_LAYER_NAME));
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

    #[test]
    fn placed_dimension_geometry_is_not_transformable() {
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(72),
            start: Vec3::ZERO,
            end: Vec3::new(2.0, 0.0, 0.0),
            line_point: Vec3::new(1.0, 0.0, 0.5),
            extension: 0.2,
            visible: true,
            label: None,
            display_unit: None,
            precision: None,
        };

        assert_eq!(
            snapshot.translate_by(Vec3::new(10.0, 0.0, 0.0)).0.to_json(),
            snapshot.to_json()
        );
        assert_eq!(
            snapshot.rotate_by(Quat::from_rotation_y(1.0)).0.to_json(),
            snapshot.to_json()
        );
        assert_eq!(
            snapshot
                .scale_by(Vec3::splat(2.0), Vec3::new(1.0, 0.0, 0.0))
                .0
                .to_json(),
            snapshot.to_json()
        );
        assert!(snapshot.handles().is_empty());
        assert!(snapshot.drag_handle("line_point", Vec3::Y).is_none());
    }

    #[test]
    fn dimension_geometry_properties_are_read_only_after_placement() {
        let snapshot = DimensionLineSnapshot {
            element_id: ElementId(73),
            start: Vec3::ZERO,
            end: Vec3::new(2.0, 0.0, 0.0),
            line_point: Vec3::new(1.0, 0.0, 0.5),
            extension: 0.2,
            visible: true,
            label: None,
            display_unit: None,
            precision: None,
        };
        let fields = snapshot.property_fields();
        for name in ["start", "end", "line_point", "offset", "extension"] {
            let field = fields
                .iter()
                .find(|field| field.name == name)
                .expect("dimension geometry field should be exposed");
            assert!(!field.editable, "{name} should be read-only");
        }
        assert!(fields
            .iter()
            .find(|field| field.name == "label")
            .is_some_and(|field| field.editable));
    }
}
