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
    /// Alternatively measure minimum surface distance for explicit element pairs.
    /// At most 32 pairs and 16 distinct directly meshed elements, each <=256 triangles.
    /// Does not classify solid containment, expected contact or structural support.
    #[serde(default)]
    #[schemars(length(max = 32))]
    pub pairs: Vec<[u64; 2]>,
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
        "observations":["triangle_vertices","triangle_normals","pair_surface_distance","pair_solid_interference"],
        "ordinary_source":"current_direct_render_mesh","scenario_source":"native_mesh_generator",
        "coverage":"one directly meshed element; not an assembly, child hierarchy or entire document",
        "limits":{"vertices":MAX_VERTICES,"triangles":MAX_TRIANGLES,"page_triangles":MAX_PAGE,"pairs":32,"pair_elements":16,"pair_mesh_triangles":MAX_PAGE},
        "unsupported":"Missing direct meshes, unsupported topology and exceeded bounds are errors. No fallback to bounding boxes.",
        "acceptance":false})
}

pub fn inspect(world: &World, request: Request) -> Result<Value, String> {
    if !request.pairs.is_empty() {
        if request.element_id.is_some() || request.triangle_offset != 0 {
            return Err("Choose either one paged element or explicit pairs".into());
        }
        return inspect_pairs(world, &request.pairs);
    }
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

/// Exact solid interference for candidate member pairs (for example AABB overlaps),
/// one bounded pair measurement per pair. Returns interpenetrating pairs deeper than
/// `tolerance_m`, plus pairs that could not be measured. Offline use only.
pub(crate) fn interpenetrating_pairs(world: &World, pairs: &[[u64; 2]], tolerance_m: f64) -> Value {
    const MAX_PAIRS: usize = 128;
    let mut interpenetrating = Vec::new();
    let mut unmeasured = Vec::new();
    for pair in pairs.iter().take(MAX_PAIRS) {
        match inspect_pairs(world, &[*pair]) {
            Ok(result) => {
                let interference = &result["pairs"][0]["solid_interference"];
                let depth = interference["penetration_depth_m"].as_f64().unwrap_or(0.0);
                if depth > tolerance_m {
                    interpenetrating.push(json!({"elements":pair,"penetration_depth_m":depth,
                        "exact":interference["exact"]}));
                }
            }
            Err(reason) => unmeasured.push(json!({"elements":pair,"reason":reason})),
        }
    }
    json!({"method":"convex_hull_separating_axes","tolerance_m":tolerance_m,
        "pairs_checked":pairs.len().min(MAX_PAIRS),"truncated":pairs.len() > MAX_PAIRS,
        "interpenetrating":interpenetrating,"unmeasured":unmeasured,
        "notice":"Members meeting at an angle should be cut or mitred so they neither interpenetrate nor leave gaps; listed pairs interpenetrate. Use inspect_element_geometry pairs for gap distances."})
}

/// On-demand BVHs are built once per distinct selected mesh, reused for the batch
/// and dropped on return. No scene-wide geometry rebuild or persistent mesh copy.
fn inspect_pairs(world: &World, pairs: &[[u64; 2]]) -> Result<Value, String> {
    use parry3d::{
        math::{Pose, Vector},
        shape::TriMesh,
    };
    use std::collections::{BTreeMap, BTreeSet};
    let ids: BTreeSet<_> = pairs.iter().flatten().copied().collect();
    if pairs.len() > 32 || ids.len() > 16 || pairs.iter().any(|p| p[0] == p[1]) {
        return Err(
            "Pair measurement requires <=32 distinct-element pairs and <=16 elements".into(),
        );
    }
    let mut meshes = BTreeMap::new();
    for id in ids {
        let observation = inspect(
            world,
            Request {
                element_id: Some(id),
                pairs: Vec::new(),
                triangle_offset: 0,
                triangle_limit: MAX_PAGE,
            },
        )?;
        if observation["complete_mesh"] != true {
            return Err(format!(
                "Element {id} exceeds the complete pair-mesh bound of 256 triangles"
            ));
        }
        let triangles = observation["triangles"]
            .as_array()
            .ok_or("Missing measured triangles")?;
        let mut vertices = Vec::with_capacity(triangles.len() * 3);
        let mut indices = Vec::with_capacity(triangles.len());
        for (index, triangle) in triangles.iter().enumerate() {
            if triangle["degenerate"] != false {
                return Err(format!(
                    "Element {id} has a degenerate triangle; pair measurement unavailable"
                ));
            }
            for point in triangle["vertices"]
                .as_array()
                .ok_or("Missing measured vertices")?
            {
                let xyz = point.as_array().ok_or("Invalid measured vertex")?;
                let component = |i: usize| {
                    xyz[i]
                        .as_f64()
                        .ok_or("Invalid measured coordinate")
                        .map(|v| v as f32)
                };
                vertices.push(Vector::new(component(0)?, component(1)?, component(2)?));
            }
            let base = (index * 3) as u32;
            indices.push([base, base + 1, base + 2]);
        }
        let hull = SolidHull::new(&vertices, &indices);
        let mesh = TriMesh::new(vertices, indices).map_err(|_| "Invalid measured triangle mesh")?;
        meshes.insert(id, (mesh, observation, hull));
    }
    let mut results = Vec::with_capacity(pairs.len());
    for [a, b] in pairs {
        let (ma, oa, ha) = &meshes[a];
        let (mb, ob, hb) = &meshes[b];
        let distance = parry3d::query::distance(&Pose::IDENTITY, ma, &Pose::IDENTITY, mb)
            .map_err(|_| "Evaluated surface pair is unsupported")?;
        if !distance.distance.is_finite() {
            return Err("Non-finite surface distance".into());
        }
        results.push(
            json!({"elements":[a,b],"minimum_surface_distance_m":distance.distance,
            "solid_interference":solid_interference(ha, hb)?,
            "nearest_triangle_a":oa["triangles"].get(distance.subshape1 as usize),
            "nearest_triangle_b":ob["triangles"].get(distance.subshape2 as usize)}),
        );
    }
    Ok(
        json!({"schema":"talos3d.geometry-pairs.v1","coordinates":"world_metres",
        "meshes":meshes.iter().map(|(id,(_,o,_))| json!({"element_id":id,
            "geometry_sha256":o["geometry_sha256"],"source":o["source"],"total_triangles":o["total_triangles"]})).collect::<Vec<_>>(),
        "pairs":results,"acceptance":false,
        "limitations":["Surface distance alone: positive distance does not exclude complete solid containment; use solid_interference.",
            "solid_interference is exact for convex members; for non-convex members (convex=false) penetration is an upper bound from convex hulls.",
            "Members that meet at an angle without cut or mitred ends show either penetration_depth_m > 0 (interpenetration) or minimum_surface_distance_m > 0 (gap); judging the intended joint and tolerance remains with the caller.",
            "Direct current render meshes only; no aggregate traversal or bounding-box substitute."]}),
    )
}

/// One measured member as a convex solid: its hull (for exact separation) and its
/// candidate separating directions (face normals and edge directions) for SAT
/// penetration depth. `convex` is false when the mesh is not its own hull, in which
/// case penetration is an upper bound.
struct SolidHull {
    hull: Option<parry3d::shape::ConvexPolyhedron>,
    points: Vec<Vec3>,
    normals: Vec<Vec3>,
    edges: Vec<Vec3>,
    convex: bool,
}

/// Bound on separating-axis candidates per pair so the test stays O(axes * points).
const MAX_SAT_AXES: usize = 4096;

fn push_unique(directions: &mut Vec<Vec3>, candidate: Vec3) {
    if let Some(unit) = candidate.try_normalize() {
        if !directions.iter().any(|d| d.dot(unit).abs() > 1.0 - 1e-6) {
            directions.push(unit);
        }
    }
}

impl SolidHull {
    fn new(vertices: &[parry3d::math::Vector], triangles: &[[u32; 3]]) -> Self {
        let point = |i: u32| {
            let v = vertices[i as usize];
            Vec3::new(v.x, v.y, v.z)
        };
        let mesh_volume = triangles
            .iter()
            .map(|t| point(t[0]).dot(point(t[1]).cross(point(t[2]))) / 6.0)
            .sum::<f32>()
            .abs();
        let hull = parry3d::shape::ConvexPolyhedron::from_convex_hull(vertices);
        let hull_volume = hull.as_ref().map_or(0.0, |h| {
            use parry3d::shape::Shape;
            h.mass_properties(1.0).mass()
        });
        let (mut normals, mut edges) = (Vec::new(), Vec::new());
        for t in triangles {
            let [a, b, c] = t.map(point);
            push_unique(&mut normals, (b - a).cross(c - a));
            for edge in [b - a, c - b, a - c] {
                push_unique(&mut edges, edge);
            }
        }
        let points = hull.as_ref().map_or_else(Vec::new, |h| {
            h.points()
                .iter()
                .map(|v| Vec3::new(v.x, v.y, v.z))
                .collect()
        });
        let convex = hull.is_some() && hull_volume <= mesh_volume * 1.001 + 1e-12;
        Self {
            hull,
            points,
            normals,
            edges,
            convex,
        }
    }

    fn interval(&self, axis: Vec3) -> (f32, f32) {
        self.points
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                let d = p.dot(axis);
                (lo.min(d), hi.max(d))
            })
    }
}

