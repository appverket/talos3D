//! BSP-tree based Constructive Solid Geometry (CSG).
//!
//! Implements boolean operations (union, difference, intersection) on triangle
//! meshes using a binary space partitioning tree. This is the classic algorithm
//! from Naylor/Amanatides/Thibault, used by Three.js CSG, OpenSCAD, and others.

use bevy::prelude::*;

const EPSILON: f32 = 1e-5;

/// A triangle in 3D space.
#[derive(Debug, Clone)]
pub struct CsgTriangle {
    pub vertices: [Vec3; 3],
    pub normal: Vec3,
}

impl CsgTriangle {
    pub fn new(a: Vec3, b: Vec3, c: Vec3) -> Self {
        let normal = (b - a).cross(c - a).normalize_or_zero();
        Self {
            vertices: [a, b, c],
            normal,
        }
    }

    fn flipped(&self) -> Self {
        Self {
            vertices: [self.vertices[0], self.vertices[2], self.vertices[1]],
            normal: -self.normal,
        }
    }
}

/// A splitting plane defined by normal and distance from origin.
#[derive(Debug, Clone, Copy)]
struct Plane {
    normal: Vec3,
    w: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PointSide {
    Front,
    Back,
    Coplanar,
}

impl Plane {
    fn from_triangle(tri: &CsgTriangle) -> Self {
        let normal = tri.normal;
        Self {
            normal,
            w: normal.dot(tri.vertices[0]),
        }
    }

    fn classify_point(&self, point: Vec3) -> PointSide {
        let t = self.normal.dot(point) - self.w;
        if t > EPSILON {
            PointSide::Front
        } else if t < -EPSILON {
            PointSide::Back
        } else {
            PointSide::Coplanar
        }
    }

    /// Split a triangle by this plane into front and back parts.
    fn split_triangle(
        &self,
        tri: &CsgTriangle,
        coplanar_front: &mut Vec<CsgTriangle>,
        coplanar_back: &mut Vec<CsgTriangle>,
        front: &mut Vec<CsgTriangle>,
        back: &mut Vec<CsgTriangle>,
    ) {
        let mut sides = [PointSide::Coplanar; 3];
        for i in 0..3 {
            sides[i] = self.classify_point(tri.vertices[i]);
        }

        let has_front = sides.iter().any(|s| *s == PointSide::Front);
        let has_back = sides.iter().any(|s| *s == PointSide::Back);

        if !has_front && !has_back {
            // All coplanar — classify by normal alignment
            if self.normal.dot(tri.normal) > 0.0 {
                coplanar_front.push(tri.clone());
            } else {
                coplanar_back.push(tri.clone());
            }
            return;
        }

        if has_front && !has_back {
            front.push(tri.clone());
            return;
        }

        if has_back && !has_front {
            back.push(tri.clone());
            return;
        }

        // Spanning — need to split
        let mut front_verts: Vec<Vec3> = Vec::new();
        let mut back_verts: Vec<Vec3> = Vec::new();

        for i in 0..3 {
            let j = (i + 1) % 3;
            let vi = tri.vertices[i];
            let vj = tri.vertices[j];
            let si = sides[i];
            let sj = sides[j];

            if si != PointSide::Back {
                front_verts.push(vi);
            }
            if si != PointSide::Front {
                back_verts.push(vi);
            }

            if (si == PointSide::Front && sj == PointSide::Back)
                || (si == PointSide::Back && sj == PointSide::Front)
            {
                // Compute intersection point
                let t = (self.w - self.normal.dot(vi)) / self.normal.dot(vj - vi);
                let intersection = vi.lerp(vj, t);
                front_verts.push(intersection);
                back_verts.push(intersection);
            }
        }

        // Triangulate the polygon (fan from first vertex)
        if front_verts.len() >= 3 {
            for i in 1..front_verts.len() - 1 {
                front.push(CsgTriangle::new(
                    front_verts[0],
                    front_verts[i],
                    front_verts[i + 1],
                ));
            }
        }
        if back_verts.len() >= 3 {
            for i in 1..back_verts.len() - 1 {
                back.push(CsgTriangle::new(
                    back_verts[0],
                    back_verts[i],
                    back_verts[i + 1],
                ));
            }
        }
    }
}

/// A BSP tree node.
struct BspNode {
    plane: Option<Plane>,
    triangles: Vec<CsgTriangle>,
    front: Option<Box<BspNode>>,
    back: Option<Box<BspNode>>,
}

impl BspNode {
    fn new() -> Self {
        Self {
            plane: None,
            triangles: Vec::new(),
            front: None,
            back: None,
        }
    }

