//! Pure rendering of a dimension into primitive drawing instructions.
//!
//! This module intentionally has no knowledge of ECS, cameras, or file formats.
//! It takes a dimension annotation, its resolved style, and a 2D paper-plane
//! placement, and returns a list of [`DimPrimitive`] operations that SVG/PDF/DXF
//! writers can translate into their native syntax.
//!
//! # Coordinate system
//!
//! All primitives are emitted in **paper-millimetre** coordinates in a right-handed
//! 2D frame with `+x` to the right and `+y` up. The writers flip to `+y` down
//! for SVG.
//!
//! # Current kinds supported in Phase 1
//!
//! - [`DimensionKind::Linear`] — horizontal/vertical distance along `direction`
//! - [`DimensionKind::Aligned`] — distance along the line between two points
//!
//! Angular, Radial, Diameter, and Leader kinds are scaffolded in the enum and
//! will be filled in Phase 2 per the plan. They currently return a diagnostic
//! text primitive so the pipeline degrades gracefully rather than panics.

use bevy::math::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use super::kind::DimensionKind;
use super::style::{DimensionStyle, Terminator, TextPlacement};

/// Minimal, fully-serializable dimension description for rendering. This is
/// distinct from the authored-entity snapshot so the renderer can be exercised
/// in tests without spinning up ECS or bringing in ElementId.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionInput {
    /// What's being measured.
    pub kind: DimensionKind,

    /// First extension-line origin in world 3D. For linear dimensions this is
    /// one endpoint of the measured feature. For radial/diameter, repurposed
    /// as the centre.
    pub a: Vec3,

    /// Second extension-line origin.
    pub b: Vec3,

    /// Offset vector from the feature to the dimension line position, in
    /// world units. Perpendicular distance for linear/aligned; for radial/
    /// diameter this positions the text and arrow.
    pub offset: Vec3,

    /// Optional text override. When `None`, the renderer formats the measured
    /// value using the style's `NumberFormat`.
    pub text_override: Option<String>,
}

/// A drawing primitive in paper-millimetre space. Writers convert these to
/// their native geometry.
#[derive(Debug, Clone, PartialEq)]
pub enum DimPrimitive {
    /// Straight stroked line.
    LineSegment { a: Vec2, b: Vec2, stroke_mm: f32 },

    /// Architectural tick — a short oblique line centred on `pos`, oriented so
    /// its midpoint falls on the dimension line and it makes `angle_deg` with
    /// the dimension line's direction.
    Tick {
        pos: Vec2,
        /// Paper-space rotation of the tick, in radians. Equal to the dim line
        /// angle + the tick-vs-line angle from the style.
        rotation_rad: f32,
        length_mm: f32,
        stroke_mm: f32,
    },

    /// Triangular arrowhead, tip at `tip`, tail at `tail`. `width_mm` is the
    /// base width perpendicular to the tip→tail axis.
    Arrow {
        tip: Vec2,
        tail: Vec2,
        width_mm: f32,
        filled: bool,
        stroke_mm: f32,
    },

    /// Filled disc terminator.
    Dot { pos: Vec2, radius_mm: f32 },

    /// Text anchored at `anchor`. `rotation_rad` rotates the glyphs about the
    /// anchor. `anchor_mode` describes which point of the text box lands on
    /// `anchor`.
    Text {
        anchor: Vec2,
        content: String,
        height_mm: f32,
        rotation_rad: f32,
        anchor_mode: TextAnchor,
        font_family: String,
        color_hex: String,
    },
}

/// Where `anchor` lies on the text glyph box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextAnchor {
    /// Anchor at the centre of the baseline — text extends upward and is
    /// horizontally centred on the anchor. Use with `Above` placement.
    CenterBaseline,

    /// Anchor at the visual centre of the text. Use with `Centered` placement.
    Center,
}

// ─── Main entry point ────────────────────────────────────────────────────────

