pub mod box_tool;
pub mod cylinder_tool;
pub mod plane_tool;
pub mod polyline_tool;
pub mod sphere_tool;

use bevy::prelude::*;

pub struct ModelingToolPlugin;

impl Plugin for ModelingToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(box_tool::BoxToolPlugin)
            .add_plugins(cylinder_tool::CylinderToolPlugin)
            .add_plugins(sphere_tool::SphereToolPlugin)
            .add_plugins(plane_tool::PlaneToolPlugin)
            .add_plugins(polyline_tool::PolylineToolPlugin);
    }
}
