//! Export the model's 3D geometry to an interchange format.
//!
//! The Exchange menu could only produce 2D drawings (PNG/SVG/DXF/PDF), so a
//! model could be authored here but never handed to another 3D tool. This adds
//! the 3D direction.
//!
//! Geometry is taken from the generated render meshes rather than re-derived
//! from snapshots, so what is exported is exactly what the viewport shows —
//! including anything a capability produced procedurally.
//!
//! Wavefront OBJ is the first format: plain text, no dependencies, and read by
//! every 3D tool. The writer is factored so another format only has to supply a
//! serializer over the same collected [`ExportMesh`] list.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy::render::mesh::{Indices, VertexAttributeValues};
use serde_json::Value;

use crate::plugins::{
    command_registry::{CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult},
    identity::ElementId,
    ui::StatusBarData,
};

pub struct ModelExportPlugin;

impl Plugin for ModelExportPlugin {
    fn build(&self, app: &mut App) {
        app.register_command(
            CommandDescriptor {
                id: "core.export_model_obj".to_string(),
                label: "Export Model as OBJ...".to_string(),
                description: "Export the model's 3D geometry as a Wavefront OBJ file.".to_string(),
                category: CommandCategory::File,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Destination .obj path. Defaults to the document \
                                            name beside the current document."
                        }
                    }
                })),
                default_shortcut: None,
                icon: None,
                hint: Some("Export 3D geometry as Wavefront OBJ".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: None,
            },
            execute_export_model_obj,
        );
    }
}

/// One mesh in world space, ready to serialize.
#[derive(Debug, Clone)]
pub struct ExportMesh {
    /// Object name in the exported file — the element id keeps it traceable
    /// back to the authored model.
    pub name: String,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    /// Triangle indices into `positions`.
    pub indices: Vec<[u32; 3]>,
}

/// Collect every visible render mesh, baked into world space.
pub fn collect_export_meshes(world: &mut World) -> Vec<ExportMesh> {
    let entries: Vec<(Option<ElementId>, Handle<Mesh>, GlobalTransform)> = {
        let mut query = world.query::<(
            Option<&ElementId>,
            &Mesh3d,
            &GlobalTransform,
            &ViewVisibility,
        )>();
        query
            .iter(world)
            .filter(|(_, _, _, visibility)| visibility.get())
            .map(|(element_id, mesh, transform, _)| {
                (element_id.copied(), mesh.0.clone(), *transform)
            })
            .collect()
    };

    let assets = world.resource::<Assets<Mesh>>();
    entries
        .into_iter()
        .filter_map(|(element_id, handle, transform)| {
            let mesh = assets.get(&handle)?;
            let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION)? {
                VertexAttributeValues::Float32x3(values) => values,
                _ => return None,
            };
            let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
                Some(VertexAttributeValues::Float32x3(values)) => values.clone(),
                _ => Vec::new(),
            };
            let indices: Vec<u32> = match mesh.indices()? {
                Indices::U16(values) => values.iter().map(|index| *index as u32).collect(),
                Indices::U32(values) => values.clone(),
            };
            let affine = transform.affine();
            Some(ExportMesh {
                name: element_id
                    .map(|id| format!("element_{}", id.0))
                    .unwrap_or_else(|| "mesh".to_string()),
                positions: positions
                    .iter()
                    .map(|p| affine.transform_point3(Vec3::from_array(*p)))
                    .collect(),
                normals: normals
                    .iter()
                    .map(|n| {
                        affine
                            .transform_vector3(Vec3::from_array(*n))
                            .normalize_or_zero()
                    })
                    .collect(),
                indices: indices
                    .chunks_exact(3)
                    .map(|chunk| [chunk[0], chunk[1], chunk[2]])
                    .collect(),
            })
        })
        .filter(|mesh| !mesh.indices.is_empty())
        .collect()
}

