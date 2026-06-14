//! Viewport compass rose.
//!
//! Draws a compass rose anchored to the bottom-left corner of the 3D viewport.
//! The rose is drawn **in world orientation** (lying in the world XZ plane,
//! needle pointing at geographic north) rather than as a flat screen-space
//! widget: when the camera tilts, the rose tilts with the model in true
//! perspective, so it keeps indicating azimuth from any viewing angle. A short
//! vertical post at the hub keeps the disc readable in near-edge-on views.
//!
//! Rendering uses a dedicated [`Gizmos`] config group (GPU-batched line
//! rendering, ~80 segments per frame, bounded) with a strongly negative
//! `depth_bias` so the rose stays visible on top of the model, the same
//! mechanism the selection manipulator uses.
//!
//! North follows the site convention used by domain layers
//! (`north_axis_deg`: clockwise angle from project +Z to geographic north, so
//! east is world −X in the app's top view). Domain code that knows the site
//! orientation can write it into [`CompassSettings::north_axis_deg`]; the
//! default of `0.0` keeps north on +Z.

use bevy::prelude::*;

use crate::plugins::camera::OrbitCamera;
use crate::plugins::command_registry::{
    CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
};
use crate::plugins::cursor::ViewportUiInset;
#[cfg(feature = "perf-stats")]
use crate::plugins::perf_stats::{add_gizmo_line_count, PerfStats};
use serde_json::Value;

/// Rose ring radius in logical viewport pixels.
const COMPASS_RADIUS_PX: f32 = 44.0;
/// Gap between the rose and the viewport edges, in logical pixels.
const COMPASS_MARGIN_PX: f32 = 18.0;
/// Distance in front of the camera at which the rose is anchored. The drawn
/// size compensates for this depth, so the on-screen size stays constant.
const COMPASS_ANCHOR_DEPTH: f32 = 10.0;

const RING_COLOR: Color = Color::srgba(0.85, 0.85, 0.88, 0.85);
const TICK_COLOR: Color = Color::srgba(0.85, 0.85, 0.88, 0.65);
const NORTH_COLOR: Color = Color::srgb(0.95, 0.35, 0.2);
const SOUTH_NEEDLE_COLOR: Color = Color::srgba(0.65, 0.65, 0.7, 0.8);
const LABEL_COLOR: Color = Color::srgba(0.85, 0.85, 0.88, 0.9);
const POST_COLOR: Color = Color::srgba(0.85, 0.85, 0.88, 0.7);

/// View-state settings for the compass rose. Not part of the authored model;
/// toggling it does not touch undo history (same contract as the grid toggle).
#[derive(Resource)]
pub struct CompassSettings {
    /// Whether the compass rose is drawn.
    pub enabled: bool,
    /// Clockwise angle in degrees from project +Z to geographic north.
    /// Matches the site-context `north_axis_deg` convention; domain layers
    /// that know the site orientation can set this.
    pub north_axis_deg: f32,
}

impl Default for CompassSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            north_axis_deg: 0.0,
        }
    }
}

/// Gizmo config group for the compass rose. Configured at startup with a
/// strongly negative `depth_bias` so the rose always draws on top of the
/// model, like the selection manipulator's [`HandleGizmos`].
///
/// [`HandleGizmos`]: crate::plugins::handles::HandleGizmos
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct CompassGizmos;

pub struct CompassPlugin;

impl Plugin for CompassPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CompassSettings>()
            .init_gizmo_group::<CompassGizmos>()
            .add_systems(Startup, configure_compass_gizmos)
            // Draw after transform propagation so the rose anchors to the
            // camera's final transform for this frame (no swim while orbiting).
            .add_systems(PostUpdate, draw_compass.after(TransformSystems::Propagate))
            .register_command(
                CommandDescriptor {
                    id: "view.toggle_compass".to_string(),
                    label: "Compass Rose".to_string(),
                    description: "Show or hide the viewport compass rose".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: Some("Shift+C".to_string()),
                    icon: Some("icon.compass".to_string()),
                    hint: Some(
                        "Toggle the corner compass rose that shows geographic north".to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_toggle_compass,
            );
    }
}

fn configure_compass_gizmos(mut config_store: ResMut<GizmoConfigStore>) {
    let (config, _ext) = config_store.config_mut::<CompassGizmos>();
    config.depth_bias = -1.0;
}

fn execute_toggle_compass(world: &mut World, _params: &Value) -> Result<CommandResult, String> {
    let mut settings = world.resource_mut::<CompassSettings>();
    settings.enabled = !settings.enabled;
    Ok(CommandResult::empty())
}

/// World-space horizontal direction for a compass azimuth, where `deg` is the
/// clockwise angle from project +Z (the `north_axis_deg` convention). In this
/// app's top view +Z reads up and −X reads right, so clockwise on screen is a
/// negative rotation about +Y.
fn azimuth_direction(deg: f32) -> Vec3 {
    Quat::from_rotation_y(-deg.to_radians()) * Vec3::Z
}