/// Render a dimension into a list of paper-space primitives.
///
/// - `input` coordinates (`a`, `b`, `offset`) are in **world units** (metres).
/// - `style` sizes (gaps, text height, line weights, etc.) are in **paper mm**.
/// - `world_to_paper` converts world metres to paper mm. For a 1:50 plan drawn
///   to print at 1 m world = 20 mm paper, pass `world_to_paper = 20.0`. For
///   mechanical drawings at 1:1 in mm, pass `1000.0` (1 m → 1000 mm). Pass
///   `1.0` if the caller has already pre-scaled the inputs.
///
/// The renderer projects the world-space endpoints onto the 2D paper plane by
/// dropping `z` (talos3D plans are drawn on the XY plane in paper-space mode).
/// Elevation and section views should pre-transform their inputs before calling.
#[must_use]
pub fn render_dimension(
    input: &DimensionInput,
    style: &DimensionStyle,
    world_to_paper: f32,
) -> Vec<DimPrimitive> {
    match &input.kind {
        DimensionKind::Linear { direction } => {
            render_linear(input, style, direction.truncate2d(), world_to_paper)
        }
        DimensionKind::Aligned => render_aligned(input, style, world_to_paper),
        DimensionKind::Angular { vertex } => render_angular(input, style, *vertex, world_to_paper),
        DimensionKind::Radial { center } => {
            render_radial_or_diameter(input, style, *center, world_to_paper, false)
        }
        DimensionKind::Diameter { center } => {
            render_radial_or_diameter(input, style, *center, world_to_paper, true)
        }
        DimensionKind::Leader { text } => render_leader(input, style, text, world_to_paper),
    }
}

// ─── Linear dimension ────────────────────────────────────────────────────────

fn render_linear(
    input: &DimensionInput,
    style: &DimensionStyle,
    direction: Vec2,
    world_to_paper: f32,
) -> Vec<DimPrimitive> {
    // Measured length stays in world metres for correct text formatting.
    let measured_metres = match input.kind {
        DimensionKind::Linear { direction } => {
            let dir3 = direction.try_normalize().unwrap_or(bevy::math::Vec3::X);
            ((input.b - input.a).dot(dir3)).abs()
        }
        _ => (input.a - input.b).length(),
    };

    // Scale geometry into paper mm up front. From this point on, everything is
    // in paper units.
    let a = input.a.truncate2d() * world_to_paper;
    let b = input.b.truncate2d() * world_to_paper;
    let offset = input.offset.truncate2d() * world_to_paper;

    let dir = direction
        .try_normalize()
        .unwrap_or_else(|| (b - a).try_normalize().unwrap_or(Vec2::X));

    // The dimension line lies parallel to `dir`, offset perpendicularly from
    // the feature by the signed component of `offset` along the normal.
    // Each extension-line foot sits directly over its feature point (preserving
    // the feature's position along `dir`), shifted perpendicularly by
    // `signed_offset`.
    let normal = Vec2::new(-dir.y, dir.x);
    let signed_offset = offset.dot(normal);

    let line_a = a + normal * signed_offset;
    let line_b = b + normal * signed_offset;

    emit_linear_primitives(
        a,
        b,
        line_a,
        line_b,
        normal,
        signed_offset,
        style,
        &resolved_text(input, style, measured_metres),
    )
}

// ─── Aligned dimension ───────────────────────────────────────────────────────

fn render_aligned(
    input: &DimensionInput,
    style: &DimensionStyle,
    world_to_paper: f32,
) -> Vec<DimPrimitive> {
    let measured_metres = (input.a - input.b).length();
    let a = input.a.truncate2d() * world_to_paper;
    let b = input.b.truncate2d() * world_to_paper;
    let offset = input.offset.truncate2d() * world_to_paper;

    let axis = (b - a).try_normalize().unwrap_or(Vec2::X);
    let normal = Vec2::new(-axis.y, axis.x);
    let signed_offset = offset.dot(normal);

    let line_a = a + normal * signed_offset;
    let line_b = b + normal * signed_offset;

    emit_linear_primitives(
        a,
        b,
        line_a,
        line_b,
        normal,
        signed_offset,
        style,
        &resolved_text(input, style, measured_metres),
    )
}

