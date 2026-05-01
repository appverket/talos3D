//! Occurrence system for PP51: Reusable Definition Foundation.
//!
//! An occurrence is a placed instance of a `Definition`. It carries an
//! `OccurrenceIdentity` (which definition, which version, which overrides)
//! and an `OccurrenceClassification` (dirty flag). The ECS evaluation
//! systems consume these components to produce geometry.

use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    sync::Arc,
};

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
                AxisRef, ChildSlotDef, ConstraintSeverity, Definition, DefinitionId,
                DefinitionRegistry, DefinitionVersion, EvaluatorDecl, ExprNode, GeometryParamsHash,
                OverrideMap, ParamType, RepresentationKind, SlotCount, SlotLayout,
                SlotMultiplicity, TransformBinding,
            },
            mesh_generation::NeedsMesh,
            primitive_trait::Primitive,
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
    /// Per-occurrence material override that shadows
    /// `Definition.material_assignment` when set (PP-099 / PP-MATREL-1
    /// slice 1). `None` means the occurrence inherits the Definition's
    /// default material binding via
    /// `Definition::resolve_material_assignment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_override: Option<MaterialAssignment>,
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
            material_override: None,
            domain_data: Value::Null,
            hosting: None,
        }
    }
}

// ---------------------------------------------------------------------------
// OccurrenceClassification — ECS component
// ---------------------------------------------------------------------------

/// Lightweight classification component tracking which occurrence channels
/// are up to date.
#[derive(Debug, Clone, Component)]
pub struct OccurrenceClassification {
    /// When `true` the occurrence needs to be re-evaluated by
    /// `evaluate_occurrences`.
    pub mesh_dirty: bool,
    /// When `true` material assignment or finish data needs to be rebound.
    pub material_dirty: bool,
    /// When `true` the occurrence transform needs to be propagated without
    /// necessarily rebuilding geometry.
    pub transform_dirty: bool,
}

impl OccurrenceClassification {
    pub fn dirty_all() -> Self {
        Self {
            mesh_dirty: true,
            material_dirty: true,
            transform_dirty: true,
        }
    }

    pub fn clean() -> Self {
        Self {
            mesh_dirty: false,
            material_dirty: false,
            transform_dirty: false,
        }
    }

    pub fn mark_mesh_dirty(&mut self) {
        self.mesh_dirty = true;
    }

    pub fn mark_material_dirty(&mut self) {
        self.material_dirty = true;
    }

    pub fn mark_transform_dirty(&mut self) {
        self.transform_dirty = true;
    }

    pub fn clear_all(&mut self) {
        *self = Self::clean();
    }
}

impl Default for OccurrenceClassification {
    fn default() -> Self {
        Self::dirty_all()
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
// RepresentationCache — Bevy resource
// ---------------------------------------------------------------------------

pub const DEFAULT_REPRESENTATION_CACHE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MeshCacheKey {
    pub definition_id: DefinitionId,
    pub definition_version: DefinitionVersion,
    pub geometry_params_hash: GeometryParamsHash,
    pub representation_kind: RepresentationKind,
}

impl MeshCacheKey {
    pub fn new(
        definition_id: DefinitionId,
        definition_version: DefinitionVersion,
        geometry_params_hash: GeometryParamsHash,
        representation_kind: RepresentationKind,
    ) -> Self {
        Self {
            definition_id,
            definition_version,
            geometry_params_hash,
            representation_kind,
        }
    }
}

#[derive(Debug, Clone, Resource)]
pub struct RepresentationCache {
    capacity: usize,
    entries: HashMap<MeshCacheKey, Arc<Handle<Mesh>>>,
    lru: VecDeque<MeshCacheKey>,
}

impl Default for RepresentationCache {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_REPRESENTATION_CACHE_CAPACITY)
    }
}

