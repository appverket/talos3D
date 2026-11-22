//! Core-owned material and refinement metadata for capability snapshots.
//! Registered factories provide the envelope on capture and restore. Native
//! material-aware snapshots (especially occurrence inheritance) keep their contract.
use crate::authored_entity::*;
use crate::capability_registry::*;
use crate::plugins::modeling::dependency_graph::EntityDependencies;
use crate::plugins::{identity::ElementId, materials::MaterialAssignment};
use bevy::prelude::*;
use serde_json::Value;
use std::any::Any;

const REFINEMENT_KEY: &str = "core.refinement_metadata.v1";
use crate::plugins::refinement::RefinementMetadataSnapshot;

const MATERIAL_KEY: &str = "core.material_assignment.v1";

#[derive(Clone)]
struct AuthoredMetadataSnapshot {
    inner: BoxedEntity,
    assignment: Option<MaterialAssignment>,
    material_owned: bool,
    refinement: Option<RefinementMetadataSnapshot>,
    overlay: bool,
}

pub(crate) fn with_material(
    inner: BoxedEntity,
    assignment: Option<MaterialAssignment>,
) -> BoxedEntity {
    BoxedEntity(Box::new(AuthoredMetadataSnapshot {
        inner,
        assignment,
        material_owned: true,
        refinement: None,
        overlay: false,
    }))
}

/// The same envelope carries non-geometric authorities through deletion, undo,
/// copy and persistence. Creation overlays preserve factory-owned native defaults.
pub(crate) fn with_refinement(
    inner: BoxedEntity,
    metadata: RefinementMetadataSnapshot,
    overlay: bool,
) -> BoxedEntity {
    if metadata.is_empty() {
        return inner;
    }
    BoxedEntity(Box::new(AuthoredMetadataSnapshot {
        inner,
        assignment: None,
        material_owned: false,
        refinement: Some(metadata),
        overlay,
    }))
}

impl AuthoredMetadataSnapshot {
    fn carry(&self, inner: BoxedEntity) -> BoxedEntity {
        BoxedEntity(Box::new(Self {
            inner,
            ..self.clone()
        }))
    }
    fn json(&self, mut data: Value, persisted: bool) -> Value {
        let fields = data.as_object_mut().expect("authored snapshot object");
        if self.material_owned {
            fields.insert(
                MATERIAL_KEY.into(),
                serde_json::to_value(&self.assignment).expect("material assignment"),
            );
        }
        if let Some(refinement) = self.refinement.as_ref().filter(|_| persisted) {
            fields.insert(
                REFINEMENT_KEY.into(),
                serde_json::json!({"metadata":refinement,"overlay":self.overlay}),
            );
        }
        data
    }
    fn apply_metadata(&self, world: &mut World) {
        if let Some(entity) =
            crate::plugins::commands::find_entity_by_element_id(world, self.element_id())
        {
            if self.material_owned {
                crate::plugins::materials::set_authored_material_assignment(
                    world,
                    entity,
                    self.assignment.clone(),
                );
            }
            if let Some(metadata) = &self.refinement {
                if self.overlay {
                    metadata.apply_overlay(world, entity);
                } else {
                    metadata.apply(world, entity);
                }
            }
        }
    }
}

impl AuthoredEntity for AuthoredMetadataSnapshot {
    fn snapshot_body(&self) -> Option<&dyn AuthoredEntity> {
        Some(self.inner.0.as_ref())
    }

