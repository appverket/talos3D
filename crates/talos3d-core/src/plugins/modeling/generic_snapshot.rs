use std::any::Any;

use bevy::prelude::*;
use serde_json::Value;

use crate::{
    authored_entity::{
        property_field, AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef,
        PropertyValue, PropertyValueKind, PushPullAffordance,
    },
    capability_registry::FaceId,
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
        }
        .into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        self.primitive.handles(self.rotation.0)
    }

    fn bounds(&self) -> Option<EntityBounds> {
        self.primitive.bounds(self.rotation.0)
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
        self.primitive
            .draw_wireframe(gizmos, self.rotation.0, color);
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
