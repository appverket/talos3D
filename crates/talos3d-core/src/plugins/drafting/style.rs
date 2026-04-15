//! [`DimensionStyle`] — the visual contract for a dimension.
//!
//! A dimension's geometry (endpoints, offset) is universal. Everything that
//! distinguishes an arch plan from a mech part drawing — terminator kind, text
//! placement, line weights, fonts, number format — is a `DimensionStyle` that
//! the dimension references by name. This gives document-wide restyle-in-place
//! for free: swap the preset, all dimensions update.
//!
//! All measurements on a style are in **paper millimetres**. They are the sizes
//! the drawing will print at. The renderer is responsible for scaling paper units
//! into the target viewport; styles don't care about the drawing scale.
//!
//! Four built-in presets cover the overwhelming majority of production drafting:
//!
//! - [`DimensionStyle::architectural_imperial`] — US/NCS, feet-inches, 45° ticks
//! - [`DimensionStyle::architectural_metric`] — ISO arch, mm, 45° ticks
//! - [`DimensionStyle::engineering_mm`] — ASME/ISO mechanical, metric, arrows
//! - [`DimensionStyle::engineering_inch`] — ASME mechanical, decimal inch, arrows

use std::collections::HashMap;

use bevy::ecs::resource::Resource;
use serde::{Deserialize, Serialize};

use crate::plugins::units::DisplayUnit;

use super::format::NumberFormat;

/// Visual style for a dimension. Units are paper millimetres unless noted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionStyle {
    /// Stable identifier used by [`DimensionStyleRegistry`] and persisted on
    /// every dimension annotation.
    pub name: String,

    /// Terminator at each end of the dimension line.
    pub terminator: Terminator,
    /// Paper-size length of the terminator (tick length, arrow length, dot
    /// diameter). Typically 2.5 mm for mech, 3.2 mm (1/8") for arch.
    pub terminator_size_mm: f32,

    /// Distance the dimension line extends past the intersection with each
    /// extension line. Gives visual breathing room for ticks. Usually zero for
    /// arrowheads, ~half the tick length for ticks.
    pub dim_line_extend_past_tick_mm: f32,

    /// Gap between the feature being dimensioned and the near end of the
    /// extension line. Typically 1.6 mm (1/16").
    pub extension_gap_mm: f32,

    /// Distance the extension line protrudes past the dimension line. Typically
    /// 3.2 mm (1/8") in arch, 1.25 mm in mech.
    pub extension_past_mm: f32,

    /// Stroke weight of the extension line, in paper mm. ISO 128: 0.18.
    pub extension_stroke_mm: f32,

    /// Stroke weight of the dimension line, in paper mm. ISO 128: 0.18.
    pub dim_line_stroke_mm: f32,

    /// Typical paper distance from the object to the first dimension line when
    /// the plugin is auto-placing. Not a hard constraint — per-dimension offset
    /// overrides this.
    pub first_offset_mm: f32,

    /// Spacing between stacked dimension strings.
    pub stack_spacing_mm: f32,

    /// Where the text sits relative to the dimension line.
    pub text_placement: TextPlacement,

    /// Text height in paper mm. 2.5 for mech, 3.2 (1/8") for arch.
    pub text_height_mm: f32,

    /// Font family name. Viewers substitute if unavailable.
    pub text_font: String,

    /// Text colour as a 6-digit hex string, no `#`.
    pub text_color_hex: String,

    /// How to format the measured length as text.
    pub number_format: NumberFormat,

    /// Optional prefix baked into the label (e.g. `"R"` for radial, `"Ø"` for
    /// diameter). Kind-specific defaults apply when this is `None`.
    pub prefix: Option<String>,

    /// Optional suffix, e.g. `" TYP."`.
    pub suffix: Option<String>,
}

/// Terminator kinds. Architectural drafting uses `ArchTick`; mechanical uses
/// `Arrow`. `Dot` is for space-constrained situations (dimensioning between
/// close extension lines). `None` omits the terminator — sometimes used on
/// leaders or ordinate dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Terminator {
    /// Short oblique line drawn at `angle_deg` through the extension-line
    /// intersection. 45° is the canonical architectural tick.
    ArchTick { angle_deg: f32 },

    /// Filled or open triangular arrow with `length : width = ratio : 1`.
    /// ASME Y14.5 closed-filled arrows use ratio 3, filled = true.
    Arrow { length_to_width_ratio: f32, filled: bool },

    /// Solid disc centred on the intersection. Used when extension lines are
    /// very close.
    Dot { radius_mm: f32 },

    /// No terminator. Rare — used on leaders or some ordinate dimensions.
    None,
}

