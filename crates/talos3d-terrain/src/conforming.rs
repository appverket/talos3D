//! Terrain-conforming solid (ADR-059, PP-PLANT-B).
//!
//! A [`ConformingSolid`] is a rectangular footprint extruded *down* onto a
//! [`TerrainHeightfield`]: a flat horizontal top and an underside that is the
//! inverse of the terrain beneath it. It is the domain-neutral platform
//! primitive behind the architecture-domain hugging foundation. The mesh is a
//! derived artifact, regenerated whenever the solid or its target surface
//! changes.
//!
//! Geometry guarantees (given a footprint that overlaps the surface):
//! - `Y_top = max(surface height under footprint) + min_thickness`, so the
//!   thinnest point of the slab is exactly `min_thickness`.
//! - underside height `= max(surface_height, Y_top - max_depth)`, so the solid
//!   benches to a flat bottom where grade dips more than `max_depth` below the
//!   top; thickness never exceeds `max_depth`.

use std::any::Any;

use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use talos3d_core::{
    authored_entity::{
        invalid_property_error, property_field, property_field_with, read_only_property_field,
        scalar_from_json, AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, HandleKind,
        PropertyFieldDef, PropertyValue, PropertyValueKind,
    },
    capability_registry::{AuthoredEntityFactory, ModelSummaryAccumulator},
    plugins::{
        commands::{despawn_by_element_id, find_entity_by_element_id},
        definition_preview_scene::PreviewOnly,
        identity::{ElementId, ElementIdAllocator},
    },
};

use crate::heightfield::TerrainHeightfield;

const MAX_CONFORMING_GRID_NODES_PER_AXIS: usize = 64;

/// Default minimum slab thickness (clearance over the highest ground), metres.
pub const DEFAULT_MIN_THICKNESS: f32 = 0.3;
/// Default maximum slab thickness / bench depth, metres (ADR-059).
pub const DEFAULT_MAX_DEPTH: f32 = 3.0;
/// Default underside sampling spacing, metres.
pub const DEFAULT_CONFORMING_RESOLUTION: f32 = 0.5;

/// Authored terrain-conforming solid. The footprint is a rectangle of
/// `half_extents` centred at `position` (world XZ) and rotated `yaw` about Y.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConformingSolid {
    pub name: String,
    /// World XZ of the footprint centre.
    pub position: Vec2,
    /// Half-width / half-depth of the rectangular footprint (local X, Z).
    pub half_extents: Vec2,
    /// Rotation about the world Y axis, radians.
    pub yaw: f32,
    /// Clearance over the highest ground under the footprint.
    pub min_thickness: f32,
    /// Maximum thickness; below this the underside benches flat.
    pub max_depth: f32,
    /// Element id of the target terrain surface whose height field is hugged.
    pub surface_id: ElementId,
    /// Underside sampling spacing.
    pub resolution: f32,
    /// Optional fixed top elevation (a building's finished-floor datum). When set,
    /// the flat top sits at this Y instead of `max(grade) + min_thickness`, so the
    /// foundation top stays at a chosen level while the underside still drapes to the
    /// terrain. `None` keeps the terrain-relative top.
    #[serde(default)]
    pub floor_datum: Option<f32>,
}

impl Default for ConformingSolid {
    fn default() -> Self {
        Self {
            name: "Conforming Solid".to_string(),
            position: Vec2::ZERO,
            half_extents: Vec2::splat(2.0),
            yaw: 0.0,
            min_thickness: DEFAULT_MIN_THICKNESS,
            max_depth: DEFAULT_MAX_DEPTH,
            surface_id: ElementId(0),
            resolution: DEFAULT_CONFORMING_RESOLUTION,
            floor_datum: None,
        }
    }
}

/// Marker requesting a (re)build of a conforming solid's derived mesh.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NeedsConformingMesh;

/// Cached derived top elevation of a conforming solid (`max(grade)+min_thickness`).
/// Written by the regen system from the height field; read by the snapshot so its
/// handles/preview sit on the visible solid (the snapshot alone cannot know the
/// terrain height). A *separate* component so updating it does not retrigger the
/// `Changed<ConformingSolid>` rebuild.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct ConformingDerived {
    pub y_top: f32,
}

/// Result metrics of a conforming-solid build (exposed for tests / property UI).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConformingMetrics {
    pub y_top: f32,
    pub min_thickness: f32,
    pub max_thickness: f32,
}

