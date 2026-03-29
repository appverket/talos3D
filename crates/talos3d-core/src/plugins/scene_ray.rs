//! Shared scene ray-casting utilities.
//!
//! Provides `build_camera_ray` and `ray_cast_nearest_face` so that any tool
//! (face editing, line tool, etc.) can find geometry under the cursor without
//! duplicating ray-cast boilerplate.

use bevy::{prelude::*, window::PrimaryWindow};

use crate::capability_registry::{CapabilityRegistry, FaceHitCandidate, HitCandidate};

/// Build a camera ray from the current cursor position.
///
/// Requires `&mut World` because `world.query()` borrows mutably in exclusive
/// system contexts.
pub fn build_camera_ray(world: &mut World) -> Option<Ray3d> {
    let mut window_query = world.query_filtered::<&Window, With<PrimaryWindow>>();
    let window = window_query.iter(world).next()?;
    let cursor_pos = window.cursor_position()?;

    let mut camera_query = world.query::<(&Camera, &GlobalTransform)>();
    let (camera, cam_tf) = camera_query.iter(world).next()?;
    let viewport_cursor = match camera.logical_viewport_rect() {
        Some(rect) => cursor_pos - rect.min,
        None => cursor_pos,
    };
    camera.viewport_to_world(cam_tf, viewport_cursor).ok()
}

/// Find the nearest entity hit by a ray, across all registered factories.
pub fn ray_cast_nearest_entity(world: &World, ray: Ray3d) -> Option<HitCandidate> {
    let registry = world.resource::<CapabilityRegistry>();
    let factories = registry.factories().to_vec();
    let mut best: Option<HitCandidate> = None;
    for factory in &factories {
        if let Some(hit) = factory.hit_test(world, ray) {
            if best.is_none() || hit.distance < best.as_ref().unwrap().distance {
                best = Some(hit);
            }
        }
    }
    best
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
