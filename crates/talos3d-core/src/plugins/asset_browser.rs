//! Shared scaffolding for floating egui asset-browser windows.
//!
//! The definition browser, material browser, and similar panels (a lights
//! browser is planned) all follow the same shape: a floating tool window, a
//! search box with lowercase substring filtering, a scrollable entry list
//! with painted thumbnails, and a selected entry id.  This module owns those
//! generic building blocks; each browser keeps its domain-specific row layout
//! and editor panes.

use bevy_egui::egui;

use crate::plugins::ui::{tool_window_bounds, tool_window_max_size, tool_window_rect};

// ---------------------------------------------------------------------------
// Browsable assets and search filtering
// ---------------------------------------------------------------------------

/// An asset that can be listed, searched, and selected in a browser panel.
pub trait BrowsableAsset {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    /// One-line summary rendered next to or under the display name.
    fn meta_label(&self) -> String;
    /// Whether this asset matches a search needle produced by
    /// [`search_needle`] (already trimmed and lowercased).
    ///
    /// The default matches a substring of the display name or the id.
    /// Override when ids are internal and should not be searchable, or when
    /// more fields participate in the match.
    fn matches_search(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.display_name().to_lowercase().contains(needle)
            || self.id().to_lowercase().contains(needle)
    }
}

/// Normalizes raw search-box text into the needle passed to
/// [`BrowsableAsset::matches_search`].
pub fn search_needle(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Drops entries that do not match the raw search-box text.
pub fn retain_matching<T: BrowsableAsset>(entries: &mut Vec<T>, raw_search: &str) {
    let needle = search_needle(raw_search);
    if needle.is_empty() {
        return;
    }
    entries.retain(|entry| entry.matches_search(&needle));
}

/// Ensures `selected` refers to an entry in `entries`, falling back to the
/// first entry (or `None` when the list is empty).
///
/// Returns `true` when the selection was (re)assigned so the caller can reset
/// any selection-dependent state.
pub fn ensure_selected_id<T: BrowsableAsset>(selected: &mut Option<String>, entries: &[T]) -> bool {
    if selected
        .as_deref()
        .is_some_and(|id| entries.iter().any(|entry| entry.id() == id))
    {
        return false;
    }
    *selected = entries.first().map(|entry| entry.id().to_string());
    true
}

// ---------------------------------------------------------------------------
// Window scaffold
// ---------------------------------------------------------------------------

/// Static layout configuration for a floating asset-browser window.
///
/// Wraps the shared `tool_window_*` geometry helpers so every browser window
/// gets the same default-rect clamping, min/max sizing, viewport constraint,
/// and close-button handling.
pub struct AssetBrowserWindow {
    pub title: &'static str,
    pub id_salt: &'static str,
    pub default_pos: egui::Pos2,
    pub default_size: egui::Vec2,
    pub min_size: egui::Vec2,
    pub max_size: egui::Vec2,
    /// Constrain dragging to [`tool_window_bounds`] (viewport shrunk by the
    /// standard margin) instead of the full content rect.
    pub constrain_with_margin: bool,
}

impl AssetBrowserWindow {
    /// Shows the window when `visible` and returns the new visibility (the
    /// title-bar close button can clear it).
    pub fn show(
        &self,
        ctx: &egui::Context,
        visible: bool,
        add_contents: impl FnOnce(&mut egui::Ui),
    ) -> bool {
        if !visible {
            return false;
        }
        let default_rect = tool_window_rect(ctx, self.default_pos, self.default_size);
        let constrain_rect = if self.constrain_with_margin {
            tool_window_bounds(ctx)
        } else {
            ctx.content_rect()
        };
        let mut open = visible;
        egui::Window::new(self.title)
            .id(egui::Id::new(self.id_salt))
            .default_rect(default_rect)
            .min_size(self.min_size)
            .max_size(tool_window_max_size(ctx, self.max_size))
            .constrain_to(constrain_rect)
            .open(&mut open)
            .show(ctx, |ui| add_contents(ui));
        open
    }
}

// ---------------------------------------------------------------------------
// Search bar and entry list
// ---------------------------------------------------------------------------

/// Draws the standard browser search box, filling the available width.
///
/// With `label`, the box is laid out on one row after the label text;
/// without, it stretches to the full panel width on its own.
pub fn draw_search_bar(
    ui: &mut egui::Ui,
    search: &mut String,
    label: Option<&str>,
    hint: &str,
) -> egui::Response {
    match label {
        Some(text) => {
            ui.horizontal(|ui| {
                ui.label(text);
                ui.add(
                    egui::TextEdit::singleline(search)
                        .desired_width(ui.available_width())
                        .hint_text(hint),
                )
            })
            .inner
        }
        None => ui.add(
            egui::TextEdit::singleline(search)
                .desired_width(f32::INFINITY)
                .hint_text(hint),
        ),
    }
}

/// Scroll-area scaffold for a browser entry list.
pub struct AssetListView<'a> {
    pub id_salt: &'a str,
    pub max_height: Option<f32>,
    pub auto_shrink: [bool; 2],
}

