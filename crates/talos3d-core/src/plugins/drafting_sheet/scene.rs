//! Backend-independent normalized drawing scene.
//!
//! Geometry positions use normalized drawing coordinates: `(0, 0)` is the
//! lower-left of the orthographic view and `(1, 1)` is the upper-right.
//! Paper-sized style attributes embedded in [`DimPrimitive`] remain
//! millimetres. A layout backend maps only positions to paper or viewport
//! coordinates, preserving authored line weights and annotation sizes.

use bevy::math::Vec2;

use crate::plugins::section_fill::{generate_hatch_lines, DRAWING_HATCH_DENSITY};
use crate::plugins::{
    drafting::{DimPrimitive, DraftPlane},
    identity::ElementId,
};

use super::sheet::{SheetBounds, SheetStroke, SheetView};

/// Stable semantic role of one projected primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrawingPrimitiveRole {
    VisibleEdge,
    SectionCutEdge,
    SectionFill,
    Annotation,
}

/// Stable primitive identity within a semantic owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrawingPrimitiveId {
    pub owner: ElementId,
    pub role: DrawingPrimitiveRole,
    pub ordinal: u32,
}

/// One normalized line segment with stable semantic ownership.
#[derive(Debug, Clone, Copy)]
pub struct DrawingSceneLine {
    pub id: DrawingPrimitiveId,
    pub owner: ElementId,
    pub a: Vec2,
    pub b: Vec2,
    pub stroke: SheetStroke,
}

/// One normalized section-fill polygon with stable semantic ownership.
#[derive(Debug, Clone)]
pub struct DrawingSceneHatch {
    pub id: DrawingPrimitiveId,
    pub owner: ElementId,
    pub polygon: Vec<Vec2>,
    pub pattern: crate::plugins::section_fill::HatchPattern,
}

/// One authored annotation rendered into normalized positions. Paper-sized
/// fields on the primitives remain millimetres.
#[derive(Debug, Clone)]
pub struct DrawingSceneAnnotation {
    pub id: DrawingPrimitiveId,
    pub owner: ElementId,
    pub primitives: Vec<DimPrimitive>,
}

/// A non-blocking projection finding. Missing stable identity is reported and
/// omitted rather than producing ownerless scene geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawingSceneFinding {
    pub code: &'static str,
    pub message: String,
}

/// One contiguous range in a batched live line vertex buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawingSceneLineSpan {
    pub id: DrawingPrimitiveId,
    pub owner: ElementId,
    pub stroke: SheetStroke,
    pub vertices: std::ops::Range<u32>,
}

/// Bevy/GPU-friendly line input derived from the same normalized scene used by
/// export. This adapter performs no model projection or semantic inference.
#[derive(Debug, Clone, Default)]
pub struct DrawingSceneLineBatch {
    pub vertices: Vec<Vec2>,
    pub spans: Vec<DrawingSceneLineSpan>,
}

/// The sole transient 3D-to-2D semantic projection result.
///
/// `DraftingSheet` is a paper-layout adapter derived from this value; exporters
/// and live presentation must not recapture the model independently.
#[derive(Debug, Clone)]
pub struct DrawingScene {
    /// Selected durable Draft whose referenced membership scoped this scene.
    /// `None` preserves the legacy all-authored-content projection.
    pub draft_id: Option<ElementId>,
    /// Shared canonical drawing-tool plane, copied only as derived scene input.
    pub draft_plane: Option<DraftPlane>,
    /// Deterministic fingerprint of normalized emitted content and its view.
    pub source_model_revision: u64,
    pub view: SheetView,
    pub lines: Vec<DrawingSceneLine>,
    pub hatches: Vec<DrawingSceneHatch>,
    pub annotations: Vec<DrawingSceneAnnotation>,
    pub bounds: SheetBounds,
    pub findings: Vec<DrawingSceneFinding>,
}

impl DrawingScene {
    pub fn new(view: SheetView) -> Self {
        Self {
            draft_id: None,
            draft_plane: None,
            source_model_revision: 0,
            view,
            lines: Vec::new(),
            hatches: Vec::new(),
            annotations: Vec::new(),
            bounds: SheetBounds::empty(),
            findings: Vec::new(),
        }
    }

