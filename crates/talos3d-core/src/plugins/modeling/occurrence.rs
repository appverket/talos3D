//! Occurrence system for PP51: Reusable Definition Foundation.
//!
//! An occurrence is a placed instance of a `Definition`. It carries an
//! `OccurrenceIdentity` (which definition, which version, which overrides)
//! and an `OccurrenceClassification` (dirty flag). The ECS evaluation
//! systems consume these components to produce geometry.

use std::any::Any;
use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::{
        AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef,
        PushPullAffordance, PushPullBlockReason,
    },
    capability_registry::{AuthoredEntityFactory, FaceId},
    plugins::{
        commands::{apply_mesh_primitive, despawn_by_element_id},
        identity::ElementId,
        materials::{material_assignment_from_value, MaterialAssignment},
        modeling::{
            definition::{
                ChildSlotDef, ConstraintSeverity, Definition, DefinitionId, DefinitionRegistry,
                DefinitionVersion, EvaluatorDecl, ExprNode, OverrideMap, ParamType,
            },
            mesh_generation::NeedsMesh,
            primitives::ShapeRotation,
            profile::{Profile2d, ProfileExtrusion},
        },
    },
};

// ---------------------------------------------------------------------------
// OccurrenceIdentity — ECS component
// ---------------------------------------------------------------------------

/// ECS component that binds an entity to a `Definition` and records its
/// per-occurrence parameter overrides.
///
/// Domain-specific metadata is stored as opaque JSON so it can round-trip
/// through serialisation without any schema knowledge at this layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostedAnchor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub position: [f32; 3],
}

impl HostedAnchor {
    pub fn vec3(&self) -> Vec3 {
        Vec3::new(self.position[0], self.position[1], self.position[2])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostedOccurrenceContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_element_id: Option<ElementId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_element_id: Option<ElementId>,
    #[serde(default)]
    pub anchors: Vec<HostedAnchor>,
}

impl HostedOccurrenceContext {
    pub fn anchor_position(&self, anchor_id: &str) -> Option<Vec3> {
        self.anchors
            .iter()
            .find(|anchor| anchor.id == anchor_id)
            .map(HostedAnchor::vec3)
    }
}

#[derive(Debug, Clone, Component, Serialize, Deserialize)]
pub struct OccurrenceIdentity {
    /// The definition this occurrence instantiates.
    pub definition_id: DefinitionId,
    /// The definition version this occurrence was last evaluated against.
    pub definition_version: DefinitionVersion,
    /// Parameter values that override the definition's defaults.
    pub overrides: OverrideMap,
    /// Domain-specific extension payload owned by higher-level products.
    ///
    /// Core does not interpret this value and excludes it from geometric
    /// dirty-tracking by design.
    #[serde(default)]
    pub domain_data: Value,
    /// Optional resolved hosting context used for hosted instantiation flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosting: Option<HostedOccurrenceContext>,
}

impl OccurrenceIdentity {
    /// Construct a minimal identity referencing `definition_id` with no overrides.
    pub fn new(definition_id: DefinitionId, definition_version: DefinitionVersion) -> Self {
        Self {
            definition_id,
            definition_version,
            overrides: OverrideMap::default(),
            domain_data: Value::Null,
            hosting: None,
        }
    }
}

// ---------------------------------------------------------------------------
// OccurrenceClassification — ECS component
// ---------------------------------------------------------------------------

/// Lightweight classification component tracking whether the occurrence's
/// mesh is up to date.
#[derive(Debug, Clone, Component)]
pub struct OccurrenceClassification {
    /// When `true` the occurrence needs to be re-evaluated by
    /// `evaluate_occurrences`.
    pub mesh_dirty: bool,
}

impl Default for OccurrenceClassification {
    fn default() -> Self {
        Self { mesh_dirty: true }
    }
}

// ---------------------------------------------------------------------------
// GeneratedOccurrencePart — ECS component
// ---------------------------------------------------------------------------

/// Transient generated geometry owned by an authored occurrence.
///
/// These entities are not persisted; they are rebuilt whenever the root
/// occurrence is re-evaluated.
#[derive(Debug, Clone, Component)]
pub struct GeneratedOccurrencePart {
    pub owner: ElementId,
    pub slot_path: String,
    pub definition_id: DefinitionId,
}

// ---------------------------------------------------------------------------
// NeedsEval — ECS marker
// ---------------------------------------------------------------------------

/// Marker component: this occurrence entity must be re-evaluated this frame.
#[derive(Component)]
pub struct NeedsEval;

// ---------------------------------------------------------------------------
// ChangedDefinitions — Bevy resource
// ---------------------------------------------------------------------------

/// Resource that collects `DefinitionId`s whose definitions changed since the
/// last propagation pass.
#[derive(Debug, Clone, Default, Resource)]
pub struct ChangedDefinitions {
    definitions: Vec<DefinitionId>,
}

impl ChangedDefinitions {
    /// Record that `id` has been changed and all its occurrences need re-evaluation.
    pub fn mark_changed(&mut self, id: DefinitionId) {
        if !self.definitions.contains(&id) {
            self.definitions.push(id);
        }
    }

