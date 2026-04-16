use bevy::prelude::*;

pub fn snap_to_increment(value: f32, increment: f32) -> f32 {
    (value / increment).round() * increment
}

pub fn rotate_point(point: Vec2, pivot: Vec2, delta_radians: f32) -> Vec2 {
    let sin = delta_radians.sin();
    let cos = delta_radians.cos();
    let offset = point - pivot;
    pivot
        + Vec2::new(
            offset.x * cos - offset.y * sin,
            offset.x * sin + offset.y * cos,
        )
}

pub fn scale_point_around_center(point: Vec3, center: Vec3, factor: Vec3) -> Vec3 {
    center + (point - center) * factor
}

pub fn project_direction_to_plane(direction: Vec3, plane_normal: Vec3) -> Option<Vec3> {
    let plane_normal = plane_normal.normalize_or_zero();
    let projected = direction - plane_normal * direction.dot(plane_normal);
    projected.try_normalize()
}

pub fn rectangle_corners(
    center: Vec3,
    horizontal_offset: Vec3,
    vertical_offset: Vec3,
) -> [Vec3; 4] {
    [
        center - horizontal_offset - vertical_offset,
        center - horizontal_offset + vertical_offset,
        center + horizontal_offset + vertical_offset,
        center + horizontal_offset - vertical_offset,
    ]
}

pub fn draw_loop(gizmos: &mut Gizmos, corners: [Vec3; 4], color: Color) {
    for index in 0..corners.len() {
        gizmos.line(corners[index], corners[(index + 1) % corners.len()], color);
    }
}