// ─── Shared linear emission ──────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit_linear_primitives(
    feature_a: Vec2,
    feature_b: Vec2,
    dim_line_a: Vec2,
    dim_line_b: Vec2,
    normal: Vec2,
    signed_offset: f32,
    style: &DimensionStyle,
    text: &str,
) -> Vec<DimPrimitive> {
    let mut out = Vec::with_capacity(8);
    let offset_sign = if signed_offset >= 0.0 { 1.0 } else { -1.0 };

    // Unit vector pointing from feature towards dim line (along `normal`
    // scaled by sign).
    let toward_line = normal * offset_sign;

    // Extension lines: start at feature + gap*toward_line, end at dim line +
    // extend_past*toward_line.
    let gap = style.extension_gap_mm;
    let past = style.extension_past_mm;
    let stroke_ext = style.extension_stroke_mm;

    let ext_a_start = feature_a + toward_line * gap;
    let ext_a_end = dim_line_a + toward_line * past;
    let ext_b_start = feature_b + toward_line * gap;
    let ext_b_end = dim_line_b + toward_line * past;

    out.push(DimPrimitive::LineSegment {
        a: ext_a_start,
        b: ext_a_end,
        stroke_mm: stroke_ext,
    });
    out.push(DimPrimitive::LineSegment {
        a: ext_b_start,
        b: ext_b_end,
        stroke_mm: stroke_ext,
    });

    // Dimension line: extends slightly past each tick for arch-style, or stops
    // at the tip for arrows.
    let dim_line_axis = (dim_line_b - dim_line_a).try_normalize().unwrap_or(Vec2::X);
    let past_tick = style.dim_line_extend_past_tick_mm;
    let dim_a = dim_line_a - dim_line_axis * past_tick;
    let dim_b = dim_line_b + dim_line_axis * past_tick;
    let stroke_dim = style.dim_line_stroke_mm;

    // If the text breaks the dim line (mechanical convention), draw two
    // segments with a gap around the text. Otherwise draw a single continuous
    // line.
    let text_height = style.text_height_mm;
    let mid = (dim_line_a + dim_line_b) * 0.5;
    let dim_line_angle = dim_line_axis.y.atan2(dim_line_axis.x);

    match style.text_placement {
        TextPlacement::Centered {
            break_line: true,
            gap_mm,
        } => {
            let half_text = approximate_text_half_width(text, text_height);
            let break_half = half_text + gap_mm;
            let break_start = mid - dim_line_axis * break_half;
            let break_end = mid + dim_line_axis * break_half;
            out.push(DimPrimitive::LineSegment {
                a: dim_a,
                b: break_start,
                stroke_mm: stroke_dim,
            });
            out.push(DimPrimitive::LineSegment {
                a: break_end,
                b: dim_b,
                stroke_mm: stroke_dim,
            });
            out.push(DimPrimitive::Text {
                anchor: mid,
                content: text.to_string(),
                height_mm: text_height,
                rotation_rad: normalise_text_angle(dim_line_angle),
                anchor_mode: TextAnchor::Center,
                font_family: style.text_font.clone(),
                color_hex: style.text_color_hex.clone(),
            });
        }
        TextPlacement::Centered {
            break_line: false,
            gap_mm: _,
        } => {
            out.push(DimPrimitive::LineSegment {
                a: dim_a,
                b: dim_b,
                stroke_mm: stroke_dim,
            });
            out.push(DimPrimitive::Text {
                anchor: mid,
                content: text.to_string(),
                height_mm: text_height,
                rotation_rad: normalise_text_angle(dim_line_angle),
                anchor_mode: TextAnchor::Center,
                font_family: style.text_font.clone(),
                color_hex: style.text_color_hex.clone(),
            });
        }
        TextPlacement::Above { gap_mm } => {
            out.push(DimPrimitive::LineSegment {
                a: dim_a,
                b: dim_b,
                stroke_mm: stroke_dim,
            });
            // Text sits on the side AWAY from the feature (opposite to the
            // toward_line direction), at `gap_mm` above the dim line. Without
            // the sign flip, text can end up inside the object.
            let text_offset = -toward_line * gap_mm;
            out.push(DimPrimitive::Text {
                anchor: mid + text_offset,
                content: text.to_string(),
                height_mm: text_height,
                rotation_rad: normalise_text_angle(dim_line_angle),
                anchor_mode: TextAnchor::CenterBaseline,
                font_family: style.text_font.clone(),
                color_hex: style.text_color_hex.clone(),
            });
        }
        TextPlacement::Horizontal { gap_mm } => {
            out.push(DimPrimitive::LineSegment {
                a: dim_a,
                b: dim_b,
                stroke_mm: stroke_dim,
            });
            out.push(DimPrimitive::Text {
                anchor: mid - toward_line * gap_mm,
                content: text.to_string(),
                height_mm: text_height,
                rotation_rad: 0.0,
                anchor_mode: TextAnchor::CenterBaseline,
                font_family: style.text_font.clone(),
                color_hex: style.text_color_hex.clone(),
            });
        }
    }

    // Terminators at both ends.
    match style.terminator {
        Terminator::ArchTick { angle_deg } => {
            let tick_angle = angle_deg.to_radians();
            out.push(DimPrimitive::Tick {
                pos: dim_line_a,
                rotation_rad: dim_line_angle + tick_angle,
                length_mm: style.terminator_size_mm,
                stroke_mm: stroke_dim,
            });
            out.push(DimPrimitive::Tick {
                pos: dim_line_b,
                rotation_rad: dim_line_angle + tick_angle,
                length_mm: style.terminator_size_mm,
                stroke_mm: stroke_dim,
            });
        }
        Terminator::Arrow {
            length_to_width_ratio,
            filled,
        } => {
            let length = style.terminator_size_mm;
            let width = length / length_to_width_ratio.max(0.01);
            // Arrow A: tip points toward a, tail extends along +dim_line_axis.
            out.push(DimPrimitive::Arrow {
                tip: dim_line_a,
                tail: dim_line_a + dim_line_axis * length,
                width_mm: width,
                filled,
                stroke_mm: stroke_dim,
            });
            out.push(DimPrimitive::Arrow {
                tip: dim_line_b,
                tail: dim_line_b - dim_line_axis * length,
                width_mm: width,
                filled,
                stroke_mm: stroke_dim,
            });
        }
        Terminator::Dot { radius_mm } => {
            out.push(DimPrimitive::Dot {
                pos: dim_line_a,
                radius_mm,
            });
            out.push(DimPrimitive::Dot {
                pos: dim_line_b,
                radius_mm,
            });
        }
        Terminator::None => {}
    }

    out
}

