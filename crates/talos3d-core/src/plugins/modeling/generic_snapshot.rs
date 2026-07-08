use std::any::Any;

use bevy::prelude::*;
use serde_json::Value;

use crate::{
    authored_entity::{
        property_field, AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef,
        PropertyValue, PropertyValueKind, PushPullAffordance,
    },
    capability_registry::{FaceId, GeneratedEdgeRef, SubobjectDisplayOverrides},
    plugins::{
        commands::{apply_mesh_primitive, despawn_by_element_id, find_entity_by_element_id},
        identity::ElementId,
        materials::{
            material_assignment_display_id, material_assignment_option_from_value,
            material_assignment_to_value, MaterialAssignment,
        },
        modeling::{primitives::ShapeRotation, void_declaration::OpeningContext},
    },
};

use super::primitive_trait::{Primitive, PrimitivePushPullResult};

/// Generic snapshot that wraps any `Primitive` type, implementing `AuthoredEntity`
/// via blanket delegation to the primitive's trait methods.
#[derive(Debug, Clone)]
pub struct PrimitiveSnapshot<P: Primitive> {
    pub element_id: ElementId,
    pub primitive: P,
    pub rotation: ShapeRotation,
    pub material_assignment: Option<MaterialAssignment>,
    pub opening_context: Option<OpeningContext>,
    pub subobject_display_overrides: Option<SubobjectDisplayOverrides>,
}

impl<P: Primitive> PartialEq for PrimitiveSnapshot<P>
where
    P: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id
            && self.primitive == other.primitive
            && self.rotation == other.rotation
            && self.material_assignment == other.material_assignment
            && self.opening_context == other.opening_context
            && self.subobject_display_overrides == other.subobject_display_overrides
    }
}

pub(crate) fn draw_primitive_wireframe_with_overrides<P: Primitive>(
    primitive: &P,
    rotation: ShapeRotation,
    overrides: Option<&SubobjectDisplayOverrides>,
    gizmos: &mut Gizmos,
    color: Color,
) {
    let Some(overrides) = overrides.filter(|overrides| !overrides.is_empty()) else {
        primitive.draw_wireframe(gizmos, rotation.0, color);
        return;
    };
    let Some(mesh) = primitive.to_editable_mesh(rotation.0) else {
        primitive.draw_wireframe(gizmos, rotation.0, color);
        return;
    };

    let mut seen_edges = std::collections::HashSet::new();
    for half_edge_index in 0..mesh.half_edges.len() as u32 {
        let canonical = canonical_half_edge_index(&mesh, half_edge_index);
        if !seen_edges.insert(canonical) {
            continue;
        }
        let edge_ref = generated_edge_ref_for_half_edge(primitive, &mesh, canonical);
        if overrides.is_edge_hidden(&edge_ref) {
            continue;
        }

        let half_edge = &mesh.half_edges[canonical as usize];
        let dest = mesh.half_edges[half_edge.next as usize].origin;
        gizmos.line(
            mesh.vertices[half_edge.origin as usize],
            mesh.vertices[dest as usize],
            color,
        );
    }
}

fn canonical_half_edge_index(
    mesh: &super::editable_mesh::EditableMesh,
    half_edge_index: u32,
) -> u32 {
    let half_edge = &mesh.half_edges[half_edge_index as usize];
    if half_edge.twin == u32::MAX {
        half_edge_index
    } else {
        half_edge_index.min(half_edge.twin)
    }
}

fn generated_edge_ref_for_half_edge<P: Primitive>(
    primitive: &P,
    mesh: &super::editable_mesh::EditableMesh,
    half_edge_index: u32,
) -> GeneratedEdgeRef {
    let canonical = canonical_half_edge_index(mesh, half_edge_index);
    let (face_a, face_b) = mesh.faces_adjacent_to_edge(canonical);
    let first = primitive.generated_face_ref(FaceId(face_a));
    let second = face_b.and_then(|face_id| primitive.generated_face_ref(FaceId(face_id)));
    match (first, second) {
        (Some(a), Some(b)) => {
            let (first, second) = if a.label() <= b.label() {
                (a, b)
            } else {
                (b, a)
            };
            GeneratedEdgeRef::BetweenFaces {
                first,
                second,
                edge_index: canonical,
            }
        }
        (Some(face), None) | (None, Some(face)) => GeneratedEdgeRef::BoundaryOfFace {
            face,
            edge_index: canonical,
        },
        (None, None) => GeneratedEdgeRef::EditableMeshEdge(canonical),
    }
}