impl Terminator {
    /// Canonical 45° architectural tick.
    #[must_use]
    pub fn arch_tick() -> Self {
        Self::ArchTick { angle_deg: 45.0 }
    }

    /// Canonical ASME Y14.5 filled arrow, 3:1 length-to-width.
    #[must_use]
    pub fn filled_arrow() -> Self {
        Self::Arrow {
            length_to_width_ratio: 3.0,
            filled: true,
        }
    }
}

/// Where dimension text sits relative to the dimension line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TextPlacement {
    /// Text sits `gap_mm` above the dimension line, rotated with it. The line
    /// is drawn continuous underneath. Architectural default.
    Above { gap_mm: f32 },

    /// Text sits centred on the dimension line. If `break_line` is true, the
    /// line is split with `gap_mm` of space on either side of the text (ASME
    /// default). If false, the text overprints the line.
    Centered { break_line: bool, gap_mm: f32 },

    /// Text is always horizontal regardless of the dimension line's angle,
    /// with a `gap_mm` above the line. ASME unidirectional style.
    Horizontal { gap_mm: f32 },
}

// ─── Presets ─────────────────────────────────────────────────────────────────

impl DimensionStyle {
    /// US architectural practice: feet-inches to nearest 1/2", 45° ticks, text
    /// above the continuous dim line, 1/8" text, Architects Daughter font.
    /// Numbers follow AIA / NCS conventions.
    #[must_use]
    pub fn architectural_imperial() -> Self {
        Self {
            name: "architectural_imperial".to_string(),
            terminator: Terminator::arch_tick(),
            terminator_size_mm: 3.2, // 1/8"
            dim_line_extend_past_tick_mm: 1.6, // 1/16" past the tick
            extension_gap_mm: 1.6, // 1/16"
            extension_past_mm: 3.2, // 1/8"
            extension_stroke_mm: 0.18,
            dim_line_stroke_mm: 0.18,
            first_offset_mm: 12.7, // 1/2"
            stack_spacing_mm: 9.5, // 3/8"
            text_placement: TextPlacement::Above { gap_mm: 1.0 },
            text_height_mm: 3.2, // 1/8"
            text_font: "Architects Daughter, 'CountryBlueprint', Arial, sans-serif".to_string(),
            text_color_hex: "000000".to_string(),
            number_format: NumberFormat::FeetInchesFractional { denominator: 2 },
            prefix: None,
            suffix: None,
        }
    }

    /// ISO architectural practice: mm integers, 45° ticks, text above.
    #[must_use]
    pub fn architectural_metric() -> Self {
        Self {
            name: "architectural_metric".to_string(),
            terminator: Terminator::arch_tick(),
            terminator_size_mm: 3.0,
            dim_line_extend_past_tick_mm: 1.5,
            extension_gap_mm: 1.5,
            extension_past_mm: 3.0,
            extension_stroke_mm: 0.18,
            dim_line_stroke_mm: 0.18,
            first_offset_mm: 12.0,
            stack_spacing_mm: 10.0,
            text_placement: TextPlacement::Above { gap_mm: 1.0 },
            text_height_mm: 2.5,
            text_font: "Architects Daughter, 'CountryBlueprint', Arial, sans-serif".to_string(),
            text_color_hex: "000000".to_string(),
            number_format: NumberFormat::MetricArchitectural {
                thousands_separator: false,
            },
            prefix: None,
            suffix: None,
        }
    }

    /// ASME/ISO mechanical metric: filled 3:1 arrows, text breaking the dim
    /// line, decimal mm with no decimal places.
    #[must_use]
    pub fn engineering_mm() -> Self {
        Self {
            name: "engineering_mm".to_string(),
            terminator: Terminator::filled_arrow(),
            terminator_size_mm: 2.5,
            dim_line_extend_past_tick_mm: 0.0,
            extension_gap_mm: 0.625,
            extension_past_mm: 1.25,
            extension_stroke_mm: 0.18,
            dim_line_stroke_mm: 0.18,
            first_offset_mm: 10.0,
            stack_spacing_mm: 8.0,
            text_placement: TextPlacement::Centered {
                break_line: true,
                gap_mm: 0.625,
            },
            text_height_mm: 2.5,
            text_font: "ISOCPEUR, 'ISOCP', Arial, sans-serif".to_string(),
            text_color_hex: "000000".to_string(),
            number_format: NumberFormat::Decimal {
                unit: DisplayUnit::Millimetres,
                precision: 0,
                omit_leading_zero: false,
                strip_trailing_zeros: false,
            },
            prefix: None,
            suffix: None,
        }
    }