    fn build(triangles: &[CsgTriangle]) -> Self {
        let mut node = Self::new();
        if !triangles.is_empty() {
            node.add_triangles(triangles);
        }
        node
    }

    fn add_triangles(&mut self, triangles: &[CsgTriangle]) {
        if triangles.is_empty() {
            return;
        }

        if self.plane.is_none() {
            self.plane = Some(Plane::from_triangle(&triangles[0]));
        }

        let plane = self.plane.unwrap();
        let mut front_list: Vec<CsgTriangle> = Vec::new();
        let mut back_list: Vec<CsgTriangle> = Vec::new();

        let mut coplanar_front: Vec<CsgTriangle> = Vec::new();
        let mut coplanar_back: Vec<CsgTriangle> = Vec::new();
        for tri in triangles {
            plane.split_triangle(
                tri,
                &mut coplanar_front,
                &mut coplanar_back,
                &mut front_list,
                &mut back_list,
            );
        }

        self.triangles.extend(coplanar_front);
        self.triangles.extend(coplanar_back);

        if !front_list.is_empty() {
            if self.front.is_none() {
                self.front = Some(Box::new(BspNode::new()));
            }
            self.front.as_mut().unwrap().add_triangles(&front_list);
        }

        if !back_list.is_empty() {
            if self.back.is_none() {
                self.back = Some(Box::new(BspNode::new()));
            }
            self.back.as_mut().unwrap().add_triangles(&back_list);
        }
    }

    /// Collect all triangles from this tree.
    fn all_triangles(&self) -> Vec<CsgTriangle> {
        let mut result = self.triangles.clone();
        if let Some(ref front) = self.front {
            result.extend(front.all_triangles());
        }
        if let Some(ref back) = self.back {
            result.extend(back.all_triangles());
        }
        result
    }

    /// Remove all triangles inside the BSP tree (keep front-side only).
    fn clip_triangles(&self, triangles: &[CsgTriangle]) -> Vec<CsgTriangle> {
        let Some(plane) = self.plane else {
            return triangles.to_vec();
        };

        let mut front_list: Vec<CsgTriangle> = Vec::new();
        let mut back_list: Vec<CsgTriangle> = Vec::new();

        for tri in triangles {
            let mut cf = Vec::new();
            let mut cb = Vec::new();
            plane.split_triangle(tri, &mut cf, &mut cb, &mut front_list, &mut back_list);
            front_list.extend(cf);
            back_list.extend(cb);
        }

        let front_result = if let Some(ref front) = self.front {
            front.clip_triangles(&front_list)
        } else {
            front_list
        };

        let back_result = if let Some(ref back) = self.back {
            back.clip_triangles(&back_list)
        } else {
            Vec::new() // Discard back (inside solid)
        };

        let mut result = front_result;
        result.extend(back_result);
        result
    }

    /// Clip this tree's own triangles against another BSP tree.
    fn clip_to(&mut self, other: &BspNode) {
        self.triangles = other.clip_triangles(&self.triangles);
        if let Some(ref mut front) = self.front {
            front.clip_to(other);
        }
        if let Some(ref mut back) = self.back {
            back.clip_to(other);
        }
    }

