use std::{collections::HashMap, fs, path::Path};

use serde_json::Value;

use crate::plugins::import::FormatImporter;

pub struct ObjImporter;

const OBJ_EXTENSIONS: &[&str] = &["obj"];

#[derive(Default)]
struct ObjMeshBuilder {
    name: Option<String>,
    vertices: Vec<[f32; 3]>,
    normals: Option<Vec<[f32; 3]>>,
    faces: Vec<[u32; 3]>,
    vertex_map: HashMap<(usize, Option<usize>), u32>,
}

impl ObjMeshBuilder {
    fn new(name: Option<String>) -> Self {
        Self {
            name,
            vertices: Vec::new(),
            normals: None,
            faces: Vec::new(),
            vertex_map: HashMap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    fn triangle_mesh_request(self) -> Value {
        serde_json::json!({
            "type": "triangle_mesh",
            "name": self.name,
            "vertices": self.vertices,
            "faces": self.faces,
            "normals": self.normals,
        })
    }
}

impl FormatImporter for ObjImporter {
    fn format_name(&self) -> &'static str {
        "Wavefront OBJ"
    }

    fn extensions(&self) -> &'static [&'static str] {
        OBJ_EXTENSIONS
    }

    fn import(&self, path: &Path) -> Result<Vec<Value>, String> {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
        parse_obj_requests(&contents)
    }
}

pub fn parse_obj_requests(contents: &str) -> Result<Vec<Value>, String> {
    let mut global_positions = Vec::new();
    let mut global_normals = Vec::new();
    let mut requests = Vec::new();
    let mut current = ObjMeshBuilder::default();

    for (line_index, raw_line) in contents.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(keyword) = parts.next() else {
            continue;
        };

        match keyword {
            "o" | "g" => {
                if !current.is_empty() {
                    requests.push(std::mem::take(&mut current).triangle_mesh_request());
                }
                let name = parts.collect::<Vec<_>>().join(" ");
                current = ObjMeshBuilder::new((!name.is_empty()).then_some(name));
            }
            "v" => {
                let coordinates = parse_f32_triplet(parts.collect(), line_number, "vertex")?;
                global_positions.push(coordinates);
            }
            "vn" => {
                let normal = parse_f32_triplet(parts.collect(), line_number, "normal")?;
                global_normals.push(normal);
            }
            "f" => {
                let face_tokens = parts.collect::<Vec<_>>();
                if face_tokens.len() < 3 {
                    return Err(format!(
                        "OBJ line {line_number}: face must have at least 3 vertices"
                    ));
                }

                let face_indices = face_tokens
                    .into_iter()
                    .map(|token| {
                        parse_face_vertex(
                            token,
                            global_positions.len(),
                            global_normals.len(),
                            line_number,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                for triangle in triangulate_face(&face_indices) {
                    let mut resolved_face = [0_u32; 3];
                    for (index, key) in triangle.into_iter().enumerate() {
                        resolved_face[index] = resolve_obj_vertex(
                            &mut current,
                            key,
                            &global_positions,
                            &global_normals,
                            line_number,
                        )?;
                    }
                    current.faces.push(resolved_face);
                }
            }
            _ => {}
        }
    }

    if !current.is_empty() {
        requests.push(current.triangle_mesh_request());
    }

    if requests.is_empty() {
        return Err("OBJ file did not contain any mesh faces".to_string());
    }

    Ok(requests)
}

fn parse_f32_triplet(
    parts: Vec<&str>,
    line_number: usize,
    label: &str,
) -> Result<[f32; 3], String> {
    if parts.len() < 3 {
        return Err(format!(
            "OBJ line {line_number}: {label} requires 3 numeric values"
        ));
    }

    Ok([
        parts[0]
            .parse::<f32>()
            .map_err(|_| format!("OBJ line {line_number}: invalid {} x coordinate", label))?,
        parts[1]
            .parse::<f32>()
            .map_err(|_| format!("OBJ line {line_number}: invalid {} y coordinate", label))?,
        parts[2]
            .parse::<f32>()
            .map_err(|_| format!("OBJ line {line_number}: invalid {} z coordinate", label))?,
    ])
}

fn parse_face_vertex(
    token: &str,
    position_count: usize,
    normal_count: usize,
    line_number: usize,
) -> Result<(usize, Option<usize>), String> {
    let segments = token.split('/').collect::<Vec<_>>();
    if segments.is_empty() || segments[0].is_empty() {
        return Err(format!("OBJ line {line_number}: missing vertex index"));
    }

    let position_index = parse_obj_index(segments[0], position_count, line_number, "vertex")?;
    let normal_index = match segments.len() {
        0 | 1 => None,
        2 => None,
        _ if segments[2].is_empty() => None,
        _ => Some(parse_obj_index(
            segments[2],
            normal_count,
            line_number,
            "normal",
        )?),
    };

    Ok((position_index, normal_index))
}

fn parse_obj_index(
    raw_index: &str,
    count: usize,
    line_number: usize,
    label: &str,
) -> Result<usize, String> {
    let index = raw_index
        .parse::<isize>()
        .map_err(|_| format!("OBJ line {line_number}: invalid {label} index"))?;
    if index == 0 {
        return Err(format!("OBJ line {line_number}: OBJ indices are 1-based"));
    }

    let resolved = if index > 0 {
        index - 1
    } else {
        count as isize + index
    };

    if resolved < 0 || resolved as usize >= count {
        return Err(format!(
            "OBJ line {line_number}: {label} index {index} is out of range"
        ));
    }

    Ok(resolved as usize)
}

fn triangulate_face(face: &[(usize, Option<usize>)]) -> Vec<[(usize, Option<usize>); 3]> {
    let mut triangles = Vec::new();
    for index in 1..face.len() - 1 {
        triangles.push([face[0], face[index], face[index + 1]]);
    }
    triangles
}

fn resolve_obj_vertex(
    builder: &mut ObjMeshBuilder,
    key: (usize, Option<usize>),
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    line_number: usize,
) -> Result<u32, String> {
    if let Some(index) = builder.vertex_map.get(&key) {
        return Ok(*index);
    }

    let position = positions
        .get(key.0)
        .copied()
        .ok_or_else(|| format!("OBJ line {line_number}: vertex index out of range"))?;
    builder.vertices.push(position);

    match key.1 {
        Some(normal_index) => {
            let normal = normals
                .get(normal_index)
                .copied()
                .ok_or_else(|| format!("OBJ line {line_number}: normal index out of range"))?;
            builder.normals.get_or_insert_with(Vec::new).push(normal);
        }
        None => {
            if let Some(existing_normals) = builder.normals.as_mut() {
                existing_normals.push([0.0, 1.0, 0.0]);
            }
        }
    }

    let index = (builder.vertices.len() - 1) as u32;
    builder.vertex_map.insert(key, index);
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_obj_groups_into_triangle_mesh_requests() {
        let requests = parse_obj_requests(
            r#"
o First
v 0 0 0
v 1 0 0
v 0 1 0
f 1 2 3
o Second
v 0 0 1
v 1 0 1
v 0 1 1
f 4 5 6
"#,
        )
        .expect("OBJ should parse");

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["type"], "triangle_mesh");
        assert_eq!(requests[0]["name"], "First");
        assert_eq!(requests[1]["name"], "Second");
    }

    #[test]
    fn rejects_invalid_face_index() {
        let error = parse_obj_requests(
            r#"
v 0 0 0
v 1 0 0
f 1 2 3
"#,
        )
        .expect_err("OBJ should reject out-of-range face indices");

        assert!(error.contains("out of range"));
    }
}
