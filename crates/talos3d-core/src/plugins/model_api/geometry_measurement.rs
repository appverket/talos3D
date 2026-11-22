//! Bounded, on-demand inspection of the same meshes used for presentation.
//! No reconstruction from domain parameters, recursive CSG, or scene-wide mesh build.
use super::*;
use crate::plugins::modeling::{
    primitive_trait::{MeshGenerator, Primitive},
    primitives::BoxPrimitive,
    profile::ProfileExtrusion,
};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Request {
    /// Omit to inspect the measurement contract without touching geometry.
    pub element_id: Option<u64>,
    #[serde(default)]
    pub triangle_offset: usize,
    #[serde(default = "page_size")]
    #[schemars(range(min = 1, max = 256))]
    pub triangle_limit: usize,
}
fn page_size() -> usize {
    64
}
const MAX_VERTICES: usize = 100_000;
const MAX_TRIANGLES: usize = 100_000;
const MAX_PAGE: usize = 256;

pub fn contract() -> Value {
    json!({"schema":"talos3d.geometry-measurement.v1","coordinates":"world_metres",
        "observations":["triangle_vertices","triangle_normals"],
        "ordinary_source":"current_direct_render_mesh","scenario_source":"native_mesh_generator",
        "coverage":"one directly meshed element; not an assembly, child hierarchy or entire document",
        "limits":{"vertices":MAX_VERTICES,"triangles":MAX_TRIANGLES,"page_triangles":MAX_PAGE},
        "unsupported":"Missing direct meshes, unsupported topology and exceeded bounds are errors. No fallback to bounding boxes.",
        "acceptance":false})
}

pub fn inspect(world: &World, request: Request) -> Result<Value, String> {
    let Some(id) = request.element_id else {
        return Ok(contract());
    };
    if request.triangle_limit == 0 || request.triangle_limit > MAX_PAGE {
        return Err("triangle_limit must be 1..=256".into());
    }
    let entity =
        find_entity_by_element_id_readonly(world, ElementId(id)).ok_or("Unknown element")?;
    if let Some(handle) = world.get::<Mesh3d>(entity) {
        let meshes = world
            .get_resource::<Assets<Mesh>>()
            .ok_or("Render mesh assets unavailable")?;
        let mesh = meshes.get(&handle.0).ok_or("Render mesh unavailable")?;
        let transform = world
            .get::<GlobalTransform>(entity)
            .ok_or("World transform unavailable")?;
        return observe(
            mesh,
            transform.affine(),
            &request,
            "current_direct_render_mesh",
        );
    }
    // Isolated native scenarios have no renderer/assets. Use their existing
    // presentation generator only; never substitute authored geometry in a live app.
    if world.contains_resource::<document_dispatch::DocumentCallBudget>() {
        let rotation = world
            .get::<crate::plugins::modeling::primitives::ShapeRotation>(entity)
            .copied()
            .unwrap_or_default()
            .0;
        if let Some(p) = world.get::<BoxPrimitive>(entity) {
            return observe(
                &p.to_bevy_mesh(rotation),
                GlobalTransform::from(p.entity_transform(rotation)).affine(),
                &request,
                "native_mesh_generator",
            );
        }
        if let Some(p) = world.get::<ProfileExtrusion>(entity) {
            if p.profile.segments.len() > 512 {
                return Err("Scenario profile exceeds measurement generation bound".into());
            }
            return observe(
                &p.to_bevy_mesh(rotation),
                GlobalTransform::from(p.entity_transform(rotation)).affine(),
                &request,
                "native_mesh_generator",
            );
        }
    }
    Err("No supported direct evaluated mesh; aggregate and unsupported geometry require another measurement capability".into())
}