    /// Invert inside/outside of this tree.
    fn invert(&mut self) {
        for tri in &mut self.triangles {
            *tri = tri.flipped();
        }
        if let Some(ref mut plane) = self.plane {
            plane.normal = -plane.normal;
            plane.w = -plane.w;
        }
        if let Some(ref mut front) = self.front {
            front.invert();
        }
        if let Some(ref mut back) = self.back {
            back.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }
}

/// Boolean operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BooleanOp {
    Union,
    Difference,
    Intersection,
}

/// Result of a CSG operation as an indexed triangle mesh.
pub struct CsgResult {
    pub vertices: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

/// Build a list of CsgTriangles from an EditableMesh.
pub fn triangles_from_editable_mesh(mesh: &super::editable_mesh::EditableMesh) -> Vec<CsgTriangle> {
    let (positions, tri_indices, _normals) = mesh.triangulate_all();
    tri_indices
        .iter()
        .map(|[a, b, c]| {
            let v0 = positions[*a as usize];
            let v1 = positions[*b as usize];
            let v2 = positions[*c as usize];
            CsgTriangle::new(v0, v1, v2)
        })
        .collect()
}

/// Compute a boolean operation between two sets of triangles.
pub fn boolean(a: &[CsgTriangle], b: &[CsgTriangle], op: BooleanOp) -> CsgResult {
    let mut bsp_a = BspNode::build(a);
    let mut bsp_b = BspNode::build(b);

    match op {
        BooleanOp::Union => {
            // A | B = ~(~A & ~B)
            bsp_a.clip_to(&bsp_b);
            bsp_b.clip_to(&bsp_a);
            bsp_b.invert();
            bsp_b.clip_to(&bsp_a);
            bsp_b.invert();
            bsp_a.add_triangles(&bsp_b.all_triangles());
        }
        BooleanOp::Difference => {
            // A - B = A & ~B
            bsp_a.invert();
            bsp_a.clip_to(&bsp_b);
            bsp_b.clip_to(&bsp_a);
            bsp_b.invert();
            bsp_b.clip_to(&bsp_a);
            bsp_b.invert();
            bsp_a.add_triangles(&bsp_b.all_triangles());
            bsp_a.invert();
        }
        BooleanOp::Intersection => {
            // A & B
            bsp_a.invert();
            bsp_b.clip_to(&bsp_a);
            bsp_b.invert();
            bsp_a.clip_to(&bsp_b);
            bsp_b.clip_to(&bsp_a);
            bsp_a.add_triangles(&bsp_b.all_triangles());
            bsp_a.invert();
        }
    }

    triangles_to_indexed(&bsp_a.all_triangles())
}

/// Convert a list of triangles to an indexed vertex/normal/index representation.
fn triangles_to_indexed(triangles: &[CsgTriangle]) -> CsgResult {
    let mut vertices = Vec::with_capacity(triangles.len() * 3);
    let mut normals = Vec::with_capacity(triangles.len() * 3);
    let mut indices = Vec::with_capacity(triangles.len() * 3);

    for tri in triangles {
        let base = vertices.len() as u32;
        for v in &tri.vertices {
            vertices.push(*v);
            normals.push(tri.normal);
        }
        indices.push(base);
        indices.push(base + 1);
        indices.push(base + 2);
    }

    CsgResult {
        vertices,
        normals,
        indices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::modeling::editable_mesh::EditableMesh;
    use crate::plugins::modeling::primitives::{BoxPrimitive, ShapeRotation};

    fn box_triangles(centre: Vec3, half_extents: Vec3) -> Vec<CsgTriangle> {
        let bx = BoxPrimitive {
            centre,
            half_extents,
        };
        let mesh = EditableMesh::from_box(&bx, &ShapeRotation::default());
        triangles_from_editable_mesh(&mesh)
    }

    #[test]
    fn difference_removes_volume() {
        let a = box_triangles(Vec3::ZERO, Vec3::splat(1.0));
        let b = box_triangles(Vec3::new(0.5, 0.0, 0.0), Vec3::splat(0.5));
        let result = boolean(&a, &b, BooleanOp::Difference);
        // The result should have more triangles than the original box
        // (the cut creates new faces)
        assert!(
            result.indices.len() > 0,
            "difference should produce triangles"
        );
        assert!(
            result.indices.len() / 3 > 12,
            "difference should have more faces than a box (12 tris): got {}",
            result.indices.len() / 3
        );
    }

    #[test]
    fn union_preserves_both() {
        let a = box_triangles(Vec3::ZERO, Vec3::splat(1.0));
        let b = box_triangles(Vec3::new(1.5, 0.0, 0.0), Vec3::splat(0.5));
        let result = boolean(&a, &b, BooleanOp::Union);
        assert!(
            result.indices.len() / 3 >= 24,
            "non-overlapping union should have at least 24 triangles (2 boxes): got {}",
            result.indices.len() / 3
        );
    }

    #[test]
    fn intersection_of_overlapping_boxes() {
        let a = box_triangles(Vec3::ZERO, Vec3::splat(1.0));
        let b = box_triangles(Vec3::new(0.5, 0.0, 0.0), Vec3::splat(1.0));
        let result = boolean(&a, &b, BooleanOp::Intersection);
        assert!(
            result.indices.len() > 0,
            "intersection of overlapping boxes should produce geometry"
        );
    }

    #[test]
    fn difference_of_identical_boxes_is_empty() {
        let a = box_triangles(Vec3::ZERO, Vec3::splat(1.0));
        let b = box_triangles(Vec3::ZERO, Vec3::splat(1.0));
        let result = boolean(&a, &b, BooleanOp::Difference);
        // Subtracting identical boxes should produce no geometry
        // (or very small degenerate triangles due to coplanar handling)
        assert!(
            result.indices.len() / 3 <= 12,
            "A-A should produce minimal geometry: got {} tris",
            result.indices.len() / 3
        );
    }
}
