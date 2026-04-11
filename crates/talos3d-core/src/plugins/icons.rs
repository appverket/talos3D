use std::f32::consts::TAU;

const SIZE: usize = 24;
const STROKE_COLOR: [u8; 3] = [220, 225, 235];
// Dark background matching toolbar CHROME_BG
const BG_COLOR: [u8; 4] = [20, 23, 28, 255];

struct Canvas {
    pixels: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        let mut pixels = vec![0u8; SIZE * SIZE * 4];
        for i in 0..SIZE * SIZE {
            let idx = i * 4;
            pixels[idx] = BG_COLOR[0];
            pixels[idx + 1] = BG_COLOR[1];
            pixels[idx + 2] = BG_COLOR[2];
            pixels[idx + 3] = BG_COLOR[3];
        }
        Self { pixels }
    }

    fn into_rgba(self) -> Vec<u8> {
        self.pixels
    }

    fn set(&mut self, x: i32, y: i32, alpha: u8) {
        if x < 0 || y < 0 || x >= SIZE as i32 || y >= SIZE as i32 {
            return;
        }
        let idx = (y as usize * SIZE + x as usize) * 4;
        let a = alpha as f32 / 255.0;
        let inv = 1.0 - a;
        self.pixels[idx] = (STROKE_COLOR[0] as f32 * a + self.pixels[idx] as f32 * inv) as u8;
        self.pixels[idx + 1] =
            (STROKE_COLOR[1] as f32 * a + self.pixels[idx + 1] as f32 * inv) as u8;
        self.pixels[idx + 2] =
            (STROKE_COLOR[2] as f32 * a + self.pixels[idx + 2] as f32 * inv) as u8;
        self.pixels[idx + 3] = 255;
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        self.thick_line(x0, y0, x1, y1, 1.8);
    }

    fn thick_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt().max(0.01);
        let steps = (len * 3.0) as usize + 1;
        let half = thickness / 2.0;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let cx = x0 + dx * t;
            let cy = y0 + dy * t;
            for py in (cy - half - 1.0) as i32..=(cy + half + 1.0) as i32 {
                for px in (cx - half - 1.0) as i32..=(cx + half + 1.0) as i32 {
                    let dist =
                        ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
                    if dist <= half + 0.5 {
                        let coverage = (half + 0.5 - dist).clamp(0.0, 1.0);
                        self.set(px, py, (255.0 * coverage) as u8);
                    }
                }
            }
        }
    }

    fn arc(&mut self, cx: f32, cy: f32, r: f32, start_angle: f32, end_angle: f32) {
        let sweep = end_angle - start_angle;
        let steps = ((sweep.abs() * r * 2.0) as usize).max(16);
        for i in 0..steps {
            let t0 = i as f32 / steps as f32;
            let t1 = (i + 1) as f32 / steps as f32;
            let a0 = start_angle + sweep * t0;
            let a1 = start_angle + sweep * t1;
            self.line(
                cx + r * a0.cos(),
                cy + r * a0.sin(),
                cx + r * a1.cos(),
                cy + r * a1.sin(),
            );
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32) {
        self.arc(cx, cy, r, 0.0, TAU);
    }

    fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let r = r.min(w / 2.0).min(h / 2.0);
        self.line(x + r, y, x + w - r, y);
        self.line(x + w, y + r, x + w, y + h - r);
        self.line(x + w - r, y + h, x + r, y + h);
        self.line(x, y + h - r, x, y + r);
        self.arc_corner(x + w - r, y + r, r, -TAU / 4.0, 0.0);
        self.arc_corner(x + w - r, y + h - r, r, 0.0, TAU / 4.0);
        self.arc_corner(x + r, y + h - r, r, TAU / 4.0, TAU / 2.0);
        self.arc_corner(x + r, y + r, r, TAU / 2.0, TAU * 3.0 / 4.0);
    }

    fn arc_corner(&mut self, cx: f32, cy: f32, r: f32, start: f32, end: f32) {
        let steps = 8;
        let sweep = end - start;
        for i in 0..steps {
            let t0 = i as f32 / steps as f32;
            let t1 = (i + 1) as f32 / steps as f32;
            let a0 = start + sweep * t0;
            let a1 = start + sweep * t1;
            self.line(
                cx + r * a0.cos(),
                cy + r * a0.sin(),
                cx + r * a1.cos(),
                cy + r * a1.sin(),
            );
        }
    }
}

