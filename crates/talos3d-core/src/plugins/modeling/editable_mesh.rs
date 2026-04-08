use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::primitives::{
    BoxPrimitive, CylinderPrimitive, PlanePrimitive, ShapeRotation, SpherePrimitive,
};

/// Sentinel value for "no linked half-edge/face" (boundary).
const NONE: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Operation Log — preserves parametric provenance across mesh promotion
// ---------------------------------------------------------------------------

/// Records the origin and mutation history of an EditableMesh.
///
/// The EditableMesh is always the geometric ground truth. The OperationLog is
/// metadata that lets the AI (and future re-parameterization) understand *what
/// happened* to produce the current mesh from a parametric original.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationLog {
    /// The parametric entity this mesh was promoted from.
    pub origin: OperationOrigin,
    /// Ordered list of topology-mutating operations applied after promotion.
    pub ops: Vec<MeshOp>,
}

/// Identifies the original parametric source of a promoted mesh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationOrigin {
    /// Type name of the original entity ("box", "cylinder", "plane").
    pub type_name: String,
    /// Full JSON snapshot of the original parametric entity.
    pub params: Value,
}

/// A single topology-mutating operation on an EditableMesh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MeshOp {
    /// A face was subdivided by a line between two edge points.
    SubdivideFace {
        face_id: u32,
        point_a: [f32; 3],
        point_b: [f32; 3],
    },
    /// A face was push/pulled along its normal.
    PushPull { face_id: u32, distance: f32 },
}

