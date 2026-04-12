use std::any::Any;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    authored_entity::{
        invalid_property_error, property_field_with, AuthoredEntity, BoxedEntity, EntityBounds,
        HandleInfo, PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::CapabilityRegistryAppExt,
    capability_registry::{AuthoredEntityFactory, HitCandidate, ModelSummaryAccumulator},
    capability_registry::{CapabilityDescriptor, CapabilityDistribution, CapabilityMaturity},
    plugins::{
        command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt},
        commands::{despawn_by_element_id, find_entity_by_element_id},
        document_properties::DocumentProperties,
        identity::{ElementId, ElementIdAllocator},
    },
};

pub struct LightingPlugin;
const LIGHT_OBJECT_VISIBILITY_KEY: &str = "scene_light_object_visibility";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SceneLightKind {
    #[default]
    Directional,
    Point,
    Spot,
}

impl SceneLightKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Directional => "directional",
            Self::Point => "point",
            Self::Spot => "spot",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "directional" | "sun" => Some(Self::Directional),
            "point" => Some(Self::Point),
            "spot" | "spotlight" => Some(Self::Spot),
            _ => None,
        }
    }
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneLightNode {
    pub name: String,
    pub kind: SceneLightKind,
    pub enabled: bool,
    pub color: [f32; 3],
    /// Directional lights use lux; point/spot lights use lumens.
    pub intensity: f32,
    pub shadows_enabled: bool,
    pub range: f32,
    pub radius: f32,
    pub inner_angle_deg: f32,
    pub outer_angle_deg: f32,
}

impl Default for SceneLightNode {
    fn default() -> Self {
        Self {
            name: "Light".to_string(),
            kind: SceneLightKind::Directional,
            enabled: true,
            color: [1.0, 1.0, 1.0],
            intensity: 8_000.0,
            shadows_enabled: true,
            range: 20.0,
            radius: 0.2,
            inner_angle_deg: 18.0,
            outer_angle_deg: 32.0,
        }
    }
}

#[derive(Resource, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneLightingSettings {
    pub ambient_color: [f32; 3],
    pub ambient_brightness: f32,
    pub affects_lightmapped_meshes: bool,
}

impl Default for SceneLightingSettings {
    fn default() -> Self {
        Self {
            ambient_color: [0.9, 0.92, 1.0],
            ambient_brightness: 40.0,
            affects_lightmapped_meshes: true,
        }
    }
}

#[derive(Resource, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneLightObjectVisibility {
    pub visible: bool,
}

impl Default for SceneLightObjectVisibility {
    fn default() -> Self {
        Self { visible: false }
    }
}

pub fn scene_light_objects_visible(world: &World) -> bool {
    world
        .get_resource::<SceneLightObjectVisibility>()
        .map(|state| state.visible)
        .unwrap_or(false)
}