// Lucide: mouse-pointer-2
fn draw_mouse_pointer(c: &mut Canvas) {
    // Simplified cursor arrow
    c.line(5.0, 4.0, 5.0, 18.0);
    c.line(5.0, 4.0, 17.0, 13.0);
    c.line(5.0, 18.0, 10.0, 14.0);
    c.line(10.0, 14.0, 14.0, 20.0);
    c.line(14.0, 20.0, 16.5, 19.0);
    c.line(16.5, 19.0, 12.5, 13.0);
    c.line(12.5, 13.0, 17.0, 13.0);
}

// Lucide: undo-2
fn draw_undo(c: &mut Canvas) {
    // Arrow pointing left
    c.line(9.0, 14.0, 4.0, 9.0);
    c.line(4.0, 9.0, 9.0, 4.0);
    // Curved path
    c.line(4.0, 9.0, 14.5, 9.0);
    c.arc(14.5, 14.5, 5.5, -TAU / 4.0, TAU / 4.0);
    c.line(14.5, 20.0, 11.0, 20.0);
}

// Lucide: redo-2
fn draw_redo(c: &mut Canvas) {
    c.line(15.0, 14.0, 20.0, 9.0);
    c.line(20.0, 9.0, 15.0, 4.0);
    c.line(20.0, 9.0, 9.5, 9.0);
    c.arc(9.5, 14.5, 5.5, -TAU * 3.0 / 4.0, -TAU / 4.0);
    c.line(9.5, 20.0, 13.0, 20.0);
}

// Lucide: trash-2
fn draw_trash(c: &mut Canvas) {
    c.line(3.0, 6.0, 21.0, 6.0);
    c.rounded_rect(5.0, 6.0, 14.0, 14.0, 2.0);
    c.line(10.0, 11.0, 10.0, 17.0);
    c.line(14.0, 11.0, 14.0, 17.0);
    // Lid handle
    c.line(8.0, 6.0, 8.0, 4.0);
    c.line(8.0, 4.0, 16.0, 4.0);
    c.line(16.0, 4.0, 16.0, 6.0);
}

// Lucide: move (4-directional arrows)
fn draw_move(c: &mut Canvas) {
    c.line(12.0, 2.0, 12.0, 22.0);
    c.line(2.0, 12.0, 22.0, 12.0);
    // Top arrow
    c.line(9.0, 5.0, 12.0, 2.0);
    c.line(15.0, 5.0, 12.0, 2.0);
    // Bottom arrow
    c.line(9.0, 19.0, 12.0, 22.0);
    c.line(15.0, 19.0, 12.0, 22.0);
    // Left arrow
    c.line(5.0, 9.0, 2.0, 12.0);
    c.line(5.0, 15.0, 2.0, 12.0);
    // Right arrow
    c.line(19.0, 9.0, 22.0, 12.0);
    c.line(19.0, 15.0, 22.0, 12.0);
}

// Lucide: rotate-cw
fn draw_rotate(c: &mut Canvas) {
    c.arc(12.0, 12.0, 9.0, TAU * 0.6, TAU * 1.5);
    // Arrow at top-right
    c.line(21.0, 3.0, 21.0, 8.0);
    c.line(21.0, 8.0, 16.0, 8.0);
}

// Lucide: scaling
fn draw_scale(c: &mut Canvas) {
    // Outer rounded rect (bottom-left portion)
    c.line(12.0, 3.0, 5.0, 3.0);
    c.arc_corner(5.0, 5.0, 2.0, TAU / 2.0, TAU * 3.0 / 4.0);
    c.line(3.0, 5.0, 3.0, 19.0);
    c.arc_corner(5.0, 19.0, 2.0, TAU / 4.0, TAU / 2.0);
    c.line(5.0, 21.0, 19.0, 21.0);
    c.arc_corner(19.0, 19.0, 2.0, 0.0, TAU / 4.0);
    c.line(21.0, 19.0, 21.0, 12.0);
    // Diagonal line
    c.line(21.0, 3.0, 9.0, 15.0);
    // Top-right corner bracket
    c.line(16.0, 3.0, 21.0, 3.0);
    c.line(21.0, 3.0, 21.0, 8.0);
    // Bottom-left corner bracket
    c.line(14.0, 15.0, 9.0, 15.0);
    c.line(9.0, 15.0, 9.0, 10.0);
}