/// Angle text so it reads left-to-right. Text along a dim line pointing at
/// angle θ should be drawn at angle θ, but if θ is in the left half (|θ| > π/2)
/// flip by π so text is never upside-down. Convention in both arch and mech.
fn normalise_text_angle(angle_rad: f32) -> f32 {
    let pi = std::f32::consts::PI;
    let half = pi / 2.0;
    if angle_rad > half {
        angle_rad - pi
    } else if angle_rad < -half {
        angle_rad + pi
    } else {
        angle_rad
    }
}

/// Rough glyph width estimate — good enough for placing dim-line breaks
/// around text. Viewers that measure text accurately may produce tighter
/// breaks; that's fine, the visual difference is subpixel.
fn approximate_text_half_width(text: &str, height_mm: f32) -> f32 {
    // Typical sans widths are ~0.55 × height per glyph including kerning.
    0.55 * height_mm * text.chars().count() as f32 * 0.5
}

fn resolved_text(input: &DimensionInput, style: &DimensionStyle, measured_metres: f32) -> String {
    if let Some(override_text) = &input.text_override {
        return override_text.clone();
    }
    let formatted = style.number_format.format_metres(measured_metres);
    let prefix = style.prefix.as_deref().unwrap_or("");
    let suffix = style.suffix.as_deref().unwrap_or("");
    format!("{prefix}{formatted}{suffix}")
}

// ─── Radial / Diameter ───────────────────────────────────────────────────────