impl RepresentationCache {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, key: &MeshCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn get(&mut self, key: &MeshCacheKey) -> Option<Arc<Handle<Mesh>>> {
        let handle = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(handle)
    }

    pub fn insert(&mut self, key: MeshCacheKey, handle: Handle<Mesh>) -> Arc<Handle<Mesh>> {
        self.insert_shared(key, Arc::new(handle))
    }

    pub fn insert_shared(
        &mut self,
        key: MeshCacheKey,
        handle: Arc<Handle<Mesh>>,
    ) -> Arc<Handle<Mesh>> {
        if self.capacity == 0 {
            self.clear();
            return handle;
        }

        self.entries.insert(key.clone(), handle.clone());
        self.touch(&key);
        self.evict_to_capacity();
        handle
    }

    pub fn invalidate_definition(&mut self, definition_id: &DefinitionId) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|key, _| &key.definition_id != definition_id);
        self.lru.retain(|key| &key.definition_id != definition_id);
        before - self.entries.len()
    }

    pub fn invalidate_definition_version(
        &mut self,
        definition_id: &DefinitionId,
        definition_version: DefinitionVersion,
    ) -> usize {
        let before = self.entries.len();
        self.entries.retain(|key, _| {
            &key.definition_id != definition_id || key.definition_version != definition_version
        });
        self.lru.retain(|key| {
            &key.definition_id != definition_id || key.definition_version != definition_version
        });
        before - self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
    }

    fn touch(&mut self, key: &MeshCacheKey) {
        self.lru.retain(|existing| existing != key);
        self.lru.push_back(key.clone());
    }

    fn evict_to_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
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
        let Some(previous) = previous.and_then(|p| p.as_any().downcast_ref::<OccurrenceSnapshot>())
        else {
            self.apply_to(world);
            return;
        };

        let registry = world.resource::<DefinitionRegistry>().clone();
        let dirty = classify_occurrence_snapshot_change(&registry, self, previous);

        if dirty.mesh_dirty || dirty.transform_dirty {
            self.apply_to(world);
        } else if let Some(entity) =
            crate::plugins::commands::find_entity_by_element_id(world, self.element_id)
        {
            let mut entity_mut = world.entity_mut(entity);
            entity_mut.insert((self.identity.clone(), Name::new(self.label.clone())));
            if let Some(mut classification) = entity_mut.get_mut::<OccurrenceClassification>() {
                if dirty.material_dirty {
                    classification.mark_material_dirty();
                }
            } else {
                entity_mut.insert(dirty);
            }
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

fn classify_occurrence_snapshot_change(
    registry: &DefinitionRegistry,
    next: &OccurrenceSnapshot,
    previous: &OccurrenceSnapshot,
) -> OccurrenceClassification {
    let mut dirty = OccurrenceClassification::clean();

    if next.offset != previous.offset
        || next.rotation != previous.rotation
        || next.scale != previous.scale
    {
        dirty.mark_transform_dirty();
    }

    if next.identity.definition_id != previous.identity.definition_id
        || next.identity.definition_version != previous.identity.definition_version
        || next.identity.hosting != previous.identity.hosting
    {
        dirty.mark_mesh_dirty();
    }

    if next.identity.material_override != previous.identity.material_override {
        dirty.mark_material_dirty();
    }

    classify_override_map_change(registry, &next.identity, &previous.identity, &mut dirty);
    dirty
}

fn classify_override_map_change(
    registry: &DefinitionRegistry,
    next: &OccurrenceIdentity,
    previous: &OccurrenceIdentity,
    dirty: &mut OccurrenceClassification,
) {
    if next.overrides.0 == previous.overrides.0 {
        return;
    }

    let Ok(definition) = registry.effective_definition(&next.definition_id) else {
        dirty.mark_mesh_dirty();
        return;
    };

    for name in changed_override_names(&next.overrides, &previous.overrides) {
        match definition.interface.parameters.get(&name) {
            Some(parameter) if !parameter.geometry_affecting => dirty.mark_material_dirty(),
            Some(_) | None => dirty.mark_mesh_dirty(),
        }
    }
}

fn changed_override_names(next: &OverrideMap, previous: &OverrideMap) -> Vec<String> {
    let mut names: Vec<String> = next
        .0
        .keys()
        .chain(previous.0.keys())
        .filter(|name| next.get(name) != previous.get(name))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
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

    fn draw_selection(&self, world: &World, entity: Entity, gizmos: &mut Gizmos, color: Color) {
        let Some(owner) = world.get::<ElementId>(entity).copied() else {
            return;
        };

        if let Some((extrusion, rotation)) =
            world.get::<ProfileExtrusion>(entity).map(|extrusion| {
                (
                    extrusion,
                    world
                        .get::<ShapeRotation>(entity)
                        .copied()
                        .unwrap_or_default(),
                )
            })
        {
            extrusion.draw_wireframe(gizmos, rotation.0, color);
        }

        let mut drew_generated = false;
        let mut query = world
            .try_query::<(
                &GeneratedOccurrencePart,
                &ProfileExtrusion,
                Option<&ShapeRotation>,
            )>()
            .unwrap();
        for (generated, extrusion, rotation) in query.iter(world) {
            if generated.owner != owner {
                continue;
            }
            extrusion.draw_wireframe(gizmos, rotation.copied().unwrap_or_default().0, color);
            drew_generated = true;
        }

        if drew_generated {
            if let Some(bounds) = occurrence_generated_bounds(world, owner) {
                draw_occurrence_bounds(gizmos, bounds, color.with_alpha(0.55));
            }
        }
    }

    fn selection_line_count(&self, world: &World, entity: Entity) -> usize {
        let Some(owner) = world.get::<ElementId>(entity).copied() else {
            return 0;
        };
        let mut count = world
            .get::<ProfileExtrusion>(entity)
            .map(ProfileExtrusion::wireframe_line_count)
            .unwrap_or(0);
        let mut query = world
            .try_query::<(&GeneratedOccurrencePart, &ProfileExtrusion)>()
            .unwrap();
        for (generated, extrusion) in query.iter(world) {
            if generated.owner == owner {
                count += extrusion.wireframe_line_count();
            }
        }
        if count > 0 {
            count + 12
        } else {
            0
        }
    }
}

fn occurrence_generated_bounds(world: &World, owner: ElementId) -> Option<EntityBounds> {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut any = false;
    let mut query = world
        .try_query::<(
            &GeneratedOccurrencePart,
            &ProfileExtrusion,
            Option<&ShapeRotation>,
        )>()
        .unwrap();
    for (generated, extrusion, rotation) in query.iter(world) {
        if generated.owner != owner {
            continue;
        }
        let Some(bounds) = extrusion.bounds(rotation.copied().unwrap_or_default().0) else {
            continue;
        };
        min = min.min(bounds.min);
        max = max.max(bounds.max);
        any = true;
    }
    any.then_some(EntityBounds { min, max })
}

fn draw_occurrence_bounds(gizmos: &mut Gizmos, bounds: EntityBounds, color: Color) {
    let corners = bounds.corners();
    for i in 0..4 {
        gizmos.line(corners[i], corners[(i + 1) % 4], color);
    }
    for i in 4..8 {
        gizmos.line(corners[i], corners[4 + (i - 4 + 1) % 4], color);
    }
    for i in 0..4 {
        gizmos.line(corners[i], corners[i + 4], color);
    }
}

// ---------------------------------------------------------------------------
// ECS evaluation systems
// ---------------------------------------------------------------------------

/// Mark all occurrences of changed definitions as needing re-evaluation.
pub fn propagate_definition_changes(
    mut changed: ResMut<ChangedDefinitions>,
    mut representation_cache: ResMut<RepresentationCache>,
    mut query: Query<(&OccurrenceIdentity, &mut OccurrenceClassification)>,
    mut commands: Commands,
) {
    let ids = changed.drain();
    if ids.is_empty() {
        return;
    }

    invalidate_changed_definition_cache(&ids, &mut representation_cache);

    for (identity, mut classification) in &mut query {
        if ids.contains(&identity.definition_id) {
            classification.mark_mesh_dirty();
        }
        let _ = &mut commands;
    }
}

/// Full-entity variant that can insert the `NeedsEval` marker.
pub fn propagate_definition_changes_with_commands(
    mut changed: ResMut<ChangedDefinitions>,
    mut representation_cache: ResMut<RepresentationCache>,
    query: Query<(Entity, &OccurrenceIdentity)>,
    mut commands: Commands,
) {
    let ids = changed.drain();
    if ids.is_empty() {
        return;
    }

    invalidate_changed_definition_cache(&ids, &mut representation_cache);

    for (entity, identity) in &query {
        if ids.contains(&identity.definition_id) {
            commands.entity(entity).insert(NeedsEval);
        }
    }
}

fn invalidate_changed_definition_cache(
    ids: &[DefinitionId],
    representation_cache: &mut RepresentationCache,
) -> usize {
    ids.iter()
        .map(|id| representation_cache.invalidate_definition(id))
        .sum()
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
                classification.clear_all();
            } else {
                entity_mut.insert(OccurrenceClassification::clean());
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
    local_translation_offset: Vec3,
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
                    local_translation_offset: Vec3::ZERO,
                },
            )?;
        }
    }

    if let Ok(mut entity_mut) = world.get_entity_mut(root_entity) {
        entity_mut.insert((
            identity.clone(),
            OccurrenceClassification::clean(),
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
                OccurrenceClassification::clean(),
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
            OccurrenceClassification::clean(),
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
    match &slot.multiplicity {
        SlotMultiplicity::Single => {
            spawn_compound_slot_instance(world, registry, slot, parent_values, context)
        }
        SlotMultiplicity::Collection { layout, count } => {
            let instances = resolve_collection_instances(slot, layout, count, parent_values)?;
            for instance in instances {
                spawn_compound_slot_instance(
                    world,
                    registry,
                    slot,
                    parent_values,
                    CompoundSpawnContext {
                        owner: context.owner,
                        parent_translation: context.parent_translation,
                        parent_rotation: context.parent_rotation,
                        slot_path: format!("{}[{}]", context.slot_path, instance.index),
                        local_translation_offset: instance.translation,
                    },
                )?;
            }
            Ok(())
        }
    }
}

fn spawn_compound_slot_instance(
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
    let local_translation =
        evaluate_translation(slot, parent_values)? + context.local_translation_offset;
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
                    local_translation_offset: Vec3::ZERO,
                },
            )?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct CollectionInstance {
    index: usize,
    translation: Vec3,
}

fn resolve_collection_instances(
    slot: &ChildSlotDef,
    layout: &SlotLayout,
    count: &SlotCount,
    values: &HashMap<String, Value>,
) -> Result<Vec<CollectionInstance>, String> {
    let requested_count = resolve_slot_count(&slot.slot_id, count, values)?;
    match layout {
        SlotLayout::Linear {
            axis,
            spacing,
            origin,
        } => {
            let axis = axis_vector(axis)?;
            let spacing = evaluate_expr_f32(spacing, values)?;
            let origin = evaluate_transform_binding(origin, &slot.slot_id, values)?;
            Ok((0..requested_count)
                .map(|index| CollectionInstance {
                    index,
                    translation: origin + axis * spacing * (index as f32 + 1.0),
                })
                .collect())
        }
        SlotLayout::Grid {
            axis_u,
            count_u,
            spacing_u,
            axis_v,
            count_v,
            spacing_v,
            origin,
        } => {
            let u_count = resolve_expr_count(&slot.slot_id, "count_u", count_u, values)?;
            let v_count = resolve_expr_count(&slot.slot_id, "count_v", count_v, values)?;
            let grid_count = u_count.checked_mul(v_count).ok_or_else(|| {
                format!(
                    "Collection slot '{}' grid count overflows usize",
                    slot.slot_id
                )
            })?;
            ensure_layout_count_matches(slot, requested_count, grid_count)?;
            let axis_u = axis_vector(axis_u)?;
            let axis_v = axis_vector(axis_v)?;
            let spacing_u = evaluate_expr_f32(spacing_u, values)?;
            let spacing_v = evaluate_expr_f32(spacing_v, values)?;
            let origin = evaluate_transform_binding(origin, &slot.slot_id, values)?;
            let mut instances = Vec::with_capacity(grid_count);
            for v in 0..v_count {
                for u in 0..u_count {
                    let index = v * u_count + u;
                    instances.push(CollectionInstance {
                        index,
                        translation: origin
                            + axis_u * spacing_u * (u as f32 + 1.0)
                            + axis_v * spacing_v * (v as f32 + 1.0),
                    });
                }
            }
            Ok(instances)
        }
        SlotLayout::BySpacingFromHost { host_param, axis } => {
            let axis = axis_vector(axis)?;
            let spacing = values
                .get(&host_param.name)
                .and_then(Value::as_f64)
                .ok_or_else(|| {
                    format!(
                        "Collection slot '{}' host spacing parameter '{}' must resolve to a number",
                        slot.slot_id, host_param.name
                    )
                })? as f32;
            Ok((0..requested_count)
                .map(|index| CollectionInstance {
                    index,
                    translation: axis * spacing * (index as f32 + 1.0),
                })
                .collect())
        }
        SlotLayout::LitePattern { pattern } => {
            let pattern = evaluate_expr(pattern, values)?;
            let pattern = pattern.as_str().ok_or_else(|| {
                format!(
                    "Collection slot '{}' lite pattern must resolve to a string",
                    slot.slot_id
                )
            })?;
            let (u_count, v_count) = parse_lite_pattern_dims(&slot.slot_id, pattern)?;
            let pattern_count = u_count.checked_mul(v_count).ok_or_else(|| {
                format!(
                    "Collection slot '{}' lite pattern count overflows usize",
                    slot.slot_id
                )
            })?;
            ensure_layout_count_matches(slot, requested_count, pattern_count)?;
            let mut instances = Vec::with_capacity(pattern_count);
            for v in 0..v_count {
                for u in 0..u_count {
                    let index = v * u_count + u;
                    instances.push(CollectionInstance {
                        index,
                        translation: Vec3::new(u as f32, v as f32, 0.0),
                    });
                }
            }
            Ok(instances)
        }
    }
}

fn ensure_layout_count_matches(
    slot: &ChildSlotDef,
    requested_count: usize,
    layout_count: usize,
) -> Result<(), String> {
    if requested_count == layout_count {
        Ok(())
    } else {
        Err(format!(
            "Collection slot '{}' count ({requested_count}) must match layout count ({layout_count})",
            slot.slot_id
        ))
    }
}

fn resolve_slot_count(
    slot_id: &str,
    count: &SlotCount,
    values: &HashMap<String, Value>,
) -> Result<usize, String> {
    match count {
        SlotCount::Fixed(count) => usize::try_from(*count).map_err(|_| {
            format!("Collection slot '{slot_id}' fixed count does not fit this platform")
        }),
        SlotCount::DerivedFromExpr(expr) => resolve_expr_count(slot_id, "count", expr, values),
    }
}

fn resolve_expr_count(
    slot_id: &str,
    field: &str,
    expr: &ExprNode,
    values: &HashMap<String, Value>,
) -> Result<usize, String> {
    let value = evaluate_expr_f64(expr, values)?;
    if value < 0.0 || value.fract().abs() > f64::EPSILON {
        return Err(format!(
            "Collection slot '{slot_id}' {field} must resolve to a non-negative integer"
        ));
    }
    if value > usize::MAX as f64 {
        return Err(format!(
            "Collection slot '{slot_id}' {field} is too large for this platform"
        ));
    }
    Ok(value as usize)
}

fn evaluate_transform_binding(
    binding: &TransformBinding,
    slot_id: &str,
    values: &HashMap<String, Value>,
) -> Result<Vec3, String> {
    let Some(translation) = &binding.translation else {
        return Ok(Vec3::ZERO);
    };
    if translation.len() != 3 {
        return Err(format!(
            "Child slot '{slot_id}' collection origin must contain exactly 3 expressions"
        ));
    }
    Ok(Vec3::new(
        evaluate_expr_f32(&translation[0], values)?,
        evaluate_expr_f32(&translation[1], values)?,
        evaluate_expr_f32(&translation[2], values)?,
    ))
}

fn axis_vector(axis: &AxisRef) -> Result<Vec3, String> {
    match axis.0.as_str() {
        "x" | "X" | "u" | "U" | "horizontal" => Ok(Vec3::X),
        "y" | "Y" | "v" | "V" | "vertical" => Ok(Vec3::Y),
        "z" | "Z" | "w" | "W" | "depth" => Ok(Vec3::Z),
        other => Err(format!(
            "Unknown collection slot axis '{other}'; expected x/y/z or u/v/w"
        )),
    }
}

fn parse_lite_pattern_dims(slot_id: &str, pattern: &str) -> Result<(usize, usize), String> {
    let normalized = pattern.trim().replace(['X', '*'], "x").replace('×', "x");
    let Some((u, v)) = normalized.split_once('x') else {
        return Err(format!(
            "Collection slot '{slot_id}' lite pattern '{pattern}' must use '<columns>x<rows>'"
        ));
    };
    let u = u.trim().parse::<usize>().map_err(|_| {
        format!("Collection slot '{slot_id}' lite pattern '{pattern}' has invalid column count")
    })?;
    let v = v.trim().parse::<usize>().map_err(|_| {
        format!("Collection slot '{slot_id}' lite pattern '{pattern}' has invalid row count")
    })?;
    if u == 0 || v == 0 {
        return Err(format!(
            "Collection slot '{slot_id}' lite pattern '{pattern}' must have non-zero dimensions"
        ));
    }
    Ok((u, v))
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
        ParamType::AxisRef if value.is_string() => Ok(()),
        ParamType::ParameterRef { .. } if value.is_string() => Ok(()),
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
        ParamType::AxisRef => Err(format!(
            "{context} must resolve to a host-frame axis reference"
        )),
        ParamType::ParameterRef { side } => Err(format!(
            "{context} must resolve to a parameter reference on the {side:?} side"
        )),
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

#[cfg(test)]
mod pp_098_dirty_taxonomy_tests {
    use super::*;
    use crate::plugins::modeling::definition::{
        DefinitionKind, Interface, OverridePolicy, ParameterDef, ParameterMetadata, ParameterSchema,
    };
    use serde_json::json;

    fn parameter(name: &str, geometry_affecting: bool) -> ParameterDef {
        ParameterDef {
            name: name.to_string(),
            param_type: ParamType::StringVal,
            default_value: json!("default"),
            override_policy: OverridePolicy::Overridable,
            geometry_affecting,
            metadata: ParameterMetadata::default(),
        }
    }

    fn registry_with_definition() -> (DefinitionRegistry, DefinitionId) {
        let definition_id = DefinitionId("dirty.test".to_string());
        let definition = Definition {
            id: definition_id.clone(),
            base_definition_id: None,
            name: "Dirty Test".to_string(),
            definition_kind: DefinitionKind::Solid,
            definition_version: 1,
            interface: Interface {
                parameters: ParameterSchema(vec![
                    parameter("width", true),
                    parameter("finish_color", false),
                ]),
                void_declaration: None,
                external_context_requirements: Vec::new(),
            },
            evaluators: Vec::new(),
            representations: Vec::new(),
            compound: None,
            material_assignment: None,
            domain_data: Value::Null,
        };
        let mut registry = DefinitionRegistry::default();
        registry.insert(definition);
        (registry, definition_id)
    }

    fn snapshot_with_override(
        element_id: ElementId,
        definition_id: DefinitionId,
        name: &str,
        value: Value,
    ) -> OccurrenceSnapshot {
        let mut identity = OccurrenceIdentity::new(definition_id, 1);
        identity.overrides.set(name, value);
        OccurrenceSnapshot::new(element_id, identity, "Occurrence")
    }

    #[test]
    fn occurrence_classification_default_marks_all_channels_dirty() {
        let classification = OccurrenceClassification::default();

        assert!(classification.mesh_dirty);
        assert!(classification.material_dirty);
        assert!(classification.transform_dirty);
    }

    #[test]
    fn occurrence_classification_clean_marks_all_channels_clean() {
        let classification = OccurrenceClassification::clean();

        assert!(!classification.mesh_dirty);
        assert!(!classification.material_dirty);
        assert!(!classification.transform_dirty);
    }

    #[test]
    fn occurrence_classification_channels_can_be_marked_independently() {
        let mut classification = OccurrenceClassification::clean();

        classification.mark_material_dirty();
        assert!(!classification.mesh_dirty);
        assert!(classification.material_dirty);
        assert!(!classification.transform_dirty);

        classification.mark_transform_dirty();
        assert!(!classification.mesh_dirty);
        assert!(classification.material_dirty);
        assert!(classification.transform_dirty);

        classification.mark_mesh_dirty();
        assert!(classification.mesh_dirty);
        assert!(classification.material_dirty);
        assert!(classification.transform_dirty);

        classification.clear_all();
        assert_eq!(
            (
                classification.mesh_dirty,
                classification.material_dirty,
                classification.transform_dirty
            ),
            (false, false, false)
        );
    }

    #[test]
    fn non_geometry_override_change_marks_material_not_mesh_dirty() {
        let (registry, definition_id) = registry_with_definition();
        let element_id = ElementId(1);
        let previous = snapshot_with_override(
            element_id,
            definition_id.clone(),
            "finish_color",
            json!("white"),
        );
        let next =
            snapshot_with_override(element_id, definition_id, "finish_color", json!("black"));

        let dirty = classify_occurrence_snapshot_change(&registry, &next, &previous);

        assert!(!dirty.mesh_dirty);
        assert!(dirty.material_dirty);
        assert!(!dirty.transform_dirty);
    }

    #[test]
    fn geometry_override_change_marks_mesh_dirty() {
        let (registry, definition_id) = registry_with_definition();
        let element_id = ElementId(2);
        let previous =
            snapshot_with_override(element_id, definition_id.clone(), "width", json!(1.0));
        let next = snapshot_with_override(element_id, definition_id, "width", json!(2.0));

        let dirty = classify_occurrence_snapshot_change(&registry, &next, &previous);

        assert!(dirty.mesh_dirty);
        assert!(!dirty.material_dirty);
        assert!(!dirty.transform_dirty);
    }
}

#[cfg(test)]
mod pp_098_representation_cache_tests {
    use super::*;

    fn key(id: &str, version: DefinitionVersion, hash: &str) -> MeshCacheKey {
        MeshCacheKey::new(
            DefinitionId(id.to_string()),
            version,
            GeometryParamsHash(hash.to_string()),
            RepresentationKind::PrimaryGeometry,
        )
    }

    #[test]
    fn representation_cache_default_capacity_is_1024() {
        let cache = RepresentationCache::default();
        assert_eq!(cache.capacity(), DEFAULT_REPRESENTATION_CACHE_CAPACITY);
        assert!(cache.is_empty());
    }

    #[test]
    fn representation_cache_hit_returns_shared_mesh_handle_arc() {
        let mut cache = RepresentationCache::with_capacity(4);
        let key = key("window", 1, "blake3:a");
        let inserted = cache.insert(key.clone(), Handle::<Mesh>::default());
        let hit = cache.get(&key).expect("cache hit");

        assert!(Arc::ptr_eq(&inserted, &hit));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn representation_cache_evicts_least_recently_used_entry() {
        let mut cache = RepresentationCache::with_capacity(2);
        let a = key("window", 1, "blake3:a");
        let b = key("window", 1, "blake3:b");
        let c = key("window", 1, "blake3:c");

        cache.insert(a.clone(), Handle::<Mesh>::default());
        cache.insert(b.clone(), Handle::<Mesh>::default());
        assert!(cache.get(&a).is_some(), "hit should refresh recency");
        cache.insert(c.clone(), Handle::<Mesh>::default());

        assert!(cache.contains_key(&a));
        assert!(!cache.contains_key(&b));
        assert!(cache.contains_key(&c));
    }

    #[test]
    fn representation_cache_invalidates_definition_entries() {
        let mut cache = RepresentationCache::with_capacity(4);
        let window_a = key("window", 1, "blake3:a");
        let window_b = key("window", 2, "blake3:b");
        let door = key("door", 1, "blake3:c");
        cache.insert(window_a.clone(), Handle::<Mesh>::default());
        cache.insert(window_b.clone(), Handle::<Mesh>::default());
        cache.insert(door.clone(), Handle::<Mesh>::default());

        let removed = cache.invalidate_definition(&DefinitionId("window".to_string()));

        assert_eq!(removed, 2);
        assert!(!cache.contains_key(&window_a));
        assert!(!cache.contains_key(&window_b));
        assert!(cache.contains_key(&door));
    }

    #[test]
    fn representation_cache_can_invalidate_one_definition_version() {
        let mut cache = RepresentationCache::with_capacity(4);
        let old = key("window", 1, "blake3:a");
        let current = key("window", 2, "blake3:b");
        cache.insert(old.clone(), Handle::<Mesh>::default());
        cache.insert(current.clone(), Handle::<Mesh>::default());

        let removed = cache.invalidate_definition_version(&DefinitionId("window".to_string()), 1);

        assert_eq!(removed, 1);
        assert!(!cache.contains_key(&old));
        assert!(cache.contains_key(&current));
    }

    #[test]
    fn changed_definition_propagation_invalidates_matching_cache_entries() {
        let mut app = App::new();
        app.init_resource::<ChangedDefinitions>()
            .init_resource::<RepresentationCache>()
            .add_systems(Update, propagate_definition_changes_with_commands);

        let window_key = key("window", 1, "blake3:a");
        let window_next_key = key("window", 2, "blake3:b");
        let door_key = key("door", 1, "blake3:c");
        {
            let mut cache = app.world_mut().resource_mut::<RepresentationCache>();
            cache.insert(window_key.clone(), Handle::<Mesh>::default());
            cache.insert(window_next_key.clone(), Handle::<Mesh>::default());
            cache.insert(door_key.clone(), Handle::<Mesh>::default());
        }

        let entity = app
            .world_mut()
            .spawn((
                OccurrenceIdentity::new(DefinitionId("window".to_string()), 1),
                OccurrenceClassification::clean(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<ChangedDefinitions>()
            .mark_changed(DefinitionId("window".to_string()));

        app.update();

        let cache = app.world().resource::<RepresentationCache>();
        assert!(!cache.contains_key(&window_key));
        assert!(!cache.contains_key(&window_next_key));
        assert!(cache.contains_key(&door_key));
        assert!(app.world().entity(entity).contains::<NeedsEval>());
    }
}
