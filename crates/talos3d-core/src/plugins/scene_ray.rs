//! Shared scene ray-casting utilities.
//!
//! Provides `build_camera_ray` and `ray_cast_nearest_face` so that any tool
//! (face editing, line tool, etc.) can find geometry under the cursor without
//! duplicating ray-cast boilerplate. [`PickApertures`] holds how close the
//! cursor must come to a target for the pointing tools that measure in screen
//! space rather than by ray-cast.

use bevy::{prelude::*, window::PrimaryWindow};

use crate::capability_registry::{CapabilityRegistry, FaceHitCandidate, HitCandidate};
use crate::plugins::camera::OrbitCamera;
use crate::plugins::commands::find_entity_by_element_id_readonly;
use crate::plugins::cursor::cursor_window_position;
use crate::plugins::layers::entity_on_visible_layer;
use crate::plugins::modeling::occurrence::GeneratedOccurrencePart;
use crate::plugins::render_pipeline::WireframeSurfaceVisibilityOverride;

/// How close the cursor must come to a target, in logical pixels, for the
/// tools that pick by screen distance rather than by ray-cast.
///
/// A pick aperture belongs in pixels, not in model units: the cursor's
/// precision is a property of the pointing device and the screen, so the same
/// aperture must mean the same aiming effort whether the viewport spans a city
/// block or a mortise. This is what `PICKBOX` and `APERTURE` are in AutoCAD,
/// and like them these are settings rather than constants, so a user working
/// at a trackpad and one working at a mouse are not forced onto the same
/// tolerance.
///
/// In model units an aperture is worth `pixels x metres-per-pixel`, so it
/// tightens as the view zooms in — which is the point.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct PickApertures {
    /// Manipulator grips. Generous: a grip is a deliberate, isolated target
    /// and missing one costs a whole drag.
    pub handle_px: f32,
    /// Generated edges. Tighter than a grip, because an edge is always
    /// adjacent to a face that competes with it for the same click.
    pub edge_px: f32,
}

impl Default for PickApertures {
    fn default() -> Self {
        Self {
            handle_px: 12.0,
            edge_px: 6.0,
        }
    }
}

/// Build a pick ray through a window-space screen position.
///
/// Every cursor-driven ray in the app must go through here. `viewport_to_world`
/// already maps render-target logical pixels through
/// `logical_viewport_rect()`, so subtracting that rect's origin first
/// double-counts the viewport offset and aims the ray at the wrong pixel. Four
/// call sites used to hand-roll that subtraction; keeping one helper means the
/// convention cannot drift back apart.
pub fn pick_ray_at(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    screen: Vec2,
) -> Option<Ray3d> {
    camera.viewport_to_world(camera_transform, screen).ok()
}

/// Build a camera ray from the current cursor position.
///
/// Requires `&mut World` because `world.query()` borrows mutably in exclusive
/// system contexts.
pub fn build_camera_ray(world: &mut World) -> Option<Ray3d> {
    let mut window_query = world.query_filtered::<&Window, With<PrimaryWindow>>();
    let window = window_query.iter(world).next()?;
    let cursor_position = cursor_window_position(window)?;

    let mut camera_query = world.query_filtered::<(&Camera, &GlobalTransform), With<OrbitCamera>>();
    let (camera, cam_tf) = camera_query.iter(world).next()?;
    pick_ray_at(camera, cam_tf, cursor_position)
}

/// Find the nearest entity hit by a ray, across all registered factories.
pub fn ray_cast_nearest_entity(world: &World, ray: Ray3d) -> Option<HitCandidate> {
    let registry = world.resource::<CapabilityRegistry>();
    let factories = registry.factories().to_vec();
    let mut best: Option<HitCandidate> = None;
    for factory in &factories {
        if let Some(hit) = factory.hit_test(world, ray) {
            if !entity_is_pick_visible(world, hit.entity) {
                continue;
            }
            if best.is_none() || hit.distance < best.as_ref().unwrap().distance {
                best = Some(hit);
            }
        }
    }
    best
}

fn entity_is_pick_visible(world: &World, entity: Entity) -> bool {
    if !entity_on_visible_layer(world, entity) {
        return false;
    }
    if let Some(generated) = world.get::<GeneratedOccurrencePart>(entity) {
        if let Some(owner_entity) = find_entity_by_element_id_readonly(world, generated.owner) {
            if !entity_on_visible_layer(world, owner_entity) {
                return false;
            }
        }
    }
    world
        .get::<Visibility>(entity)
        .is_none_or(|visibility| *visibility != Visibility::Hidden)
        || world
            .get::<WireframeSurfaceVisibilityOverride>(entity)
            .is_some()
}

/// Find the nearest face hit by a ray, across all registered factories.
///
/// First finds the nearest entity via `hit_test()`, then calls `hit_test_face()`
/// on that entity to get face-level detail.
pub fn ray_cast_nearest_face(world: &World, ray: Ray3d) -> Option<FaceHitCandidate> {
    let entity_hit = ray_cast_nearest_entity(world, ray)?;
    let registry = world.resource::<CapabilityRegistry>();
    let entity_ref = world.get_entity(entity_hit.entity).ok()?;
    let snapshot = registry.capture_snapshot(&entity_ref, world)?;
    let factory = registry.factory_for(snapshot.type_name())?;
    factory.hit_test_face(world, entity_hit.entity, ray)
}

/// Project a ray onto a plane, returning the intersection point.
pub fn project_ray_to_plane(ray: Ray3d, plane_point: Vec3, plane_normal: Vec3) -> Option<Vec3> {
    let denom = ray.direction.dot(plane_normal);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (plane_point - ray.origin).dot(plane_normal) / denom;
    if t > 0.0 {
        Some(ray.origin + *ray.direction * t)
    } else {
        None
    }
}