    /// Drain and return all pending changed definition ids.
    pub fn drain(&mut self) -> Vec<DefinitionId> {
        std::mem::take(&mut self.definitions)
    }
}

// ---------------------------------------------------------------------------
// OccurrenceSnapshot — AuthoredEntity impl
// ---------------------------------------------------------------------------

/// Serialisable snapshot of an occurrence, used by the undo/redo history and
/// the persistence layer.
///
/// Stores a local translation offset, rotation, and scale so that transforms
/// applied by the user are preserved without touching the definition's
/// parameters.
#[derive(Debug, Clone)]
pub struct OccurrenceSnapshot {
    /// Unique identity of the entity in this document.
    pub element_id: ElementId,
    /// Definition binding and parameter overrides.
    pub identity: OccurrenceIdentity,
    /// Human-readable label shown in the UI.
    pub label: String,
    /// World-space translation offset (baked from user transforms).
    pub offset: Vec3,
    /// Additional rotation applied on top of the evaluated geometry.
    pub rotation: Quat,
    /// Non-uniform scale applied on top of the evaluated geometry.
    pub scale: Vec3,
}

impl OccurrenceSnapshot {
    /// Construct a new snapshot with identity transform.
    pub fn new(
        element_id: ElementId,
        identity: OccurrenceIdentity,
        label: impl Into<String>,
    ) -> Self {
        Self {
            element_id,
            identity,
            label: label.into(),
            offset: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl AuthoredEntity for OccurrenceSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "occurrence"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn center(&self) -> Vec3 {
        self.offset
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut next = self.clone();
        next.offset += delta;
        BoxedEntity(Box::new(next))
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut next = self.clone();
        next.rotation = rotation * next.rotation;
        BoxedEntity(Box::new(next))
    }

    fn scale_by(&self, factor: Vec3, _center: Vec3) -> BoxedEntity {
        let mut next = self.clone();
        next.scale *= factor;
        BoxedEntity(Box::new(next))
    }

    fn push_pull_affordance(&self, _face_id: FaceId) -> PushPullAffordance {
        PushPullAffordance::Blocked(PushPullBlockReason::UnsupportedFace)
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut next = self.clone();
        next.identity.overrides.set(property_name, value.clone());
        Ok(BoxedEntity(Box::new(next)))
    }

    fn handles(&self) -> Vec<HandleInfo> {
        vec![]
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "element_id": self.element_id,
            "identity": self.identity,
            "label": self.label,
            "offset": [self.offset.x, self.offset.y, self.offset.z],
            "rotation": [self.rotation.x, self.rotation.y, self.rotation.z, self.rotation.w],
            "scale": [self.scale.x, self.scale.y, self.scale.z],
        })
    }

    fn apply_to(&self, world: &mut World) {
        let registry = world.resource::<DefinitionRegistry>().clone();
        let transform = Transform {
            translation: self.offset,
            rotation: self.rotation,
            scale: self.scale,
        };

        if let Err(error) = render_occurrence(
            world,
            &registry,
            self.element_id,
            &self.identity,
            transform,
            Some(&self.label),
        ) {
            warn!(
                "Failed to evaluate occurrence {} from definition '{}': {}",
                self.element_id.0, self.identity.definition_id, error
            );
        }
    }

