use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Architectural wall definition.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wall {
    /// Start point in world space (XZ plane).
    pub start: Vec2,
    /// End point in world space (XZ plane).
    pub end: Vec2,
    /// Wall height in metres.
    pub height: f32,
    /// Wall thickness in metres.
    pub thickness: f32,
}

impl Wall {
    pub fn length(&self) -> f32 {
        self.start.distance(self.end)
    }

    pub fn direction(&self) -> Option<Vec2> {
        (self.end - self.start).try_normalize()
    }
}

/// Optional BIM metadata attached to any architectural element.
#[derive(Component, Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BimData {
    pub id: Option<String>,
    pub material_code: Option<String>,
    pub fire_rating: Option<String>,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Opening {
    pub width: f32,
    pub height: f32,
    pub sill_height: f32,
    pub kind: OpeningKind,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpeningKind {
    Window,
    Door,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParentWall {
    #[serde(skip, default = "placeholder_entity")]
    pub wall_entity: Entity,
    pub position_along_wall: f32,
}

impl Default for ParentWall {
    fn default() -> Self {
        Self {
            wall_entity: placeholder_entity(),
            position_along_wall: 0.0,
        }
    }
}

fn placeholder_entity() -> Entity {
    Entity::PLACEHOLDER
}