impl AssetListView<'_> {
    pub fn show(&self, ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
        let mut area = egui::ScrollArea::vertical()
            .id_salt(self.id_salt)
            .auto_shrink(self.auto_shrink);
        if let Some(max_height) = self.max_height {
            area = area.max_height(max_height);
        }
        area.show(ui, body);
    }
}

/// Weak "nothing to show" label for an empty entry list: `no_match` when a
/// search filter is active, `no_entries` otherwise.
pub fn draw_empty_list_message(ui: &mut egui::Ui, search: &str, no_entries: &str, no_match: &str) {
    let message = if search.trim().is_empty() {
        no_entries
    } else {
        no_match
    };
    ui.label(egui::RichText::new(message).weak());
}

// ---------------------------------------------------------------------------
// Thumbnail painting
// ---------------------------------------------------------------------------

/// Allocates `size` and paints the rounded thumbnail backdrop (fill plus a
/// 1px inside stroke).  Returns the rect for the caller to paint glyph
/// content into.
pub fn thumbnail_frame(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    corner_radius: f32,
    fill: egui::Color32,
    stroke_color: egui::Color32,
) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, corner_radius, fill);
    ui.painter().rect_stroke(
        rect,
        corner_radius,
        egui::Stroke::new(1.0, stroke_color),
        egui::StrokeKind::Inside,
    );
    rect
}

/// Thumbnail backdrop with an optional small centered text label.
pub fn draw_placeholder_thumbnail(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    corner_radius: f32,
    fill: egui::Color32,
    stroke_color: egui::Color32,
    label: &str,
) {
    let rect = thumbnail_frame(ui, size, corner_radius, fill, stroke_color);
    if !label.is_empty() {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::TextStyle::Small.resolve(ui.style()),
            egui::Color32::from_gray(220),
        );
    }
}

/// Small dark circle badge with centered white text in the bottom-right
/// corner of a thumbnail rect (e.g. a slot or parameter count).
pub fn draw_count_badge(ui: &mut egui::Ui, thumb_rect: egui::Rect, text: &str) {
    let badge_rect = egui::Rect::from_min_size(
        thumb_rect.right_bottom() - egui::vec2(16.0, 16.0),
        egui::vec2(14.0, 14.0),
    );
    ui.painter().circle_filled(
        badge_rect.center(),
        7.0,
        egui::Color32::from_black_alpha(70),
    );
    ui.painter().text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::TextStyle::Small.resolve(ui.style()),
        egui::Color32::WHITE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestAsset {
        id: &'static str,
        name: &'static str,
    }

    impl BrowsableAsset for TestAsset {
        fn id(&self) -> &str {
            self.id
        }

        fn display_name(&self) -> &str {
            self.name
        }

        fn meta_label(&self) -> String {
            String::new()
        }
    }

    fn assets() -> Vec<TestAsset> {
        vec![
            TestAsset {
                id: "def.window",
                name: "Casement Window",
            },
            TestAsset {
                id: "def.door",
                name: "Entry Door",
            },
        ]
    }

    #[test]
    fn search_needle_trims_and_lowercases() {
        assert_eq!(search_needle("  Casement "), "casement");
        assert_eq!(search_needle(""), "");
    }

    #[test]
    fn default_match_covers_name_and_id() {
        let assets = assets();
        assert!(assets[0].matches_search("casement"));
        assert!(assets[0].matches_search("def.window"));
        assert!(!assets[0].matches_search("door"));
        assert!(assets[0].matches_search(""));
    }

    #[test]
    fn retain_matching_filters_and_keeps_all_on_empty_search() {
        let mut entries = assets();
        retain_matching(&mut entries, "   ");
        assert_eq!(entries.len(), 2);
        retain_matching(&mut entries, " Door ");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "def.door");
    }

    #[test]
    fn ensure_selected_id_keeps_valid_selection() {
        let entries = assets();
        let mut selected = Some("def.door".to_string());
        assert!(!ensure_selected_id(&mut selected, &entries));
        assert_eq!(selected.as_deref(), Some("def.door"));
    }

    #[test]
    fn ensure_selected_id_falls_back_to_first() {
        let entries = assets();
        let mut selected = Some("def.gone".to_string());
        assert!(ensure_selected_id(&mut selected, &entries));
        assert_eq!(selected.as_deref(), Some("def.window"));

        let mut none_selected: Option<String> = None;
        assert!(ensure_selected_id(&mut none_selected, &entries));
        assert_eq!(none_selected.as_deref(), Some("def.window"));
    }

    #[test]
    fn ensure_selected_id_clears_on_empty_list() {
        let entries: Vec<TestAsset> = Vec::new();
        let mut selected = Some("def.window".to_string());
        assert!(ensure_selected_id(&mut selected, &entries));
        assert_eq!(selected, None);
    }
}