/// Separation from the exact hull distance; when hulls touch, penetration depth
/// is the minimum overlap over all separating-axis candidates (exact for convex).
fn solid_interference(a: &SolidHull, b: &SolidHull) -> Result<Value, String> {
    use parry3d::math::Pose;
    let (Some(ha), Some(hb)) = (&a.hull, &b.hull) else {
        return Ok(json!({"available":false,"reason":"degenerate member hull"}));
    };
    let separation = parry3d::query::distance(&Pose::IDENTITY, ha, &Pose::IDENTITY, hb)
        .map_err(|_| "Solid interference is unsupported for this pair")?
        .distance;
    if !separation.is_finite() {
        return Err("Non-finite solid interference".into());
    }
    let mut axes: Vec<Vec3> = a.normals.iter().chain(&b.normals).copied().collect();
    let mut complete = true;
    'edges: for ea in &a.edges {
        for eb in &b.edges {
            if axes.len() >= MAX_SAT_AXES {
                complete = false;
                break 'edges;
            }
            push_unique(&mut axes, ea.cross(*eb));
        }
    }
    let penetration = if separation > 0.0 {
        0.0
    } else {
        axes.iter()
            .map(|axis| {
                let ((alo, ahi), (blo, bhi)) = (a.interval(*axis), b.interval(*axis));
                (ahi.min(bhi) - alo.max(blo)).max(0.0)
            })
            .fold(f32::MAX, f32::min)
    };
    let exact = a.convex && b.convex && complete;
    Ok(
        json!({"available":true,"method":"convex_hull_separating_axes",
        "intersecting":penetration > 1e-6,
        "penetration_depth_m":penetration,
        "separation_m":separation,
        "convex":a.convex && b.convex,
        "exact":exact}),
    )
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
            pairs: Vec::new(),
            triangle_offset: offset,
            triangle_limit: limit,
        }
    }
    fn pair_request(pairs: Vec<[u64; 2]>) -> Request {
        Request {
            element_id: None,
            pairs,
            triangle_offset: 0,
            triangle_limit: 64,
        }
    }
    fn mesh_element(world: &mut World, id: u64, size: Vec3, transform: Transform) {
        let handle = world
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::from_size(size));
        world.spawn((
            ElementId(id),
            Mesh3d(handle),
            GlobalTransform::from(transform),
        ));
    }
    /// Closed prism mesh: convex polygon in XY extruded along Z.
    fn prism_element(world: &mut World, id: u64, outline: &[[f32; 2]], depth: f32) {
        let ring = |z: f32| outline.iter().map(|p| [p[0], p[1], z]).collect::<Vec<_>>();
        let (bottom, top) = (ring(0.0), ring(depth));
        let mut triangles = Vec::new();
        for i in 1..outline.len() - 1 {
            triangles.extend([bottom[0], bottom[i + 1], bottom[i]]);
            triangles.extend([top[0], top[i], top[i + 1]]);
        }
        for i in 0..outline.len() {
            let j = (i + 1) % outline.len();
            triangles.extend([bottom[i], bottom[j], top[j], bottom[i], top[j], top[i]]);
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, Default::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, triangles);
        let handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
        world.spawn((ElementId(id), Mesh3d(handle), GlobalTransform::IDENTITY));
    }

    #[test]
    fn mitred_corner_reports_neither_interpenetration_nor_gap() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        // Two 100 mm members meeting at a right angle on a shared 45-degree cut.
        prism_element(
            &mut world,
            1,
            &[[0.0, 0.0], [1.0, 0.0], [0.9, 0.1], [0.0, 0.1]],
            0.05,
        );
        prism_element(
            &mut world,
            2,
            &[[1.0, 0.0], [1.0, 1.0], [0.9, 1.0], [0.9, 0.1]],
            0.05,
        );
        let result = inspect(&world, pair_request(vec![[1, 2]])).unwrap();
        let interference = &result["pairs"][0]["solid_interference"];
        assert_eq!(interference["exact"], true);
        assert!(
            interference["penetration_depth_m"].as_f64().unwrap() < 1e-5,
            "{interference}"
        );
        assert!(
            interference["separation_m"].as_f64().unwrap() < 1e-5,
            "{interference}"
        );
    }

    #[test]
    fn angled_members_report_interpenetration_or_gap_not_just_touching() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        // A horizontal member and a 30-degree member whose square end runs into it
        // (no mitre): surfaces cross, so only penetration depth reveals the defect.
        mesh_element(
            &mut world,
            1,
            Vec3::new(4.0, 0.1, 0.05),
            Transform::IDENTITY,
        );
        mesh_element(
            &mut world,
            2,
            Vec3::new(2.0, 0.1, 0.05),
            Transform::from_xyz(-1.0, 0.45, 0.0)
                .with_rotation(Quat::from_rotation_z(30f32.to_radians())),
        );
        // A member stopping 20 mm short of the horizontal one: a gap.
        mesh_element(
            &mut world,
            3,
            Vec3::new(0.1, 1.0, 0.05),
            Transform::from_xyz(1.5, 0.57, 0.0),
        );
        // A member resting exactly on the horizontal one: contact, no penetration.
        mesh_element(
            &mut world,
            4,
            Vec3::new(0.1, 1.0, 0.05),
            Transform::from_xyz(0.5, 0.55, 0.0),
        );
        let result = inspect(&world, pair_request(vec![[1, 2], [1, 3], [1, 4]])).unwrap();
        let interference = |i: usize| &result["pairs"][i]["solid_interference"];
        assert_eq!(interference(0)["intersecting"], true);
        assert!(interference(0)["penetration_depth_m"].as_f64().unwrap() > 0.01);
        assert_eq!(interference(0)["exact"], true);
        assert_eq!(interference(1)["intersecting"], false);
        assert!((interference(1)["separation_m"].as_f64().unwrap() - 0.02).abs() < 1e-4);
        assert!(
            (result["pairs"][1]["minimum_surface_distance_m"]
                .as_f64()
                .unwrap()
                - 0.02)
                .abs()
                < 1e-4
        );
        assert!(interference(2)["penetration_depth_m"].as_f64().unwrap() < 1e-4);
        assert!(interference(2)["separation_m"].as_f64().unwrap() < 1e-4);
    }

    #[test]
    fn pair_distances_use_evaluated_surfaces_and_do_not_claim_solid_containment() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        for (id, x, size) in [
            (1, 0., 2.),
            (2, 3., 2.),
            (3, 2., 2.),
            (4, 1., 2.),
            (5, 0., 0.5),
        ] {
            mesh_element(
                &mut world,
                id,
                Vec3::splat(size),
                Transform::from_xyz(x, 0., 0.),
            );
        }
        let result = inspect(&world, pair_request(vec![[1, 2], [1, 3], [1, 4], [1, 5]])).unwrap();
        for (index, expected) in [1., 0., 0., 0.75].into_iter().enumerate() {
            assert!(
                (result["pairs"][index]["minimum_surface_distance_m"]
                    .as_f64()
                    .unwrap()
                    - expected)
                    .abs()
                    < 1e-5
            );
            assert!(result["pairs"][index]["nearest_triangle_a"]["vertices"].is_array());
            assert!(result["pairs"][index]["nearest_triangle_b"]["vertices"].is_array());
        }
        assert_eq!(result["meshes"].as_array().unwrap().len(), 5);
        assert_eq!(result["acceptance"], false);
        assert!(result["limitations"][0]
            .as_str()
            .unwrap()
            .contains("containment"));
    }
    #[test]
    fn overlapping_aabbs_can_have_positive_evaluated_surface_distance() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        let rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4);
        mesh_element(
            &mut world,
            1,
            Vec3::new(2., 0.2, 0.2),
            Transform::from_rotation(rotation),
        );
        mesh_element(
            &mut world,
            2,
            Vec3::new(2., 0.2, 0.2),
            Transform::from_translation(rotation * Vec3::Z * 0.3).with_rotation(rotation),
        );
        let result = inspect(&world, pair_request(vec![[1, 2]])).unwrap();
        assert!(
            (result["pairs"][0]["minimum_surface_distance_m"]
                .as_f64()
                .unwrap()
                - 0.1)
                .abs()
                < 1e-5
        );
    }
    #[test]
    fn pair_bound_and_missing_mesh_fail_without_aabb_fallback() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        mesh_element(&mut world, 1, Vec3::ONE, Transform::IDENTITY);
        world.spawn((ElementId(2), GlobalTransform::IDENTITY));
        assert!(inspect(&world, pair_request(vec![[1, 2]])).is_err());
        assert!(inspect(&world, pair_request(vec![[1, 1]])).is_err());
        assert!(inspect(&world, pair_request(vec![[1, 2]; 33])).is_err());
        let mut conflicting = pair_request(vec![[1, 2]]);
        conflicting.element_id = Some(1);
        assert!(inspect(&world, conflicting).is_err());
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