impl<P: Primitive> AuthoredEntity for PrimitiveSnapshot<P>
where
    P: PartialEq,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        P::TYPE_NAME
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        self.primitive.label()
    }

    fn center(&self) -> Vec3 {
        self.primitive.centre()
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: self.primitive.translated(delta),
            rotation: self.rotation,
            material_assignment: self.material_assignment.clone(),
            opening_context: self.opening_context,
            subobject_display_overrides: self.subobject_display_overrides.clone(),
        }
        .into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let (new_primitive, new_rotation) = self.primitive.rotated(rotation, self.rotation.0);
        PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: new_primitive,
            rotation: ShapeRotation(new_rotation),
            material_assignment: self.material_assignment.clone(),
            opening_context: self.opening_context,
            subobject_display_overrides: self.subobject_display_overrides.clone(),
        }
        .into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: self.primitive.scaled(factor, center),
            rotation: self.rotation,
            material_assignment: self.material_assignment.clone(),
            opening_context: self.opening_context,
            subobject_display_overrides: self.subobject_display_overrides.clone(),
        }
        .into()
    }

    fn push_pull(&self, face_id: FaceId, distance: f32) -> Option<BoxedEntity> {
        match self
            .primitive
            .push_pull(face_id, distance, self.rotation.0, self.element_id)?
        {
            PrimitivePushPullResult::SameType(new_prim, new_rot) => Some(
                PrimitiveSnapshot {
                    element_id: self.element_id,
                    primitive: new_prim,
                    rotation: ShapeRotation(new_rot),
                    material_assignment: self.material_assignment.clone(),
                    opening_context: self.opening_context,
                    subobject_display_overrides: self.subobject_display_overrides.clone(),
                }
                .into(),
            ),
            PrimitivePushPullResult::Promoted(boxed) => Some(boxed),
        }
    }

    fn push_pull_affordance(&self, face_id: FaceId) -> PushPullAffordance {
        self.primitive.push_pull_affordance(face_id)
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        let mut fields = self.primitive.property_fields(self.rotation.0);
        fields.push(property_field(
            "material",
            PropertyValueKind::Text,
            material_assignment_display_id(self.material_assignment.as_ref())
                .map(PropertyValue::Text),
        ));
        fields
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        if matches!(property_name, "material" | "material_assignment") {
            return self.set_material_assignment(material_assignment_option_from_value(value)?);
        }
        let new_prim = self.primitive.set_property(property_name, value)?;
        Ok(PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: new_prim,
            rotation: self.rotation,
            material_assignment: self.material_assignment.clone(),
            opening_context: self.opening_context,
            subobject_display_overrides: self.subobject_display_overrides.clone(),
        }
        .into())
    }

    fn material_assignment(&self) -> Option<MaterialAssignment> {
        self.material_assignment.clone()
    }

    fn set_material_assignment(
        &self,
        assignment: Option<MaterialAssignment>,
    ) -> Result<BoxedEntity, String> {
        Ok(PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: self.primitive.clone(),
            rotation: self.rotation,
            material_assignment: assignment,
            opening_context: self.opening_context,
            subobject_display_overrides: self.subobject_display_overrides.clone(),
        }
        .into())
    }

    fn subobject_display_overrides(&self) -> Option<SubobjectDisplayOverrides> {
        self.subobject_display_overrides.clone()
    }

    fn set_subobject_display_overrides(
        &self,
        overrides: Option<SubobjectDisplayOverrides>,
    ) -> Result<BoxedEntity, String> {
        Ok(PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: self.primitive.clone(),
            rotation: self.rotation,
            material_assignment: self.material_assignment.clone(),
            opening_context: self.opening_context,
            subobject_display_overrides: overrides.filter(|overrides| !overrides.is_empty()),
        }
        .into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        self.primitive.handles(self.rotation.0)
    }

    fn bounds(&self) -> Option<EntityBounds> {
        self.primitive.bounds(self.rotation.0)
    }

    fn snap_segments(&self) -> Vec<(Vec3, Vec3)> {
        self.primitive.wireframe_segments(self.rotation.0)
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        let new_prim = self
            .primitive
            .drag_handle(handle_id, cursor, self.rotation.0)?;
        Some(
            PrimitiveSnapshot {
                element_id: self.element_id,
                primitive: new_prim,
                rotation: self.rotation,
                material_assignment: self.material_assignment.clone(),
                opening_context: self.opening_context,
                subobject_display_overrides: self.subobject_display_overrides.clone(),
            }
            .into(),
        )
    }

    fn to_json(&self) -> Value {
        self.primitive.to_json()
    }

    fn to_persisted_json(&self) -> Value {
        let mut json = self.primitive.to_json();
        if let Some(object) = json.as_object_mut() {
            object.insert(
                "element_id".to_string(),
                serde_json::to_value(self.element_id).unwrap_or(Value::Null),
            );
            object.insert(
                "rotation".to_string(),
                serde_json::to_value(self.rotation).unwrap_or(Value::Null),
            );
            if let Some(material_assignment) = &self.material_assignment {
                object.insert(
                    "material_assignment".to_string(),
                    material_assignment_to_value(material_assignment),
                );
            }
            if let Some(opening_context) = &self.opening_context {
                object.insert(
                    "opening_context".to_string(),
                    serde_json::to_value(opening_context).unwrap_or(Value::Null),
                );
            }
            if let Some(overrides) = &self.subobject_display_overrides {
                if !overrides.is_empty() {
                    object.insert(
                        "subobject_display_overrides".to_string(),
                        serde_json::to_value(overrides).unwrap_or(Value::Null),
                    );
                }
            }
        }
        json
    }

    fn apply_to(&self, world: &mut World) {
        apply_mesh_primitive(
            world,
            self.element_id,
            self.primitive.clone(),
            self.rotation,
        );
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let mut entity_mut = world.entity_mut(entity);
            if let Some(material_assignment) = &self.material_assignment {
                entity_mut.insert(material_assignment.clone());
            } else {
                entity_mut.remove::<MaterialAssignment>();
            }
            if let Some(opening_context) = self.opening_context {
                entity_mut.insert((opening_context, Visibility::Hidden));
            } else {
                entity_mut.remove::<OpeningContext>();
            }
            if let Some(overrides) = self
                .subobject_display_overrides
                .as_ref()
                .filter(|overrides| !overrides.is_empty())
            {
                entity_mut.insert(overrides.clone());
            } else {
                entity_mut.remove::<SubobjectDisplayOverrides>();
            }
        }
    }

    fn apply_with_previous(&self, world: &mut World, previous: Option<&dyn AuthoredEntity>) {
        let Some(previous) = previous.and_then(|snapshot| snapshot.as_any().downcast_ref::<Self>())
        else {
            self.apply_to(world);
            return;
        };

        if !self.primitive.shape_eq(&previous.primitive) {
            self.apply_to(world);
            return;
        }

        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let mut entity_mut = world.entity_mut(entity);
            entity_mut.insert((
                self.primitive.clone(),
                self.rotation,
                self.primitive.entity_transform(self.rotation.0),
            ));
            if let Some(material_assignment) = &self.material_assignment {
                entity_mut.insert(material_assignment.clone());
            } else {
                entity_mut.remove::<MaterialAssignment>();
            }
            if let Some(opening_context) = self.opening_context {
                entity_mut.insert((opening_context, Visibility::Hidden));
            } else {
                entity_mut.remove::<OpeningContext>();
            }
            if let Some(overrides) = self
                .subobject_display_overrides
                .as_ref()
                .filter(|overrides| !overrides.is_empty())
            {
                entity_mut.insert(overrides.clone());
            } else {
                entity_mut.remove::<SubobjectDisplayOverrides>();
            }
        } else {
            self.apply_to(world);
        }
    }

    fn remove_from(&self, world: &mut World) {
        despawn_by_element_id(world, self.element_id);
    }

    fn preview_transform(&self) -> Option<Transform> {
        Some(self.primitive.entity_transform(self.rotation.0))
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        draw_primitive_wireframe_with_overrides(
            &self.primitive,
            self.rotation,
            self.subobject_display_overrides.as_ref(),
            gizmos,
            color,
        );
    }

    fn preview_line_count(&self) -> usize {
        self.primitive.wireframe_line_count()
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| other == self)
    }
}

impl<P: Primitive + PartialEq> From<PrimitiveSnapshot<P>> for BoxedEntity {
    fn from(snapshot: PrimitiveSnapshot<P>) -> Self {
        Self(Box::new(snapshot))
    }
}