// Lucide: save (floppy disk)
fn draw_save(c: &mut Canvas) {
    // Main body
    c.line(5.0, 3.0, 15.0, 3.0);
    c.line(15.0, 3.0, 21.0, 9.0);
    c.line(21.0, 9.0, 21.0, 19.0);
    c.arc_corner(19.0, 19.0, 2.0, 0.0, TAU / 4.0);
    c.line(19.0, 21.0, 5.0, 21.0);
    c.arc_corner(5.0, 19.0, 2.0, TAU / 4.0, TAU / 2.0);
    c.line(3.0, 19.0, 3.0, 5.0);
    c.arc_corner(5.0, 5.0, 2.0, TAU / 2.0, TAU * 3.0 / 4.0);
    // Bottom panel
    c.line(7.0, 21.0, 7.0, 14.0);
    c.line(7.0, 14.0, 17.0, 14.0);
    c.line(17.0, 14.0, 17.0, 21.0);
    // Top panel
    c.line(7.0, 3.0, 7.0, 8.0);
    c.line(7.0, 8.0, 15.0, 8.0);
}

// Lucide: folder-open
fn draw_folder_open(c: &mut Canvas) {
    // Back of folder
    c.line(2.0, 19.0, 2.0, 5.0);
    c.arc_corner(4.0, 5.0, 2.0, TAU / 2.0, TAU * 3.0 / 4.0);
    c.line(4.0, 3.0, 7.9, 3.0);
    c.line(7.9, 3.0, 10.3, 6.0);
    c.line(10.3, 6.0, 18.0, 6.0);
    c.arc_corner(18.0, 8.0, 2.0, -TAU / 4.0, 0.0);
    c.line(20.0, 8.0, 20.0, 10.0);
    // Front flap
    c.line(6.0, 14.0, 9.2, 10.0);
    c.line(9.2, 10.0, 20.0, 10.0);
    c.line(20.0, 10.0, 22.0, 12.5);
    c.line(22.0, 12.5, 20.5, 18.5);
    c.line(20.5, 18.5, 18.5, 20.0);
    c.line(18.5, 20.0, 4.0, 20.0);
    c.arc_corner(4.0, 18.0, 2.0, TAU / 4.0, TAU / 2.0);
}

// Lucide: plus
fn draw_plus(c: &mut Canvas) {
    c.line(5.0, 12.0, 19.0, 12.0);
    c.line(12.0, 5.0, 12.0, 19.0);
}

// Lucide: scan (corner brackets - zoom/view)
fn draw_scan(c: &mut Canvas) {
    // Top-left corner
    c.line(3.0, 7.0, 3.0, 5.0);
    c.arc_corner(5.0, 5.0, 2.0, TAU / 2.0, TAU * 3.0 / 4.0);
    c.line(5.0, 3.0, 7.0, 3.0);
    // Top-right corner
    c.line(17.0, 3.0, 19.0, 3.0);
    c.arc_corner(19.0, 5.0, 2.0, -TAU / 4.0, 0.0);
    c.line(21.0, 5.0, 21.0, 7.0);
    // Bottom-right corner
    c.line(21.0, 17.0, 21.0, 19.0);
    c.arc_corner(19.0, 19.0, 2.0, 0.0, TAU / 4.0);
    c.line(19.0, 21.0, 17.0, 21.0);
    // Bottom-left corner
    c.line(7.0, 21.0, 5.0, 21.0);
    c.arc_corner(5.0, 19.0, 2.0, TAU / 4.0, TAU / 2.0);
    c.line(3.0, 19.0, 3.0, 17.0);
}