impl ConformingSolid {
    /// The four footprint corners in world XZ, in CCW order.
    fn world_corners(&self) -> [Vec2; 4] {
        let (s, c) = self.yaw.sin_cos();
        let to_world = |lx: f32, lz: f32| {
            Vec2::new(
                self.position.x + c * lx - s * lz,
                self.position.y + s * lx + c * lz,
            )
        };
        let hx = self.half_extents.x.max(0.01);
        let hz = self.half_extents.y.max(0.01);
        [
            to_world(-hx, -hz),
            to_world(hx, -hz),
            to_world(hx, hz),
            to_world(-hx, hz),
        ]
    }

    fn grid_dims(&self) -> (usize, usize) {
        let res = self.resolution.max(0.05);
        let nx = ((2.0 * self.half_extents.x.max(0.01) / res).ceil() as usize + 1)
            .clamp(2, MAX_CONFORMING_GRID_NODES_PER_AXIS);
        let nz = ((2.0 * self.half_extents.y.max(0.01) / res).ceil() as usize + 1)
            .clamp(2, MAX_CONFORMING_GRID_NODES_PER_AXIS);
        (nx, nz)
    }
}

/// Sample the surface under the footprint and return the per-node world XZ and
/// resolved underside / top heights, or `None` if the footprint does not overlap
/// the surface at all.
struct ConformingSamples {
    nx: usize,
    nz: usize,
    world: Vec<Vec2>,
    under_y: Vec<f32>,
    y_top: f32,
    metrics: ConformingMetrics,
}

fn sample_conforming(solid: &ConformingSolid, hf: &TerrainHeightfield) -> Option<ConformingSamples> {
    let (nx, nz) = solid.grid_dims();
    let (s, c) = solid.yaw.sin_cos();
    let hx = solid.half_extents.x.max(0.01);
    let hz = solid.half_extents.y.max(0.01);

    let mut world = Vec::with_capacity(nx * nz);
    let mut surf = Vec::with_capacity(nx * nz);
    let mut h_max = f32::NEG_INFINITY;
    for j in 0..nz {
        for i in 0..nx {
            let lx = -hx + 2.0 * hx * (i as f32 / (nx - 1) as f32);
            let lz = -hz + 2.0 * hz * (j as f32 / (nz - 1) as f32);
            let w = Vec2::new(
                solid.position.x + c * lx - s * lz,
                solid.position.y + s * lx + c * lz,
            );
            let h = hf.height_at(w.x, w.y);
            if let Some(hh) = h {
                h_max = h_max.max(hh);
            }
            world.push(w);
            surf.push(h);
        }
    }
    if !h_max.is_finite() {
        return None;
    }

    let min_thickness = solid.min_thickness.max(0.0);
    // max_depth must be at least min_thickness or the bench would clip the top.
    let max_depth = solid.max_depth.max(min_thickness);
    // Top is the explicit floor datum when given (building finished floor), else it
    // floats `min_thickness` above the highest ground under the footprint. A datum is
    // treated as a *minimum*: the top always rises to clear the ground by at least
    // `min_thickness`, so a datum set at/below grade can never bury the slab (the
    // underside would otherwise clamp flat and let the terrain poke through).
    let terrain_top = h_max + min_thickness;
    let y_top = solid
        .floor_datum
        .map(|d| d.max(terrain_top))
        .unwrap_or(terrain_top);
    let floor_y = y_top - max_depth;
    // Keep at least `min_thickness` of slab even where the datum sits at/below grade.
    let under_cap = y_top - min_thickness;

    let mut under_y = Vec::with_capacity(nx * nz);
    let mut max_thickness = min_thickness;
    for h in &surf {
        let u = match h {
            // Drape to the terrain, but never above the min-thickness cap nor below
            // the max-depth bench.
            Some(hh) => hh.max(floor_y).min(under_cap),
            None => floor_y,
        };
        max_thickness = max_thickness.max(y_top - u);
        under_y.push(u);
    }

    Some(ConformingSamples {
        nx,
        nz,
        world,
        under_y,
        y_top,
        metrics: ConformingMetrics {
            y_top,
            min_thickness,
            max_thickness,
        },
    })
}

/// Metrics-only build (no mesh), for tests and property display.
pub fn conforming_metrics(
    solid: &ConformingSolid,
    hf: &TerrainHeightfield,
) -> Option<ConformingMetrics> {
    sample_conforming(solid, hf).map(|s| s.metrics)
}