/// Half-edge topological mesh for direct modeling.
///
/// Supports face subdivision, edge splitting, and topological queries.
/// Created by promoting a parametric primitive or via the Line tool.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditableMesh {
    pub vertices: Vec<Vec3>,
    pub half_edges: Vec<HalfEdge>,
    pub faces: Vec<MeshFace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HalfEdge {
    /// Index of the vertex this half-edge originates from.
    pub origin: u32,
    /// Index of the opposite half-edge (NONE for boundary).
    pub twin: u32,
    /// Index of the next half-edge in the face loop.
    pub next: u32,
    /// Index of the face this half-edge bounds (NONE for boundary).
    pub face: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshFace {
    /// Index of any half-edge in the outer boundary loop.
    pub half_edge: u32,
    /// Cached face normal (recomputed on topology change).
    pub normal: Vec3,
}

impl EditableMesh {
    /// Create from a box primitive.
    /// Produces a 6-face mesh with 8 vertices, 24 half-edges (12 edges).
    pub fn from_box(primitive: &BoxPrimitive, rotation: &ShapeRotation) -> Self {
        let he = primitive.half_extents;
        let c = primitive.centre;
        let r = rotation.0;

        // 8 corners in world space
        let local_corners = [
            Vec3::new(-he.x, -he.y, -he.z), // 0: ---
            Vec3::new(he.x, -he.y, -he.z),  // 1: +--
            Vec3::new(he.x, -he.y, he.z),   // 2: +-+
            Vec3::new(-he.x, -he.y, he.z),  // 3: --+
            Vec3::new(-he.x, he.y, -he.z),  // 4: -+-
            Vec3::new(he.x, he.y, -he.z),   // 5: ++-
            Vec3::new(he.x, he.y, he.z),    // 6: +++
            Vec3::new(-he.x, he.y, he.z),   // 7: -++
        ];
        let vertices: Vec<Vec3> = local_corners.iter().map(|&lc| c + r * lc).collect();

        // 6 faces, each a quad (4 vertices in CCW order when viewed from outside)
        // Face ordering matches FaceId: 0=-X, 1=+X, 2=-Y, 3=+Y, 4=-Z, 5=+Z
        let face_verts: [[u32; 4]; 6] = [
            [3, 7, 4, 0], // -X face
            [1, 5, 6, 2], // +X face
            [0, 1, 2, 3], // -Y face (bottom)
            [4, 7, 6, 5], // +Y face (top)
            [0, 4, 5, 1], // -Z face
            [2, 6, 7, 3], // +Z face
        ];

        build_mesh_from_quads(&vertices, &face_verts)
    }

    /// Create from a cylinder primitive (approximated with N-gon caps and quad sides).
    pub fn from_cylinder(
        primitive: &CylinderPrimitive,
        rotation: &ShapeRotation,
        segments: u32,
    ) -> Self {
        let n = segments.max(3);
        let c = primitive.centre;
        let r = rotation.0;
        let half_h = primitive.height * 0.5;
        let radius = primitive.radius;

        let mut vertices = Vec::with_capacity((n * 2 + 2) as usize);

        // Bottom ring vertices: 0..n
        for i in 0..n {
            let angle = (i as f32 / n as f32) * std::f32::consts::TAU;
            let local = Vec3::new(angle.cos() * radius, -half_h, angle.sin() * radius);
            vertices.push(c + r * local);
        }
        // Top ring vertices: n..2n
        for i in 0..n {
            let angle = (i as f32 / n as f32) * std::f32::consts::TAU;
            let local = Vec3::new(angle.cos() * radius, half_h, angle.sin() * radius);
            vertices.push(c + r * local);
        }
        // Build faces as polygon vertex lists
        let mut face_vertex_lists: Vec<Vec<u32>> = Vec::new();

        // Bottom cap: triangles from center to ring (CW when viewed from below = CCW from outside)
        // Actually a single N-gon face
        let mut bottom_face = Vec::with_capacity(n as usize);
        for i in (0..n).rev() {
            bottom_face.push(i);
        }
        face_vertex_lists.push(bottom_face);

        // Top cap: single N-gon face
        let mut top_face = Vec::with_capacity(n as usize);
        for i in 0..n {
            top_face.push(n + i);
        }
        face_vertex_lists.push(top_face);

        // Side quads
        for i in 0..n {
            let next_i = (i + 1) % n;
            face_vertex_lists.push(vec![i, next_i, n + next_i, n + i]);
        }

        build_mesh_from_polygons(&vertices, &face_vertex_lists)
    }

    /// Create from a plane primitive (single quad face).
    pub fn from_plane(primitive: &PlanePrimitive) -> Self {
        let ca = primitive.corner_a;
        let cb = primitive.corner_b;
        let e = primitive.elevation;

        let vertices = vec![
            Vec3::new(ca.x, e, ca.y),
            Vec3::new(cb.x, e, ca.y),
            Vec3::new(cb.x, e, cb.y),
            Vec3::new(ca.x, e, cb.y),
        ];

        let face_verts: [[u32; 4]; 1] = [[0, 1, 2, 3]];
        build_mesh_from_quads(&vertices, &face_verts)
    }

    /// Create from a sphere primitive using a UV-sphere approximation.
    pub fn from_sphere(
        primitive: &SpherePrimitive,
        rotation: &ShapeRotation,
        sectors: u32,
        stacks: u32,
    ) -> Self {
        let sectors = sectors.max(3);
        let stacks = stacks.max(3);
        let ring_count = stacks - 1;
        let c = primitive.centre;
        let r = rotation.0;
        let radius = primitive.radius;

        let mut vertices = Vec::with_capacity((2 + ring_count * sectors) as usize);
        vertices.push(c + r * Vec3::Y * radius);

        for stack in 1..stacks {
            let phi = std::f32::consts::PI * stack as f32 / stacks as f32;
            let y = radius * phi.cos();
            let ring_radius = radius * phi.sin();
            for sector in 0..sectors {
                let theta = std::f32::consts::TAU * sector as f32 / sectors as f32;
                let local = Vec3::new(ring_radius * theta.cos(), y, ring_radius * theta.sin());
                vertices.push(c + r * local);
            }
        }

        let bottom_index = vertices.len() as u32;
        vertices.push(c - r * Vec3::Y * radius);

        let ring_vertex = |ring: u32, sector: u32| -> u32 { 1 + ring * sectors + sector % sectors };
        let mut face_vertex_lists = Vec::new();
        let mut push_face = |mut face: Vec<u32>| {
            if face.len() >= 3 {
                let v0 = vertices[face[0] as usize];
                let v1 = vertices[face[1] as usize];
                let v2 = vertices[face[2] as usize];
                let normal = (v1 - v0).cross(v2 - v0);
                let centroid = face
                    .iter()
                    .map(|index| vertices[*index as usize])
                    .sum::<Vec3>()
                    / face.len() as f32;
                if normal.dot(centroid - primitive.centre) < 0.0 {
                    face.reverse();
                }
            }
            face_vertex_lists.push(face);
        };

        for sector in 0..sectors {
            push_face(vec![0, ring_vertex(0, sector), ring_vertex(0, sector + 1)]);
        }

        for ring in 0..ring_count - 1 {
            for sector in 0..sectors {
                push_face(vec![
                    ring_vertex(ring, sector),
                    ring_vertex(ring + 1, sector),
                    ring_vertex(ring + 1, sector + 1),
                    ring_vertex(ring, sector + 1),
                ]);
            }
        }

        let last_ring = ring_count - 1;
        for sector in 0..sectors {
            push_face(vec![
                bottom_index,
                ring_vertex(last_ring, sector + 1),
                ring_vertex(last_ring, sector),
            ]);
        }

        build_mesh_from_polygons(&vertices, &face_vertex_lists)
    }

    /// Number of logical edges (half_edges / 2, approximately).
    pub fn edge_count(&self) -> usize {
        // Count pairs: each interior edge has a twin; boundary edges have twin == NONE
        let mut count = 0;
        for (i, he) in self.half_edges.iter().enumerate() {
            if he.twin == NONE || (he.twin as usize) > i {
                count += 1;
            }
        }
        count
    }

    /// Euler formula check: V - E + F = 2 (closed) or 1 (open with boundary).
    pub fn validate_euler(&self) -> bool {
        let v = self.vertices.len() as i32;
        let e = self.edge_count() as i32;
        let f = self.faces.len() as i32;
        let chi = v - e + f;
        chi == 2 || chi == 1
    }

    /// Get the two faces adjacent to an edge (identified by half-edge index).
    /// Returns (face_of_this_half_edge, face_of_twin_if_exists).
    pub fn faces_adjacent_to_edge(&self, half_edge_idx: u32) -> (u32, Option<u32>) {
        let he = &self.half_edges[half_edge_idx as usize];
        let f1 = he.face;
        let f2 = if he.twin != NONE {
            let twin_face = self.half_edges[he.twin as usize].face;
            if twin_face != NONE {
                Some(twin_face)
            } else {
                None
            }
        } else {
            None
        };
        (f1, f2)
    }

    /// Get all half-edge indices bounding a face, in order.
    pub fn edges_of_face(&self, face_idx: u32) -> Vec<u32> {
        let start = self.faces[face_idx as usize].half_edge;
        let mut result = vec![start];
        let mut current = self.half_edges[start as usize].next;
        while current != start {
            result.push(current);
            current = self.half_edges[current as usize].next;
            if result.len() > self.half_edges.len() {
                break; // safety: prevent infinite loop
            }
        }
        result
    }

    /// Get vertex indices of a face in winding order.
    pub fn vertices_of_face(&self, face_idx: u32) -> Vec<u32> {
        self.edges_of_face(face_idx)
            .iter()
            .map(|&he_idx| self.half_edges[he_idx as usize].origin)
            .collect()
    }

    /// Check if a half-edge is on the boundary (no twin or twin has no face).
    pub fn is_boundary_edge(&self, half_edge_idx: u32) -> bool {
        let he = &self.half_edges[half_edge_idx as usize];
        he.twin == NONE || self.half_edges[he.twin as usize].face == NONE
    }

    /// Recompute the normal for a face from its vertices.
    pub fn recompute_face_normal(&mut self, face_idx: u32) {
        let verts = self.vertices_of_face(face_idx);
        if verts.len() < 3 {
            return;
        }
        let v0 = self.vertices[verts[0] as usize];
        let v1 = self.vertices[verts[1] as usize];
        let v2 = self.vertices[verts[2] as usize];
        let normal = (v1 - v0).cross(v2 - v0).normalize_or_zero();
        self.faces[face_idx as usize].normal = normal;
    }

    /// Recompute normals for all faces.
    pub fn recompute_all_normals(&mut self) {
        for i in 0..self.faces.len() {
            self.recompute_face_normal(i as u32);
        }
    }

    /// Compute the centroid of a face.
    pub fn face_centroid(&self, face_idx: u32) -> Vec3 {
        let verts = self.vertices_of_face(face_idx);
        if verts.is_empty() {
            return Vec3::ZERO;
        }
        let sum: Vec3 = verts.iter().map(|&vi| self.vertices[vi as usize]).sum();
        sum / verts.len() as f32
    }

    /// Find the nearest edge of a face to a given point.
    /// Returns `(half_edge_index, t_parameter, closest_point)` where t is 0..1 along the edge.
    pub fn nearest_edge_on_face(&self, face_idx: u32, point: Vec3) -> Option<(u32, f32, Vec3)> {
        let edges = self.edges_of_face(face_idx);
        let mut best: Option<(u32, f32, Vec3, f32)> = None;

        for &he_idx in &edges {
            let he = &self.half_edges[he_idx as usize];
            let a = self.vertices[he.origin as usize];
            let next_he = &self.half_edges[he.next as usize];
            let b = self.vertices[next_he.origin as usize];

            let ab = b - a;
            let len_sq = ab.length_squared();
            if len_sq < 1e-10 {
                continue;
            }
            let t = ((point - a).dot(ab) / len_sq).clamp(0.0, 1.0);
            let closest = a + ab * t;
            let dist = (point - closest).length();

            if best.is_none() || dist < best.unwrap().3 {
                best = Some((he_idx, t, closest, dist));
            }
        }

        best.map(|(he, t, closest, _dist)| (he, t, closest))
    }

    /// Compute the bounding box of all vertices.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for &v in &self.vertices {
            min = min.min(v);
            max = max.max(v);
        }
        (min, max)
    }

    /// Split an edge at parameter t (0..1) between its two endpoints.
    /// Returns the index of the new vertex.
    pub fn split_edge(&mut self, half_edge_idx: u32, t: f32) -> u32 {
        let he = self.half_edges[half_edge_idx as usize];
        let dest_he = self.half_edges[he.next as usize];
        let origin = self.vertices[he.origin as usize];
        let destination = self.vertices[dest_he.origin as usize];
        let new_pos = origin.lerp(destination, t);

        let new_vertex_idx = self.vertices.len() as u32;
        self.vertices.push(new_pos);

        // Create new half-edge from new_vertex to destination (takes over the rest of the loop)
        let new_he_idx = self.half_edges.len() as u32;
        let new_he = HalfEdge {
            origin: new_vertex_idx,
            twin: NONE, // will be set below if twin exists
            next: he.next,
            face: he.face,
        };
        self.half_edges.push(new_he);

        // Update original half-edge to point to new vertex's half-edge
        self.half_edges[half_edge_idx as usize].next = new_he_idx;

        // Handle twin
        if he.twin != NONE {
            let twin = self.half_edges[he.twin as usize];
            let new_twin_idx = self.half_edges.len() as u32;
            let new_twin = HalfEdge {
                origin: new_vertex_idx,
                twin: he.twin,
                next: twin.next,
                face: twin.face,
            };
            self.half_edges.push(new_twin);

            // Original twin now points to new_twin as its next
            self.half_edges[he.twin as usize].next = new_twin_idx;

            // Set twin links
            self.half_edges[new_he_idx as usize].twin = he.twin;
            self.half_edges[he.twin as usize].twin = new_he_idx;
            self.half_edges[new_twin_idx as usize].twin = half_edge_idx;
            self.half_edges[half_edge_idx as usize].twin = new_twin_idx;
        }

        new_vertex_idx
    }

    /// Subdivide a face by connecting two vertices that lie on its boundary.
    /// Both vertices must already exist in the mesh and be on edges of the face.
    /// Returns the indices of the two new faces.
    pub fn subdivide_face(
        &mut self,
        face_idx: u32,
        vertex_a: u32,
        vertex_b: u32,
    ) -> Option<(u32, u32)> {
        let edges = self.edges_of_face(face_idx);
        let verts = self.vertices_of_face(face_idx);

        let pos_a = verts.iter().position(|&v| v == vertex_a)?;
        let pos_b = verts.iter().position(|&v| v == vertex_b)?;
        if pos_a == pos_b {
            return None;
        }

        let n = verts.len();

        // Collect two vertex sub-loops
        let mut loop1 = Vec::new();
        let mut i = pos_a;
        loop {
            loop1.push(verts[i]);
            if i == pos_b {
                break;
            }
            i = (i + 1) % n;
        }

        let mut loop2 = Vec::new();
        i = pos_b;
        loop {
            loop2.push(verts[i]);
            if i == pos_a {
                break;
            }
            i = (i + 1) % n;
        }

        if loop1.len() < 3 || loop2.len() < 3 {
            return None;
        }

        // Collect the half-edge indices corresponding to each sub-loop
        // We need to find, for each consecutive pair in the loop, the half-edge
        // from the original face that starts at that vertex.
        let he_for_vertex: std::collections::HashMap<u32, u32> = edges
            .iter()
            .map(|&he_idx| (self.half_edges[he_idx as usize].origin, he_idx))
            .collect();

        // Create the two dividing half-edges
        let div_he1_idx = self.half_edges.len() as u32;
        let div_he2_idx = div_he1_idx + 1;

        // Face indices for the two new faces
        let face1_idx = self.faces.len() as u32;
        let face2_idx = face1_idx + 1;

        self.half_edges.push(HalfEdge {
            origin: vertex_a,
            twin: div_he2_idx,
            next: NONE,
            face: face1_idx,
        });
        self.half_edges.push(HalfEdge {
            origin: vertex_b,
            twin: div_he1_idx,
            next: NONE,
            face: face2_idx,
        });

        self.faces.push(MeshFace {
            half_edge: div_he1_idx,
            normal: Vec3::ZERO,
        });
        self.faces.push(MeshFace {
            half_edge: div_he2_idx,
            normal: Vec3::ZERO,
        });

        // Re-link loop 1: vertex_a -> ... -> vertex_b -> (dividing_he2 back to vertex_a)
        // The last edge in loop1 is from the vertex before vertex_b TO vertex_b.
        // Then dividing_he1 (A->B) is NOT in loop1's boundary — loop1 goes A...B then B->A.
        // Wait: loop1 contains vertices A...B. The face bounded by loop1 needs edges:
        //   A->next(A), next(A)->next2(A), ..., prev(B)->B, then the dividing edge B->A.
        // Actually: loop1 = [A, x1, x2, ..., B]. The face needs half-edges going around:
        //   he(A->x1), he(x1->x2), ..., he(xN->B), then dividing B->A.
        // So the dividing half-edge in loop1 is div_he2 (B->A), and in loop2 is div_he1 (A->B).

        // Re-link loop1 edges + dividing_he2
        for i in 0..loop1.len() - 1 {
            let v = loop1[i];
            if let Some(&he_idx) = he_for_vertex.get(&v) {
                self.half_edges[he_idx as usize].face = face1_idx;
                let _next_v = loop1[i + 1];
                if i + 1 == loop1.len() - 1 {
                    // Last real edge before the dividing edge
                    // Its next should be the dividing half-edge B->A
                    self.half_edges[he_idx as usize].next = div_he2_idx;
                }
                // Otherwise keep existing next pointer (it should already point correctly)
            }
        }
        // Dividing he2 (B->A) next should point to the first half-edge of loop1
        if let Some(&first_he) = he_for_vertex.get(&vertex_a) {
            self.half_edges[div_he2_idx as usize].next = first_he;
            self.faces[face1_idx as usize].half_edge = first_he;
        }

        // Re-link loop2 edges + dividing_he1
        for i in 0..loop2.len() - 1 {
            let v = loop2[i];
            if let Some(&he_idx) = he_for_vertex.get(&v) {
                self.half_edges[he_idx as usize].face = face2_idx;
                if i + 1 == loop2.len() - 1 {
                    self.half_edges[he_idx as usize].next = div_he1_idx;
                }
            }
        }
        if let Some(&first_he) = he_for_vertex.get(&vertex_b) {
            self.half_edges[div_he1_idx as usize].next = first_he;
            self.faces[face2_idx as usize].half_edge = first_he;
        }

        // Invalidate old face
        self.faces[face_idx as usize].half_edge = NONE;

        self.recompute_face_normal(face1_idx);
        self.recompute_face_normal(face2_idx);

        Some((face1_idx, face2_idx))
    }

    /// Triangulate a face for rendering. Returns triangles as (v0, v1, v2) index triples.
    pub fn triangulate_face(&self, face_idx: u32) -> Vec<[u32; 3]> {
        let verts = self.vertices_of_face(face_idx);
        if verts.len() < 3 {
            return Vec::new();
        }
        if verts.len() == 3 {
            return vec![[verts[0], verts[1], verts[2]]];
        }
        // Fan triangulation (works for convex polygons)
        let mut triangles = Vec::with_capacity(verts.len() - 2);
        for i in 1..verts.len() - 1 {
            triangles.push([verts[0], verts[i], verts[i + 1]]);
        }
        triangles
    }

    /// Triangulate all faces for rendering.
    pub fn triangulate_all(&self) -> (Vec<Vec3>, Vec<[u32; 3]>, Vec<Vec3>) {
        let mut out_verts = Vec::new();
        let mut out_tris = Vec::new();
        let mut out_normals = Vec::new();

        for (face_idx, face) in self.faces.iter().enumerate() {
            if face.half_edge == NONE {
                continue; // removed face
            }
            let tris = self.triangulate_face(face_idx as u32);
            let base = out_verts.len() as u32;
            let face_verts = self.vertices_of_face(face_idx as u32);
            for &vi in &face_verts {
                out_verts.push(self.vertices[vi as usize]);
                out_normals.push(face.normal);
            }
            // Remap triangle indices to local face vertex indices
            for tri in &tris {
                let local: Vec<u32> = tri
                    .iter()
                    .map(|&global_vi| {
                        face_verts.iter().position(|&v| v == global_vi).unwrap_or(0) as u32 + base
                    })
                    .collect();
                out_tris.push([local[0], local[1], local[2]]);
            }
        }

        (out_verts, out_tris, out_normals)
    }

    /// Get all edge segments as (start, end) pairs for wireframe rendering.
    pub fn edge_segments(&self) -> Vec<(Vec3, Vec3)> {
        let mut segments = Vec::new();
        let mut seen = vec![false; self.half_edges.len()];
        for (i, he) in self.half_edges.iter().enumerate() {
            if seen[i] || he.face == NONE {
                continue;
            }
            seen[i] = true;
            if he.twin != NONE {
                seen[he.twin as usize] = true;
            }
            let dest = self.half_edges[he.next as usize].origin;
            segments.push((
                self.vertices[he.origin as usize],
                self.vertices[dest as usize],
            ));
        }
        segments
    }

    /// Count active (non-removed) faces.
    pub fn active_face_count(&self) -> usize {
        self.faces.iter().filter(|f| f.half_edge != NONE).count()
    }
}