pub fn scene_light_object_exposed(entity_ref: &bevy::ecs::world::EntityRef, world: &World) -> bool {
    !entity_ref.contains::<SceneLightNode>() || scene_light_objects_visible(world)
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneLightSnapshot {
    pub element_id: ElementId,
    pub node: SceneLightNode,
    pub translation: Vec3,
    pub rotation: Quat,
}

impl SceneLightSnapshot {
    pub fn default_directional(
        allocator: &ElementIdAllocator,
        name: impl Into<String>,
        translation: Vec3,
        look_target: Vec3,
        color: [f32; 3],
        intensity: f32,
        shadows_enabled: bool,
    ) -> Self {
        let mut transform = Transform::from_translation(translation);
        transform.look_at(look_target, Vec3::Y);
        Self {
            element_id: allocator.next_id(),
            node: SceneLightNode {
                name: name.into(),
                kind: SceneLightKind::Directional,
                color,
                intensity,
                shadows_enabled,
                ..Default::default()
            },
            translation,
            rotation: transform.rotation,
        }
    }

    pub fn default_point(
        allocator: &ElementIdAllocator,
        name: impl Into<String>,
        translation: Vec3,
    ) -> Self {
        Self {
            element_id: allocator.next_id(),
            node: SceneLightNode {
                name: name.into(),
                kind: SceneLightKind::Point,
                intensity: 2_500.0,
                shadows_enabled: true,
                range: 18.0,
                radius: 0.25,
                ..Default::default()
            },
            translation,
            rotation: Quat::IDENTITY,
        }
    }

    pub fn default_spot(
        allocator: &ElementIdAllocator,
        name: impl Into<String>,
        translation: Vec3,
        look_target: Vec3,
    ) -> Self {
        let mut transform = Transform::from_translation(translation);
        transform.look_at(look_target, Vec3::Y);
        Self {
            element_id: allocator.next_id(),
            node: SceneLightNode {
                name: name.into(),
                kind: SceneLightKind::Spot,
                intensity: 4_000.0,
                shadows_enabled: true,
                range: 25.0,
                radius: 0.15,
                inner_angle_deg: 16.0,
                outer_angle_deg: 28.0,
                ..Default::default()
            },
            translation,
            rotation: transform.rotation,
        }
    }
}

impl AuthoredEntity for SceneLightSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "scene_light"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.node.name.clone()
    }

    fn center(&self) -> Vec3 {
        self.translation
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.translation += delta;
        snapshot.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.translation = rotation * snapshot.translation;
        snapshot.rotation = rotation * snapshot.rotation;
        snapshot.into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.translation = center + (snapshot.translation - center) * factor;
        snapshot.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let (yaw, pitch, _roll) = self.rotation.to_euler(EulerRot::YXZ);
        vec![
            property_field_with(
                "name",
                "Name",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.node.name.clone())),
                false,
            ),
            property_field_with(
                "kind",
                "Kind",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.node.kind.as_str().to_string())),
                false,
            ),
            property_field_with(
                "enabled",
                "Enabled",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(
                    if self.node.enabled { "true" } else { "false" }.to_string(),
                )),
                false,
            ),
            property_field_with(
                "position",
                "Position",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(self.translation)),
                false,
            ),
            property_field_with(
                "yaw_deg",
                "Yaw",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(yaw.to_degrees())),
                false,
            ),
            property_field_with(
                "pitch_deg",
                "Pitch",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(pitch.to_degrees())),
                false,
            ),
            property_field_with(
                "color",
                "Color",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(Vec3::from_array(self.node.color))),
                false,
            ),
            property_field_with(
                "intensity",
                "Intensity",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.intensity)),
                false,
            ),
            property_field_with(
                "shadows_enabled",
                "Shadows",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(
                    if self.node.shadows_enabled {
                        "true"
                    } else {
                        "false"
                    }
                    .to_string(),
                )),
                false,
            ),
            property_field_with(
                "range",
                "Range",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.range)),
                false,
            ),
            property_field_with(
                "radius",
                "Radius",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.radius)),
                false,
            ),
            property_field_with(
                "inner_angle_deg",
                "Inner Angle",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.inner_angle_deg)),
                false,
            ),
            property_field_with(
                "outer_angle_deg",
                "Outer Angle",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.node.outer_angle_deg)),
                false,
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "name" => {
                snapshot.node.name = value
                    .as_str()
                    .ok_or_else(|| "name must be a string".to_string())?
                    .to_string();
            }
            "kind" => {
                let kind = value
                    .as_str()
                    .and_then(SceneLightKind::from_str)
                    .ok_or_else(|| "kind must be directional, point, or spot".to_string())?;
                snapshot.node.kind = kind;
            }
            "enabled" => {
                snapshot.node.enabled = parse_bool(value, "enabled")?;
            }
            "position" => {
                snapshot.translation = vec3_from_json(value)?;
            }
            "yaw_deg" => {
                let pitch = extract_pitch(snapshot.rotation);
                let yaw = scalar_from_json(value)?;
                snapshot.rotation = rotation_from_yaw_pitch(yaw, pitch);
            }
            "pitch_deg" => {
                let yaw = extract_yaw(snapshot.rotation);
                let pitch = scalar_from_json(value)?;
                snapshot.rotation = rotation_from_yaw_pitch(yaw, pitch);
            }
            "color" => {
                snapshot.node.color = vec3_from_json(value)?.to_array();
            }
            "intensity" => {
                snapshot.node.intensity = scalar_from_json(value)?.max(0.0);
            }
            "shadows_enabled" => {
                snapshot.node.shadows_enabled = parse_bool(value, "shadows_enabled")?;
            }
            "range" => {
                snapshot.node.range = scalar_from_json(value)?.max(0.01);
            }
            "radius" => {
                snapshot.node.radius = scalar_from_json(value)?.max(0.0);
            }
            "inner_angle_deg" => {
                snapshot.node.inner_angle_deg = scalar_from_json(value)?.clamp(0.1, 89.0);
            }
            "outer_angle_deg" => {
                snapshot.node.outer_angle_deg = scalar_from_json(value)?.clamp(0.1, 89.0);
            }
            _ => {
                return Err(invalid_property_error(
                    "scene_light",
                    &[
                        "name",
                        "kind",
                        "enabled",
                        "position",
                        "yaw_deg",
                        "pitch_deg",
                        "color",
                        "intensity",
                        "shadows_enabled",
                        "range",
                        "radius",
                        "inner_angle_deg",
                        "outer_angle_deg",
                    ],
                ));
            }
        }

        if snapshot.node.inner_angle_deg > snapshot.node.outer_angle_deg {
            snapshot.node.inner_angle_deg = snapshot.node.outer_angle_deg;
        }

        Ok(snapshot.into())
    }

    fn bounds(&self) -> Option<EntityBounds> {
        let half = Vec3::splat(light_pick_radius(&self.node));
        Some(EntityBounds {
            min: self.translation - half,
            max: self.translation + half,
        })
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![]
    }

    fn to_json(&self) -> Value {
        json!({
            "element_id": self.element_id,
            "node": self.node,
            "translation": self.translation.to_array(),
            "rotation": [self.rotation.x, self.rotation.y, self.rotation.z, self.rotation.w],
        })
    }

    fn apply_to(&self, world: &mut World) {
        let transform = Transform::from_translation(self.translation).with_rotation(self.rotation);
        let entity = if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world
                .entity_mut(entity)
                .insert((self.node.clone(), transform));
            entity
        } else {
            world
                .spawn((self.element_id, self.node.clone(), transform))
                .id()
        };
        insert_bevy_light_direct(&mut world.entity_mut(entity), &self.node);
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        let transform = Transform::from_translation(self.translation).with_rotation(self.rotation);
        draw_light_gizmo(gizmos, &self.node, &transform, color, false);
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == "scene_light" && other.to_json() == self.to_json()
    }
}