/// Serialize meshes as Wavefront OBJ.
///
/// OBJ indices are 1-based and file-global, so each mesh's vertices are offset
/// by the running total rather than restarting per object.
pub fn write_obj(meshes: &[ExportMesh]) -> String {
    let mut out = String::from("# Exported by Talos3D\n");
    let mut position_base: u32 = 1;
    let mut normal_base: u32 = 1;
    for mesh in meshes {
        let _ = writeln!(out, "o {}", mesh.name);
        for position in &mesh.positions {
            let _ = writeln!(out, "v {} {} {}", position.x, position.y, position.z);
        }
        for normal in &mesh.normals {
            let _ = writeln!(out, "vn {} {} {}", normal.x, normal.y, normal.z);
        }
        let has_normals = mesh.normals.len() == mesh.positions.len();
        for [a, b, c] in &mesh.indices {
            if has_normals {
                let _ = writeln!(
                    out,
                    "f {}//{} {}//{} {}//{}",
                    position_base + a,
                    normal_base + a,
                    position_base + b,
                    normal_base + b,
                    position_base + c,
                    normal_base + c
                );
            } else {
                let _ = writeln!(
                    out,
                    "f {} {} {}",
                    position_base + a,
                    position_base + b,
                    position_base + c
                );
            }
        }
        position_base += mesh.positions.len() as u32;
        normal_base += mesh.normals.len() as u32;
    }
    out
}

pub fn export_model_obj_to_path(world: &mut World, path: PathBuf) -> Result<PathBuf, String> {
    let meshes = collect_export_meshes(world);
    if meshes.is_empty() {
        return Err("The model has no 3D geometry to export".to_string());
    }
    let contents = write_obj(&meshes);
    std::fs::write(&path, contents).map_err(|error| error.to_string())?;
    Ok(path)
}

fn execute_export_model_obj(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let path = match parameters.get("path").and_then(Value::as_str) {
        Some(path) => PathBuf::from(path),
        None => default_export_path(world)?,
    };
    let written = export_model_obj_to_path(world, path)?;
    if let Some(mut status) = world.get_resource_mut::<StatusBarData>() {
        status.set_feedback(format!("Exported {}", written.display()), 3.0);
    }
    Ok(CommandResult {
        output: Some(serde_json::json!({ "path": written.to_string_lossy() })),
        ..Default::default()
    })
}

/// Beside the current document, with the extension swapped — so a menu click
/// with no arguments still lands somewhere predictable.
fn default_export_path(world: &World) -> Result<PathBuf, String> {
    let doc_state = world
        .get_resource::<crate::plugins::document_state::DocumentState>()
        .ok_or_else(|| "No document state".to_string())?;
    let current = doc_state
        .current_path
        .as_ref()
        .ok_or_else(|| "Save the document first, or pass an explicit path".to_string())?;
    Ok(with_obj_extension(current))
}

fn with_obj_extension(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    path.set_extension("obj");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> ExportMesh {
        ExportMesh {
            name: "element_7".to_string(),
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
            ],
            normals: vec![Vec3::Z, Vec3::Z, Vec3::Z],
            indices: vec![[0, 1, 2]],
        }
    }

    #[test]
    fn obj_faces_are_one_based() {
        let obj = write_obj(&[quad()]);
        assert!(obj.contains("o element_7"));
        assert!(obj.contains("v 0 0 0"));
        assert!(
            obj.contains("f 1//1 2//2 3//3"),
            "OBJ indices start at 1, got:\n{obj}"
        );
    }

    /// OBJ indices are file-global, so a second object must continue numbering
    /// rather than restart — the classic way an exporter produces a mangled file.
    #[test]
    fn second_object_continues_the_global_vertex_numbering() {
        let mut second = quad();
        second.name = "element_8".to_string();
        let obj = write_obj(&[quad(), second]);
        assert!(obj.contains("f 4//4 5//5 6//6"), "got:\n{obj}");
    }

    #[test]
    fn meshes_without_normals_emit_position_only_faces() {
        let mut mesh = quad();
        mesh.normals.clear();
        let obj = write_obj(&[mesh]);
        assert!(obj.contains("f 1 2 3"), "got:\n{obj}");
        assert!(!obj.contains("//"));
    }

    #[test]
    fn export_path_swaps_the_document_extension() {
        assert_eq!(
            with_obj_extension(Path::new("/projects/house.talos3d")),
            PathBuf::from("/projects/house.obj")
        );
    }
}