    /// Recompute normalized content bounds without paper margin.
    pub fn recompute_bounds(&mut self) {
        let mut bounds = SheetBounds::empty();
        for line in &self.lines {
            bounds.include(line.a);
            bounds.include(line.b);
        }
        for hatch in &self.hatches {
            for point in &hatch.polygon {
                bounds.include(*point);
            }
        }
        for annotation in &self.annotations {
            for primitive in &annotation.primitives {
                for point in primitive_positions(primitive) {
                    bounds.include(point);
                }
            }
        }
        self.bounds = bounds;
    }

    /// Map normalized model linework into one contiguous target-space vertex
    /// buffer. A live Bevy renderer can upload this batch without recapturing
    /// or reclassifying the model.
    #[must_use]
    pub fn model_line_batch(&self, target_size: Vec2) -> DrawingSceneLineBatch {
        let mut batch = DrawingSceneLineBatch::default();
        for line in &self.lines {
            let start = batch.vertices.len() as u32;
            batch.vertices.push(line.a * target_size);
            batch.vertices.push(line.b * target_size);
            batch.spans.push(DrawingSceneLineSpan {
                id: line.id,
                owner: line.owner,
                stroke: line.stroke,
                vertices: start..start + 2,
            });
        }
        batch
    }

    /// Build the complete line-based presentation batch consumed by live
    /// Drafting. Model edges, section hatches, and non-text annotation
    /// primitives all come from this scene; the adapter performs no model
    /// queries or semantic inference. Filled regions and glyph presentation
    /// remain separate backend concerns over the same scene primitives.
    #[must_use]
    pub fn presentation_line_batch(&self, target_size: Vec2) -> DrawingSceneLineBatch {
        let mut batch = self.model_line_batch(target_size);

        for hatch in &self.hatches {
            let polygon: Vec<[f32; 2]> = hatch
                .polygon
                .iter()
                .map(|point| {
                    let point = *point * target_size;
                    [point.x, point.y]
                })
                .collect();
            for segment in generate_hatch_lines(&polygon, hatch.pattern, DRAWING_HATCH_DENSITY) {
                push_line_span(
                    &mut batch,
                    hatch.id,
                    hatch.owner,
                    SheetStroke::Hatch,
                    Vec2::new(segment[0], segment[1]),
                    Vec2::new(segment[2], segment[3]),
                );
            }
        }

        for annotation in &self.annotations {
            for primitive in &annotation.primitives {
                append_annotation_lines(
                    &mut batch,
                    annotation.id,
                    annotation.owner,
                    primitive,
                    target_size,
                );
            }
        }

        batch
    }
}

fn push_line_span(
    batch: &mut DrawingSceneLineBatch,
    id: DrawingPrimitiveId,
    owner: ElementId,
    stroke: SheetStroke,
    a: Vec2,
    b: Vec2,
) {
    let start = batch.vertices.len() as u32;
    batch.vertices.extend([a, b]);
    batch.spans.push(DrawingSceneLineSpan {
        id,
        owner,
        stroke,
        vertices: start..start + 2,
    });
}

