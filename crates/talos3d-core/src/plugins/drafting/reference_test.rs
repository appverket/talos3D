//! End-to-end reference: recreate the 15'×15' shed image as a dimensioned SVG.
//!
//! Writes output SVGs to `/tmp/talos3d_drafting_shed_15x15_<preset>.svg`. The
//! test asserts structural invariants (expected dimension text values, minimum
//! primitive counts); visual parity with the user's reference image is verified
//! by opening the SVG in a browser or Inkscape.
//!
//! All drafting inputs are in world metres; the renderer converts to paper mm
//! via the `world_to_paper` scale passed in. Printed at 1:50 → 1 m world =
//! 20 mm paper; walls are drawn separately using the same scale so the output
//! composes cleanly.

use std::f32::consts::PI;

use bevy::math::{Vec2, Vec3};

use super::{
    export_dxf::{export_dxf, DxfUnit},
    export_svg::render_dimensions_svg_document,
    kind::DimensionKind,
    render::{render_dimension, DimPrimitive, DimensionInput},
    style::DimensionStyle,
};

const FT: f32 = 0.3048;

fn fi(feet: f32, inches: f32) -> f32 {
    feet * FT + inches * (FT / 12.0)
}

fn horiz(x_start: f32, x_end: f32, y: f32, offset_y_world: f32) -> DimensionInput {
    DimensionInput {
        kind: DimensionKind::Linear { direction: Vec3::X },
        a: Vec3::new(x_start, y, 0.0),
        b: Vec3::new(x_end, y, 0.0),
        offset: Vec3::new(0.0, offset_y_world, 0.0),
        text_override: None,
    }
}

fn vert(x: f32, y_start: f32, y_end: f32, offset_x_world: f32) -> DimensionInput {
    DimensionInput {
        kind: DimensionKind::Linear { direction: Vec3::Y },
        a: Vec3::new(x, y_start, 0.0),
        b: Vec3::new(x, y_end, 0.0),
        offset: Vec3::new(offset_x_world, 0.0, 0.0),
        text_override: None,
    }
}

/// Walls for the shed, emitted directly in paper-mm using the same
/// `world_to_paper` scale the dimensions use. `tx`/`ty` shift the drawing away
/// from the SVG origin so dimension strings have room outside the footprint.
fn shed_walls(world_to_paper: f32, tx: f32, ty: f32) -> Vec<DimPrimitive> {
    let w_paper = 15.0 * FT * world_to_paper;
    let partition_x = 7.0 * FT * world_to_paper;
    let partition_y_bottom = fi(7.0, 8.5) * world_to_paper;
    let wall_stroke = 0.5; // ISO 128 "wide" line

    let p = |x: f32, y: f32| Vec2::new(x + tx, y + ty);
    let mut out = Vec::new();
    // Outer square
    for (a, b) in [
        (p(0.0, 0.0), p(w_paper, 0.0)),
        (p(w_paper, 0.0), p(w_paper, w_paper)),
        (p(w_paper, w_paper), p(0.0, w_paper)),
        (p(0.0, w_paper), p(0.0, 0.0)),
    ] {
        out.push(DimPrimitive::LineSegment {
            a,
            b,
            stroke_mm: wall_stroke,
        });
    }
    // Partition (L-shape inside NW corner)
    out.push(DimPrimitive::LineSegment {
        a: p(0.0, partition_y_bottom),
        b: p(partition_x, partition_y_bottom),
        stroke_mm: wall_stroke,
    });
    out.push(DimPrimitive::LineSegment {
        a: p(partition_x, partition_y_bottom),
        b: p(partition_x, w_paper),
        stroke_mm: wall_stroke,
    });
    out
}

