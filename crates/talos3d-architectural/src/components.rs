use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use talos3d_core::plugins::{
    identity::ElementId,
    modeling::host_chart::{ChartSpaceProfileLoop, OpeningClearancePolicy, OpeningDepthPolicy},
};

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

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildingPad {
    pub boundary: Vec<Vec2>,
    pub pad_elevation: f32,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct BuildingPadExcavation {
    pub volume: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpeningKind {
    Window,
    Door,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpeningFeature {
    pub host_ref: ElementId,
    pub chart_anchor: String,
    pub profile_loop_2d: ChartSpaceProfileLoop,
    #[serde(default)]
    pub depth_policy: OpeningDepthPolicy,
    #[serde(default)]
    pub clearance_policy: OpeningClearancePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_fill_ref: Option<ElementId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural_role: Option<String>,
    #[serde(default)]
    pub operation: OpeningFeatureOperation,
}

impl OpeningFeature {
    pub const WALL_EXTERIOR_CHART_ANCHOR: &'static str = "wall_exterior";

    pub fn rectangular_wall(
        host_ref: ElementId,
        wall: &Wall,
        opening: &Opening,
        position_along_wall: f32,
    ) -> Option<Self> {
        let wall_length = wall.length();
        if wall_length <= f32::EPSILON || opening.width <= 0.0 || opening.height <= 0.0 {
            return None;
        }

        let center_x = position_along_wall.clamp(0.0, 1.0) * wall_length;
        let half_width = opening.width * 0.5;
        let x_min = (center_x - half_width).clamp(0.0, wall_length);
        let x_max = (center_x + half_width).clamp(0.0, wall_length);
        let y_min = opening.sill_height.clamp(0.0, wall.height);
        let y_max = (opening.sill_height + opening.height).clamp(0.0, wall.height);

        (x_max - x_min > f32::EPSILON && y_max - y_min > f32::EPSILON).then_some(Self {
            host_ref,
            chart_anchor: Self::WALL_EXTERIOR_CHART_ANCHOR.to_string(),
            profile_loop_2d: ChartSpaceProfileLoop::rectangle(
                Vec2::new(x_min, y_min),
                Vec2::new(x_max, y_max),
            ),
            depth_policy: OpeningDepthPolicy::ThroughHost,
            clearance_policy: OpeningClearancePolicy::None,
            hosted_fill_ref: None,
            structural_role: Some(match opening.kind {
                OpeningKind::Window => "window".to_string(),
                OpeningKind::Door => "door".to_string(),
            }),
            operation: OpeningFeatureOperation::Cut,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OpeningFeatureOperation {
    #[default]
    Cut,
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
