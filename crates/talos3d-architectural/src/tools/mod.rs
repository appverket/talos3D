pub mod opening_tool;
pub mod wall_tool;

use bevy::prelude::*;

pub struct ArchitecturalToolPlugin;

impl Plugin for ArchitecturalToolPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(wall_tool::WallToolPlugin)
            .add_plugins(opening_tool::OpeningToolPlugin);
    }
}