    fn apply_with_previous(&self, world: &mut World, previous: Option<&dyn AuthoredEntity>) {
        let identity_changed = previous
            .and_then(|p| p.as_any().downcast_ref::<OccurrenceSnapshot>())
            .map(|prev| {
                let identity_json = serde_json::to_value(&self.identity).unwrap_or(Value::Null);
                let prev_json = serde_json::to_value(&prev.identity).unwrap_or(Value::Null);
                identity_json != prev_json
                    || self.offset != prev.offset
                    || self.rotation != prev.rotation
                    || self.scale != prev.scale
            })
            .unwrap_or(true);

        if identity_changed {
            self.apply_to(world);
        } else if let Some(entity) =
            crate::plugins::commands::find_entity_by_element_id(world, self.element_id)
        {
            world
                .entity_mut(entity)
                .insert((self.identity.clone(), Name::new(self.label.clone())));
        }
    }

    fn remove_from(&self, world: &mut World) {
        cleanup_generated_occurrence_parts(world, self.element_id);
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        Some(Transform {
            translation: self.offset,
            rotation: self.rotation,
            scale: self.scale,
        })
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn preview_line_count(&self) -> usize {
        0
    }

    fn box_clone(&self) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl From<OccurrenceSnapshot> for BoxedEntity {
    fn from(snapshot: OccurrenceSnapshot) -> Self {
        BoxedEntity(Box::new(snapshot))
    }
}

// ---------------------------------------------------------------------------
// OccurrenceFactory — AuthoredEntityFactory impl
// ---------------------------------------------------------------------------

/// Factory that reconstructs `OccurrenceSnapshot`s from persisted JSON.
pub struct OccurrenceFactory;

impl AuthoredEntityFactory for OccurrenceFactory {
    fn type_name(&self) -> &'static str {
        "occurrence"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        let element_id = *entity_ref.get::<ElementId>()?;
        let identity = entity_ref.get::<OccurrenceIdentity>()?.clone();
        let label = entity_ref
            .get::<Name>()
            .map(|name| name.as_str().to_string())
            .unwrap_or_else(|| "Occurrence".to_string());
        let transform = entity_ref
            .get::<bevy::prelude::Transform>()
            .copied()
            .unwrap_or_default();
        Some(
            OccurrenceSnapshot {
                element_id,
                identity,
                label,
                offset: transform.translation,
                rotation: transform.rotation,
                scale: transform.scale,
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let element_id_val = data
            .get("element_id")
            .ok_or_else(|| "Missing 'element_id'".to_string())?;
        let element_id: ElementId =
            serde_json::from_value(element_id_val.clone()).map_err(|e| e.to_string())?;

        let identity_val = data
            .get("identity")
            .ok_or_else(|| "Missing 'identity'".to_string())?;
        let identity: OccurrenceIdentity =
            serde_json::from_value(identity_val.clone()).map_err(|e| e.to_string())?;

        let label = data
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("Occurrence")
            .to_string();

        let offset = data
            .get("offset")
            .and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok())
            .map(|[x, y, z]| Vec3::new(x, y, z))
            .unwrap_or(Vec3::ZERO);

        let rotation = data
            .get("rotation")
            .and_then(|v| serde_json::from_value::<[f32; 4]>(v.clone()).ok())
            .map(|[x, y, z, w]| Quat::from_xyzw(x, y, z, w))
            .unwrap_or(Quat::IDENTITY);

        let scale = data
            .get("scale")
            .and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok())
            .map(|[x, y, z]| Vec3::new(x, y, z))
            .unwrap_or(Vec3::ONE);

        Ok(OccurrenceSnapshot {
            element_id,
            identity,
            label,
            offset,
            rotation,
            scale,
        }
        .into())
    }

    fn from_create_request(&self, _world: &World, _request: &Value) -> Result<BoxedEntity, String> {
        Err(
            "Occurrences are created via the definition API, not from a generic create request"
                .to_string(),
        )
    }
}

// ---------------------------------------------------------------------------
// ECS evaluation systems
// ---------------------------------------------------------------------------

/// Mark all occurrences of changed definitions as needing re-evaluation.
pub fn propagate_definition_changes(
    mut changed: ResMut<ChangedDefinitions>,
    mut query: Query<(&OccurrenceIdentity, &mut OccurrenceClassification)>,
    mut commands: Commands,
) {
    let ids = changed.drain();
    if ids.is_empty() {
        return;
    }

    for (identity, mut classification) in &mut query {
        if ids.contains(&identity.definition_id) {
            classification.mesh_dirty = true;
        }
        let _ = &mut commands;
    }
}

/// Full-entity variant that can insert the `NeedsEval` marker.
pub fn propagate_definition_changes_with_commands(
    mut changed: ResMut<ChangedDefinitions>,
    query: Query<(Entity, &OccurrenceIdentity)>,
    mut commands: Commands,
) {
    let ids = changed.drain();
    if ids.is_empty() {
        return;
    }

    for (entity, identity) in &query {
        if ids.contains(&identity.definition_id) {
            commands.entity(entity).insert(NeedsEval);
        }
    }
}

/// Re-evaluate occurrences that have the `NeedsEval` marker component.
pub fn evaluate_occurrences(world: &mut World) {
    let registry = world.resource::<DefinitionRegistry>().clone();
    let occurrences: Vec<(Entity, ElementId, OccurrenceIdentity, Transform)> = {
        let mut query = world.query_filtered::<
            (Entity, &ElementId, &OccurrenceIdentity, Option<&Transform>),
            With<NeedsEval>,
        >();
        query
            .iter(world)
            .map(|(entity, element_id, identity, transform)| {
                (
                    entity,
                    *element_id,
                    identity.clone(),
                    transform.copied().unwrap_or_default(),
                )
            })
            .collect()
    };

    for (entity, element_id, identity, transform) in occurrences {
        if let Err(error) =
            render_occurrence(world, &registry, element_id, &identity, transform, None)
        {
            warn!(
                "Failed to re-evaluate occurrence {} from definition '{}': {}",
                element_id.0, identity.definition_id, error
            );
            cleanup_generated_occurrence_parts(world, element_id);
            clear_occurrence_root_geometry(world, entity);
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert((identity.clone(), transform));
            }
        }

        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.remove::<NeedsEval>();
            if let Some(mut classification) = entity_mut.get_mut::<OccurrenceClassification>() {
                classification.mesh_dirty = false;
            } else {
                entity_mut.insert(OccurrenceClassification { mesh_dirty: false });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluation helpers
// ---------------------------------------------------------------------------

struct EvaluatedDefinitionState {
    values: HashMap<String, Value>,
}

struct CompoundSpawnContext {
    owner: ElementId,
    parent_translation: Vec3,
    parent_rotation: Quat,
    slot_path: String,
}

fn render_occurrence(
    world: &mut World,
    registry: &DefinitionRegistry,
    element_id: ElementId,
    identity: &OccurrenceIdentity,
    transform: Transform,
    label: Option<&str>,
) -> Result<(), String> {
    let root_entity = ensure_occurrence_root_entity(world, element_id, identity, transform, label);
    cleanup_generated_occurrence_parts(world, element_id);

    let definition = registry.effective_definition(&identity.definition_id)?;
    let resolved = registry.resolve_params_checked(&identity.definition_id, &identity.overrides)?;
    let state = evaluate_definition_state(&definition, &resolved)?;

    if let Some(extrusion) =
        build_rectangular_extrusion_from_values(&definition, &state.values, transform.translation)
    {
        apply_mesh_primitive(
            world,
            element_id,
            extrusion,
            ShapeRotation(transform.rotation),
        );
        apply_occurrence_material_assignment(world, root_entity, &definition);
    } else {
        clear_occurrence_root_geometry(world, root_entity);
        world.entity_mut(root_entity).insert(transform);
        clear_occurrence_material_assignment(world, root_entity);
    }

    if let Some(compound) = &definition.compound {
        for slot in &compound.child_slots {
            spawn_compound_slot(
                world,
                registry,
                slot,
                &state.values,
                CompoundSpawnContext {
                    owner: element_id,
                    parent_translation: transform.translation,
                    parent_rotation: transform.rotation,
                    slot_path: slot.slot_id.clone(),
                },
            )?;
        }
    }

    if let Ok(mut entity_mut) = world.get_entity_mut(root_entity) {
        entity_mut.insert((
            identity.clone(),
            OccurrenceClassification { mesh_dirty: false },
            transform,
        ));
    }

    Ok(())
}

fn ensure_occurrence_root_entity(
    world: &mut World,
    element_id: ElementId,
    identity: &OccurrenceIdentity,
    transform: Transform,
    label: Option<&str>,
) -> Entity {
    if let Some(entity) = crate::plugins::commands::find_entity_by_element_id(world, element_id) {
        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.insert((
                identity.clone(),
                OccurrenceClassification { mesh_dirty: false },
                transform,
            ));
            if let Some(label) = label {
                entity_mut.insert(Name::new(label.to_string()));
            }
        }
        entity
    } else {
        let mut entity = world.spawn((
            element_id,
            identity.clone(),
            OccurrenceClassification { mesh_dirty: false },
            transform,
            GlobalTransform::default(),
        ));
        entity.insert(Name::new(label.unwrap_or("Occurrence").to_string()));
        entity.id()
    }
}

fn spawn_compound_slot(
    world: &mut World,
    registry: &DefinitionRegistry,
    slot: &ChildSlotDef,
    parent_values: &HashMap<String, Value>,
    context: CompoundSpawnContext,
) -> Result<(), String> {
    if let Some(expr) = &slot.suppression_expr {
        if !evaluate_expr_bool(expr, parent_values)? {
            return Ok(());
        }
    }

    let mut child_overrides = OverrideMap::default();
    for binding in &slot.parameter_bindings {
        let value = evaluate_expr(&binding.expr, parent_values)?;
        child_overrides.set(binding.target_param.clone(), value);
    }

    let child_definition = registry.effective_definition(&slot.definition_id)?;
    let resolved = registry.resolve_bound_params_checked(&slot.definition_id, &child_overrides)?;
    let state = evaluate_definition_state(&child_definition, &resolved)?;
    let local_translation = evaluate_translation(slot, parent_values)?;
    let world_translation =
        context.parent_translation + context.parent_rotation * local_translation;

    if let Some(extrusion) =
        build_rectangular_extrusion_from_values(&child_definition, &state.values, world_translation)
    {
        let mut entity = world.spawn((
            extrusion,
            ShapeRotation(context.parent_rotation),
            NeedsMesh,
            Visibility::Visible,
            GeneratedOccurrencePart {
                owner: context.owner,
                slot_path: context.slot_path.clone(),
                definition_id: slot.definition_id.clone(),
            },
        ));
        apply_spawned_material_assignment(&mut entity, &child_definition);
    }

    if let Some(compound) = &child_definition.compound {
        for child_slot in &compound.child_slots {
            spawn_compound_slot(
                world,
                registry,
                child_slot,
                &state.values,
                CompoundSpawnContext {
                    owner: context.owner,
                    parent_translation: world_translation,
                    parent_rotation: context.parent_rotation,
                    slot_path: format!("{}.{}", context.slot_path, child_slot.slot_id),
                },
            )?;
        }
    }

    Ok(())
}

fn evaluate_definition_state(
    definition: &Definition,
    resolved: &HashMap<String, crate::plugins::modeling::definition::ResolvedParam>,
) -> Result<EvaluatedDefinitionState, String> {
    let mut values: HashMap<String, Value> = resolved
        .iter()
        .map(|(name, param)| (name.clone(), param.value.clone()))
        .collect();

    if let Some(compound) = &definition.compound {
        for derived in &compound.derived_parameters {
            let value = evaluate_expr(&derived.expr, &values)?;
            validate_value_against_param_type(
                &derived.param_type,
                &value,
                &format!("derived parameter '{}'", derived.name),
            )?;
            values.insert(derived.name.clone(), value);
        }

        for constraint in &compound.constraints {
            let passed = evaluate_expr_bool(&constraint.expr, &values)?;
            if !passed {
                let message = format!(
                    "Definition '{}' constraint '{}' failed: {}",
                    definition.name, constraint.id, constraint.message
                );
                match constraint.severity {
                    ConstraintSeverity::Error => return Err(message),
                    ConstraintSeverity::Warning => warn!("{message}"),
                }
            }
        }
    }

    Ok(EvaluatedDefinitionState { values })
}

fn evaluate_translation(
    slot: &ChildSlotDef,
    values: &HashMap<String, Value>,
) -> Result<Vec3, String> {
    let Some(translation) = &slot.transform_binding.translation else {
        return Ok(Vec3::ZERO);
    };
    if translation.len() != 3 {
        return Err(format!(
            "Child slot '{}' translation must contain exactly 3 expressions",
            slot.slot_id
        ));
    }

    Ok(Vec3::new(
        evaluate_expr_f32(&translation[0], values)?,
        evaluate_expr_f32(&translation[1], values)?,
        evaluate_expr_f32(&translation[2], values)?,
    ))
}

fn evaluate_expr(expr: &ExprNode, values: &HashMap<String, Value>) -> Result<Value, String> {
    match expr {
        ExprNode::Literal { value } => Ok(value.clone()),
        ExprNode::ParamRef { path } => lookup_expr_value(path, values),
        ExprNode::Add { left, right } => Ok(Value::from(
            evaluate_expr_f64(left, values)? + evaluate_expr_f64(right, values)?,
        )),
        ExprNode::Sub { left, right } => Ok(Value::from(
            evaluate_expr_f64(left, values)? - evaluate_expr_f64(right, values)?,
        )),
        ExprNode::Mul { left, right } => Ok(Value::from(
            evaluate_expr_f64(left, values)? * evaluate_expr_f64(right, values)?,
        )),
        ExprNode::Div { left, right } => Ok(Value::from(
            evaluate_expr_f64(left, values)? / evaluate_expr_f64(right, values)?,
        )),
        ExprNode::Min { left, right } => Ok(Value::from(
            evaluate_expr_f64(left, values)?.min(evaluate_expr_f64(right, values)?),
        )),
        ExprNode::Max { left, right } => Ok(Value::from(
            evaluate_expr_f64(left, values)?.max(evaluate_expr_f64(right, values)?),
        )),
        ExprNode::Eq { left, right } => Ok(Value::Bool(
            evaluate_expr(left, values)? == evaluate_expr(right, values)?,
        )),
        ExprNode::Gt { left, right } => Ok(Value::Bool(
            evaluate_expr_f64(left, values)? > evaluate_expr_f64(right, values)?,
        )),
        ExprNode::Lt { left, right } => Ok(Value::Bool(
            evaluate_expr_f64(left, values)? < evaluate_expr_f64(right, values)?,
        )),
        ExprNode::And { nodes } => {
            for node in nodes {
                if !evaluate_expr_bool(node, values)? {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        ExprNode::IfElse {
            condition,
            when_true,
            when_false,
        } => {
            if evaluate_expr_bool(condition, values)? {
                evaluate_expr(when_true, values)
            } else {
                evaluate_expr(when_false, values)
            }
        }
    }
}

fn evaluate_expr_f64(expr: &ExprNode, values: &HashMap<String, Value>) -> Result<f64, String> {
    evaluate_expr(expr, values)?
        .as_f64()
        .ok_or_else(|| "expression must evaluate to a numeric value".to_string())
}

fn evaluate_expr_f32(expr: &ExprNode, values: &HashMap<String, Value>) -> Result<f32, String> {
    Ok(evaluate_expr_f64(expr, values)? as f32)
}

fn evaluate_expr_bool(expr: &ExprNode, values: &HashMap<String, Value>) -> Result<bool, String> {
    evaluate_expr(expr, values)?
        .as_bool()
        .ok_or_else(|| "expression must evaluate to a boolean value".to_string())
}

fn lookup_expr_value(path: &str, values: &HashMap<String, Value>) -> Result<Value, String> {
    if let Some(value) = values.get(path) {
        return Ok(value.clone());
    }

    if let Some(last_segment) = path.rsplit('.').next() {
        if let Some(value) = values.get(last_segment) {
            return Ok(value.clone());
        }
    }

    Err(format!("Expression references unknown parameter '{path}'"))
}

fn validate_value_against_param_type(
    param_type: &ParamType,
    value: &Value,
    context: &str,
) -> Result<(), String> {
    match param_type {
        ParamType::Numeric if value.is_number() => Ok(()),
        ParamType::Boolean if value.is_boolean() => Ok(()),
        ParamType::StringVal if value.is_string() => Ok(()),
        ParamType::Enum(variants) => {
            let Some(current) = value.as_str() else {
                return Err(format!("{context} must resolve to an enum string"));
            };
            if variants.iter().any(|variant| variant == current) {
                Ok(())
            } else {
                Err(format!(
                    "{context} must be one of [{}]",
                    variants.join(", ")
                ))
            }
        }
        ParamType::Numeric => Err(format!("{context} must resolve to a number")),
        ParamType::Boolean => Err(format!("{context} must resolve to a boolean")),
        ParamType::StringVal => Err(format!("{context} must resolve to a string")),
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

fn build_rectangular_extrusion_from_values(
    definition: &Definition,
    values: &HashMap<String, Value>,
    centre: Vec3,
) -> Option<ProfileExtrusion> {
    let EvaluatorDecl::RectangularExtrusion(evaluator) = definition.evaluators.first()?;

    let width = values.get(&evaluator.width_param)?.as_f64()? as f32;
    let depth = values.get(&evaluator.depth_param)?.as_f64()? as f32;
    let height = values.get(&evaluator.height_param)?.as_f64()? as f32;

    Some(ProfileExtrusion {
        centre,
        profile: Profile2d::rectangle(width, depth),
        height,
    })
}

fn clear_occurrence_root_geometry(world: &mut World, entity: Entity) {
    remove_entity_mesh_assets(world, entity);
    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.remove::<(
            ProfileExtrusion,
            ShapeRotation,
            NeedsMesh,
            Mesh3d,
            MeshMaterial3d<StandardMaterial>,
        )>();
    }
}

fn apply_occurrence_material_assignment(
    world: &mut World,
    entity: Entity,
    definition: &Definition,
) {
    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        apply_spawned_material_assignment(&mut entity_mut, definition);
    }
}

fn clear_occurrence_material_assignment(world: &mut World, entity: Entity) {
    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.remove::<MaterialAssignment>();
    }
}

fn apply_spawned_material_assignment(entity: &mut EntityWorldMut<'_>, definition: &Definition) {
    if let Some(assignment) = definition_material_assignment(definition) {
        entity.insert(assignment);
    } else {
        entity.remove::<MaterialAssignment>();
    }
}

fn definition_material_assignment(definition: &Definition) -> Option<MaterialAssignment> {
    definition
        .domain_data
        .get("architectural")
        .and_then(|architectural| architectural.get("material_assignment"))
        .and_then(material_assignment_from_value)
}

fn cleanup_generated_occurrence_parts(world: &mut World, owner: ElementId) {
    let entities: Vec<Entity> = {
        let mut query = world.query::<(Entity, &GeneratedOccurrencePart)>();
        query
            .iter(world)
            .filter_map(|(entity, generated)| (generated.owner == owner).then_some(entity))
            .collect()
    };

    for entity in entities {
        if world.get_entity(entity).is_ok() {
            despawn_generated_entity(world, entity);
        }
    }
}

fn despawn_generated_entity(world: &mut World, entity: Entity) {
    remove_entity_mesh_assets(world, entity);
    let _ = world.despawn(entity);
}

fn remove_entity_mesh_assets(world: &mut World, entity: Entity) {
    let mesh_asset_id = world.get::<Mesh3d>(entity).map(|mesh| mesh.id());
    if let Some(mesh_asset_id) = mesh_asset_id {
        world.resource_mut::<Assets<Mesh>>().remove(mesh_asset_id);
    }
}