/// Radial or diameter dimension. `a` is a point on the circle; `b` is a second
/// point (for diameter, the opposite point through `center`; for radial,
/// unused). `offset` positions the text and terminator anchor relative to the
/// centre.
///
/// Radial layout: arrow points from outside the circle INTO the edge point `a`.
/// Text reads along the radius line, positioned at the end of the arrow away
/// from the circle (or just past `a` along the radius direction).
///
/// Diameter layout: two arrows from outside the circle pointing to opposite
/// edge points. Text reads along the diameter line.
fn render_radial_or_diameter(
    input: &DimensionInput,
    style: &DimensionStyle,
    center: Vec3,
    world_to_paper: f32,
    is_diameter: bool,
) -> Vec<DimPrimitive> {
    let measured_metres = if is_diameter {
        // Diameter: distance between a and b (through centre).
        (input.a - input.b).length()
    } else {
        // Radial: distance from centre to a.
        (input.a - center).length()
    };

    let centre_px = center.truncate2d() * world_to_paper;
    let a_px = input.a.truncate2d() * world_to_paper;
    let offset_px = input.offset.truncate2d() * world_to_paper;

    // Direction from centre to edge point.
    let radial_dir = (a_px - centre_px).try_normalize().unwrap_or(Vec2::X);
    let perp = Vec2::new(-radial_dir.y, radial_dir.x);

    let text = {
        let base = style.number_format.format_metres(measured_metres);
        let prefix = style.prefix.clone().unwrap_or_else(|| {
            if is_diameter {
                "Ø".to_string()
            } else {
                "R".to_string()
            }
        });
        let suffix = style.suffix.as_deref().unwrap_or("");
        let inner = input
            .text_override
            .clone()
            .unwrap_or_else(|| format!("{prefix}{base}{suffix}"));
        inner
    };

    let mut out = Vec::new();
    let stroke = style.dim_line_stroke_mm;
    let arrow_len = style.terminator_size_mm;

    // Leader / extension line from centre (or past a) to the text position.
    let offset_signed = offset_px.dot(radial_dir);
    // If offset has magnitude, the leader extends beyond `a` by that amount
    // (offset in world frame is projected onto the radial). If offset is
    // roughly perpendicular we interpret it as the text position.
    let leader_end = a_px + radial_dir * offset_signed.abs().max(6.0);

    out.push(DimPrimitive::LineSegment {
        a: a_px,
        b: leader_end,
        stroke_mm: stroke,
    });
    // Arrow at edge of circle pointing in along radial_dir (tip at a, tail
    // away from centre).
    emit_radial_terminator(&mut out, a_px, -radial_dir, style);

    // For diameter, add a second arrow on the opposite side.
    if is_diameter {
        let b_px = input.b.truncate2d() * world_to_paper;
        let opp_dir = (b_px - centre_px).try_normalize().unwrap_or(-radial_dir);
        out.push(DimPrimitive::LineSegment {
            a: centre_px,
            b: a_px,
            stroke_mm: stroke,
        });
        out.push(DimPrimitive::LineSegment {
            a: centre_px,
            b: b_px,
            stroke_mm: stroke,
        });
        emit_radial_terminator(&mut out, b_px, -opp_dir, style);
    }

    // Text at the end of the leader, rotated with the radial.
    let text_pos = leader_end + radial_dir * (arrow_len + 1.0);
    let rotation = normalise_text_angle(radial_dir.y.atan2(radial_dir.x));
    let anchor_mode = match style.text_placement {
        TextPlacement::Centered { .. } => TextAnchor::Center,
        _ => TextAnchor::CenterBaseline,
    };
    // For CenterBaseline, lift slightly off the leader using perp direction.
    let text_anchor = match style.text_placement {
        TextPlacement::Above { gap_mm } => text_pos + perp * gap_mm,
        _ => text_pos,
    };
    out.push(DimPrimitive::Text {
        anchor: text_anchor,
        content: text,
        height_mm: style.text_height_mm,
        rotation_rad: rotation,
        anchor_mode,
        font_family: style.text_font.clone(),
        color_hex: style.text_color_hex.clone(),
    });

    out
}

fn emit_radial_terminator(
    out: &mut Vec<DimPrimitive>,
    tip: Vec2,
    direction_outward: Vec2,
    style: &DimensionStyle,
) {
    let size = style.terminator_size_mm;
    let stroke = style.dim_line_stroke_mm;
    match style.terminator {
        Terminator::ArchTick { angle_deg } => {
            let base_angle = direction_outward.y.atan2(direction_outward.x);
            out.push(DimPrimitive::Tick {
                pos: tip,
                rotation_rad: base_angle + angle_deg.to_radians(),
                length_mm: size,
                stroke_mm: stroke,
            });
        }
        Terminator::Arrow {
            length_to_width_ratio,
            filled,
        } => {
            let width = size / length_to_width_ratio.max(0.01);
            out.push(DimPrimitive::Arrow {
                tip,
                tail: tip + direction_outward * size,
                width_mm: width,
                filled,
                stroke_mm: stroke,
            });
        }
        Terminator::Dot { radius_mm } => {
            out.push(DimPrimitive::Dot {
                pos: tip,
                radius_mm,
            });
        }
        Terminator::None => {}
    }
}

// ─── Angular ─────────────────────────────────────────────────────────────────