    /// ASME mechanical inch: filled 3:1 arrows, text centred, decimal inch,
    /// leading zero omitted per ASME Y14.5.
    #[must_use]
    pub fn engineering_inch() -> Self {
        Self {
            name: "engineering_inch".to_string(),
            terminator: Terminator::filled_arrow(),
            terminator_size_mm: 2.5,
            dim_line_extend_past_tick_mm: 0.0,
            extension_gap_mm: 0.625,
            extension_past_mm: 1.25,
            extension_stroke_mm: 0.18,
            dim_line_stroke_mm: 0.18,
            first_offset_mm: 10.0,
            stack_spacing_mm: 8.0,
            text_placement: TextPlacement::Centered {
                break_line: true,
                gap_mm: 0.625,
            },
            text_height_mm: 2.5,
            text_font: "ISOCPEUR, 'ISOCP', Arial, sans-serif".to_string(),
            text_color_hex: "000000".to_string(),
            number_format: NumberFormat::Decimal {
                unit: DisplayUnit::Inches,
                precision: 3,
                omit_leading_zero: true,
                strip_trailing_zeros: false,
            },
            prefix: None,
            suffix: None,
        }
    }
}

// ─── Registry ────────────────────────────────────────────────────────────────

/// Document-scoped registry of named [`DimensionStyle`] instances. Dimensions
/// reference styles by name; global restyle-in-place is a registry mutation.
#[derive(Debug, Clone, Resource)]
pub struct DimensionStyleRegistry {
    styles: HashMap<String, DimensionStyle>,
    default_name: String,
}

impl Default for DimensionStyleRegistry {
    /// Seeds all four built-in presets and picks `architectural_metric` as the
    /// default — matching talos3D's default `DisplayUnit::Metres`.
    fn default() -> Self {
        let mut registry = Self {
            styles: HashMap::new(),
            default_name: String::new(),
        };
        for preset in [
            DimensionStyle::architectural_imperial(),
            DimensionStyle::architectural_metric(),
            DimensionStyle::engineering_mm(),
            DimensionStyle::engineering_inch(),
        ] {
            registry.insert(preset);
        }
        registry.default_name = "architectural_metric".to_string();
        registry
    }
}

impl DimensionStyleRegistry {
    /// Insert or replace a style. If the registry has no default name set,
    /// the first style inserted becomes the default.
    pub fn insert(&mut self, style: DimensionStyle) {
        let name = style.name.clone();
        if self.default_name.is_empty() {
            self.default_name = name.clone();
        }
        self.styles.insert(name, style);
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&DimensionStyle> {
        self.styles.get(name)
    }

    /// Returns the default style, or the first registered style if the default
    /// name is stale. Never panics; if the registry is empty, returns a
    /// freshly-built `architectural_metric` preset as a safety net.
    #[must_use]
    pub fn resolve<'a>(&'a self, name: Option<&str>) -> DimensionStyle {
        if let Some(n) = name {
            if let Some(s) = self.styles.get(n) {
                return s.clone();
            }
        }
        self.styles
            .get(&self.default_name)
            .cloned()
            .or_else(|| self.styles.values().next().cloned())
            .unwrap_or_else(DimensionStyle::architectural_metric)
    }

    pub fn set_default(&mut self, name: impl Into<String>) {
        self.default_name = name.into();
    }

    #[must_use]
    pub fn default_name(&self) -> &str {
        &self.default_name
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.styles.keys().map(String::as_str)
    }

    pub fn styles(&self) -> impl Iterator<Item = &DimensionStyle> {
        self.styles.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_four_presets_registered_by_default() {
        let reg = DimensionStyleRegistry::default();
        assert!(reg.get("architectural_imperial").is_some());
        assert!(reg.get("architectural_metric").is_some());
        assert!(reg.get("engineering_mm").is_some());
        assert!(reg.get("engineering_inch").is_some());
    }

    #[test]
    fn resolve_falls_back_to_default_on_unknown_name() {
        let reg = DimensionStyleRegistry::default();
        let resolved = reg.resolve(Some("nonexistent"));
        assert_eq!(resolved.name, "architectural_metric");
    }

    #[test]
    fn resolve_picks_named_style() {
        let reg = DimensionStyleRegistry::default();
        let resolved = reg.resolve(Some("engineering_inch"));
        assert_eq!(resolved.name, "engineering_inch");
        assert!(matches!(
            resolved.terminator,
            Terminator::Arrow { filled: true, .. }
        ));
    }

    #[test]
    fn presets_have_sensible_values() {
        let arch = DimensionStyle::architectural_imperial();
        assert!(arch.text_height_mm > 0.0);
        assert!(arch.dim_line_stroke_mm > 0.0);
        assert!(arch.extension_past_mm > arch.extension_gap_mm);
        assert!(matches!(
            arch.terminator,
            Terminator::ArchTick { angle_deg } if (angle_deg - 45.0).abs() < 0.01
        ));
    }
}