// Lucide: file-plus (new document)
fn draw_file_plus(c: &mut Canvas) {
    // File outline
    c.line(6.0, 22.0, 6.0, 4.0);
    c.arc_corner(6.0, 4.0, 2.0, TAU / 2.0, TAU * 3.0 / 4.0);
    c.line(6.0, 2.0, 14.0, 2.0);
    c.line(14.0, 2.0, 20.0, 8.0);
    c.line(20.0, 8.0, 20.0, 20.0);
    c.arc_corner(18.0, 20.0, 2.0, 0.0, TAU / 4.0);
    c.line(18.0, 22.0, 6.0, 22.0);
    // Page fold
    c.line(14.0, 2.0, 14.0, 7.0);
    c.line(14.0, 8.0, 20.0, 8.0);
    // Plus inside
    c.line(9.0, 15.0, 17.0, 15.0);
    c.line(13.0, 12.0, 13.0, 18.0);
}

// Lucide: import (arrow into box)
fn draw_import(c: &mut Canvas) {
    // Arrow down
    c.line(12.0, 3.0, 12.0, 15.0);
    c.line(8.0, 11.0, 12.0, 15.0);
    c.line(16.0, 11.0, 12.0, 15.0);
    // Box
    c.line(8.0, 5.0, 4.0, 5.0);
    c.arc_corner(4.0, 7.0, 2.0, TAU / 2.0, TAU * 3.0 / 4.0);
    c.line(2.0, 7.0, 2.0, 17.0);
    c.arc_corner(4.0, 17.0, 2.0, TAU / 4.0, TAU / 2.0);
    c.line(4.0, 19.0, 20.0, 19.0);
    c.arc_corner(20.0, 17.0, 2.0, 0.0, TAU / 4.0);
    c.line(22.0, 17.0, 22.0, 7.0);
    c.arc_corner(20.0, 7.0, 2.0, -TAU / 4.0, 0.0);
    c.line(20.0, 5.0, 16.0, 5.0);
}

// Lucide: box-select (select all - dashed corners)
fn draw_box_select(c: &mut Canvas) {
    // Corner arcs
    c.arc_corner(5.0, 5.0, 2.0, TAU / 2.0, TAU * 3.0 / 4.0);
    c.arc_corner(19.0, 5.0, 2.0, -TAU / 4.0, 0.0);
    c.arc_corner(19.0, 19.0, 2.0, 0.0, TAU / 4.0);
    c.arc_corner(5.0, 19.0, 2.0, TAU / 4.0, TAU / 2.0);
    // Dashed edges - top/bottom
    c.line(9.0, 3.0, 10.0, 3.0);
    c.line(14.0, 3.0, 15.0, 3.0);
    c.line(9.0, 21.0, 10.0, 21.0);
    c.line(14.0, 21.0, 15.0, 21.0);
    // Dashed edges - left/right
    c.line(3.0, 9.0, 3.0, 10.0);
    c.line(3.0, 14.0, 3.0, 15.0);
    c.line(21.0, 9.0, 21.0, 10.0);
    c.line(21.0, 14.0, 21.0, 15.0);
}

// Lucide: circle-off (deselect)
fn draw_deselect(c: &mut Canvas) {
    c.arc(12.0, 12.0, 9.0, 0.35, TAU * 0.37);
    c.arc(12.0, 12.0, 9.0, TAU * 0.39, TAU * 0.87);
    c.line(2.0, 2.0, 22.0, 22.0);
}

// Lucide: crosshair (set pivot)
fn draw_crosshair(c: &mut Canvas) {
    c.circle(12.0, 12.0, 10.0);
    c.line(22.0, 12.0, 18.0, 12.0);
    c.line(6.0, 12.0, 2.0, 12.0);
    c.line(12.0, 6.0, 12.0, 2.0);
    c.line(12.0, 22.0, 12.0, 18.0);
}

// Zoom to extents - scan with expand arrows
fn draw_zoom_extents(c: &mut Canvas) {
    // Corner brackets (scan)
    c.line(3.0, 8.0, 3.0, 5.0);
    c.line(3.0, 5.0, 5.0, 3.0);
    c.line(5.0, 3.0, 8.0, 3.0);
    c.line(16.0, 3.0, 19.0, 3.0);
    c.line(19.0, 3.0, 21.0, 5.0);
    c.line(21.0, 5.0, 21.0, 8.0);
    c.line(21.0, 16.0, 21.0, 19.0);
    c.line(21.0, 19.0, 19.0, 21.0);
    c.line(19.0, 21.0, 16.0, 21.0);
    c.line(8.0, 21.0, 5.0, 21.0);
    c.line(5.0, 21.0, 3.0, 19.0);
    c.line(3.0, 19.0, 3.0, 16.0);
    // Inner expand arrows
    c.line(8.0, 8.0, 16.0, 16.0);
    c.line(8.0, 8.0, 8.0, 12.0);
    c.line(8.0, 8.0, 12.0, 8.0);
    c.line(16.0, 16.0, 16.0, 12.0);
    c.line(16.0, 16.0, 12.0, 16.0);
}

