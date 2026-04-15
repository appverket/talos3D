//! Visibility control for drafting dimensions.
//!
//! Independent of per-annotation `.visible`. Filters at the system level so
//! that show/hide commands toggle sets of dimensions en masse by kind or style.

use std::collections::HashSet;

use bevy::ecs::resource::Resource;

use super::kind::DimensionKindTag;

#[derive(Debug, Clone, Resource)]
pub struct DraftingVisibility {
    pub show_all: bool,
    pub hidden_styles: HashSet<String>,
    pub hidden_kinds: HashSet<DimensionKindTag>,
}

impl Default for DraftingVisibility {
    fn default() -> Self {
        Self {
            show_all: true,
            hidden_styles: HashSet::new(),
            hidden_kinds: HashSet::new(),
        }
    }
}

impl DraftingVisibility {
    /// Whether a given annotation is currently visible.
    #[must_use]
    pub fn is_visible(&self, style_name: &str, kind: DimensionKindTag) -> bool {
        if !self.show_all {
            return false;
        }
        if self.hidden_styles.contains(style_name) {
            return false;
        }
        if self.hidden_kinds.contains(&kind) {
            return false;
        }
        true
    }

    pub fn toggle_all(&mut self) {
        self.show_all = !self.show_all;
    }

    pub fn toggle_style(&mut self, style_name: &str) {
        if self.hidden_styles.contains(style_name) {
            self.hidden_styles.remove(style_name);
        } else {
            self.hidden_styles.insert(style_name.to_string());
        }
    }

    pub fn toggle_kind(&mut self, kind: DimensionKindTag) {
        if self.hidden_kinds.contains(&kind) {
            self.hidden_kinds.remove(&kind);
        } else {
            self.hidden_kinds.insert(kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_by_default() {
        let v = DraftingVisibility::default();
        assert!(v.is_visible("architectural_imperial", DimensionKindTag::Linear));
    }

    #[test]
    fn show_all_false_hides_everything() {
        let mut v = DraftingVisibility::default();
        v.show_all = false;
        assert!(!v.is_visible("architectural_imperial", DimensionKindTag::Linear));
    }

    #[test]
    fn hidden_style_is_hidden() {
        let mut v = DraftingVisibility::default();
        v.toggle_style("architectural_imperial");
        assert!(!v.is_visible("architectural_imperial", DimensionKindTag::Linear));
        assert!(v.is_visible("engineering_mm", DimensionKindTag::Linear));
    }

    #[test]
    fn hidden_kind_is_hidden() {
        let mut v = DraftingVisibility::default();
        v.toggle_kind(DimensionKindTag::Angular);
        assert!(!v.is_visible("architectural_imperial", DimensionKindTag::Angular));
        assert!(v.is_visible("architectural_imperial", DimensionKindTag::Linear));
    }
}