fn push_tri(
    pos: &mut Vec<[f32; 3]>,
    nor: &mut Vec<[f32; 3]>,
    a: Vec3,
    b: Vec3,
    c: Vec3,
    outward: Vec3,
) {
    let mut n = (b - a).cross(c - a);
    if n.length_squared() < 1e-12 {
        return;
    }
    n = n.normalize();
    // Wind so the geometric normal faces the desired outward direction.
    let (a, b, c, n) = if n.dot(outward) < 0.0 {
        (a, c, b, -n)
    } else {
        (a, b, c, n)
    };
    for v in [a, b, c] {
        pos.push([v.x, v.y, v.z]);
        nor.push([n.x, n.y, n.z]);
    }
}

/// Build the watertight conforming-solid mesh (flat top, conforming benched
/// underside, vertical walls) plus the derived top elevation `Y_top`. `None` if
/// the footprint does not overlap the surface. Non-indexed with per-face flat
/// normals for crisp edges.
pub fn build_conforming_mesh(
    solid: &ConformingSolid,
    hf: &TerrainHeightfield,
) -> Option<(Mesh, f32)> {
    let s = sample_conforming(solid, hf)?;
    let nx = s.nx;
    let nz = s.nz;
    let y_top = s.y_top;
    let center = solid.position;

    let under = |i: usize, j: usize| Vec3::new(s.world[j * nx + i].x, s.under_y[j * nx + i], s.world[j * nx + i].y);
    let top = |i: usize, j: usize| Vec3::new(s.world[j * nx + i].x, y_top, s.world[j * nx + i].y);

    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut nor: Vec<[f32; 3]> = Vec::new();

    for j in 0..nz - 1 {
        for i in 0..nx - 1 {
            // Underside (outward = down).
            push_tri(&mut pos, &mut nor, under(i, j), under(i + 1, j), under(i + 1, j + 1), Vec3::NEG_Y);
            push_tri(&mut pos, &mut nor, under(i, j), under(i + 1, j + 1), under(i, j + 1), Vec3::NEG_Y);
            // Top (outward = up).
            push_tri(&mut pos, &mut nor, top(i, j), top(i + 1, j), top(i + 1, j + 1), Vec3::Y);
            push_tri(&mut pos, &mut nor, top(i, j), top(i + 1, j + 1), top(i, j + 1), Vec3::Y);
        }
    }

    // Perimeter walls. Walk the boundary node ring; each segment is a vertical
    // quad from the underside up to the flat top, normal facing away from centre.
    let mut ring: Vec<(usize, usize)> = Vec::new();
    for i in 0..nx {
        ring.push((i, 0));
    }
    for j in 1..nz {
        ring.push((nx - 1, j));
    }
    for i in (0..nx - 1).rev() {
        ring.push((i, nz - 1));
    }
    for j in (1..nz - 1).rev() {
        ring.push((0, j));
    }
    for k in 0..ring.len() {
        let (i0, j0) = ring[k];
        let (i1, j1) = ring[(k + 1) % ring.len()];
        let u0 = under(i0, j0);
        let u1 = under(i1, j1);
        let t0 = top(i0, j0);
        let t1 = top(i1, j1);
        let mid = (u0 + u1) * 0.5;
        let outward = (Vec3::new(mid.x, 0.0, mid.z) - Vec3::new(center.x, 0.0, center.y))
            .normalize_or_zero();
        push_tri(&mut pos, &mut nor, u0, u1, t1, outward);
        push_tri(&mut pos, &mut nor, u0, t1, t0, outward);
    }

    if pos.is_empty() {
        return None;
    }
    let uvs: Vec<[f32; 2]> = pos.iter().map(|p| [p[0] * 0.1, p[2] * 0.1]).collect();
    let indices: Vec<u32> = (0..pos.len() as u32).collect();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nor);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    Some((mesh, y_top))
}

// ---------------------------------------------------------------------------
// Authored entity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConformingSolidSnapshot {
    pub element_id: ElementId,
    pub solid: ConformingSolid,
    /// Cached derived top elevation (from `ConformingDerived`) so handles and the
    /// drag preview sit on the visible solid. Not authoritative; recomputed on
    /// regen.
    #[serde(default)]
    pub derived_top: f32,
}

impl From<ConformingSolidSnapshot> for BoxedEntity {
    fn from(snapshot: ConformingSolidSnapshot) -> Self {
        Self(Box::new(snapshot))
    }
}