fn observe(
    mesh: &Mesh,
    transform: bevy::math::Affine3A,
    request: &Request,
    source: &str,
) -> Result<Value, String> {
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        return Err("Only triangle-list meshes are supported".into());
    }
    let Some(VertexAttributeValues::Float32x3(vertices)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return Err("Float32x3 mesh positions unavailable".into());
    };
    let index_count = mesh.indices().map_or(vertices.len(), |i| i.len());
    if vertices.len() > MAX_VERTICES || index_count > MAX_TRIANGLES * 3 || index_count % 3 != 0 {
        return Err("Mesh exceeds measurement bounds or has incomplete triangles".into());
    }
    let total = index_count / 3;
    if request.triangle_offset > total {
        return Err("triangle_offset exceeds mesh".into());
    }
    let end = request
        .triangle_offset
        .saturating_add(request.triangle_limit)
        .min(total);
    let index = |n: usize| -> usize {
        match mesh.indices() {
            Some(Indices::U16(v)) => v[n] as usize,
            Some(Indices::U32(v)) => v[n] as usize,
            None => n,
        }
    };
    let mut triangles = Vec::with_capacity(end - request.triangle_offset);
    for t in request.triangle_offset..end {
        let mut points = [Vec3::ZERO; 3];
        for j in 0..3 {
            points[j] = transform.transform_point3(Vec3::from(
                *vertices.get(index(t * 3 + j)).ok_or("Invalid mesh index")?,
            ));
            if !points[j].is_finite() {
                return Err("Non-finite evaluated vertex".into());
            }
        }
        let normal = (points[1] - points[0]).cross(points[2] - points[0]);
        let unit = normal.try_normalize().map(|v| v.to_array());
        triangles.push(json!({"triangle_index":t,"vertices":points.map(|p| p.to_array()),"normal":unit,"degenerate":unit.is_none()}));
    }
    use sha2::{Digest, Sha256};
    // A bounded whole-mesh fingerprint detects edits between pages, including
    // edits outside the requested page. Hash without allocating another mesh.
    let mut hash = Sha256::new();
    for v in vertices {
        for x in v {
            hash.update(x.to_le_bytes());
        }
    }
    for n in 0..index_count {
        hash.update((index(n) as u64).to_le_bytes());
    }
    for x in transform.to_cols_array() {
        hash.update(x.to_le_bytes());
    }
    Ok(
        json!({"schema":"talos3d.geometry-measurement.v1","element_id":request.element_id,
        "source":source,"coordinates":"world_metres","coverage":"direct_element_mesh_only",
        "geometry_sha256":hex::encode(hash.finalize()),"total_triangles":total,
        "triangle_offset":request.triangle_offset,"triangles":triangles,
        "next_triangle_offset":(end < total).then_some(end),"complete_mesh":request.triangle_offset == 0 && end == total,
        "notice":"Triangle indices are snapshot-local, not stable authored face IDs. All pages must have the same geometry_sha256. Measurements alone confer no construction or release approval."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req(offset: usize, limit: usize) -> Request {
        Request {
            element_id: Some(1),
            triangle_offset: offset,
            triangle_limit: limit,
        }
    }
    #[test]
    fn reads_actual_transformed_mesh_and_fingerprints_all_pages_without_mutating_assets() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        let handle = world
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2., 4., 6.));
        let transform = Transform::from_translation(Vec3::new(7., 8., 9.))
            .with_rotation(Quat::from_rotation_z(0.3));
        world.spawn((
            ElementId(1),
            Mesh3d(handle.clone()),
            GlobalTransform::from(transform),
        ));
        let full = inspect(&world, req(0, 256)).unwrap();
        assert_eq!(full["total_triangles"], 12);
        assert_eq!(full["complete_mesh"], true);
        let first = inspect(&world, req(0, 6)).unwrap();
        let second = inspect(&world, req(6, 6)).unwrap();
        assert_eq!(first["geometry_sha256"], second["geometry_sha256"]);
        assert_eq!(
            first["triangles"].as_array().unwrap(),
            &full["triangles"].as_array().unwrap()[..6]
        );
        for triangle in full["triangles"].as_array().unwrap() {
            for point in triangle["vertices"].as_array().unwrap() {
                let p: [f32; 3] = serde_json::from_value(point.clone()).unwrap();
                let local = transform
                    .compute_affine()
                    .inverse()
                    .transform_point3(Vec3::from(p));
                assert!((local.x.abs() - 1.).abs() < 1e-5);
                assert!((local.y.abs() - 2.).abs() < 1e-5);
                assert!((local.z.abs() - 3.).abs() < 1e-5);
            }
        }
        assert_eq!(world.resource::<Assets<Mesh>>().len(), 1);
        // An edit outside the first page still invalidates its fingerprint.
        let mut mesh = world
            .resource_mut::<Assets<Mesh>>()
            .into_inner()
            .get_mut(&handle)
            .unwrap();
        if let Some(VertexAttributeValues::Float32x3(v)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        {
            v.last_mut().unwrap()[0] += 0.25;
        }
        drop(mesh);
        assert_ne!(
            first["geometry_sha256"],
            inspect(&world, req(0, 6)).unwrap()["geometry_sha256"]
        );
        assert!(inspect(&world, req(13, 6)).is_err());
        assert!(inspect(&world, req(0, 257)).is_err());
    }
    #[test]
    fn live_missing_mesh_is_not_reconstructed_from_authored_parameters() {
        let mut world = World::new();
        world.spawn((
            ElementId(1),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::ONE,
            },
        ));
        assert!(inspect(&world, req(0, 64)).is_err());
        world.insert_resource(document_dispatch::DocumentCallBudget(4));
        assert_eq!(
            inspect(&world, req(0, 64)).unwrap()["source"],
            "native_mesh_generator"
        );
    }
    #[test]
    fn generated_router_serves_contract_and_propagates_measurement_errors() {
        let mut world = World::new();
        let contract =
            document_dispatch::execute(&mut world, "inspect_element_geometry", json!({}), false)
                .unwrap();
        assert_eq!(contract["coordinates"], "world_metres");
        assert!(document_dispatch::execute(
            &mut world,
            "inspect_element_geometry",
            json!({"element_id":999}),
            false
        )
        .is_err());
    }
}
