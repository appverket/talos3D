//! Portable, render-only publication. No authored snapshots, names, IDs or URIs.
//! Collection runs once at publication; material batches become GPU primitives.
use crate::plugins::identity::ElementId;
use bevy::{
    mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
    prelude::*,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_BYTES: usize = 64 * 1024 * 1024;
#[derive(Default)]
struct Batch {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

/// Publish all non-hidden authored geometry, including parts outside the camera
/// frustum. Helpers without an authored ancestor (grid, grips, previews) stay out.
/// Texture images are intentionally not published; PBR base colors are retained.
pub fn capture_view_glb(world: &mut World) -> Result<Vec<u8>, String> {
    if world
        .get_resource::<crate::plugins::history::PendingCommandQueue>()
        .is_some_and(|q| !q.is_empty())
        || world
            .get_resource::<crate::plugins::transform::ActiveTransformPreview>()
            .is_some_and(|p| p.interaction.is_some())
    {
        return Err("Finish the current edit before publishing a view".into());
    }
    if !crate::plugins::vector_drawing::collect_active_clip_planes(world).is_empty() {
        return Err("Turn off section cuts before publishing this model".into());
    }
    let entries: Vec<_> = world
        .query::<(
            Entity,
            &Mesh3d,
            &GlobalTransform,
            &InheritedVisibility,
            Option<&MeshMaterial3d<StandardMaterial>>,
        )>()
        .iter(world)
        .filter(|(_, _, _, v, _)| v.get())
        .map(|(e, m, t, _, mat)| (e, m.0.clone(), *t, mat.map(|m| m.0.clone())))
        .collect();
    let meshes = world.resource::<Assets<Mesh>>();
    let materials = world.get_resource::<Assets<StandardMaterial>>();
    let mut batches: BTreeMap<[u32; 6], Batch> = BTreeMap::new();
    let mut vertex_count = 0usize;
    for (entity, handle, transform, material) in entries {
        let mut current = Some(entity);
        let mut authored = false;
        while let Some(e) = current {
            if world.get::<ElementId>(e).is_some() {
                authored = true;
                break;
            }
            current = world.get::<ChildOf>(e).map(|p| p.parent());
        }
        if !authored {
            continue;
        }
        let Some(mesh) = meshes.get(&handle) else {
            return Err("Model geometry is still loading".into());
        };
        if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
            continue;
        }
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };
        let local_indices: Vec<u32> = match mesh.indices() {
            Some(Indices::U16(v)) => v.iter().map(|i| *i as u32).collect(),
            Some(Indices::U32(v)) => v.clone(),
            None => (0..positions.len() as u32).collect(),
        };
        if local_indices.is_empty() {
            continue;
        }
        if local_indices.len() % 3 != 0
            || local_indices.iter().any(|i| *i as usize >= positions.len())
        {
            return Err("Invalid model triangles".into());
        }
        vertex_count += positions.len();
        // Bound memory before allocating the publication buffers.
        if vertex_count > MAX_BYTES / 36 {
            return Err("Shared view exceeds 64 MB; reduce model detail".into());
        }
        let mat = material
            .as_ref()
            .and_then(|h| materials.and_then(|a| a.get(h)));
        let color = mat
            .map(|m| m.base_color.to_linear().to_f32_array())
            .unwrap_or([0.7, 0.7, 0.7, 1.0]);
        let key = [
            color[0].to_bits(),
            color[1].to_bits(),
            color[2].to_bits(),
            color[3].to_bits(),
            mat.map_or(0.8, |m| m.perceptual_roughness).to_bits(),
            mat.map_or(0.0, |m| m.metallic).to_bits(),
        ];
        let batch = batches.entry(key).or_default();
        let base = batch.positions.len() as u32;
        let affine = transform.affine();
        let determinant = affine.matrix3.determinant();
        if !determinant.is_finite() || determinant.abs() < 1e-12 {
            return Err("Cannot publish singular model transform".into());
        }
        let normal_matrix = affine.matrix3.inverse().transpose();
        let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(v)) if v.len() == positions.len() => Some(v),
            _ => None,
        };
        for (i, p) in positions.iter().enumerate() {
            let p = affine.transform_point3(Vec3::from_array(*p));
            if !p.is_finite() {
                return Err("Cannot publish non-finite geometry".into());
            }
            batch.positions.push(p.to_array());
            let n = normals
                .map(|v| normal_matrix * Vec3::from_array(v[i]))
                .unwrap_or(Vec3::ZERO)
                .normalize_or_zero();
            batch.normals.push(n.to_array());
        }
        for tri in local_indices.chunks_exact(3) {
            let [a, b, c] = if determinant < 0.0 {
                [tri[0], tri[2], tri[1]]
            } else {
                [tri[0], tri[1], tri[2]]
            };
            batch.indices.extend([base + a, base + b, base + c]);
            if normals.is_none() {
                let p = |i: u32| Vec3::from_array(batch.positions[i as usize]);
                let n = (p(base + b) - p(base + a)).cross(p(base + c) - p(base + a));
                for i in [base + a, base + b, base + c] {
                    let old = Vec3::from_array(batch.normals[i as usize]);
                    batch.normals[i as usize] = (old + n).to_array();
                }
            }
        }
    }
    if batches.is_empty() {
        return Err("The model has no visible 3D geometry to share".into());
    }
    encode(batches)
}
fn encode(batches: BTreeMap<[u32; 6], Batch>) -> Result<Vec<u8>, String> {
    let (mut bin, mut views, mut accessors, mut materials, mut primitives) = (
        Vec::new(),
        Vec::<Value>::new(),
        Vec::<Value>::new(),
        Vec::new(),
        Vec::new(),
    );
    fn attribute(
        bin: &mut Vec<u8>,
        views: &mut Vec<Value>,
        accessors: &mut Vec<Value>,
        bytes: &[u8],
        count: usize,
        component: u32,
        kind: &str,
        bounds: Option<([f32; 3], [f32; 3])>,
    ) -> usize {
        let view = views.len();
        views.push(json!({"buffer":0,"byteOffset":bin.len(),"byteLength":bytes.len(),"target":if kind=="SCALAR" {34963} else {34962}}));
        bin.extend(bytes);
        let id = accessors.len();
        let mut a = json!({"bufferView":view,"componentType":component,"count":count,"type":kind});
        if let Some((min, max)) = bounds {
            a["min"] = json!(min);
            a["max"] = json!(max);
        }
        accessors.push(a);
        id
    }
    for (key, mut batch) in batches {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in &batch.positions {
            min = min.min(Vec3::from_array(*p));
            max = max.max(Vec3::from_array(*p));
        }
        let floats = |v: &[[f32; 3]]| {
            v.iter()
                .flatten()
                .flat_map(|f| f.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        for n in &mut batch.normals {
            *n = Vec3::from_array(*n).normalize_or_zero().to_array();
        }
        let pos = attribute(
            &mut bin,
            &mut views,
            &mut accessors,
            &floats(&batch.positions),
            batch.positions.len(),
            5126,
            "VEC3",
            Some((min.to_array(), max.to_array())),
        );
        let norm = attribute(
            &mut bin,
            &mut views,
            &mut accessors,
            &floats(&batch.normals),
            batch.normals.len(),
            5126,
            "VEC3",
            None,
        );
        let indices: Vec<u8> = batch.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let idx = attribute(
            &mut bin,
            &mut views,
            &mut accessors,
            &indices,
            batch.indices.len(),
            5125,
            "SCALAR",
            None,
        );
        let color = key[..4]
            .iter()
            .map(|v| f32::from_bits(*v))
            .collect::<Vec<_>>();
        let mat = materials.len();
        materials.push(json!({"pbrMetallicRoughness":{"baseColorFactor":color,"roughnessFactor":f32::from_bits(key[4]),"metallicFactor":f32::from_bits(key[5])},"alphaMode":if color[3]<1.0 {"BLEND"} else {"OPAQUE"},"doubleSided":true}));
        primitives.push(json!({"attributes":{"POSITION":pos,"NORMAL":norm},"indices":idx,"material":mat,"mode":4}));
    }
    let doc = json!({"asset":{"version":"2.0","generator":"Talos3D"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"meshes":[{"primitives":primitives}],"materials":materials,"accessors":accessors,"bufferViews":views,"buffers":[{"byteLength":bin.len()}]});
    let mut json = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let length = 12 + 8 + json.len() + 8 + bin.len();
    if length > MAX_BYTES {
        return Err("Shared view exceeds 64 MB".into());
    }
    let mut out = Vec::with_capacity(length);
    for v in [0x46546c67, 2, length as u32, json.len() as u32, 0x4e4f534a] {
        out.extend(v.to_le_bytes());
    }
    out.extend(json);
    for v in [bin.len() as u32, 0x004e4942] {
        out.extend(v.to_le_bytes());
    }
    out.extend(bin);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_uses_authored_ancestry_visibility_world_transforms_and_material_batches() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        let mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2., 2., 2.));
        let parent = world.spawn(ElementId(42)).id();
        for x in [0., 4.] {
            world.spawn((
                ChildOf(parent),
                Mesh3d(mesh.clone()),
                GlobalTransform::from(Transform::from_xyz(x, 2., 0.)),
                InheritedVisibility::VISIBLE,
            ));
        }
        world.spawn((
            Mesh3d(mesh.clone()),
            GlobalTransform::IDENTITY,
            InheritedVisibility::VISIBLE,
        )); // helper
        world.spawn((
            ElementId(99),
            Mesh3d(mesh),
            GlobalTransform::IDENTITY,
            InheritedVisibility::HIDDEN,
        ));
        let bytes = capture_view_glb(&mut world).unwrap();
        if let Ok(path) = std::env::var("TALOS_TEST_VIEW_OUTPUT") {
            std::fs::write(path, &bytes).unwrap();
        }
        assert_eq!(&bytes[..4], b"glTF");
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize,
            bytes.len()
        );
        let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let doc: Value = serde_json::from_slice(&bytes[20..20 + len]).unwrap();
        assert_eq!(doc["meshes"][0]["primitives"].as_array().unwrap().len(), 1);
        assert_eq!(doc["accessors"][0]["count"], 48);
        assert_eq!(doc["accessors"][0]["min"], json!([-1., 1., -1.]));
        assert_eq!(doc["accessors"][0]["max"], json!([5., 3., 1.]));
        assert!(!String::from_utf8_lossy(&bytes).contains("element_"));
        assert_eq!(len % 4, 0);
        world.spawn(crate::plugins::clipping_planes::ClipPlaneNode::at_y(2.));
        assert!(capture_view_glb(&mut world)
            .unwrap_err()
            .contains("section cuts"));
    }
}