/// Angular dimension. `vertex` is the corner; `a` and `b` are points on each
/// ray. Draws the two rays, an arc between them, arrows at the arc ends, and
/// the angle text near the arc midpoint.
fn render_angular(
    input: &DimensionInput,
    style: &DimensionStyle,
    vertex: Vec3,
    world_to_paper: f32,
) -> Vec<DimPrimitive> {
    let v_px = vertex.truncate2d() * world_to_paper;
    let a_px = input.a.truncate2d() * world_to_paper;
    let b_px = input.b.truncate2d() * world_to_paper;
    let offset_px = input.offset.truncate2d() * world_to_paper;

    let dir_a = (a_px - v_px).try_normalize().unwrap_or(Vec2::X);
    let dir_b = (b_px - v_px).try_normalize().unwrap_or(Vec2::Y);

    // Angle in world: use the original (world) vectors to avoid scale skew.
    let va = (input.a - vertex)
        .truncate2d()
        .try_normalize()
        .unwrap_or(Vec2::X);
    let vb = (input.b - vertex)
        .truncate2d()
        .try_normalize()
        .unwrap_or(Vec2::Y);
    let dot = va.dot(vb);
    let angle_rad = dot.clamp(-1.0, 1.0).acos();
    let angle_deg = angle_rad.to_degrees();

    // Arc radius = max of |v→a|, |v→b| in paper space, or user-specified via offset.length
    let auto_radius = (a_px - v_px).length().min((b_px - v_px).length());
    let user_radius = offset_px.length();
    let radius = if user_radius > 1.0 {
        user_radius
    } else {
        auto_radius * 0.6
    };

    let start_angle = dir_a.y.atan2(dir_a.x);
    let end_angle = dir_b.y.atan2(dir_b.x);
    // Normalise end relative to start so arc goes the short way.
    let mut delta = end_angle - start_angle;
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }

    let stroke = style.dim_line_stroke_mm;
    let mut out = Vec::new();

    // Extension lines: along each ray from past the point OR from the vertex
    // out to the radius position. For angular dims typical to draw from vertex
    // along each ray to radius, no gap at the vertex.
    out.push(DimPrimitive::LineSegment {
        a: v_px,
        b: v_px + dir_a * (radius + style.extension_past_mm),
        stroke_mm: style.extension_stroke_mm,
    });
    out.push(DimPrimitive::LineSegment {
        a: v_px,
        b: v_px + dir_b * (radius + style.extension_past_mm),
        stroke_mm: style.extension_stroke_mm,
    });

    // Approximate arc with short line segments.
    const ARC_STEPS: usize = 24;
    let step = delta / ARC_STEPS as f32;
    let mut prev = v_px + dir_a * radius;
    for i in 1..=ARC_STEPS {
        let a = start_angle + step * i as f32;
        let next = v_px + Vec2::new(a.cos(), a.sin()) * radius;
        out.push(DimPrimitive::LineSegment {
            a: prev,
            b: next,
            stroke_mm: stroke,
        });
        prev = next;
    }

    // Terminators at each arc endpoint.
    let start_pt = v_px + dir_a * radius;
    let end_pt =
        v_px + Vec2::new((start_angle + delta).cos(), (start_angle + delta).sin()) * radius;
    // Tangent direction at each endpoint (perpendicular to radius).
    let tan_a = Vec2::new(-dir_a.y, dir_a.x) * delta.signum();
    let dir_b_tan =
        Vec2::new(-(start_angle + delta).sin(), (start_angle + delta).cos()) * delta.signum();
    emit_radial_terminator(&mut out, start_pt, -tan_a, style);
    emit_radial_terminator(&mut out, end_pt, dir_b_tan, style);

    // Text at arc midpoint.
    let mid_angle = start_angle + delta * 0.5;
    let text_radius = radius + style.text_height_mm;
    let text_pos = v_px + Vec2::new(mid_angle.cos(), mid_angle.sin()) * text_radius;
    let text_rot = normalise_text_angle(mid_angle + std::f32::consts::FRAC_PI_2);

    let content = input.text_override.clone().unwrap_or_else(|| {
        // Degrees, 0 or 1 decimal place per magnitude.
        if angle_deg.fract() < 0.05 {
            format!("{}°", angle_deg.round() as i32)
        } else {
            format!("{angle_deg:.1}°")
        }
    });

    let anchor_mode = match style.text_placement {
        TextPlacement::Centered { .. } => TextAnchor::Center,
        _ => TextAnchor::CenterBaseline,
    };

    out.push(DimPrimitive::Text {
        anchor: text_pos,
        content,
        height_mm: style.text_height_mm,
        rotation_rad: text_rot,
        anchor_mode,
        font_family: style.text_font.clone(),
        color_hex: style.text_color_hex.clone(),
    });

    out
}

// ─── Leader ──────────────────────────────────────────────────────────────────

