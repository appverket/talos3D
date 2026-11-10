//! Portable, render-only publication. No authored snapshots, names, IDs or URIs.
//! Collection runs once at publication; material batches become GPU primitives.
use crate::plugins::{
    definition_preview_scene::PreviewOnly, identity::ElementId, layers::entity_document_hidden,
    modeling::occurrence::GeneratedOccurrencePart,
};
use bevy::{
    mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
    prelude::*,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

const MAX_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
enum Layout {
    Indexed,
    FlatTriangles,
    ShadedTriangles,
}
type Batches = BTreeMap<([u32; 6], Layout), Batch>;

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
    // Compound occurrences generate detached meshes with an authored owner ID.
    // Owners may be logical nodes with no inherited visibility component.
    let visible_owners: HashSet<_> = world
        .query::<(Entity, &ElementId, Option<&InheritedVisibility>)>()
        .iter(world)
        .filter(|(e, _, v)| {
            v.is_none_or(|v| v.get())
                && !entity_document_hidden(world, *e)
                && world.get::<PreviewOnly>(*e).is_none()
        })
        .map(|(_, id, _)| *id)
        .collect();
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
    let mut batches = Batches::new();
    let mut binary_size = 0usize;
    for (entity, handle, transform, material) in entries {
        let mut current = Some(entity);
        let mut authored = false;
        while let Some(e) = current {
            if world.get::<PreviewOnly>(e).is_some() {
                break;
            }
            if let Some(id) = world
                .get::<GeneratedOccurrencePart>(e)
                .map(|p| p.owner)
                .or_else(|| world.get::<ElementId>(e).copied())
            {
                authored = visible_owners.contains(&id);
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
        if !local_indices.len().is_multiple_of(3)
            || local_indices.iter().any(|i| *i as usize >= positions.len())
        {
            return Err("Invalid model triangles".into());
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
        // Imported triangle soups often repeat both the implicit sequential
        // indices and the geometric face normal for every vertex. glTF can
        // represent those triangles directly; keep authored smooth normals.
        if local_indices.len() <= positions.len().saturating_mul(4) / 3 {
            let generated;
            let normals = match normals {
                Some(values) => values.as_slice(),
                None => {
                    generated = local_vertex_normals(positions, &local_indices);
                    &generated
                }
            };
            for tri in local_indices.as_chunks::<3>().0 {
                let mut corners = tri.map(|i| i as usize);
                if determinant < 0. {
                    corners.swap(1, 2);
                }
                let points =
                    corners.map(|i| affine.transform_point3(Vec3::from_array(positions[i])));
                if !points.iter().all(|p| p.is_finite()) {
                    return Err("Cannot publish non-finite geometry".into());
                }
                let face = (points[1] - points[0])
                    .cross(points[2] - points[0])
                    .normalize_or_zero();
                let shading = corners
                    .map(|i| (normal_matrix * Vec3::from_array(normals[i])).normalize_or_zero());
                // Keep distinct shading, including reversed normals. Agreement
                // within 0.82 degrees is visually flat at publication precision.
                let flat = shading.iter().all(|n| n.dot(face) >= 0.9999);
                let layout = if flat {
                    Layout::FlatTriangles
                } else {
                    Layout::ShadedTriangles
                };
                reserve_bytes(&mut binary_size, if flat { 36 } else { 72 })?;
                let batch = batches.entry((key, layout)).or_default();
                batch.positions.extend(points.map(|v| v.to_array()));
                if !flat {
                    batch.normals.extend(shading.map(|v| v.to_array()));
                }
            }
            continue;
        }
        reserve_bytes(
            &mut binary_size,
            positions
                .len()
                .saturating_mul(24)
                .saturating_add(local_indices.len().saturating_mul(4)),
        )?;
        let batch = batches.entry((key, Layout::Indexed)).or_default();
        let base = batch.positions.len() as u32;
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
        for tri in local_indices.as_chunks::<3>().0 {
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
/// Area-weighted normals for a mesh without authored shading. Keep shared
/// vertices smooth even if this mesh is encoded as unindexed triangles.
fn local_vertex_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut result = vec![Vec3::ZERO; positions.len()];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| Vec3::from_array(positions[i as usize]));
        let normal = (b - a).cross(c - a);
        for i in tri {
            result[*i as usize] += normal;
        }
    }
    result.into_iter().map(|n| n.to_array()).collect()
}

fn reserve_bytes(total: &mut usize, bytes: usize) -> Result<(), String> {
    *total = total.saturating_add(bytes);
    if *total > MAX_BYTES {
        return Err("Shared view exceeds 64 MiB; reduce model detail".into());
    }
    Ok(())
}

fn encode(batches: Batches) -> Result<Vec<u8>, String> {
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
    for ((key, _), mut batch) in batches {
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
        let mut primitive = json!({"attributes":{"POSITION":pos},"mode":4});
        if !batch.normals.is_empty() {
            primitive["attributes"]["NORMAL"] = json!(attribute(
                &mut bin,
                &mut views,
                &mut accessors,
                &floats(&batch.normals),
                batch.normals.len(),
                5126,
                "VEC3",
                None,
            ));
        }
        if !batch.indices.is_empty() {
            let indices: Vec<_> = batch.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            primitive["indices"] = json!(attribute(
                &mut bin,
                &mut views,
                &mut accessors,
                &indices,
                batch.indices.len(),
                5125,
                "SCALAR",
                None,
            ));
        }
        let color = key[..4]
            .iter()
            .map(|v| f32::from_bits(*v))
            .collect::<Vec<_>>();
        let mat = materials.len();
        materials.push(json!({"pbrMetallicRoughness":{"baseColorFactor":color,"roughnessFactor":f32::from_bits(key[4]),"metallicFactor":f32::from_bits(key[5])},"alphaMode":if color[3]<1.0 {"BLEND"} else {"OPAQUE"},"doubleSided":true}));
        primitive["material"] = json!(mat);
        primitives.push(primitive);
    }
    let doc = json!({"asset":{"version":"2.0","generator":"Talos3D"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"meshes":[{"primitives":primitives}],"materials":materials,"accessors":accessors,"bufferViews":views,"buffers":[{"byteLength":bin.len()}]});
    let mut json = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    while !bin.len().is_multiple_of(4) {
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
    fn detached_occurrence_retains_visible_triangles_and_explicit_shading_only_where_needed() {
        use crate::plugins::{layers::DocumentVisibility, modeling::definition::DefinitionId};
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, Default::default());
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
            ],
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            vec![
                [0., 0., 1.],
                [0., 0., 1.],
                [0., 0., 1.],
                [0., 0., 1.],
                [0., 1., 0.],
                [0., 0., 1.],
            ],
        );
        let mesh = world.resource_mut::<Assets<Mesh>>().add(mesh);
        let owner = ElementId(7);
        world.spawn(owner);
        world.spawn((
            GeneratedOccurrencePart {
                owner,
                slot_path: "private".into(),
                definition_id: DefinitionId("private".into()),
            },
            Mesh3d(mesh),
            GlobalTransform::IDENTITY,
            InheritedVisibility::VISIBLE,
        ));
        let bytes = capture_view_glb(&mut world).unwrap();
        let n = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let doc: Value = serde_json::from_slice(&bytes[20..20 + n]).unwrap();
        let primitives = doc["meshes"][0]["primitives"].as_array().unwrap();
        assert_eq!(primitives.len(), 2);
        assert!(primitives.iter().all(|p| p.get("indices").is_none()));
        assert_eq!(
            primitives
                .iter()
                .filter(|p| p["attributes"].get("NORMAL").is_some())
                .count(),
            1
        );
        for p in primitives {
            let pos = p["attributes"]["POSITION"].as_u64().unwrap() as usize;
            assert_eq!(doc["accessors"][pos]["count"], 3);
        }
        world.insert_resource(DocumentVisibility {
            hidden: HashSet::from([owner]),
        });
        assert!(capture_view_glb(&mut world)
            .unwrap_err()
            .contains("no visible"));
    }
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