/// Build a mesh from quad faces defined by vertex indices.
fn build_mesh_from_quads(vertices: &[Vec3], face_quads: &[[u32; 4]]) -> EditableMesh {
    let polys: Vec<Vec<u32>> = face_quads.iter().map(|q| q.to_vec()).collect();
    build_mesh_from_polygons(vertices, &polys)
}

/// Build a mesh from polygon faces (arbitrary vertex count per face).
pub fn build_mesh_from_polygons(vertices: &[Vec3], face_polys: &[Vec<u32>]) -> EditableMesh {
    let mut mesh = EditableMesh {
        vertices: vertices.to_vec(),
        half_edges: Vec::new(),
        faces: Vec::new(),
    };

    // For twin pairing: map (origin, dest) -> half_edge_index
    let mut edge_map: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();

    for (face_idx, poly) in face_polys.iter().enumerate() {
        let n = poly.len();
        let face_idx = face_idx as u32;

        // Compute face normal
        let normal = if n >= 3 {
            let v0 = vertices[poly[0] as usize];
            let v1 = vertices[poly[1] as usize];
            let v2 = vertices[poly[2] as usize];
            (v1 - v0).cross(v2 - v0).normalize_or_zero()
        } else {
            Vec3::ZERO
        };

        let first_he = mesh.half_edges.len() as u32;
        mesh.faces.push(MeshFace {
            half_edge: first_he,
            normal,
        });

        // Create half-edges for this face
        for i in 0..n {
            let origin = poly[i];
            let dest = poly[(i + 1) % n];
            let he_idx = mesh.half_edges.len() as u32;
            let next_he = first_he + ((i + 1) % n) as u32;

            mesh.half_edges.push(HalfEdge {
                origin,
                twin: NONE,
                next: next_he,
                face: face_idx,
            });

            // Check if twin exists
            if let Some(&twin_idx) = edge_map.get(&(dest, origin)) {
                mesh.half_edges[he_idx as usize].twin = twin_idx;
                mesh.half_edges[twin_idx as usize].twin = he_idx;
            }

            edge_map.insert((origin, dest), he_idx);
        }
    }

    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_mesh_has_correct_topology() {
        let primitive = BoxPrimitive {
            centre: Vec3::ZERO,
            half_extents: Vec3::new(1.0, 1.0, 1.0),
        };
        let mesh = EditableMesh::from_box(&primitive, &ShapeRotation::default());

        assert_eq!(mesh.vertices.len(), 8);
        assert_eq!(mesh.faces.len(), 6);
        assert_eq!(mesh.half_edges.len(), 24); // 6 faces * 4 edges each
        assert_eq!(mesh.edge_count(), 12); // cube has 12 edges
        assert!(mesh.validate_euler()); // V - E + F = 8 - 12 + 6 = 2

        // Each face should have exactly 4 vertices
        for i in 0..6 {
            let verts = mesh.vertices_of_face(i);
            assert_eq!(verts.len(), 4, "Face {i} should have 4 vertices");
        }

        // All half-edges should have twins (closed mesh)
        for (i, he) in mesh.half_edges.iter().enumerate() {
            assert_ne!(
                he.twin, NONE,
                "Half-edge {i} should have a twin (closed mesh)"
            );
        }
    }

    #[test]
    fn plane_mesh_is_single_face() {
        let primitive = PlanePrimitive {
            corner_a: Vec2::new(-1.0, -1.0),
            corner_b: Vec2::new(1.0, 1.0),
            elevation: 0.0,
        };
        let mesh = EditableMesh::from_plane(&primitive);

        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.faces.len(), 1);
        assert_eq!(mesh.half_edges.len(), 4);
        // Open mesh: V - E + F = 4 - 4 + 1 = 1
        assert!(mesh.validate_euler());
    }

    #[test]
    fn cylinder_mesh_topology() {
        let primitive = CylinderPrimitive {
            centre: Vec3::ZERO,
            radius: 1.0,
            height: 2.0,
        };
        let mesh = EditableMesh::from_cylinder(&primitive, &ShapeRotation::default(), 8);

        // 8 bottom ring + 8 top ring = 16 vertices
        assert_eq!(mesh.vertices.len(), 16);
        // 1 bottom cap + 1 top cap + 8 side quads = 10 faces
        assert_eq!(mesh.faces.len(), 10);
        // Closed mesh: V=16, E=24 (8 bottom + 8 top + 8 side), F=10 → 16-24+10=2
        assert!(mesh.validate_euler());
    }

    #[test]
    fn sphere_mesh_topology() {
        let primitive = SpherePrimitive {
            centre: Vec3::ZERO,
            radius: 1.0,
        };
        let mesh = EditableMesh::from_sphere(&primitive, &ShapeRotation::default(), 8, 6);

        assert_eq!(mesh.vertices.len(), 42);
        assert_eq!(mesh.faces.len(), 48);
        assert!(mesh.validate_euler());
    }

    #[test]
    fn edge_split_creates_new_vertex() {
        let primitive = PlanePrimitive {
            corner_a: Vec2::new(0.0, 0.0),
            corner_b: Vec2::new(2.0, 2.0),
            elevation: 0.0,
        };
        let mut mesh = EditableMesh::from_plane(&primitive);
        let initial_verts = mesh.vertices.len();

        // Split the first edge at midpoint
        let new_v = mesh.split_edge(0, 0.5);
        assert_eq!(mesh.vertices.len(), initial_verts + 1);
        assert_eq!(new_v as usize, initial_verts);
    }

    #[test]
    fn face_subdivision_creates_two_faces() {
        // Use a simple plane (1 face, 4 vertices)
        let primitive = PlanePrimitive {
            corner_a: Vec2::new(0.0, 0.0),
            corner_b: Vec2::new(2.0, 2.0),
            elevation: 0.0,
        };
        let mut mesh = EditableMesh::from_plane(&primitive);
        assert_eq!(mesh.faces.len(), 1);

        // Split edge 0 (v0->v1) and edge 2 (v2->v3) at midpoints
        let edges = mesh.edges_of_face(0);
        assert_eq!(edges.len(), 4);

        let v_a = mesh.split_edge(edges[0], 0.5); // midpoint of edge 0
                                                  // After split_edge, edges[2] index is still valid but the face loop changed
                                                  // We need to re-fetch the edges
        let edges_after = mesh.edges_of_face(0);
        // Find the edge that goes from v2 to v3 (the opposite edge)
        let opposite_edge = edges_after.iter().find(|&&he_idx| {
            let he = &mesh.half_edges[he_idx as usize];
            he.origin == 2
        });
        assert!(opposite_edge.is_some(), "Should find opposite edge");
        let v_b = mesh.split_edge(*opposite_edge.unwrap(), 0.5);

        let result = mesh.subdivide_face(0, v_a, v_b);
        assert!(result.is_some(), "Subdivision should succeed");
        let (f1, f2) = result.unwrap();

        // Both new faces should have at least 3 vertices
        let f1_verts = mesh.vertices_of_face(f1);
        let f2_verts = mesh.vertices_of_face(f2);
        assert!(
            f1_verts.len() >= 3,
            "Face 1 has {} vertices",
            f1_verts.len()
        );
        assert!(
            f2_verts.len() >= 3,
            "Face 2 has {} vertices",
            f2_verts.len()
        );

        // Old face should be invalidated
        assert_eq!(mesh.faces[0].half_edge, NONE);

        // Total active faces should be 2
        assert_eq!(mesh.active_face_count(), 2);
    }
}