/// Leader: a note with an arrow. `a` is the attach point (where the arrow
/// sits), `b` is the text anchor. Simple polyline with an arrow at `a`.
fn render_leader(
    input: &DimensionInput,
    style: &DimensionStyle,
    text: &str,
    world_to_paper: f32,
) -> Vec<DimPrimitive> {
    let a = input.a.truncate2d() * world_to_paper;
    let b = input.b.truncate2d() * world_to_paper;
    let stroke = style.dim_line_stroke_mm;

    let mut out = Vec::new();
    // Two-segment leader: from a to a midpoint, then to b (typical practice is
    // the second segment horizontal at the text).
    let landing_len = style.text_height_mm * 3.0;
    let landing_end = b; // text anchor
    let landing_start = Vec2::new(landing_end.x - landing_len, landing_end.y);

    out.push(DimPrimitive::LineSegment {
        a,
        b: landing_start,
        stroke_mm: stroke,
    });
    out.push(DimPrimitive::LineSegment {
        a: landing_start,
        b: landing_end,
        stroke_mm: stroke,
    });

    // Arrow at `a` pointing toward a from landing_start direction.
    let in_dir = (a - landing_start).try_normalize().unwrap_or(Vec2::X);
    emit_radial_terminator(&mut out, a, -in_dir, style);

    let content = input
        .text_override
        .clone()
        .unwrap_or_else(|| text.to_string());
    out.push(DimPrimitive::Text {
        anchor: landing_end + Vec2::new(1.0, 0.0),
        content,
        height_mm: style.text_height_mm,
        rotation_rad: 0.0,
        anchor_mode: TextAnchor::CenterBaseline,
        font_family: style.text_font.clone(),
        color_hex: style.text_color_hex.clone(),
    });

    out
}

// ─── Vec3 → Vec2 helper ──────────────────────────────────────────────────────

trait TruncateXy {
    fn truncate2d(self) -> Vec2;
}