impl From<SceneLightSnapshot> for BoxedEntity {
    fn from(snapshot: SceneLightSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

pub struct SceneLightFactory;

impl AuthoredEntityFactory for SceneLightFactory {
    fn type_name(&self) -> &'static str {
        "scene_light"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let node = entity_ref.get::<SceneLightNode>()?.clone();
        let transform = entity_ref.get::<Transform>().cloned().unwrap_or_default();
        Some(
            SceneLightSnapshot {
                element_id,
                node,
                translation: transform.translation,
                rotation: transform.rotation,
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
        let node: SceneLightNode = serde_json::from_value(
            data.get("node")
                .cloned()
                .ok_or_else(|| "Missing node".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let translation = data
            .get("translation")
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or(Vec3::ZERO);
        let rotation = parse_quat(data.get("rotation")).unwrap_or(Quat::IDENTITY);
        Ok(SceneLightSnapshot {
            element_id,
            node,
            translation,
            rotation,
        }
        .into())
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .and_then(SceneLightKind::from_str)
            .unwrap_or(SceneLightKind::Directional);
        let element_id = world
            .get_resource::<ElementIdAllocator>()
            .ok_or_else(|| "ElementIdAllocator not available".to_string())?
            .next_id();

        let translation = object
            .get("position")
            .map(vec3_from_json)
            .transpose()?
            .unwrap_or_else(|| default_light_position(kind));

        let yaw_deg = object
            .get("yaw_deg")
            .map(scalar_from_json)
            .transpose()?
            .unwrap_or(default_light_yaw(kind));
        let pitch_deg = object
            .get("pitch_deg")
            .map(scalar_from_json)
            .transpose()?
            .unwrap_or(default_light_pitch(kind));

        let mut node = SceneLightNode {
            kind,
            name: object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(match kind {
                    SceneLightKind::Directional => "Directional Light",
                    SceneLightKind::Point => "Point Light",
                    SceneLightKind::Spot => "Spot Light",
                })
                .to_string(),
            enabled: object
                .get("enabled")
                .map(|value| parse_bool(value, "enabled"))
                .transpose()?
                .unwrap_or(true),
            color: object
                .get("color")
                .map(vec3_from_json)
                .transpose()?
                .unwrap_or(Vec3::ONE)
                .to_array(),
            intensity: object
                .get("intensity")
                .map(scalar_from_json)
                .transpose()?
                .unwrap_or(default_light_intensity(kind)),
            shadows_enabled: object
                .get("shadows_enabled")
                .map(|value| parse_bool(value, "shadows_enabled"))
                .transpose()?
                .unwrap_or(default_light_shadows(kind)),
            range: object
                .get("range")
                .map(scalar_from_json)
                .transpose()?
                .unwrap_or(20.0),
            radius: object
                .get("radius")
                .map(scalar_from_json)
                .transpose()?
                .unwrap_or(0.2),
            inner_angle_deg: object
                .get("inner_angle_deg")
                .map(scalar_from_json)
                .transpose()?
                .unwrap_or(18.0),
            outer_angle_deg: object
                .get("outer_angle_deg")
                .map(scalar_from_json)
                .transpose()?
                .unwrap_or(32.0),
        };
        node.range = node.range.max(0.01);
        node.radius = node.radius.max(0.0);
        node.inner_angle_deg = node.inner_angle_deg.clamp(0.1, 89.0);
        node.outer_angle_deg = node.outer_angle_deg.clamp(node.inner_angle_deg, 89.0);

        Ok(SceneLightSnapshot {
            element_id,
            node,
            translation,
            rotation: rotation_from_yaw_pitch(yaw_deg, pitch_deg),
        }
        .into())
    }

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        if !scene_light_objects_visible(world) {
            return;
        }
        let Some((node, transform)) = world.get_entity(entity).ok().and_then(|entity_ref| {
            Some((
                entity_ref.get::<SceneLightNode>()?.clone(),
                entity_ref.get::<Transform>().cloned().unwrap_or_default(),
            ))
        }) else {
            return;
        };
        draw_light_gizmo(gizmos, &node, &transform, color, true);
    }

    fn hit_test(&self, world: &World, ray: Ray3d) -> Option<HitCandidate> {
        if !scene_light_objects_visible(world) {
            return None;
        }
        let Some(mut query) = world.try_query::<(Entity, &SceneLightNode, &Transform)>() else {
            return None;
        };
        query
            .iter(world)
            .filter_map(|(entity, node, transform)| {
                let radius = light_pick_radius(node);
                ray_point_distance(ray, transform.translation)
                    .filter(|(_, distance)| *distance <= radius)
                    .map(|(ray_distance, _)| HitCandidate {
                        entity,
                        distance: ray_distance,
                    })
            })
            .min_by(|left, right| left.distance.total_cmp(&right.distance))
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let Some(mut query) = world.try_query::<&SceneLightNode>() else {
            return;
        };
        for node in query.iter(world) {
            *summary
                .entity_counts
                .entry("scene_light".to_string())
                .or_default() += 1;
            *summary
                .entity_counts
                .entry(format!("scene_light:{}", node.kind.as_str()))
                .or_default() += 1;
        }
    }
}

impl Plugin for LightingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SceneLightingSettings>()
            .init_resource::<SceneLightObjectVisibility>()
            .register_authored_entity_factory(SceneLightFactory)
            .register_capability(CapabilityDescriptor {
                id: "lighting".to_string(),
                name: "Lighting".to_string(),
                version: 1,
                api_version: crate::capability_registry::CAPABILITY_API_VERSION,
                description: Some(
                    "Authored scene lights and ambient lighting controls for viewport rendering."
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
                    id: "lighting.toggle_browser".to_string(),
                    label: "Lights".to_string(),
                    description: "Show or hide the Lights manager".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Manage ambient light and authored scene lights".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some("lighting".to_string()),
                },
                execute_toggle_lights_browser,
            )
            .add_systems(Startup, seed_default_lighting_scene)
            .add_systems(
                Update,
                (
                    sync_scene_light_object_visibility,
                    clear_hidden_light_selection,
                    sync_ambient_light,
                    sync_scene_lights,
                    draw_scene_light_gizmos,
                ),
            );
    }
}

pub fn ensure_default_lighting_scene(world: &mut World) {
    if world.get_resource::<SceneLightingSettings>().is_none() {
        world.insert_resource(SceneLightingSettings::default());
    }
    if list_scene_lights(world).is_empty() {
        let snapshots = {
            let allocator = world.resource::<ElementIdAllocator>();
            create_daylight_rig(allocator)
        };
        for snapshot in snapshots {
            snapshot.apply_to(world);
        }
    }
}

pub fn create_daylight_rig(allocator: &ElementIdAllocator) -> Vec<SceneLightSnapshot> {
    vec![
        SceneLightSnapshot::default_directional(
            allocator,
            "Sun Key",
            Vec3::new(10.0, 20.0, 8.0),
            Vec3::ZERO,
            [1.0, 0.97, 0.88],
            8_000.0,
            true,
        ),
        SceneLightSnapshot::default_directional(
            allocator,
            "Sky Fill",
            Vec3::new(-8.0, 4.0, -6.0),
            Vec3::ZERO,
            [0.6, 0.7, 0.9],
            2_000.0,
            false,
        ),
    ]
}

pub fn list_scene_lights(world: &World) -> Vec<(Entity, ElementId, SceneLightNode, Transform)> {
    let Some(mut query) = world.try_query::<(Entity, &ElementId, &SceneLightNode, &Transform)>()
    else {
        return Vec::new();
    };
    query
        .iter(world)
        .map(|(entity, element_id, node, transform)| {
            (entity, *element_id, node.clone(), *transform)
        })
        .collect()
}

fn execute_toggle_lights_browser(
    world: &mut World,
    _: &Value,
) -> Result<crate::plugins::command_registry::CommandResult, String> {
    let mut state = world
        .get_resource_mut::<crate::plugins::egui_chrome::LightingWindowState>()
        .ok_or_else(|| "Lighting window state is unavailable".to_string())?;
    state.visible = !state.visible;
    Ok(crate::plugins::command_registry::CommandResult::empty())
}

fn seed_default_lighting_scene(world: &mut World) {
    ensure_default_lighting_scene(world);
    sync_ambient_from_settings(world);
}

fn sync_ambient_light(
    settings: Res<SceneLightingSettings>,
    mut ambient: ResMut<GlobalAmbientLight>,
) {
    if !settings.is_changed() {
        return;
    }
    ambient.color = Color::srgb(
        settings.ambient_color[0],
        settings.ambient_color[1],
        settings.ambient_color[2],
    );
    ambient.brightness = settings.ambient_brightness;
    ambient.affects_lightmapped_meshes = settings.affects_lightmapped_meshes;
}

fn sync_scene_lights(
    mut commands: Commands,
    query: Query<(Entity, &SceneLightNode), Or<(Added<SceneLightNode>, Changed<SceneLightNode>)>>,
) {
    for (entity, node) in &query {
        apply_scene_light_components(&mut commands, entity, node);
    }
}

fn sync_scene_light_object_visibility(
    mut visibility: ResMut<SceneLightObjectVisibility>,
    mut doc_props: ResMut<DocumentProperties>,
    mut last_serialized: Local<Option<Value>>,
) {
    let saved = doc_props
        .domain_defaults
        .get(LIGHT_OBJECT_VISIBILITY_KEY)
        .cloned();
    if saved != *last_serialized {
        if let Some(saved_state) = saved
            .as_ref()
            .and_then(deserialize_scene_light_object_visibility)
        {
            if *visibility != saved_state {
                *visibility = saved_state;
            }
        } else if saved.is_none() && last_serialized.is_some() {
            *visibility = SceneLightObjectVisibility::default();
        }
        *last_serialized = saved.clone();
    }

    let serialized = serialize_scene_light_object_visibility(&visibility);
    if doc_props.domain_defaults.get(LIGHT_OBJECT_VISIBILITY_KEY) != Some(&serialized) {
        doc_props
            .domain_defaults
            .insert(LIGHT_OBJECT_VISIBILITY_KEY.to_string(), serialized.clone());
    }
    *last_serialized = Some(serialized);
}

fn clear_hidden_light_selection(
    mut commands: Commands,
    visibility: Res<SceneLightObjectVisibility>,
    selected_lights: Query<
        Entity,
        (
            With<crate::plugins::selection::Selected>,
            With<SceneLightNode>,
        ),
    >,
) {
    if !visibility.is_changed() || visibility.visible {
        return;
    }

    for entity in &selected_lights {
        commands
            .entity(entity)
            .remove::<crate::plugins::selection::Selected>();
    }
}

fn draw_scene_light_gizmos(
    query: Query<(&SceneLightNode, &Transform)>,
    visibility: Res<SceneLightObjectVisibility>,
    mut gizmos: Gizmos,
) {
    if !visibility.visible {
        return;
    }

    for (node, transform) in &query {
        draw_light_gizmo(
            &mut gizmos,
            node,
            transform,
            Color::srgba(node.color[0], node.color[1], node.color[2], 0.55),
            false,
        );
    }
}

fn serialize_scene_light_object_visibility(state: &SceneLightObjectVisibility) -> Value {
    serde_json::to_value(state).unwrap_or_else(|_| Value::Null)
}

fn deserialize_scene_light_object_visibility(value: &Value) -> Option<SceneLightObjectVisibility> {
    serde_json::from_value(value.clone()).ok()
}

/// Insert the correct Bevy light component directly via world access.
/// Used by `apply_to` so lights work immediately without waiting for
/// the deferred `sync_scene_lights` system.
fn insert_bevy_light_direct(entity_mut: &mut EntityWorldMut, node: &SceneLightNode) {
    entity_mut
        .remove::<DirectionalLight>()
        .remove::<PointLight>()
        .remove::<SpotLight>();

    if !node.enabled {
        return;
    }

    let color = Color::srgb(node.color[0], node.color[1], node.color[2]);
    match node.kind {
        SceneLightKind::Directional => {
            entity_mut.insert(DirectionalLight {
                color,
                illuminance: node.intensity.max(0.0),
                shadows_enabled: node.shadows_enabled,
                ..default()
            });
        }
        SceneLightKind::Point => {
            entity_mut.insert(PointLight {
                color,
                intensity: node.intensity.max(0.0),
                shadows_enabled: node.shadows_enabled,
                range: node.range.max(0.01),
                radius: node.radius.max(0.0),
                ..default()
            });
        }
        SceneLightKind::Spot => {
            entity_mut.insert(SpotLight {
                color,
                intensity: node.intensity.max(0.0),
                shadows_enabled: node.shadows_enabled,
                range: node.range.max(0.01),
                radius: node.radius.max(0.0),
                inner_angle: node.inner_angle_deg.to_radians(),
                outer_angle: node.outer_angle_deg.max(node.inner_angle_deg).to_radians(),
                ..default()
            });
        }
    }
}

fn apply_scene_light_components(commands: &mut Commands, entity: Entity, node: &SceneLightNode) {
    let mut entity_commands = commands.entity(entity);
    entity_commands
        .remove::<DirectionalLight>()
        .remove::<PointLight>()
        .remove::<SpotLight>();

    if !node.enabled {
        return;
    }

    let color = Color::srgb(node.color[0], node.color[1], node.color[2]);
    match node.kind {
        SceneLightKind::Directional => {
            entity_commands.insert(DirectionalLight {
                color,
                illuminance: node.intensity.max(0.0),
                shadows_enabled: node.shadows_enabled,
                ..default()
            });
        }
        SceneLightKind::Point => {
            entity_commands.insert(PointLight {
                color,
                intensity: node.intensity.max(0.0),
                shadows_enabled: node.shadows_enabled,
                range: node.range.max(0.01),
                radius: node.radius.max(0.0),
                ..default()
            });
        }
        SceneLightKind::Spot => {
            entity_commands.insert(SpotLight {
                color,
                intensity: node.intensity.max(0.0),
                shadows_enabled: node.shadows_enabled,
                range: node.range.max(0.01),
                radius: node.radius.max(0.0),
                inner_angle: node.inner_angle_deg.to_radians(),
                outer_angle: node.outer_angle_deg.max(node.inner_angle_deg).to_radians(),
                ..default()
            });
        }
    }
}

fn draw_light_gizmo(
    gizmos: &mut Gizmos,
    node: &SceneLightNode,
    transform: &Transform,
    color: Color,
    selected: bool,
) {
    let position = transform.translation;
    let scale = if selected { 0.65 } else { 0.45 };
    let forward = transform.forward();
    let right = transform.right() * scale;
    let up = transform.up() * scale;

    gizmos.line(position - right, position + right, color);
    gizmos.line(position - up, position + up, color);
    gizmos.line(
        position - forward * scale,
        position + forward * scale,
        color,
    );

    match node.kind {
        SceneLightKind::Directional => {
            gizmos.arrow(position, position + forward * (scale * 2.5), color);
        }
        SceneLightKind::Point => {
            gizmos.line(
                position - (right + up) * 0.7,
                position + (right + up) * 0.7,
                color,
            );
            gizmos.line(
                position - (right - up) * 0.7,
                position + (right - up) * 0.7,
                color,
            );
        }
        SceneLightKind::Spot => {
            gizmos.arrow(position, position + forward * (scale * 2.2), color);
            let center = position + forward * (scale * 1.5);
            let rim_right = transform.right() * (scale * 0.6);
            let rim_up = transform.up() * (scale * 0.6);
            let a = center + rim_right + rim_up;
            let b = center + rim_right - rim_up;
            let c = center - rim_right - rim_up;
            let d = center - rim_right + rim_up;
            gizmos.line(a, b, color);
            gizmos.line(b, c, color);
            gizmos.line(c, d, color);
            gizmos.line(d, a, color);
            gizmos.line(position, a, color);
            gizmos.line(position, b, color);
            gizmos.line(position, c, color);
            gizmos.line(position, d, color);
        }
    }
}

fn sync_ambient_from_settings(world: &mut World) {
    let settings = world.resource::<SceneLightingSettings>().clone();
    world.insert_resource(GlobalAmbientLight {
        color: Color::srgb(
            settings.ambient_color[0],
            settings.ambient_color[1],
            settings.ambient_color[2],
        ),
        brightness: settings.ambient_brightness,
        affects_lightmapped_meshes: settings.affects_lightmapped_meshes,
    });
}

fn scalar_from_json(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|value| value as f32)
        .ok_or_else(|| "Expected a number".to_string())
}

fn vec3_from_json(value: &Value) -> Result<Vec3, String> {
    let array = value
        .as_array()
        .ok_or_else(|| "Expected [x, y, z]".to_string())?;
    if array.len() != 3 {
        return Err("Expected [x, y, z]".to_string());
    }
    Ok(Vec3::new(
        scalar_from_json(&array[0])?,
        scalar_from_json(&array[1])?,
        scalar_from_json(&array[2])?,
    ))
}

fn parse_quat(value: Option<&Value>) -> Option<Quat> {
    let array = value?.as_array()?;
    if array.len() != 4 {
        return None;
    }
    Some(Quat::from_xyzw(
        array[0].as_f64()? as f32,
        array[1].as_f64()? as f32,
        array[2].as_f64()? as f32,
        array[3].as_f64()? as f32,
    ))
}

fn parse_bool(value: &Value, label: &str) -> Result<bool, String> {
    if let Some(value) = value.as_bool() {
        return Ok(value);
    }
    match value.as_str() {
        Some("true") | Some("1") | Some("yes") => Ok(true),
        Some("false") | Some("0") | Some("no") => Ok(false),
        _ => Err(format!("{label} must be true or false")),
    }
}

fn rotation_from_yaw_pitch(yaw_deg: f32, pitch_deg: f32) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        yaw_deg.to_radians(),
        pitch_deg.to_radians(),
        0.0,
    )
}