    fn as_any(&self) -> &dyn Any {
        self.inner.0.as_any()
    }
    fn type_name(&self) -> &'static str {
        self.inner.0.type_name()
    }
    fn creation_label(&self) -> Option<&'static str> {
        self.inner.0.creation_label()
    }
    fn element_id(&self) -> ElementId {
        self.inner.0.element_id()
    }
    fn label(&self) -> String {
        self.inner.0.label()
    }
    fn center(&self) -> Vec3 {
        self.inner.0.center()
    }
    fn copy_with_ids(
        &self,
        _ids: &std::collections::HashMap<ElementId, ElementId>,
    ) -> Option<BoxedEntity> {
        self.inner.0.copy_with_ids(_ids).map(|inner| {
            let mut copied = Self {
                inner,
                ..self.clone()
            };
            if let Some(metadata) = &mut copied.refinement {
                metadata.remap_ids(_ids);
            }
            BoxedEntity(Box::new(copied))
        })
    }
    fn dependency_order(&self) -> u8 {
        self.inner.0.dependency_order()
    }
    fn scope(&self) -> EntityScope {
        self.inner.0.scope()
    }
    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        self.carry(self.inner.0.translate_by(delta))
    }
    fn translate_with_parent(&self, delta: Vec3) -> BoxedEntity {
        self.carry(self.inner.0.translate_with_parent(delta))
    }
    fn with_transformed_parent(&self, _parent: &dyn AuthoredEntity) -> Option<BoxedEntity> {
        self.inner
            .0
            .with_transformed_parent(_parent)
            .map(|s| self.carry(s))
    }
    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        self.carry(self.inner.0.rotate_by(rotation))
    }
    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        self.carry(self.inner.0.scale_by(factor, center))
    }
    fn push_pull(
        &self,
        _face_id: crate::capability_registry::FaceId,
        _distance: f32,
    ) -> Option<BoxedEntity> {
        self.inner
            .0
            .push_pull(_face_id, _distance)
            .map(|s| self.carry(s))
    }
    fn push_pull_affordance(
        &self,
        _face_id: crate::capability_registry::FaceId,
    ) -> PushPullAffordance {
        self.inner.0.push_pull_affordance(_face_id)
    }
    fn edit_boundary(
        &self,
        _boundary: &str,
        _distance: Option<f32>,
    ) -> Result<Vec<BoxedEntity>, String> {
        self.inner
            .0
            .edit_boundary(_boundary, _distance)
            .map(|items| items.into_iter().map(|s| self.carry(s)).collect())
    }
    fn transform_parent(&self) -> Option<ElementId> {
        self.inner.0.transform_parent()
    }
    fn hosting_selection_context(
        &self,
    ) -> Option<crate::plugins::hosting_contracts::HostedSelectionContext> {
        self.inner.0.hosting_selection_context()
    }
    fn hosted_axis(&self) -> Option<Vec3> {
        self.inner.0.hosted_axis()
    }
    fn hosting_edge_margin(&self) -> Option<f32> {
        self.inner.0.hosting_edge_margin()
    }
    fn position_along_host(&self, _point: Vec3) -> Option<f64> {
        self.inner.0.position_along_host(_point)
    }
    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        self.inner.property_fields()
    }
    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        if property_name == "material_assignment" {
            return Ok(with_material(
                self.inner.clone(),
                crate::plugins::materials::material_assignment_option_from_value(value)?,
            ));
        }
        self.inner
            .set_property_json(property_name, value)
            .map(|s| self.carry(s))
    }
    fn refinement_metadata(&self) -> Option<&RefinementMetadataSnapshot> {
        self.refinement
            .as_ref()
            .or_else(|| self.inner.0.refinement_metadata())
    }
    fn material_assignment(&self) -> Option<MaterialAssignment> {
        if self.material_owned {
            self.assignment.clone()
        } else {
            self.inner.material_assignment()
        }
    }
    fn set_material_assignment(
        &self,
        assignment: Option<MaterialAssignment>,
    ) -> Result<BoxedEntity, String> {
        Ok(BoxedEntity(Box::new(Self {
            assignment,
            material_owned: true,
            ..self.clone()
        })))
    }
    fn subobject_display_overrides(&self) -> Option<SubobjectDisplayOverrides> {
        self.inner.0.subobject_display_overrides()
    }
    fn set_subobject_display_overrides(
        &self,
        overrides: Option<SubobjectDisplayOverrides>,
    ) -> Result<BoxedEntity, String> {
        self.inner
            .0
            .set_subobject_display_overrides(overrides)
            .map(|s| self.carry(s))
    }
    fn handles(&self) -> Vec<HandleInfo> {
        self.inner.0.handles()
    }
    fn bounds(&self) -> Option<EntityBounds> {
        self.inner.0.bounds()
    }
    fn bounds_in_frame(&self, _rotation: Quat) -> Option<EntityBounds> {
        self.inner.0.bounds_in_frame(_rotation)
    }
    fn snap_segments(&self) -> Vec<(Vec3, Vec3)> {
        self.inner.0.snap_segments()
    }
    fn to_json(&self) -> Value {
        self.json(self.inner.to_json(), false)
    }
    fn plan_fingerprint(&self) -> Result<(blake3::Hash, usize), String> {
        let (geometry, bytes) = self.inner.0.plan_fingerprint()?;
        let (material, material_bytes) = snapshot_fingerprint(&(
            &self.assignment,
            &self.refinement,
            self.material_owned,
            self.overlay,
        ))?;
        let mut hash = blake3::Hasher::new();
        hash.update(geometry.as_bytes());
        hash.update(material.as_bytes());
        Ok((hash.finalize(), bytes + material_bytes))
    }
    fn plan_retained_assets(&self) -> Vec<(blake3::Hash, usize)> {
        self.inner.0.plan_retained_assets()
    }
    fn matches_entity(
        &self,
        _entity: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<bool> {
        self.inner.0.matches_entity(_entity, _world).map(|same| {
            same && (!self.material_owned
                || _entity.get::<MaterialAssignment>() == self.assignment.as_ref())
                && self
                    .refinement
                    .as_ref()
                    .is_none_or(|m| m == &RefinementMetadataSnapshot::capture_entity(_entity))
        })
    }
    fn to_persisted_json(&self) -> Value {
        self.json(self.inner.to_persisted_json(), true)
    }
    fn apply_to(&self, world: &mut World) {
        self.inner.apply_to(world);
        self.apply_metadata(world);
    }
    fn apply_with_previous(&self, world: &mut World, _previous: Option<&dyn AuthoredEntity>) {
        self.inner
            .0
            .apply_with_previous(world, _previous.map(|s| s.snapshot_body().unwrap_or(s)));
        self.apply_metadata(world);
    }
    fn remove_from(&self, world: &mut World) {
        self.inner.0.remove_from(world)
    }
    fn preview_transform_from(&self, _before: &dyn AuthoredEntity) -> Option<Transform> {
        self.inner.0.preview_transform_from(_before)
    }
    fn isolated_snapshot_preview(&self) -> bool {
        self.inner.0.isolated_snapshot_preview()
    }
    fn preview_transform(&self) -> Option<Transform> {
        self.inner.0.preview_transform()
    }
    fn drag_handle(&self, _handle_id: &str, _cursor: Vec3) -> Option<BoxedEntity> {
        self.inner
            .0
            .drag_handle(_handle_id, _cursor)
            .map(|s| self.carry(s))
    }
    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        self.inner.0.draw_preview(gizmos, color)
    }
    fn preview_line_count(&self) -> usize {
        self.inner.0.preview_line_count()
    }
    fn sync_preview_entity(&self, _world: &mut World, _existing: Option<Entity>) -> Option<Entity> {
        self.inner.0.sync_preview_entity(_world, _existing)
    }
    fn cleanup_preview_entity(&self, world: &mut World, preview_entity: Entity) {
        self.inner.0.cleanup_preview_entity(world, preview_entity)
    }
    fn box_clone(&self) -> BoxedEntity {
        self.carry(self.inner.clone())
    }
    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        self.material_assignment() == other.material_assignment()
            && self.refinement_metadata() == other.refinement_metadata()
            && self
                .inner
                .0
                .eq_snapshot(other.snapshot_body().unwrap_or(other))
    }
}