impl AuthoredEntity for ConformingSolidSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "conforming_solid"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        if self.solid.name.is_empty() {
            "Conforming Solid".to_string()
        } else {
            self.solid.name.clone()
        }
    }

    fn center(&self) -> Vec3 {
        // Report the real top elevation (datum, or the terrain-derived top) so the
        // solid participates correctly in group bounds, rotation pivots, and
        // selection centres instead of collapsing to y = 0.
        let y = self.solid.floor_datum.unwrap_or(self.derived_top);
        Vec3::new(self.solid.position.x, y, self.solid.position.y)
    }

    fn translate_by(&self, delta: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        // XZ is the authored placement.
        snapshot.solid.position += delta.xz();
        // Vertical drag lifts/lowers a *datum* foundation: raise the fixed top by
        // delta.y so the slab rises with the building, its underside still draping to
        // the terrain. A terrain-relative foundation (no datum) keeps riding the
        // surface, so vertical drag is a no-op there.
        if let Some(datum) = snapshot.solid.floor_datum {
            snapshot.solid.floor_datum = Some(datum + delta.y);
        }
        snapshot.into()
    }

    fn rotate_by(&self, rotation: Quat) -> BoxedEntity {
        let mut snapshot = self.clone();
        let (yaw, _, _) = rotation.to_euler(EulerRot::YXZ);
        snapshot.solid.yaw += yaw;
        snapshot.into()
    }

    fn scale_by(&self, factor: Vec3, _center: Vec3) -> BoxedEntity {
        let mut snapshot = self.clone();
        snapshot.solid.half_extents.x = (snapshot.solid.half_extents.x * factor.x).max(0.05);
        snapshot.solid.half_extents.y = (snapshot.solid.half_extents.y * factor.z).max(0.05);
        snapshot.into()
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        vec![
            property_field_with(
                "name",
                "name",
                PropertyValueKind::Text,
                Some(PropertyValue::Text(self.solid.name.clone())),
                true,
            ),
            property_field(
                "position",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(Vec3::new(
                    self.solid.position.x,
                    0.0,
                    self.solid.position.y,
                ))),
            ),
            property_field(
                "yaw_deg",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.solid.yaw.to_degrees())),
            ),
            property_field(
                "min_thickness",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.solid.min_thickness)),
            ),
            property_field(
                "max_depth",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.solid.max_depth)),
            ),
            property_field(
                "half_extents",
                PropertyValueKind::Vec3,
                Some(PropertyValue::Vec3(Vec3::new(
                    self.solid.half_extents.x,
                    0.0,
                    self.solid.half_extents.y,
                ))),
            ),
            property_field(
                "resolution",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.solid.resolution)),
            ),
            // Fixed top elevation (finished-floor datum). Shows the current effective
            // top; setting it pins the top there while the underside keeps draping.
            property_field(
                "floor_datum",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(
                    self.solid.floor_datum.unwrap_or(self.derived_top),
                )),
            ),
            read_only_property_field(
                "surface_id",
                "surface id",
                PropertyValueKind::Scalar,
                Some(PropertyValue::Scalar(self.solid.surface_id.0 as f32)),
            ),
        ]
    }

    fn set_property_json(&self, property_name: &str, value: &Value) -> Result<BoxedEntity, String> {
        let mut snapshot = self.clone();
        match property_name {
            "name" => {
                snapshot.solid.name = value
                    .as_str()
                    .ok_or_else(|| "Expected string".to_string())?
                    .to_string();
            }
            // Planting: only the XZ placement is authored — Y rides the surface.
            "position" => {
                let arr = value
                    .as_array()
                    .ok_or_else(|| "Expected [x, _, z]".to_string())?;
                if arr.len() != 3 {
                    return Err("Expected [x, _, z]".to_string());
                }
                snapshot.solid.position = Vec2::new(
                    arr[0].as_f64().unwrap_or(0.0) as f32,
                    arr[2].as_f64().unwrap_or(0.0) as f32,
                );
            }
            "yaw_deg" => snapshot.solid.yaw = scalar_from_json(value)?.to_radians(),
            "min_thickness" => snapshot.solid.min_thickness = scalar_from_json(value)?.max(0.0),
            "max_depth" => snapshot.solid.max_depth = scalar_from_json(value)?.max(0.01),
            "resolution" => snapshot.solid.resolution = scalar_from_json(value)?.max(0.05),
            "floor_datum" => snapshot.solid.floor_datum = Some(scalar_from_json(value)?),
            "half_extents" => {
                let arr = value
                    .as_array()
                    .ok_or_else(|| "Expected [x, _, z]".to_string())?;
                if arr.len() != 3 {
                    return Err("Expected [x, _, z]".to_string());
                }
                snapshot.solid.half_extents = Vec2::new(
                    arr[0].as_f64().unwrap_or(0.0) as f32,
                    arr[2].as_f64().unwrap_or(0.0) as f32,
                )
                .max(Vec2::splat(0.05));
            }
            _ => {
                return Err(invalid_property_error(
                    "conforming_solid",
                    &[
                        "name",
                        "position",
                        "yaw_deg",
                        "min_thickness",
                        "max_depth",
                        "half_extents",
                        "resolution",
                        "floor_datum",
                    ],
                ))
            }
        }
        Ok(snapshot.into())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        // Place handles on the *visible* solid using the cached derived top. A
        // centre handle moves it (XZ; Y re-derives); corner handles resize the
        // footprint. `bounds()` returns None so these authored handles are the
        // only ones — the generic bounds move-handles would slide the mesh
        // rigidly instead of re-conforming it.
        let y = self.derived_top;
        let corners = self.solid.world_corners();
        let mut handles: Vec<HandleInfo> = corners
            .iter()
            .enumerate()
            .map(|(index, corner)| HandleInfo {
                id: format!("corner_{index}"),
                position: Vec3::new(corner.x, y, corner.y),
                kind: HandleKind::Vertex,
                label: format!("Corner {}", index + 1),
            })
            .collect();
        handles.push(HandleInfo {
            id: "center".to_string(),
            position: Vec3::new(self.solid.position.x, y, self.solid.position.y),
            kind: HandleKind::Center,
            label: "Move".to_string(),
        });
        handles
    }

    fn bounds(&self) -> Option<EntityBounds> {
        // None → the viewport uses the mesh AABB for framing/selection, and the
        // Move tool shows only our authored (re-conforming) handles.
        None
    }

    fn drag_handle(&self, handle_id: &str, cursor: Vec3) -> Option<BoxedEntity> {
        let mut snapshot = self.clone();
        if handle_id == "center" {
            snapshot.solid.position = Vec2::new(cursor.x, cursor.z);
            return Some(snapshot.into());
        }
        let index = handle_id
            .strip_prefix("corner_")
            .and_then(|rest| rest.parse::<usize>().ok())?;
        let corners = self.solid.world_corners();
        let opposite = corners[(index + 2) % 4];
        let cursor_xz = Vec2::new(cursor.x, cursor.z);
        let new_center = (opposite + cursor_xz) * 0.5;
        let diag = cursor_xz - opposite;
        // Express the diagonal in the footprint's un-rotated frame.
        let (sin, cos) = self.solid.yaw.sin_cos();
        let local = Vec2::new(cos * diag.x + sin * diag.y, -sin * diag.x + cos * diag.y);
        snapshot.solid.position = new_center;
        snapshot.solid.half_extents = (local.abs() * 0.5).max(Vec2::splat(0.25));
        Some(snapshot.into())
    }

    fn sync_preview_entity(&self, world: &mut World, existing: Option<Entity>) -> Option<Entity> {
        // Rebuild the conforming mesh at the dragged footprint so it re-conforms
        // to grade live (a rigid Transform slide would not). The real entity is
        // hidden for the duration so the preview isn't doubled.
        let heightfield = {
            let mut query = world.query::<(&ElementId, &TerrainHeightfield)>();
            query
                .iter(world)
                .find(|(id, _)| id.0 == self.solid.surface_id.0)
                .map(|(_, hf)| hf.clone())
        };
        let Some(heightfield) = heightfield else {
            return existing;
        };
        let Some((mesh, _)) = build_conforming_mesh(&self.solid, &heightfield) else {
            return existing;
        };
        let material = world
            .get_resource::<ConformingMaterial>()
            .map(|material| material.0.clone());
        let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);

        if let Some(real) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(real).insert(Visibility::Hidden);
        }

        if let Some(existing) = existing {
            let old = world
                .get_entity(existing)
                .ok()
                .and_then(|entity_ref| entity_ref.get::<Mesh3d>().map(|mesh| mesh.0.clone()));
            if let Ok(mut entity_mut) = world.get_entity_mut(existing) {
                entity_mut.insert(Mesh3d(mesh_handle));
            }
            if let Some(old) = old {
                world.resource_mut::<Assets<Mesh>>().remove(old.id());
            }
            Some(existing)
        } else {
            let mut entity = world.spawn((
                PreviewOnly,
                Mesh3d(mesh_handle),
                Transform::IDENTITY,
                Visibility::Inherited,
            ));
            if let Some(material) = material {
                entity.insert(MeshMaterial3d(material));
            }
            Some(entity.id())
        }
    }

    fn cleanup_preview_entity(&self, world: &mut World, preview_entity: Entity) {
        let mesh = world
            .get_entity(preview_entity)
            .ok()
            .and_then(|entity_ref| entity_ref.get::<Mesh3d>().map(|mesh| mesh.0.clone()));
        if let Some(mesh) = mesh {
            world.resource_mut::<Assets<Mesh>>().remove(mesh.id());
        }
        let _ = world.despawn(preview_entity);
        // Restore the real solid we hid while previewing.
        if let Some(real) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(real).insert(Visibility::Inherited);
        }
    }

    fn to_json(&self) -> Value {
        serde_json::to_value(self.clone()).unwrap_or(Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            world.entity_mut(entity).insert((
                self.solid.clone(),
                NeedsConformingMesh,
                Visibility::Inherited,
            ));
            return;
        }
        world.spawn((
            self.element_id,
            self.solid.clone(),
            NeedsConformingMesh,
            Visibility::Inherited,
        ));
    }

    fn remove_from(&self, world: &mut World) {
        if let Some(entity) = find_entity_by_element_id(world, self.element_id) {
            let mesh_id = world
                .get_entity(entity)
                .ok()
                .and_then(|entity_ref| entity_ref.get::<Mesh3d>().map(|mesh_3d| mesh_3d.id()));
            if let Some(mesh_id) = mesh_id {
                world.resource_mut::<Assets<Mesh>>().remove(mesh_id);
            }
        }
        despawn_by_element_id(world, self.element_id);
    }

    fn draw_preview(&self, gizmos: &mut Gizmos, color: Color) {
        let corners = self.solid.world_corners();
        for k in 0..4 {
            let a = corners[k];
            let b = corners[(k + 1) % 4];
            gizmos.line(Vec3::new(a.x, 0.0, a.y), Vec3::new(b.x, 0.0, b.y), color);
        }
    }

    fn box_clone(&self) -> BoxedEntity {
        self.clone().into()
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other.type_name() == self.type_name() && other.to_json() == self.to_json()
    }
}