// Zoom to selection - scan with center focus dot
fn draw_zoom_selection(c: &mut Canvas) {
    draw_scan(c);
    // Center target dot
    c.circle(12.0, 12.0, 2.5);
}

// Lucide: group (overlapping rects in brackets)
fn draw_group(c: &mut Canvas) {
    // Corner brackets
    c.line(3.0, 7.0, 3.0, 5.0);
    c.line(3.0, 5.0, 5.0, 3.0);
    c.line(5.0, 3.0, 7.0, 3.0);
    c.line(17.0, 3.0, 19.0, 3.0);
    c.line(19.0, 3.0, 21.0, 5.0);
    c.line(21.0, 5.0, 21.0, 7.0);
    c.line(21.0, 17.0, 21.0, 19.0);
    c.line(21.0, 19.0, 19.0, 21.0);
    c.line(19.0, 21.0, 17.0, 21.0);
    c.line(7.0, 21.0, 5.0, 21.0);
    c.line(5.0, 21.0, 3.0, 19.0);
    c.line(3.0, 19.0, 3.0, 17.0);
    // Two overlapping rects
    c.rounded_rect(7.0, 7.0, 7.0, 5.0, 1.0);
    c.rounded_rect(10.0, 12.0, 7.0, 5.0, 1.0);
}

// Lucide: ungroup (separated rects)
fn draw_ungroup(c: &mut Canvas) {
    c.rounded_rect(5.0, 4.0, 8.0, 6.0, 1.0);
    c.rounded_rect(11.0, 14.0, 8.0, 6.0, 1.0);
}

// Create box - Lucide: box
fn draw_create_box(c: &mut Canvas) {
    // 3D box
    c.line(12.0, 2.0, 21.0, 7.0);
    c.line(21.0, 7.0, 21.0, 17.0);
    c.line(21.0, 17.0, 12.0, 22.0);
    c.line(12.0, 22.0, 3.0, 17.0);
    c.line(3.0, 17.0, 3.0, 7.0);
    c.line(3.0, 7.0, 12.0, 2.0);
    c.line(3.0, 7.0, 12.0, 12.0);
    c.line(12.0, 12.0, 21.0, 7.0);
    c.line(12.0, 12.0, 12.0, 22.0);
}

// Create cylinder - Lucide: cylinder
fn draw_create_cylinder(c: &mut Canvas) {
    let cx = 12.0;
    let rx = 8.0;
    let ry = 3.0;
    // Top ellipse (full)
    draw_ellipse(c, cx, 7.0, rx, ry, 0.0, TAU);
    // Bottom ellipse (lower half only)
    draw_ellipse(c, cx, 17.0, rx, ry, 0.0, TAU * 0.5);
    // Side lines
    c.line(cx - rx, 7.0, cx - rx, 17.0);
    c.line(cx + rx, 7.0, cx + rx, 17.0);
}

// Create sphere - simple shaded globe outline
fn draw_create_sphere(c: &mut Canvas) {
    c.circle(12.0, 12.0, 9.0);
    draw_ellipse(c, 12.0, 12.0, 9.0, 4.0, 0.0, TAU);
    c.line(12.0, 3.0, 12.0, 21.0);
    draw_ellipse(
        c,
        12.0,
        12.0,
        5.0,
        9.0,
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
    );
    draw_ellipse(
        c,
        12.0,
        12.0,
        5.0,
        9.0,
        std::f32::consts::FRAC_PI_2 * 3.0,
        std::f32::consts::PI,
    );
}

