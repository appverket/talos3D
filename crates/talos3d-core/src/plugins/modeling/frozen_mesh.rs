//! Bounded, deterministic snapshot geometry. This never evaluates vendor code.
use super::{definition::FrozenMeshEvaluator, primitives::TriangleMesh};
use bevy::prelude::*;

pub(super) fn evaluate(e: &FrozenMeshEvaluator) -> Result<TriangleMesh, String> {
    let scale = super::polygon_evaluator::length_scale(e.coordinate_unit)?;
    if !(3..=1_000_000).contains(&e.vertices.len()) || !(1..=2_000_000).contains(&e.faces.len()) {
        return Err("Frozen mesh requires 3..=1000000 vertices and 1..=2000000 faces".into());
    }
    let vertices = e
        .vertices
        .iter()
        .map(|v| {
            let p = Vec3::new(
                (v[0] * scale) as f32,
                (v[1] * scale) as f32,
                (v[2] * scale) as f32,
            );
            if !p.is_finite() {
                return Err("Frozen mesh coordinates must be finite in world units".into());
            }
            Ok(p)
        })
        .collect::<Result<Vec<_>, String>>()?;
    for face in &e.faces {
        let [a, b, c] = face.map(|i| vertices.get(i as usize));
        let (Some(a), Some(b), Some(c)) = (a, b, c) else {
            return Err("Frozen mesh face references an out-of-range vertex".into());
        };
        let cross = (b.as_dvec3() - a.as_dvec3()).cross(c.as_dvec3() - a.as_dvec3());
        let render_cross = (*b - *a).cross(*c - *a);
        if !render_cross.length_squared().is_finite() || render_cross.length_squared() <= 0.0 {
            return Err("Frozen mesh triangle cannot produce finite render normals".into());
        }
        if cross.length_squared() <= 0.0 {
            return Err("Frozen mesh contains a degenerate triangle".into());
        }
    }
    Ok(TriangleMesh {
        vertices,
        faces: e.faces.clone(),
        normals: None,
        name: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::units::Unit;

    fn mesh() -> FrozenMeshEvaluator {
        FrozenMeshEvaluator {
            coordinate_unit: Unit::Mm,
            vertices: vec![[0.0, 0.0, 0.0], [1200.0, 0.0, 0.0], [0.0, 1400.0, 0.0]],
            faces: vec![[0, 1, 2]],
        }
    }
    #[test]
    fn frozen_mesh_converts_units_and_preserves_winding() {
        let m = evaluate(&mesh()).unwrap();
        assert_eq!(m.vertices[1], Vec3::new(1.2, 0.0, 0.0));
        assert_eq!(m.faces, vec![[0, 1, 2]]);
    }
    #[test]
    fn frozen_mesh_rejects_invalid_geometry_and_units() {
        let mut m = mesh();
        m.coordinate_unit = Unit::Dimensionless;
        assert!(evaluate(&m).is_err());
        m = mesh();
        m.faces[0][2] = 99;
        assert!(evaluate(&m).is_err());
        m = mesh();
        m.faces[0][2] = 1;
        assert!(evaluate(&m).is_err());
        m = mesh();
        m.vertices[1][0] = f64::INFINITY;
        assert!(evaluate(&m).is_err());
        m = mesh();
        m.vertices[1][0] = 1e30;
        m.vertices[2][1] = 1e30;
        assert!(evaluate(&m).is_err());
        m = mesh();
        m.vertices[1][0] = f64::MAX;
        assert!(evaluate(&m).is_err());
    }
}