fn extract_yaw(rotation: Quat) -> f32 {
    let (yaw, _, _) = rotation.to_euler(EulerRot::YXZ);
    yaw.to_degrees()
}

fn extract_pitch(rotation: Quat) -> f32 {
    let (_, pitch, _) = rotation.to_euler(EulerRot::YXZ);
    pitch.to_degrees()
}

fn light_pick_radius(node: &SceneLightNode) -> f32 {
    match node.kind {
        SceneLightKind::Directional => 0.8,
        SceneLightKind::Point => 0.65,
        SceneLightKind::Spot => 0.7,
    }
}

fn ray_point_distance(ray: Ray3d, point: Vec3) -> Option<(f32, f32)> {
    let offset = point - ray.origin;
    let ray_distance = offset.dot(*ray.direction);
    if ray_distance < 0.0 {
        return None;
    }
    let closest = ray.get_point(ray_distance);
    Some((ray_distance, point.distance(closest)))
}

fn default_light_position(kind: SceneLightKind) -> Vec3 {
    match kind {
        SceneLightKind::Directional => Vec3::new(10.0, 20.0, 8.0),
        SceneLightKind::Point => Vec3::new(0.0, 4.0, 0.0),
        SceneLightKind::Spot => Vec3::new(4.0, 5.0, 4.0),
    }
}

