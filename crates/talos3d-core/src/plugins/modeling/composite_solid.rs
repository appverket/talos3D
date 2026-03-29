use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{capability_registry::FaceId, plugins::identity::ElementId};

/// Marks a group as a composite solid — a single logical shape composed of
/// multiple parametric primitives that share internal faces.
///
/// Shared faces are not rendered and are excluded from surface area calculations.
/// Volume is the sum of member volumes (shared faces are coplanar, no overlap).
///
/// This preserves the parametric identity of each member while giving the AI
/// and user a semantic understanding of the composite shape.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositeSolid {
    /// Internal faces shared between adjacent primitives.
    pub shared_faces: Vec<SharedFace>,
}

/// A pair of coplanar faces on two different members of a CompositeSolid group.
/// These faces are internal and should not be rendered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedFace {
    /// Element ID of the first member primitive.
    pub entity_a: ElementId,
    /// Face index on entity_a.
    pub face_a: FaceId,
    /// Element ID of the second member primitive.
    pub entity_b: ElementId,
    /// Face index on entity_b.
    pub face_b: FaceId,
}

impl CompositeSolid {
    /// Check if a specific face on an entity is shared (internal).
    pub fn is_shared(&self, element_id: ElementId, face_id: FaceId) -> bool {
        self.shared_faces.iter().any(|sf| {
            (sf.entity_a == element_id && sf.face_a == face_id)
                || (sf.entity_b == element_id && sf.face_b == face_id)
        })
    }
}