fn shift_dim(prims: Vec<DimPrimitive>, tx: f32, ty: f32) -> Vec<DimPrimitive> {
    prims
        .into_iter()
        .map(|prim| match prim {
            DimPrimitive::LineSegment { a, b, stroke_mm } => DimPrimitive::LineSegment {
                a: Vec2::new(a.x + tx, a.y + ty),
                b: Vec2::new(b.x + tx, b.y + ty),
                stroke_mm,
            },
            DimPrimitive::Tick {
                pos,
                rotation_rad,
                length_mm,
                stroke_mm,
            } => DimPrimitive::Tick {
                pos: Vec2::new(pos.x + tx, pos.y + ty),
                rotation_rad,
                length_mm,
                stroke_mm,
            },
            DimPrimitive::Arrow {
                tip,
                tail,
                width_mm,
                filled,
                stroke_mm,
            } => DimPrimitive::Arrow {
                tip: Vec2::new(tip.x + tx, tip.y + ty),
                tail: Vec2::new(tail.x + tx, tail.y + ty),
                width_mm,
                filled,
                stroke_mm,
            },
            DimPrimitive::Dot { pos, radius_mm } => DimPrimitive::Dot {
                pos: Vec2::new(pos.x + tx, pos.y + ty),
                radius_mm,
            },
            DimPrimitive::Text {
                anchor,
                content,
                height_mm,
                rotation_rad,
                anchor_mode,
                font_family,
                color_hex,
            } => DimPrimitive::Text {
                anchor: Vec2::new(anchor.x + tx, anchor.y + ty),
                content,
                height_mm,
                rotation_rad,
                anchor_mode,
                font_family,
                color_hex,
            },
        })
        .collect()
}

#[test]
fn shed_15x15_arch_imperial_renders_and_dumps_svg() {
    let style = DimensionStyle::architectural_imperial();
    let w = 15.0 * FT;

    // 1:50 plot scale: 1 m world = 20 mm paper.
    let world_to_paper = 20.0;
    let pad_mm = 40.0; // room for stacked dim strings

    // Dim-string offsets in WORLD metres — the renderer multiplies by
    // world_to_paper internally. Outer string sits further out.
    let offs_1 = (style.first_offset_mm) / world_to_paper;
    let offs_2 = (style.first_offset_mm + style.stack_spacing_mm) / world_to_paper;

    let mut dims: Vec<Vec<DimPrimitive>> = Vec::new();

    // South (y=0) — inner chain 4' 3' 3' 3' 2' + overall
    let south_segments = [0.0, 4.0 * FT, 7.0 * FT, 10.0 * FT, 13.0 * FT, w];
    for pair in south_segments.windows(2) {
        dims.push(render_dimension(
            &horiz(pair[0], pair[1], 0.0, -offs_1),
            &style,
            world_to_paper,
        ));
    }
    dims.push(render_dimension(
        &horiz(0.0, w, 0.0, -offs_2),
        &style,
        world_to_paper,
    ));

    // North (y=15') — partition chain 7' 7'-8½" + overall
    dims.push(render_dimension(
        &horiz(0.0, 7.0 * FT, w, offs_1),
        &style,
        world_to_paper,
    ));
    dims.push(render_dimension(
        &horiz(7.0 * FT, w, w, offs_1),
        &style,
        world_to_paper,
    ));
    dims.push(render_dimension(
        &horiz(0.0, w, w, offs_2),
        &style,
        world_to_paper,
    ));

    // West (x=0) — 7' upper + 7'-8½" lower + overall
    let y_p = fi(7.0, 8.5);
    dims.push(render_dimension(
        &vert(0.0, y_p, w, -offs_1),
        &style,
        world_to_paper,
    ));
    dims.push(render_dimension(
        &vert(0.0, 0.0, y_p, -offs_1),
        &style,
        world_to_paper,
    ));
    dims.push(render_dimension(
        &vert(0.0, 0.0, w, -offs_2),
        &style,
        world_to_paper,
    ));

    // East (x=15') — overall only
    dims.push(render_dimension(
        &vert(w, 0.0, w, offs_1),
        &style,
        world_to_paper,
    ));

    // Compose: shift walls and dims so they sit inside a padded SVG viewBox.
    let walls = shed_walls(world_to_paper, pad_mm, pad_mm);
    let shifted_dims: Vec<_> = dims.into_iter().map(|d| shift_dim(d, pad_mm, pad_mm)).collect();

    let printed_w = w * world_to_paper;
    let svg_size = printed_w + 2.0 * pad_mm;
    let svg = render_dimensions_svg_document(svg_size, svg_size, &walls, &shifted_dims);

    // Structural assertions
    assert!(svg.contains("15'-0\""), "overall 15'-0\" dimension present");
    assert!(svg.contains("7'-0\""), "7'-0\" dim present");
    assert!(svg.contains("7'-8 1/2\""), "7'-8 1/2\" partition dim present");
    assert!(svg.contains("4'-0\""), "4'-0\" inner-string dim present");
    assert!(
        svg.matches("<line").count() >= 50,
        "expected many <line> elements, got {}",
        svg.matches("<line").count()
    );

    let path = "/tmp/talos3d_drafting_shed_15x15_arch.svg";
    std::fs::write(path, &svg).expect("write shed SVG");
    println!("shed reference SVG written to {path}");

    // Also dump a DXF of the same drawing.
    let extent_max = (printed_w + 2.0 * pad_mm, printed_w + 2.0 * pad_mm);
    let all_drawing: Vec<DimPrimitive> = walls.into_iter().collect();
    let dxf = export_dxf(
        DxfUnit::Millimetres,
        (0.0, 0.0),
        extent_max,
        &all_drawing,
        &shifted_dims,
    );
    let dxf_path = "/tmp/talos3d_drafting_shed_15x15_arch.dxf";
    std::fs::write(dxf_path, &dxf).expect("write shed DXF");
    println!("shed reference DXF written to {dxf_path}");

    // Structural assertions on DXF
    assert!(dxf.contains("AC1027"));
    assert!(dxf.contains("A-ANNO-DIMS"));
    assert!(dxf.contains("LINE"));
    assert!(dxf.contains("TEXT"));
    assert!(dxf.contains("15'-0\""));
    assert!(dxf.contains("EOF"));
}