fn draw_compass(
    mut gizmos: Gizmos<CompassGizmos>,
    settings: Res<CompassSettings>,
    inset: Res<ViewportUiInset>,
    camera_query: Query<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    #[cfg(feature = "perf-stats")] mut perf_stats: ResMut<PerfStats>,
) {
    if !settings.enabled {
        return;
    }
    let Ok((camera, camera_transform)) = camera_query.single() else {
        return;
    };
    let Some(rect) = camera.logical_viewport_rect() else {
        return;
    };

    // Pixel centre of the rose: bottom-left of the visible 3D area (the egui
    // panels overlay the window edges; ViewportUiInset tracks what they cover).
    let extent = COMPASS_MARGIN_PX + COMPASS_RADIUS_PX;
    let centre_px = Vec2::new(
        rect.min.x + inset.left + extent,
        rect.max.y - inset.bottom - extent,
    ) - rect.min;
    if centre_px.x + COMPASS_RADIUS_PX > rect.width() || centre_px.y - COMPASS_RADIUS_PX < 0.0 {
        // Viewport too small for the rose; skip rather than overlap the UI.
        return;
    }

    // Anchor the rose a fixed distance down the camera ray through that pixel,
    // and measure the world size of one pixel at that depth so the rose keeps
    // a constant on-screen size in both perspective and orthographic modes.
    let Ok(centre_ray) = camera.viewport_to_world(camera_transform, centre_px) else {
        return;
    };
    let Ok(offset_ray) = camera.viewport_to_world(camera_transform, centre_px + Vec2::X) else {
        return;
    };
    let centre = centre_ray.get_point(COMPASS_ANCHOR_DEPTH);
    let world_per_px = (offset_ray.get_point(COMPASS_ANCHOR_DEPTH) - centre).length();
    if !world_per_px.is_finite() || world_per_px <= f32::EPSILON {
        return;
    }
    let radius = COMPASS_RADIUS_PX * world_per_px;

    #[cfg(feature = "perf-stats")]
    let mut line_count = 0usize;

    // Ring, lying flat in the world XZ plane.
    const RING_RESOLUTION: u32 = 48;
    gizmos
        .circle(
            Isometry3d::new(centre, Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
            radius,
            RING_COLOR,
        )
        .resolution(RING_RESOLUTION);
    #[cfg(feature = "perf-stats")]
    {
        line_count += RING_RESOLUTION as usize;
    }

    // Cardinal (long) and intercardinal (short) ticks.
    for octant in 0..8 {
        let dir = azimuth_direction(settings.north_axis_deg + octant as f32 * 45.0);
        let inner = if octant % 2 == 0 { 0.82 } else { 0.92 };
        gizmos.line(
            centre + dir * (radius * inner),
            centre + dir * radius,
            TICK_COLOR,
        );
        #[cfg(feature = "perf-stats")]
        {
            line_count += 1;
        }
    }

    // Needle: red north kite, grey south tail, drawn as outlines through the hub.
    let north = azimuth_direction(settings.north_axis_deg);
    let east = azimuth_direction(settings.north_axis_deg + 90.0);
    let north_tip = centre + north * (radius * 0.75);
    let south_tip = centre - north * (radius * 0.45);
    let east_base = centre + east * (radius * 0.10);
    let west_base = centre - east * (radius * 0.10);
    // Shallow apex above the hub gives the needle a 3D body, so it keeps a
    // visible slope toward north when the dial is seen nearly edge-on.
    let apex = centre + Vec3::Y * (radius * 0.12);
    for (a, b, color) in [
        (north_tip, east_base, NORTH_COLOR),
        (north_tip, west_base, NORTH_COLOR),
        (north_tip, apex, NORTH_COLOR),
        (south_tip, east_base, SOUTH_NEEDLE_COLOR),
        (south_tip, west_base, SOUTH_NEEDLE_COLOR),
        (south_tip, apex, SOUTH_NEEDLE_COLOR),
        (east_base, west_base, SOUTH_NEEDLE_COLOR),
    ] {
        gizmos.line(a, b, color);
        #[cfg(feature = "perf-stats")]
        {
            line_count += 1;
        }
    }

    // Vertical post at the hub and a vertical red fin at the north rim
    // (sundial-gnomon style): they keep the rose readable, and north
    // locatable, when the camera is close to edge-on with the rose plane.
    gizmos.line(centre, centre + Vec3::Y * (radius * 0.4), POST_COLOR);
    let fin_outer = centre + north * radius;
    let fin_inner = centre + north * (radius * 0.7);
    let fin_top = centre + north * (radius * 0.85) + Vec3::Y * (radius * 0.25);
    for (a, b) in [(fin_outer, fin_top), (fin_inner, fin_top)] {
        gizmos.line(a, b, NORTH_COLOR);
    }
    #[cfg(feature = "perf-stats")]
    {
        line_count += 3;
    }

    // Cardinal letters just outside the ring, lying in the rose plane with
    // their tops pointing outward (classic rose layout).
    let letter_size = radius * 0.34;
    for (label, azimuth, color) in [
        (CARDINAL_N, 0.0, NORTH_COLOR),
        (CARDINAL_E, 90.0, LABEL_COLOR),
        (CARDINAL_S, 180.0, LABEL_COLOR),
        (CARDINAL_W, 270.0, LABEL_COLOR),
    ] {
        let outward = azimuth_direction(settings.north_axis_deg + azimuth);
        // 90° clockwise from "up = outward", so the glyph reads upright.
        let right = azimuth_direction(settings.north_axis_deg + azimuth + 90.0);
        let origin = centre + outward * (radius * 1.22);
        for (from, to) in label {
            gizmos.line(
                origin + right * (from.x * letter_size) + outward * (from.y * letter_size),
                origin + right * (to.x * letter_size) + outward * (to.y * letter_size),
                color,
            );
            #[cfg(feature = "perf-stats")]
            {
                line_count += 1;
            }
        }
    }

    #[cfg(feature = "perf-stats")]
    add_gizmo_line_count(&mut perf_stats, line_count);
}

/// Cardinal letter glyphs as line segments in a [-0.5, 0.5]² box
/// (x = letter right, y = letter up).
const CARDINAL_N: &[(Vec2, Vec2)] = &[
    (Vec2::new(-0.35, -0.5), Vec2::new(-0.35, 0.5)),
    (Vec2::new(-0.35, 0.5), Vec2::new(0.35, -0.5)),
    (Vec2::new(0.35, -0.5), Vec2::new(0.35, 0.5)),
];
const CARDINAL_E: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.3, 0.5), Vec2::new(-0.3, 0.5)),
    (Vec2::new(-0.3, 0.5), Vec2::new(-0.3, -0.5)),
    (Vec2::new(-0.3, -0.5), Vec2::new(0.3, -0.5)),
    (Vec2::new(-0.3, 0.0), Vec2::new(0.2, 0.0)),
];
const CARDINAL_S: &[(Vec2, Vec2)] = &[
    (Vec2::new(0.3, 0.5), Vec2::new(-0.3, 0.5)),
    (Vec2::new(-0.3, 0.5), Vec2::new(-0.3, 0.0)),
    (Vec2::new(-0.3, 0.0), Vec2::new(0.3, 0.0)),
    (Vec2::new(0.3, 0.0), Vec2::new(0.3, -0.5)),
    (Vec2::new(0.3, -0.5), Vec2::new(-0.3, -0.5)),
];
const CARDINAL_W: &[(Vec2, Vec2)] = &[
    (Vec2::new(-0.4, 0.5), Vec2::new(-0.2, -0.5)),
    (Vec2::new(-0.2, -0.5), Vec2::new(0.0, 0.2)),
    (Vec2::new(0.0, 0.2), Vec2::new(0.2, -0.5)),
    (Vec2::new(0.2, -0.5), Vec2::new(0.4, 0.5)),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_dir(deg: f32, expected: Vec3) {
        let dir = azimuth_direction(deg);
        assert!(
            dir.abs_diff_eq(expected, 1e-5),
            "azimuth {deg}° → {dir:?}, expected {expected:?}"
        );
    }

    /// `north_axis_deg` is clockwise from +Z in the top view, where +Z reads
    /// up and −X reads right (east).
    #[test]
    fn azimuth_direction_follows_site_convention() {
        assert_dir(0.0, Vec3::Z); // north
        assert_dir(90.0, Vec3::NEG_X); // east
        assert_dir(180.0, Vec3::NEG_Z); // south
        assert_dir(270.0, Vec3::X); // west
    }

    #[test]
    fn toggle_compass_flips_enabled_without_touching_north() {
        let mut world = World::new();
        world.insert_resource(CompassSettings {
            enabled: true,
            north_axis_deg: 33.0,
        });

        execute_toggle_compass(&mut world, &Value::Null).unwrap();
        let settings = world.resource::<CompassSettings>();
        assert!(!settings.enabled);
        assert_eq!(settings.north_axis_deg, 33.0);

        execute_toggle_compass(&mut world, &Value::Null).unwrap();
        assert!(world.resource::<CompassSettings>().enabled);
    }

    #[test]
    fn cardinal_glyphs_stay_inside_their_unit_box() {
        for glyph in [CARDINAL_N, CARDINAL_E, CARDINAL_S, CARDINAL_W] {
            for (from, to) in glyph {
                for point in [from, to] {
                    assert!(point.x.abs() <= 0.5 && point.y.abs() <= 0.5);
                }
            }
        }
    }
}
