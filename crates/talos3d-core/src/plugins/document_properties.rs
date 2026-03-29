use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::units::DisplayUnit;

#[derive(Resource, Clone, Debug, Serialize, Deserialize)]
pub struct DocumentProperties {
    pub name: String,
    pub description: String,
    pub display_unit: DisplayUnit,
    pub precision: u8,
    pub snap_increment: f32,
    pub grid_minor_spacing: f32,
    pub grid_major_spacing: f32,
    pub domain_defaults: HashMap<String, serde_json::Value>,
}

impl Default for DocumentProperties {
    fn default() -> Self {
        Self {
            name: "Untitled".to_string(),
            description: String::new(),
            display_unit: DisplayUnit::Millimetres,
            precision: 0,
            snap_increment: 0.1,
            grid_minor_spacing: 1.0,
            grid_major_spacing: 5.0,
            domain_defaults: HashMap::new(),
        }
    }
}

impl DocumentProperties {
    pub fn format_coordinate(&self, label: &str, metres: f32) -> String {
        format!(
            "{}: {}",
            label,
            self.display_unit.format_value(metres, self.precision)
        )
    }
}