impl TruncateXy for Vec3 {
    fn truncate2d(self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::drafting::kind::DimensionKind;

    fn mk_linear_horizontal(len_metres: f32, offset_y: f32) -> DimensionInput {
        DimensionInput {
            kind: DimensionKind::Linear { direction: Vec3::X },
            a: Vec3::ZERO,
            b: Vec3::new(len_metres, 0.0, 0.0),
            offset: Vec3::new(0.0, offset_y, 0.0),
            text_override: None,
        }
    }

    #[test]
    fn linear_arch_imperial_emits_expected_primitive_count() {
        // 2 extension lines + 1 dim line + 2 ticks + 1 text = 6 primitives
        let style = DimensionStyle::architectural_imperial();
        let prims = render_dimension(&mk_linear_horizontal(4.572, 0.5), &style, 1000.0);
        assert_eq!(
            prims.len(),
            6,
            "expected 6 primitives, got {:?}",
            prims.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn linear_mech_breaks_dim_line_around_text() {
        // 2 extension lines + 2 dim line halves + 2 arrows + 1 text = 7
        let style = DimensionStyle::engineering_mm();
        let prims = render_dimension(&mk_linear_horizontal(4.572, 0.5), &style, 1000.0);
        assert_eq!(prims.len(), 7);
    }

    #[test]
    fn linear_dim_text_is_formatted_per_style() {
        let style = DimensionStyle::architectural_imperial();
        let prims = render_dimension(&mk_linear_horizontal(4.572, 0.5), &style, 1000.0);
        let text = prims
            .iter()
            .find_map(|p| match p {
                DimPrimitive::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("text primitive present");
        // 4.572 m == 15 ft exactly
        assert_eq!(text, "15'-0\"");
    }

    #[test]
    fn linear_metric_architectural_formats_as_mm() {
        let style = DimensionStyle::architectural_metric();
        let prims = render_dimension(&mk_linear_horizontal(4.572, 0.5), &style, 1000.0);
        let text = prims
            .iter()
            .find_map(|p| match p {
                DimPrimitive::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("text primitive present");
        assert_eq!(text, "4572");
    }

    #[test]
    fn text_override_wins() {
        let style = DimensionStyle::architectural_imperial();
        let mut input = mk_linear_horizontal(4.572, 0.5);
        input.text_override = Some("TYP.".to_string());
        let prims = render_dimension(&input, &style, 1000.0);
        assert!(prims.iter().any(|p| matches!(
            p,
            DimPrimitive::Text { content, .. } if content == "TYP."
        )));
    }

    #[test]
    fn normalise_text_angle_flips_upside_down() {
        use std::f32::consts::PI;
        // θ slightly under π reads upside-down; must flip by π to get near 0.
        let flipped = normalise_text_angle(PI * 0.99);
        assert!(flipped < 0.0 && flipped > -PI);
        // θ in the readable range stays unchanged.
        let kept = normalise_text_angle(PI * 0.3);
        assert!((kept - PI * 0.3).abs() < 1e-6);
    }

    #[test]
    fn radial_emits_r_prefix_text() {
        let style = DimensionStyle::engineering_mm();
        let input = DimensionInput {
            kind: DimensionKind::Radial { center: Vec3::ZERO },
            a: Vec3::new(0.025, 0.0, 0.0), // 25 mm radius
            b: Vec3::ZERO,
            offset: Vec3::new(0.01, 0.0, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let text = prims
            .iter()
            .find_map(|p| match p {
                DimPrimitive::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("text present");
        assert_eq!(text, "R25");
    }

    #[test]
    fn diameter_emits_phi_prefix_text() {
        let style = DimensionStyle::engineering_mm();
        let input = DimensionInput {
            kind: DimensionKind::Diameter { center: Vec3::ZERO },
            a: Vec3::new(0.025, 0.0, 0.0),
            b: Vec3::new(-0.025, 0.0, 0.0),
            offset: Vec3::new(0.015, 0.0, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let text = prims
            .iter()
            .find_map(|p| match p {
                DimPrimitive::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("text present");
        assert_eq!(text, "Ø50");
    }

    #[test]
    fn angular_90_deg_emits_degree_symbol() {
        let style = DimensionStyle::engineering_mm();
        let input = DimensionInput {
            kind: DimensionKind::Angular { vertex: Vec3::ZERO },
            a: Vec3::new(1.0, 0.0, 0.0),
            b: Vec3::new(0.0, 1.0, 0.0),
            offset: Vec3::new(0.5, 0.5, 0.0),
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 100.0);
        let text = prims
            .iter()
            .find_map(|p| match p {
                DimPrimitive::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("text present");
        assert!(text.contains("90") && text.contains("°"));
    }

    #[test]
    fn leader_renders_two_segments_and_arrow() {
        let style = DimensionStyle::engineering_mm();
        let input = DimensionInput {
            kind: DimensionKind::Leader {
                text: "SEE DETAIL A".to_string(),
            },
            a: Vec3::new(0.1, 0.1, 0.0),
            b: Vec3::new(0.3, 0.3, 0.0),
            offset: Vec3::ZERO,
            text_override: None,
        };
        let prims = render_dimension(&input, &style, 1000.0);
        let line_count = prims
            .iter()
            .filter(|p| matches!(p, DimPrimitive::LineSegment { .. }))
            .count();
        assert_eq!(line_count, 2, "leader has 2 polyline segments");
        let arrow_count = prims
            .iter()
            .filter(|p| matches!(p, DimPrimitive::Arrow { .. }))
            .count();
        assert_eq!(arrow_count, 1, "leader has 1 arrow at tip");
        assert!(prims.iter().any(|p| matches!(
            p,
            DimPrimitive::Text { content, .. } if content == "SEE DETAIL A"
        )));
    }

    #[test]
    fn text_override_wins_on_all_kinds() {
        let style = DimensionStyle::engineering_mm();
        for kind in [
            DimensionKind::Linear { direction: Vec3::X },
            DimensionKind::Aligned,
            DimensionKind::Radial { center: Vec3::ZERO },
            DimensionKind::Diameter { center: Vec3::ZERO },
            DimensionKind::Angular { vertex: Vec3::ZERO },
            DimensionKind::Leader {
                text: "fallback".into(),
            },
        ] {
            let input = DimensionInput {
                kind,
                a: Vec3::new(0.01, 0.0, 0.0),
                b: Vec3::new(0.02, 0.01, 0.0),
                offset: Vec3::new(0.005, 0.0, 0.0),
                text_override: Some("OVERRIDE".to_string()),
            };
            let prims = render_dimension(&input, &style, 1000.0);
            assert!(
                prims.iter().any(|p| matches!(
                    p,
                    DimPrimitive::Text { content, .. } if content == "OVERRIDE"
                )),
                "override applies to each kind"
            );
        }
    }
}