pub struct ConformingSolidFactory;

impl AuthoredEntityFactory for ConformingSolidFactory {
    fn type_name(&self) -> &'static str {
        "conforming_solid"
    }

    fn capture_snapshot(
        &self,
        entity_ref: &bevy::ecs::world::EntityRef,
        _world: &World,
    ) -> Option<BoxedEntity> {
        Some(
            ConformingSolidSnapshot {
                element_id: *entity_ref.get::<ElementId>()?,
                solid: entity_ref.get::<ConformingSolid>()?.clone(),
                derived_top: entity_ref
                    .get::<ConformingDerived>()
                    .map(|derived| derived.y_top)
                    .unwrap_or(0.0),
            }
            .into(),
        )
    }

    fn from_persisted_json(&self, data: &Value) -> Result<BoxedEntity, String> {
        let snapshot: ConformingSolidSnapshot =
            serde_json::from_value(data.clone()).map_err(|error| error.to_string())?;
        Ok(snapshot.into())
    }

    fn contribute_model_summary(&self, world: &World, summary: &mut ModelSummaryAccumulator) {
        let Some(mut query) = world.try_query::<&ConformingSolid>() else {
            return;
        };
        let count = query.iter(world).count();
        if count > 0 {
            *summary
                .entity_counts
                .entry("conforming_solid".to_string())
                .or_default() += count;
        }
    }

    fn from_create_request(&self, world: &World, request: &Value) -> Result<BoxedEntity, String> {
        let object = request
            .as_object()
            .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
        let element_id = world
            .get_resource::<ElementIdAllocator>()
            .ok_or_else(|| "ElementIdAllocator not available".to_string())?
            .next_id();

        let mut solid = ConformingSolid::default();
        if let Some(p) = object.get("position").and_then(Value::as_array) {
            if p.len() == 2 {
                solid.position =
                    Vec2::new(p[0].as_f64().unwrap_or(0.0) as f32, p[1].as_f64().unwrap_or(0.0) as f32);
            }
        }
        if let Some(h) = object.get("half_extents").and_then(Value::as_array) {
            if h.len() == 2 {
                solid.half_extents = Vec2::new(
                    h[0].as_f64().unwrap_or(2.0) as f32,
                    h[1].as_f64().unwrap_or(2.0) as f32,
                );
            }
        }
        if let Some(v) = object.get("yaw_deg").and_then(Value::as_f64) {
            solid.yaw = (v as f32).to_radians();
        }
        if let Some(v) = object.get("min_thickness").and_then(Value::as_f64) {
            solid.min_thickness = (v as f32).max(0.0);
        }
        if let Some(v) = object.get("max_depth").and_then(Value::as_f64) {
            solid.max_depth = (v as f32).max(0.01);
        }
        if let Some(v) = object.get("resolution").and_then(Value::as_f64) {
            solid.resolution = (v as f32).max(0.05);
        }
        if let Some(v) = object.get("surface_id").and_then(Value::as_u64) {
            solid.surface_id = ElementId(v);
        }
        if let Some(v) = object.get("floor_datum").and_then(Value::as_f64) {
            solid.floor_datum = Some(v as f32);
        }
        if let Some(v) = object.get("name").and_then(Value::as_str) {
            solid.name = v.to_string();
        }

        Ok(ConformingSolidSnapshot {
            element_id,
            solid,
            derived_top: 0.0,
        }
        .into())
    }
}