fn append_annotation_lines(
    batch: &mut DrawingSceneLineBatch,
    id: DrawingPrimitiveId,
    owner: ElementId,
    primitive: &DimPrimitive,
    target_size: Vec2,
) {
    let position = |point: Vec2| point * target_size;
    match primitive {
        DimPrimitive::LineSegment { a, b, .. } => push_line_span(
            batch,
            id,
            owner,
            SheetStroke::Dimension,
            position(*a),
            position(*b),
        ),
        DimPrimitive::Tick {
            pos,
            rotation_rad,
            length_mm,
            ..
        } => {
            let half = *length_mm * 0.5;
            let axis = Vec2::new(rotation_rad.cos(), rotation_rad.sin()) * half;
            let center = position(*pos);
            push_line_span(
                batch,
                id,
                owner,
                SheetStroke::Dimension,
                center - axis,
                center + axis,
            );
        }
        DimPrimitive::Arrow {
            tip,
            tail,
            width_mm,
            ..
        } => {
            let tip = position(*tip);
            let tail = position(*tail);
            let axis = tail - tip;
            let axis_unit = axis.try_normalize().unwrap_or(Vec2::X);
            let perpendicular = Vec2::new(-axis_unit.y, axis_unit.x) * (*width_mm * 0.5);
            let left = tail + perpendicular;
            let right = tail - perpendicular;
            for (a, b) in [(tip, left), (left, right), (right, tip)] {
                push_line_span(batch, id, owner, SheetStroke::Dimension, a, b);
            }
        }
        DimPrimitive::Dot { pos, radius_mm } => {
            const SEGMENTS: usize = 16;
            let center = position(*pos);
            for index in 0..SEGMENTS {
                let angle_a = std::f32::consts::TAU * index as f32 / SEGMENTS as f32;
                let angle_b = std::f32::consts::TAU * (index + 1) as f32 / SEGMENTS as f32;
                let a = center + Vec2::from_angle(angle_a) * *radius_mm;
                let b = center + Vec2::from_angle(angle_b) * *radius_mm;
                push_line_span(batch, id, owner, SheetStroke::Dimension, a, b);
            }
        }
        DimPrimitive::Text { .. } => {}
    }
}

pub(crate) fn primitive_positions(primitive: &DimPrimitive) -> Vec<Vec2> {
    match primitive {
        DimPrimitive::LineSegment { a, b, .. } => vec![*a, *b],
        DimPrimitive::Tick { pos, .. } | DimPrimitive::Dot { pos, .. } => vec![*pos],
        DimPrimitive::Arrow { tip, tail, .. } => vec![*tip, *tail],
        DimPrimitive::Text { anchor, .. } => vec![*anchor],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;

    #[test]
    fn live_line_batch_preserves_scene_identity_and_only_maps_coordinates() {
        let owner = ElementId(11);
        let mut scene = DrawingScene::new(SheetView {
            eye: Vec3::Z,
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 1.0,
            aspect: 2.0,
            scale_denominator: 10.0,
            margin_mm: 0.0,
        });
        let id = DrawingPrimitiveId {
            owner,
            role: DrawingPrimitiveRole::VisibleEdge,
            ordinal: 0,
        };
        scene.lines.push(DrawingSceneLine {
            id,
            owner,
            a: Vec2::new(0.25, 0.5),
            b: Vec2::new(0.75, 1.0),
            stroke: SheetStroke::Silhouette,
        });

        let batch = scene.model_line_batch(Vec2::new(800.0, 600.0));
        assert_eq!(
            batch.vertices,
            vec![Vec2::new(200.0, 300.0), Vec2::new(600.0, 600.0)]
        );
        assert_eq!(batch.spans[0].id, id);
        assert_eq!(batch.spans[0].owner, owner);
        assert_eq!(batch.spans[0].vertices, 0..2);
    }

    #[test]
    fn presentation_batch_adds_annotation_geometry_without_new_semantics() {
        let owner = ElementId(12);
        let mut scene = DrawingScene::new(SheetView {
            eye: Vec3::Z,
            target: Vec3::ZERO,
            up: Vec3::Y,
            ortho_height_m: 1.0,
            aspect: 1.0,
            scale_denominator: 10.0,
            margin_mm: 0.0,
        });
        let id = DrawingPrimitiveId {
            owner,
            role: DrawingPrimitiveRole::Annotation,
            ordinal: 0,
        };
        scene.annotations.push(DrawingSceneAnnotation {
            id,
            owner,
            primitives: vec![DimPrimitive::Tick {
                pos: Vec2::splat(0.5),
                rotation_rad: 0.0,
                length_mm: 4.0,
                stroke_mm: 0.18,
            }],
        });

        let batch = scene.presentation_line_batch(Vec2::splat(100.0));
        assert_eq!(batch.spans.len(), 1);
        assert_eq!(batch.spans[0].id, id);
        assert_eq!(
            batch.vertices,
            vec![Vec2::new(48.0, 50.0), Vec2::new(52.0, 50.0)]
        );
    }
}