fn draw_ellipse(c: &mut Canvas, cx: f32, cy: f32, rx: f32, ry: f32, start: f32, sweep: f32) {
    let steps = 32;
    for i in 0..steps {
        let t0 = start + sweep * i as f32 / steps as f32;
        let t1 = start + sweep * (i + 1) as f32 / steps as f32;
        c.line(
            cx + rx * t0.cos(),
            cy + ry * t0.sin(),
            cx + rx * t1.cos(),
            cy + ry * t1.sin(),
        );
    }
}

// Create plane - Lucide: square
fn draw_create_plane(c: &mut Canvas) {
    // Perspective plane
    c.line(5.0, 8.0, 19.0, 5.0);
    c.line(19.0, 5.0, 21.0, 16.0);
    c.line(21.0, 16.0, 5.0, 19.0);
    c.line(5.0, 19.0, 5.0, 8.0);
}

// Create polyline - Lucide: spline/pen-line
fn draw_create_polyline(c: &mut Canvas) {
    c.line(4.0, 19.0, 8.0, 10.0);
    c.line(8.0, 10.0, 14.0, 16.0);
    c.line(14.0, 16.0, 20.0, 5.0);
    // Dots at vertices
    c.circle(4.0, 19.0, 1.5);
    c.circle(8.0, 10.0, 1.5);
    c.circle(14.0, 16.0, 1.5);
    c.circle(20.0, 5.0, 1.5);
}

fn draw_view_perspective(c: &mut Canvas) {
    draw_create_box(c);
}

fn draw_view_orthographic(c: &mut Canvas) {
    c.rounded_rect(4.0, 4.0, 16.0, 16.0, 1.5);
    c.line(8.0, 4.0, 8.0, 20.0);
    c.line(4.0, 8.0, 20.0, 8.0);
}

fn draw_view_isometric(c: &mut Canvas) {
    draw_create_box(c);
}

fn draw_view_front(c: &mut Canvas) {
    c.rounded_rect(5.0, 5.0, 14.0, 14.0, 1.5);
    c.line(3.0, 20.0, 21.0, 20.0);
    c.line(12.0, 8.0, 12.0, 16.0);
}

fn draw_view_back(c: &mut Canvas) {
    c.rounded_rect(5.0, 5.0, 14.0, 14.0, 1.5);
    c.line(3.0, 4.0, 21.0, 4.0);
    c.line(12.0, 8.0, 12.0, 16.0);
}

fn draw_view_top(c: &mut Canvas) {
    c.rounded_rect(5.0, 7.0, 14.0, 12.0, 1.5);
    c.line(12.0, 3.0, 12.0, 9.0);
    c.line(9.0, 6.0, 12.0, 3.0);
    c.line(15.0, 6.0, 12.0, 3.0);
}

fn draw_view_bottom(c: &mut Canvas) {
    c.rounded_rect(5.0, 5.0, 14.0, 12.0, 1.5);
    c.line(12.0, 15.0, 12.0, 21.0);
    c.line(9.0, 18.0, 12.0, 21.0);
    c.line(15.0, 18.0, 12.0, 21.0);
}

fn draw_view_left(c: &mut Canvas) {
    c.rounded_rect(7.0, 5.0, 12.0, 14.0, 1.5);
    c.line(3.0, 12.0, 9.0, 12.0);
    c.line(6.0, 9.0, 3.0, 12.0);
    c.line(6.0, 15.0, 3.0, 12.0);
}

fn draw_view_right(c: &mut Canvas) {
    c.rounded_rect(5.0, 5.0, 12.0, 14.0, 1.5);
    c.line(15.0, 12.0, 21.0, 12.0);
    c.line(18.0, 9.0, 21.0, 12.0);
    c.line(18.0, 15.0, 21.0, 12.0);
}

fn draw_view_wireframe(c: &mut Canvas) {
    draw_create_box(c);
}

fn draw_view_outline(c: &mut Canvas) {
    c.rounded_rect(4.0, 5.0, 16.0, 14.0, 1.5);
    c.line(7.0, 15.0, 17.0, 9.0);
}

fn draw_view_grid(c: &mut Canvas) {
    for x in [6.0, 11.0, 16.0] {
        c.line(x, 4.0, x, 20.0);
    }
    for y in [6.0, 11.0, 16.0] {
        c.line(4.0, y, 20.0, y);
    }
    c.rounded_rect(4.0, 4.0, 16.0, 16.0, 1.5);
}