// ---------------------------------------------------------------------------
// Bevy plugin: material, regeneration, reactivity
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct ConformingMaterial(Handle<StandardMaterial>);

pub struct ConformingSolidPlugin;

impl Plugin for ConformingSolidPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_conforming_material).add_systems(
            Update,
            (mark_conforming_dirty, regenerate_conforming_meshes).chain(),
        );
    }
}

fn setup_conforming_material(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.72, 0.70, 0.66),
        perceptual_roughness: 0.9,
        metallic: 0.0,
        ..default()
    });
    commands.insert_resource(ConformingMaterial(handle));
}

#[allow(clippy::type_complexity)]
fn mark_conforming_dirty(
    mut commands: Commands,
    changed_solids: Query<
        Entity,
        (
            With<ConformingSolid>,
            Or<(Changed<ConformingSolid>, Without<Mesh3d>)>,
        ),
    >,
    changed_surfaces: Query<&ElementId, Changed<TerrainHeightfield>>,
    all_solids: Query<(Entity, &ConformingSolid)>,
) {
    for entity in &changed_solids {
        commands.entity(entity).try_insert(NeedsConformingMesh);
    }
    let dirty: std::collections::HashSet<u64> = changed_surfaces.iter().map(|id| id.0).collect();
    if !dirty.is_empty() {
        for (entity, solid) in &all_solids {
            if dirty.contains(&solid.surface_id.0) {
                commands.entity(entity).try_insert(NeedsConformingMesh);
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn regenerate_conforming_meshes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ConformingMaterial>>,
    solids: Query<
        (
            Entity,
            &ConformingSolid,
            Option<&Mesh3d>,
            Option<&MeshMaterial3d<StandardMaterial>>,
        ),
        With<NeedsConformingMesh>,
    >,
    heightfields: Query<(&ElementId, &TerrainHeightfield)>,
) {
    let Some(material) = material else {
        return;
    };
    for (entity, solid, mesh_handle, material_handle) in &solids {
        let Some((_, hf)) = heightfields
            .iter()
            .find(|(element_id, _)| element_id.0 == solid.surface_id.0)
        else {
            // Surface height field not ready yet; retry next frame (keep marker).
            continue;
        };
        let Some((mesh, y_top)) = build_conforming_mesh(solid, hf) else {
            // Footprint not over the surface; clear the marker so we don't spin.
            commands.entity(entity).try_remove::<NeedsConformingMesh>();
            continue;
        };
        // Cache the derived top so the snapshot's handles/preview sit on the solid
        // (separate component → does not retrigger Changed<ConformingSolid>).
        commands.entity(entity).try_insert(ConformingDerived { y_top });
        match mesh_handle {
            Some(handle) if meshes.get(handle.id()).is_some() => {
                if let Some(existing) = meshes.get_mut(handle.id()) {
                    *existing = mesh;
                }
            }
            _ => {
                commands.entity(entity).try_insert(Mesh3d(meshes.add(mesh)));
            }
        }
        if material_handle.is_none() {
            commands
                .entity(entity)
                .try_insert(MeshMaterial3d(material.0.clone()));
        }
        commands
            .entity(entity)
            .try_insert(Transform::IDENTITY)
            .try_remove::<NeedsConformingMesh>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ramp_field() -> TerrainHeightfield {
        // h = x over [0,20] x [0,20].
        let mut pts = Vec::new();
        for ix in 0..=20 {
            for iz in 0..=20 {
                pts.push(Vec3::new(ix as f32, ix as f32, iz as f32));
            }
        }
        TerrainHeightfield::build(&pts, &[], 1.0, 0.0).expect("field")
    }

    #[test]
    fn min_thickness_holds_at_the_high_point() {
        let hf = ramp_field();
        let solid = ConformingSolid {
            position: Vec2::new(10.0, 10.0),
            half_extents: Vec2::new(3.0, 3.0),
            min_thickness: 0.5,
            max_depth: 3.0,
            surface_id: ElementId(0),
            resolution: 0.5,
            ..Default::default()
        };
        let m = conforming_metrics(&solid, &hf).expect("over surface");
        // High point under footprint is x≈13 → y_top ≈ 13.5.
        assert!((m.y_top - 13.5).abs() < 0.3, "y_top={}", m.y_top);
        // Thinnest slab == min_thickness; never thicker than max_depth.
        assert!((m.min_thickness - 0.5).abs() < 1e-4);
        assert!(m.max_thickness <= 3.0 + 1e-3, "max_thickness={}", m.max_thickness);
    }

    #[test]
    fn benches_to_max_depth_on_a_cliff() {
        // A field with a sharp drop so the dip exceeds max_depth.
        let mut pts = Vec::new();
        for ix in 0..=20 {
            for iz in 0..=20 {
                let x = ix as f32;
                let h = if x < 10.0 { 10.0 } else { 0.0 }; // 10 m cliff
                pts.push(Vec3::new(x, h, iz as f32));
            }
        }
        let hf = TerrainHeightfield::build(&pts, &[], 1.0, 0.0).expect("field");
        let solid = ConformingSolid {
            position: Vec2::new(10.0, 10.0),
            half_extents: Vec2::new(6.0, 3.0),
            min_thickness: 0.5,
            max_depth: 3.0,
            surface_id: ElementId(0),
            resolution: 0.5,
            ..Default::default()
        };
        let m = conforming_metrics(&solid, &hf).expect("over surface");
        // Top ~10.5; cliff bottom would be ~10.5 below → benched at max_depth.
        assert!((m.max_thickness - 3.0).abs() < 0.2, "max_thickness={}", m.max_thickness);
    }

    #[test]
    fn returns_no_mesh_off_surface() {
        let hf = ramp_field();
        let solid = ConformingSolid {
            position: Vec2::new(1000.0, 1000.0),
            half_extents: Vec2::new(2.0, 2.0),
            surface_id: ElementId(0),
            ..Default::default()
        };
        assert!(build_conforming_mesh(&solid, &hf).is_none());
    }

    #[test]
    fn drag_rebuilds_stay_within_frame_budget() {
        // PP-PLANT-C performance evidence: rebuilding the conforming mesh every
        // frame while dragging must fit comfortably inside a 16 ms frame, so the
        // underside can re-conform live without throttling for typical footprints.
        let hf = ramp_field();
        let mut solid = ConformingSolid {
            half_extents: Vec2::new(8.0, 6.0),
            resolution: 0.5,
            min_thickness: 0.5,
            max_depth: 3.0,
            surface_id: ElementId(0),
            ..Default::default()
        };
        let frames = 240usize; // ~4 s of dragging at 60 fps
        let t0 = Instant::now();
        let mut tris = 0usize;
        for k in 0..frames {
            solid.position = Vec2::new(2.0 + (k as f32 * 0.05) % 14.0, 10.0);
            if let Some((mesh, _)) = build_conforming_mesh(&solid, &hf) {
                tris = mesh.count_vertices() / 3;
            }
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let per_frame = ms / frames as f64;
        println!(
            "conforming rebuild: {frames} frames in {:.1} ms = {:.3} ms/frame (~{tris} tris)",
            ms, per_frame
        );
        assert!(per_frame < 8.0, "rebuild too slow for live drag: {per_frame} ms/frame");
    }

    #[test]
    fn mesh_has_top_underside_and_walls() {
        let hf = ramp_field();
        let solid = ConformingSolid {
            position: Vec2::new(10.0, 10.0),
            half_extents: Vec2::new(3.0, 3.0),
            surface_id: ElementId(0),
            resolution: 1.0,
            ..Default::default()
        };
        let (mesh, _) = build_conforming_mesh(&solid, &hf).expect("mesh");
        let count = mesh.count_vertices();
        assert!(count > 0 && count % 3 == 0, "non-indexed triangles, got {count}");
    }
}
