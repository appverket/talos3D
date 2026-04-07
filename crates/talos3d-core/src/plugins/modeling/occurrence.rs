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
        modeling::{
            definition::{DefinitionId, DefinitionRegistry, DefinitionVersion, OverrideMap},
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
}

impl OccurrenceIdentity {
    /// Construct a minimal identity referencing `definition_id` with no overrides.
    pub fn new(definition_id: DefinitionId, definition_version: DefinitionVersion) -> Self {
        Self {
            definition_id,
            definition_version,
            overrides: OverrideMap::default(),
            domain_data: Value::Null,
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
        // Placeholder — will be replaced with mesh-bounds centre when available.
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

    /// Returns an empty vec because we have no registry access at this call site.
    /// Property inspection is handled at the model-API layer where the registry
    /// is available.
    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![]
    }

    /// Unconditionally update the override map.
    ///
    /// Parameter existence is validated at the model-API layer; here we just
    /// accept the value so that the snapshot remains consistent with what the
    /// caller sent.
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
        // Clone the registry so we release the immutable borrow before mutating
        // the world below.
        let registry = world.resource::<DefinitionRegistry>().clone();

        let Some(resolved) =
            registry.resolve_params(&self.identity.definition_id, &self.identity.overrides)
        else {
            // Definition not found — nothing to render.
            return;
        };

        let Some(extrusion) = build_rectangular_extrusion(
            &registry,
            &self.identity.definition_id,
            &resolved,
            self.offset,
        ) else {
            return;
        };

        apply_mesh_primitive(world, self.element_id, extrusion, ShapeRotation::default());

        // Attach occurrence-specific ECS components so the entity is
        // query-able as an occurrence and can be re-evaluated.
        if let Some(entity) =
            crate::plugins::commands::find_entity_by_element_id(world, self.element_id)
        {
            world.entity_mut(entity).insert((
                self.identity.clone(),
                OccurrenceClassification { mesh_dirty: false },
            ));
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
            // Even if shape unchanged, keep identity component current.
            world.entity_mut(entity).insert(self.identity.clone());
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        Some(Transform {
            translation: self.offset,
            rotation: self.rotation,
            scale: self.scale,
        })
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {
        // No preview wire-frame at this layer.
    }

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
        // Restore transform components if available.
        let transform = entity_ref
            .get::<bevy::prelude::Transform>()
            .copied()
            .unwrap_or_default();
        Some(
            OccurrenceSnapshot {
                element_id,
                identity,
                label: "Occurrence".to_string(),
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
            // NeedsEval is inserted here; evaluate_occurrences will remove it.
        }
        let _ = &mut commands; // suppress unused warning — NeedsEval insert happens via entity below
    }

    // We need entity access to insert NeedsEval; do a second pass.
    // (Bevy requires Commands for structural changes inside a system.)
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
///
/// For each such entity this system:
/// 1. Looks up the `Definition` in the registry.
/// 2. Resolves parameters (applying occurrence overrides over definition defaults).
/// 3. Evaluates the first `RectangularExtrusion` evaluator it finds.
/// 4. Writes the resulting `ProfileExtrusion` back onto the entity.
/// 5. Removes the `NeedsEval` marker and clears `mesh_dirty`.
pub fn evaluate_occurrences(
    registry: Res<DefinitionRegistry>,
    query: Query<(Entity, &OccurrenceIdentity), With<NeedsEval>>,
    mut classification_query: Query<&mut OccurrenceClassification>,
    mut commands: Commands,
) {
    use crate::plugins::modeling::mesh_generation::NeedsMesh;

    for (entity, identity) in &query {
        let Some(resolved) = registry.resolve_params(&identity.definition_id, &identity.overrides)
        else {
            commands.entity(entity).remove::<NeedsEval>();
            continue;
        };

        if let Some(extrusion) =
            build_rectangular_extrusion(&registry, &identity.definition_id, &resolved, Vec3::ZERO)
        {
            commands
                .entity(entity)
                .insert((extrusion, ShapeRotation::default(), NeedsMesh));
        }

        commands.entity(entity).remove::<NeedsEval>();

        if let Ok(mut cls) = classification_query.get_mut(entity) {
            cls.mesh_dirty = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Attempt to build a `ProfileExtrusion` from resolved parameters by
/// inspecting the definition's evaluator list for the first
/// `RectangularExtrusion` evaluator.
///
/// Returns `None` if:
/// - the definition does not exist, or
/// - no `RectangularExtrusion` evaluator is declared, or
/// - the required parameters are missing or non-numeric.
fn build_rectangular_extrusion(
    registry: &DefinitionRegistry,
    id: &DefinitionId,
    resolved: &HashMap<String, crate::plugins::modeling::definition::ResolvedParam>,
    centre: Vec3,
) -> Option<ProfileExtrusion> {
    use crate::plugins::modeling::definition::EvaluatorDecl;

    let def = registry.get(id)?;

    let evaluator = def.evaluators.iter().find_map(|e| match e {
        EvaluatorDecl::RectangularExtrusion(re) => Some(re),
    })?;

    let width = resolved
        .get(&evaluator.width_param)
        .and_then(|p| p.value.as_f64())
        .map(|v| v as f32)?;

    let depth = resolved
        .get(&evaluator.depth_param)
        .and_then(|p| p.value.as_f64())
        .map(|v| v as f32)?;

    let height = resolved
        .get(&evaluator.height_param)
        .and_then(|p| p.value.as_f64())
        .map(|v| v as f32)?;

    Some(ProfileExtrusion {
        centre,
        profile: Profile2d::rectangle(width, depth),
        height,
    })
}