fn draw_view_paper(c: &mut Canvas) {
    c.line(6.0, 3.0, 15.0, 3.0);
    c.line(15.0, 3.0, 20.0, 8.0);
    c.line(20.0, 8.0, 20.0, 21.0);
    c.line(20.0, 21.0, 6.0, 21.0);
    c.line(6.0, 21.0, 6.0, 3.0);
    c.line(15.0, 3.0, 15.0, 8.0);
    c.line(15.0, 8.0, 20.0, 8.0);
    c.line(9.0, 11.0, 17.0, 11.0);
    c.line(9.0, 15.0, 17.0, 15.0);
}

// Wall: vertical rectangle with hatching
fn draw_wall(c: &mut Canvas) {
    c.rounded_rect(6.0, 3.0, 12.0, 18.0, 1.5);
    // Brick lines
    c.line(6.0, 7.0, 18.0, 7.0);
    c.line(6.0, 11.0, 18.0, 11.0);
    c.line(6.0, 15.0, 18.0, 15.0);
    // Offset vertical lines for brick pattern
    c.line(12.0, 3.0, 12.0, 7.0);
    c.line(9.0, 7.0, 9.0, 11.0);
    c.line(15.0, 7.0, 15.0, 11.0);
    c.line(12.0, 11.0, 12.0, 15.0);
    c.line(9.0, 15.0, 9.0, 21.0);
    c.line(15.0, 15.0, 15.0, 21.0);
}

// Opening: archway/door shape
fn draw_opening(c: &mut Canvas) {
    // Door frame
    c.line(5.0, 21.0, 5.0, 7.0);
    c.arc(12.0, 7.0, 7.0, std::f32::consts::PI, TAU);
    c.line(19.0, 7.0, 19.0, 21.0);
    c.line(5.0, 21.0, 19.0, 21.0);
    // Threshold
    c.line(3.0, 21.0, 21.0, 21.0);
}

pub fn render_icon(name: &str) -> Vec<u8> {
    let mut c = Canvas::new();
    match name {
        "mouse_pointer" => draw_mouse_pointer(&mut c),
        "undo" => draw_undo(&mut c),
        "redo" => draw_redo(&mut c),
        "trash" => draw_trash(&mut c),
        "move" => draw_move(&mut c),
        "rotate" => draw_rotate(&mut c),
        "scale" => draw_scale(&mut c),
        "save" => draw_save(&mut c),
        "folder_open" => draw_folder_open(&mut c),
        "plus" => draw_plus(&mut c),
        "scan" => draw_scan(&mut c),
        "file_plus" => draw_file_plus(&mut c),
        "import" => draw_import(&mut c),
        "box_select" => draw_box_select(&mut c),
        "deselect" => draw_deselect(&mut c),
        "crosshair" => draw_crosshair(&mut c),
        "zoom_extents" => draw_zoom_extents(&mut c),
        "zoom_selection" => draw_zoom_selection(&mut c),
        "group" => draw_group(&mut c),
        "ungroup" => draw_ungroup(&mut c),
        "create_box" => draw_create_box(&mut c),
        "create_cylinder" => draw_create_cylinder(&mut c),
        "create_sphere" => draw_create_sphere(&mut c),
        "create_plane" => draw_create_plane(&mut c),
        "create_polyline" => draw_create_polyline(&mut c),
        "view_perspective" => draw_view_perspective(&mut c),
        "view_orthographic" => draw_view_orthographic(&mut c),
        "view_isometric" => draw_view_isometric(&mut c),
        "view_front" => draw_view_front(&mut c),
        "view_back" => draw_view_back(&mut c),
        "view_top" => draw_view_top(&mut c),
        "view_bottom" => draw_view_bottom(&mut c),
        "view_left" => draw_view_left(&mut c),
        "view_right" => draw_view_right(&mut c),
        "view_wireframe" => draw_view_wireframe(&mut c),
        "view_outline" => draw_view_outline(&mut c),
        "view_grid" => draw_view_grid(&mut c),
        "view_paper" => draw_view_paper(&mut c),
        "wall" => draw_wall(&mut c),
        "opening" => draw_opening(&mut c),
        _ => draw_plus(&mut c),
    }
    c.into_rgba()
}

pub const ICON_SIZE: u32 = SIZE as u32;
