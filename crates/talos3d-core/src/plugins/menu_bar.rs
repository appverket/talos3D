use bevy::prelude::*;

#[derive(Resource, Debug, Clone)]
pub struct MenuBarState {
    pub visible: bool,
}

impl Default for MenuBarState {
    fn default() -> Self {
        Self { visible: true }
    }
}
