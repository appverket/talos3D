use std::collections::HashMap;

use bevy::prelude::Vec3;

use super::primitives::TriangleMesh;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceToSolidError {
    EmptySurface,
    InvalidFaceIndex {
        face_index: usize,
        vertex_index: u32,
    },
    DegenerateFace {
        face_index: usize,
    },
    VertexCountExceedsU32,
}

/// Extrude an open triangle surface to a horizontal datum and close it with
/// vertical boundary walls plus a bottom cap.
pub fn surface_to_solid(
    surface: &TriangleMesh,
    datum_y: f32,
) -> Result<TriangleMesh, SurfaceToSolidError> {
    validate_surface(surface)?;

    let vertex_count = surface.vertices.len();
    let bottom_offset =
        u32::try_from(vertex_count).map_err(|_| SurfaceToSolidError::VertexCountExceedsU32)?;
    let mut vertices = Vec::with_capacity(vertex_count * 2);
    vertices.extend(surface.vertices.iter().copied());
    vertices.extend(
        surface
            .vertices
            .iter()
            .map(|vertex| Vec3::new(vertex.x, datum_y, vertex.z)),
    );

    let boundary_edges = boundary_edges(&surface.faces);
    let mut faces = Vec::with_capacity(surface.faces.len() * 2 + boundary_edges.len() * 2);
    faces.extend(surface.faces.iter().copied());
    faces.extend(
        surface
            .faces
            .iter()
            .map(|[a, b, c]| [c + bottom_offset, b + bottom_offset, a + bottom_offset]),
    );

    for [a, b] in boundary_edges {
        let a_bottom = a + bottom_offset;
        let b_bottom = b + bottom_offset;
        faces.push([a, a_bottom, b_bottom]);
        faces.push([a, b_bottom, b]);
    }

    Ok(TriangleMesh {
        vertices,
        faces,
        normals: None,
        name: surface
            .name
            .as_deref()
            .map(|name| format!("{name} solid"))
            .or_else(|| Some("surface solid".to_string())),
    })
}

fn validate_surface(surface: &TriangleMesh) -> Result<(), SurfaceToSolidError> {
    if surface.vertices.is_empty() || surface.faces.is_empty() {
        return Err(SurfaceToSolidError::EmptySurface);
    }

    for (face_index, face) in surface.faces.iter().enumerate() {
        for vertex_index in face {
            if *vertex_index as usize >= surface.vertices.len() {
                return Err(SurfaceToSolidError::InvalidFaceIndex {
                    face_index,
                    vertex_index: *vertex_index,
                });
            }
        }

        let [a, b, c] = *face;
        if a == b || b == c || c == a {
            return Err(SurfaceToSolidError::DegenerateFace { face_index });
        }
    }

    Ok(())
}

fn boundary_edges(faces: &[[u32; 3]]) -> Vec<[u32; 2]> {
    let mut edge_counts: HashMap<[u32; 2], usize> = HashMap::new();
    for [a, b, c] in faces {
        for [from, to] in [[*a, *b], [*b, *c], [*c, *a]] {
            let key = normalized_edge(from, to);
            *edge_counts.entry(key).or_default() += 1;
        }
    }

    let mut boundary = Vec::new();
    for [a, b, c] in faces {
        for edge in [[*a, *b], [*b, *c], [*c, *a]] {
            if edge_counts.get(&normalized_edge(edge[0], edge[1])) == Some(&1) {
                boundary.push(edge);
            }
        }
    }
    boundary
}