fn default_light_yaw(kind: SceneLightKind) -> f32 {
    match kind {
        SceneLightKind::Directional => -128.0,
        SceneLightKind::Point => 0.0,
        SceneLightKind::Spot => -135.0,
    }
}

fn default_light_pitch(kind: SceneLightKind) -> f32 {
    match kind {
        SceneLightKind::Directional => -50.0,
        SceneLightKind::Point => 0.0,
        SceneLightKind::Spot => -28.0,
    }
}

fn default_light_intensity(kind: SceneLightKind) -> f32 {
    match kind {
        SceneLightKind::Directional => 8_000.0,
        SceneLightKind::Point => 2_500.0,
        SceneLightKind::Spot => 4_000.0,
    }
}

fn default_light_shadows(kind: SceneLightKind) -> bool {
    !matches!(kind, SceneLightKind::Point)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_daylight_rig_matches_pre_pp59_startup_contract() {
        let settings = SceneLightingSettings::default();
        assert_eq!(settings.ambient_color, [0.9, 0.92, 1.0]);
        assert_eq!(settings.ambient_brightness, 40.0);
        assert!(settings.affects_lightmapped_meshes);

        let allocator = ElementIdAllocator::default();
        let rig = create_daylight_rig(&allocator);
        assert_eq!(rig.len(), 2);

        let key = &rig[0];
        assert_eq!(key.node.name, "Sun Key");
        assert_eq!(key.node.intensity, 8_000.0);
        assert!(key.node.shadows_enabled);
        assert_eq!(key.translation, Vec3::new(10.0, 20.0, 8.0));

        let fill = &rig[1];
        assert_eq!(fill.node.name, "Sky Fill");
        assert_eq!(fill.node.intensity, 2_000.0);
        assert!(!fill.node.shadows_enabled);
        assert_eq!(fill.translation, Vec3::new(-8.0, 4.0, -6.0));
    }

    #[test]
    fn scene_light_object_visibility_round_trips_through_json() {
        let state = SceneLightObjectVisibility { visible: true };
        let decoded = deserialize_scene_light_object_visibility(
            &serialize_scene_light_object_visibility(&state),
        )
        .expect("light object visibility should deserialize");
        assert_eq!(decoded, state);
    }
}
