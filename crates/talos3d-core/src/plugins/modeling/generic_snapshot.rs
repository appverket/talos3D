use std::any::Any;

use bevy::prelude::*;
use serde_json::Value;

use crate::{
    authored_entity::{
        AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef, PushPullAffordance,
    },
    capability_registry::FaceId,
    plugins::{
        commands::{apply_mesh_primitive, despawn_by_element_id, find_entity_by_element_id},
        identity::ElementId,
        modeling::primitives::ShapeRotation,
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
}

impl<P: Primitive> PartialEq for PrimitiveSnapshot<P>
where
    P: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.element_id == other.element_id
            && self.primitive == other.primitive
            && self.rotation == other.rotation
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
        }
        .into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let (new_primitive, new_rotation) = self.primitive.rotated(rotation, self.rotation.0);
        PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: new_primitive,
            rotation: ShapeRotation(new_rotation),
        }
        .into()
    }

    fn scale_by(&self, factor: Vec3, center: Vec3) -> BoxedEntity {
        PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: self.primitive.scaled(factor, center),
            rotation: self.rotation,
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
        self.primitive.property_fields(self.rotation.0)
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let new_prim = self.primitive.set_property(property_name, value)?;
        Ok(PrimitiveSnapshot {
            element_id: self.element_id,
            primitive: new_prim,
            rotation: self.rotation,
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
            }
            .into(),
        )
    }

    fn to_json(&self) -> Value {
        self.primitive.to_json()
    }

    fn apply_to(&self, world: &mut World) {
        apply_mesh_primitive(
            world,
            self.element_id,
            self.primitive.clone(),
            self.rotation,
        );
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
            world.entity_mut(entity).insert((
                self.primitive.clone(),
                self.rotation,
                self.primitive.entity_transform(self.rotation.0),
            ));
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
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

impl<P: Primitive + PartialEq> From<PrimitiveSnapshot<P>> for BoxedEntity {
    fn from(snapshot: PrimitiveSnapshot<P>) -> Self {
        Self(Box::new(snapshot))
    }
}