pub(crate) struct MaterialFactory<F>(pub F);
impl<F: AuthoredEntityFactory> AuthoredEntityFactory for MaterialFactory<F> {
    fn type_name(&self) -> &'static str {
        self.0.type_name()
    }
    fn creation_schema(&self) -> Option<Value> {
        self.0.creation_schema()
    }
    fn capture_snapshot(&self, entity_ref: &EntityRef, world: &World) -> Option<BoxedEntity> {
        self.0
            .capture_snapshot(entity_ref, world)
            .map(|snapshot| {
                // Occurrences resolve inherited display material separately from overrides.
                if entity_ref.contains::<crate::plugins::modeling::occurrence::OccurrenceIdentity>()
                    || snapshot.material_assignment().is_some()
                {
                    return snapshot;
                }
                match entity_ref.get::<MaterialAssignment>() {
                    Some(assignment) => with_material(snapshot, Some(assignment.clone())),
                    None => snapshot,
                }
            })
            .map(|snapshot| {
                with_refinement(
                    snapshot,
                    RefinementMetadataSnapshot::capture_entity(entity_ref),
                    false,
                )
            })
    }
    fn capture_edit_snapshot(&self, entity_ref: &EntityRef, world: &World) -> Option<BoxedEntity> {
        self.0
            .capture_edit_snapshot(entity_ref, world)
            .map(|snapshot| {
                // Occurrences resolve inherited display material separately from overrides.
                if entity_ref.contains::<crate::plugins::modeling::occurrence::OccurrenceIdentity>()
                    || snapshot.material_assignment().is_some()
                {
                    return snapshot;
                }
                match entity_ref.get::<MaterialAssignment>() {
                    Some(assignment) => with_material(snapshot, Some(assignment.clone())),
                    None => snapshot,
                }
            })
            .map(|snapshot| {
                with_refinement(
                    snapshot,
                    RefinementMetadataSnapshot::capture_entity(entity_ref),
                    false,
                )
            })
    }
    fn display_label(&self, entity_ref: &EntityRef, world: &World) -> Option<String> {
        self.0.display_label(entity_ref, world)
    }
    fn allows_automatic_face_edit(&self, _entity_ref: &EntityRef) -> bool {
        self.0.allows_automatic_face_edit(_entity_ref)
    }
    fn capture_role(&self, _entity_ref: &EntityRef, _world: &World) -> SnapshotCaptureRole {
        self.0.capture_role(_entity_ref, _world)
    }
    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        // Strip core envelopes before a factory decodes its own representation;
        // externally tagged enum bodies must still have exactly one variant key.
        let mut geometry = data.clone();
        if let Some(fields) = geometry.as_object_mut() {
            fields.remove(MATERIAL_KEY);
            fields.remove(REFINEMENT_KEY);
        }
        let mut snapshot = self.0.from_persisted_json(&geometry)?;
        if let Some(assignment) = data.get(MATERIAL_KEY) {
            let assignment =
                serde_json::from_value::<Option<MaterialAssignment>>(assignment.clone())
                    .map_err(|e| e.to_string())?;
            snapshot = with_material(snapshot, assignment);
        }
        if let Some(value) = data.get(REFINEMENT_KEY) {
            let metadata = serde_json::from_value(value["metadata"].clone())
                .map_err(|e| format!("Invalid refinement metadata: {e}"))?;
            let overlay = value["overlay"]
                .as_bool()
                .ok_or("Invalid refinement overlay flag")?;
            snapshot = with_refinement(snapshot, metadata, overlay);
        }
        Ok(snapshot)
    }
    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        self.0.from_create_request(world, request)
    }
    fn draw_selection(&self, _world: &World, _entity: Entity, _gizmos: &mut Gizmos, _color: Color) {
        self.0.draw_selection(_world, _entity, _gizmos, _color)
    }
    fn selection_line_count(&self, _world: &World, _entity: Entity) -> usize {
        self.0.selection_line_count(_world, _entity)
    }
    fn hit_test(&self, _world: &World, _ray: Ray3d) -> Option<HitCandidate> {
        self.0.hit_test(_world, _ray)
    }
    fn hit_test_face(
        &self,
        _world: &World,
        _entity: Entity,
        _ray: Ray3d,
    ) -> Option<FaceHitCandidate> {
        self.0.hit_test_face(_world, _entity, _ray)
    }
    fn boundary_binding(
        &self,
        _world: &World,
        _reference: &SelectableSubobjectRef,
    ) -> Option<crate::plugins::modeling::boundary_edit::BoundaryBinding> {
        self.0.boundary_binding(_world, _reference)
    }
    fn collect_snap_points(&self, _world: &World, _out: &mut Vec<SnapPoint>) {
        self.0.collect_snap_points(_world, _out)
    }
    fn collect_inference_geometry(
        &self,
        _world: &World,
        _engine: &mut crate::plugins::inference::InferenceEngine,
    ) {
        self.0.collect_inference_geometry(_world, _engine)
    }
    fn contribute_model_summary(&self, _world: &World, _summary: &mut ModelSummaryAccumulator) {
        self.0.contribute_model_summary(_world, _summary)
    }
    fn collect_delete_dependencies(
        &self,
        _world: &World,
        _requested_ids: &[ElementId],
        _out: &mut Vec<ElementId>,
    ) {
        self.0
            .collect_delete_dependencies(_world, _requested_ids, _out)
    }
    fn dependency_edges(&self, _world: &World, _entity: Entity) -> EntityDependencies {
        self.0.dependency_edges(_world, _entity)
    }
}