#[test]
fn same_shed_rendered_in_arch_metric_and_eng() {
    let w = 15.0 * FT;
    let world_to_paper = 20.0;
    let pad_mm = 40.0;

    for (preset_name, style) in [
        ("archmetric", DimensionStyle::architectural_metric()),
        ("engmm", DimensionStyle::engineering_mm()),
        ("enginch", DimensionStyle::engineering_inch()),
    ] {
        let offs = style.first_offset_mm / world_to_paper;
        let dims = vec![
            render_dimension(&horiz(0.0, w, 0.0, -offs), &style, world_to_paper),
            render_dimension(&vert(0.0, 0.0, w, -offs), &style, world_to_paper),
        ];
        let walls = shed_walls(world_to_paper, pad_mm, pad_mm);
        let shifted_dims: Vec<_> = dims.into_iter().map(|d| shift_dim(d, pad_mm, pad_mm)).collect();
        let svg = render_dimensions_svg_document(
            w * world_to_paper + 2.0 * pad_mm,
            w * world_to_paper + 2.0 * pad_mm,
            &walls,
            &shifted_dims,
        );
        let path = format!("/tmp/talos3d_drafting_shed_15x15_{preset_name}.svg");
        std::fs::write(&path, &svg).unwrap_or_else(|_| panic!("write {path}"));
        println!("{preset_name} SVG: {path}");

        // Also dump DXF for each preset.
        let dxf = export_dxf(
            DxfUnit::Millimetres,
            (0.0, 0.0),
            (w * world_to_paper + 2.0 * pad_mm, w * world_to_paper + 2.0 * pad_mm),
            &walls,
            &shifted_dims,
        );
        let dxf_path = format!("/tmp/talos3d_drafting_shed_15x15_{preset_name}.dxf");
        std::fs::write(&dxf_path, &dxf).unwrap_or_else(|_| panic!("write {dxf_path}"));
        println!("{preset_name} DXF: {dxf_path}");
    }
}

/// Sanity: vertical dim text angle is near ±π/2 so it reads upright (vertical).
#[test]
fn vertical_text_reads_upward() {
    let style = DimensionStyle::architectural_imperial();
    let input = vert(0.0, 0.0, 15.0 * FT, -1.0);
    let prims = render_dimension(&input, &style, 20.0);
    let text_angle = prims
        .iter()
        .find_map(|p| match p {
            DimPrimitive::Text { rotation_rad, .. } => Some(*rotation_rad),
            _ => None,
        })
        .expect("text primitive");
    let pi2 = PI / 2.0;
    assert!(
        (text_angle - pi2).abs() < 1e-3 || (text_angle + pi2).abs() < 1e-3,
        "text angle was {text_angle}, expected ±π/2"
    );
}