fn normalized_edge(a: u32, b: u32) -> [u32; 2] {
    if a < b {
        [a, b]
    } else {
        [b, a]
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn mesh(vertices: Vec<Vec3>, faces: Vec<[u32; 3]>) -> TriangleMesh {
        TriangleMesh {
            vertices,
            faces,
            normals: None,
            name: Some("test surface".to_string()),
        }
    }

    fn square_surface() -> TriangleMesh {
        mesh(
            vec![
                Vec3::new(0.0, 2.0, 0.0),
                Vec3::new(2.0, 2.0, 0.0),
                Vec3::new(2.0, 3.0, 2.0),
                Vec3::new(0.0, 2.5, 2.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        )
    }

    fn undirected_edges(faces: &[[u32; 3]]) -> HashMap<[u32; 2], usize> {
        let mut counts = HashMap::new();
        for [a, b, c] in faces {
            for [from, to] in [[*a, *b], [*b, *c], [*c, *a]] {
                *counts.entry(normalized_edge(from, to)).or_default() += 1;
            }
        }
        counts
    }

    #[test]
    fn extrudes_surface_to_datum_with_bottom_cap_and_boundary_walls() {
        let solid = surface_to_solid(&square_surface(), 0.25).unwrap();

        assert_eq!(solid.vertices.len(), 8);
        assert_eq!(solid.faces.len(), 12);
        assert_eq!(solid.name, Some("test surface solid".to_string()));
        assert_eq!(solid.vertices[4], Vec3::new(0.0, 0.25, 0.0));
        assert_eq!(solid.vertices[5], Vec3::new(2.0, 0.25, 0.0));
        assert_eq!(solid.vertices[6], Vec3::new(2.0, 0.25, 2.0));
        assert_eq!(solid.vertices[7], Vec3::new(0.0, 0.25, 2.0));

        assert!(solid.faces.contains(&[0, 1, 2]));
        assert!(solid.faces.contains(&[6, 5, 4]));
        assert!(solid.faces.contains(&[0, 4, 5]));
        assert!(solid.faces.contains(&[0, 5, 1]));
    }

    #[test]
    fn generated_solid_has_no_open_edges() {
        let solid = surface_to_solid(&square_surface(), -1.0).unwrap();

        for count in undirected_edges(&solid.faces).values() {
            assert_eq!(*count, 2);
        }
    }

    #[test]
    fn boundary_walls_follow_concave_surfaces() {
        let concave = mesh(
            vec![
                Vec3::new(0.0, 2.0, 0.0),
                Vec3::new(2.0, 2.0, 0.0),
                Vec3::new(2.0, 2.0, 1.0),
                Vec3::new(1.0, 2.0, 1.0),
                Vec3::new(1.0, 2.0, 2.0),
                Vec3::new(0.0, 2.0, 2.0),
            ],
            vec![[0, 1, 3], [1, 2, 3], [0, 3, 5], [3, 4, 5]],
        );

        let solid = surface_to_solid(&concave, 0.0).unwrap();

        assert_eq!(solid.vertices.len(), 12);
        assert_eq!(solid.faces.len(), 20);
        for count in undirected_edges(&solid.faces).values() {
            assert_eq!(*count, 2);
        }
    }

    #[test]
    fn boundary_walls_close_inner_holes() {
        let ring = mesh(
            vec![
                Vec3::new(0.0, 2.0, 0.0),
                Vec3::new(4.0, 2.0, 0.0),
                Vec3::new(4.0, 2.0, 4.0),
                Vec3::new(0.0, 2.0, 4.0),
                Vec3::new(1.0, 2.0, 1.0),
                Vec3::new(3.0, 2.0, 1.0),
                Vec3::new(3.0, 2.0, 3.0),
                Vec3::new(1.0, 2.0, 3.0),
            ],
            vec![
                [0, 1, 5],
                [0, 5, 4],
                [1, 2, 6],
                [1, 6, 5],
                [2, 3, 7],
                [2, 7, 6],
                [3, 0, 4],
                [3, 4, 7],
            ],
        );

        let solid = surface_to_solid(&ring, 0.0).unwrap();
        let side_edges: HashSet<[u32; 2]> = solid
            .faces
            .iter()
            .flat_map(|[a, b, c]| [[*a, *b], [*b, *c], [*c, *a]])
            .filter(|edge| edge[0] < 8 && edge[1] >= 8 || edge[1] < 8 && edge[0] >= 8)
            .map(|edge| normalized_edge(edge[0], edge[1]))
            .collect();

        assert_eq!(solid.vertices.len(), 16);
        assert_eq!(solid.faces.len(), 32);
        assert_eq!(side_edges.len(), 16);
        for count in undirected_edges(&solid.faces).values() {
            assert_eq!(*count, 2);
        }
    }

    #[test]
    fn rejects_out_of_range_face_indices() {
        let surface = mesh(vec![Vec3::ZERO, Vec3::X, Vec3::Z], vec![[0, 1, 9]]);

        assert_eq!(
            surface_to_solid(&surface, 0.0),
            Err(SurfaceToSolidError::InvalidFaceIndex {
                face_index: 0,
                vertex_index: 9,
            })
        );
    }

    #[test]
    fn rejects_degenerate_faces() {
        let surface = mesh(vec![Vec3::ZERO, Vec3::X], vec![[0, 1, 1]]);

        assert_eq!(
            surface_to_solid(&surface, 0.0),
            Err(SurfaceToSolidError::DegenerateFace { face_index: 0 })
        );
    }
}
