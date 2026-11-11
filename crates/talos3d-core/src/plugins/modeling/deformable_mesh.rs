//! Pure measured-geometry deformation at evaluation time, using the normal GPU
//! mesh cache. Never scans imported triangles in the viewport update loop.
use super::{
    definition::{BodyExpr, DeformableMeshEvaluator},
    primitives::TriangleMesh,
};
use crate::plugins::units::Unit;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(super) fn validate(e: &DeformableMeshEvaluator) -> Result<(), String> {
    super::frozen_mesh::evaluate(&e.mesh)?;
    if e.axes.is_empty() || e.axes.len() > 3 {
        return Err("Deformable mesh requires 1..=3 axis controls".into());
    }
    let mut seen = HashSet::new();
    for a in &e.axes {
        if a.axis > 2
            || !seen.insert(a.axis)
            || a.parameter.trim().is_empty()
            || !a.reference_value.is_finite()
            || !(2..=256).contains(&a.stations.len())
        {
            return Err("Invalid deformable mesh axis, parameter or station count".into());
        }
        if a.stations.iter().flatten().any(|v| !v.is_finite())
            || a.stations.windows(2).any(|s| s[1][0] <= s[0][0])
        {
            return Err(
                "Deformation source stations must be finite and strictly increasing".into(),
            );
        }
        let lo = a.stations[0][0];
        let hi = a.stations.last().unwrap()[0];
        if e.mesh
            .vertices
            .iter()
            .any(|v| v[a.axis] < lo - 1e-8 || v[a.axis] > hi + 1e-8)
        {
            return Err("Deformation stations must cover every source vertex".into());
        }
    }
    Ok(())
}

pub(super) fn evaluate(
    e: &DeformableMeshEvaluator,
    values: &HashMap<String, Value>,
    units: &HashMap<String, Unit>,
) -> Result<TriangleMesh, String> {
    validate(e)?;
    let scale = super::polygon_evaluator::length_scale(e.mesh.coordinate_unit)?;
    let mut mesh = e.mesh.clone();
    for a in &e.axes {
        let value = super::polygon_evaluator::evaluate_coordinate(
            &BodyExpr::Reference {
                path: a.parameter.clone(),
            },
            values,
            units,
            e.mesh.coordinate_unit,
        )? as f64
            / scale;
        let delta = value - a.reference_value;
        let targets: Vec<f64> = a.stations.iter().map(|s| s[0] + s[1] * delta).collect();
        if targets.iter().any(|v| !v.is_finite()) || targets.windows(2).any(|s| s[1] <= s[0]) {
            return Err(format!(
                "Dimension '{}' would collapse or invert measured geometry",
                a.parameter
            ));
        }
        for v in &mut mesh.vertices {
            let x = v[a.axis];
            let i = a
                .stations
                .partition_point(|s| s[0] < x)
                .saturating_sub(1)
                .min(a.stations.len() - 2);
            let fraction = ((x - a.stations[i][0]) / (a.stations[i + 1][0] - a.stations[i][0]))
                .clamp(0.0, 1.0);
            v[a.axis] = targets[i] + fraction * (targets[i + 1] - targets[i]);
        }
    }
    super::frozen_mesh::evaluate(&mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::modeling::definition::{FrozenMeshEvaluator, MeshAxisDeformation};
    fn sample() -> DeformableMeshEvaluator {
        DeformableMeshEvaluator {
            mesh: FrozenMeshEvaluator {
                coordinate_unit: Unit::M,
                vertices: vec![[-1., 0., 0.], [-0.9, 1., 0.], [0.9, 1., 0.], [1., 0., 0.]],
                faces: vec![[0, 1, 2], [0, 2, 3]],
            },
            axes: vec![MeshAxisDeformation {
                axis: 0,
                parameter: "width".into(),
                reference_value: 2.,
                stations: vec![[-1., -0.5], [-0.9, -0.5], [0.9, 0.5], [1., 0.5]],
            }],
        }
    }
    #[test]
    fn deformation_preserves_measured_border_and_reference_geometry() {
        let e = sample();
        let units = HashMap::new();
        let values = HashMap::from([("width".into(), Value::from(2.))]);
        let original = evaluate(&e, &values, &units).unwrap();
        assert_eq!(
            original.vertices,
            super::super::frozen_mesh::evaluate(&e.mesh)
                .unwrap()
                .vertices
        );
        let values = HashMap::from([("width".into(), Value::from(3.))]);
        let m = evaluate(&e, &values, &units).unwrap();
        assert!((m.vertices[1].x - m.vertices[0].x - 0.1).abs() < 1e-6);
        assert!((m.vertices[3].x - m.vertices[0].x - 3.).abs() < 1e-6);
        assert_eq!(m.faces, e.mesh.faces);
    }
    #[test]
    fn deformation_rejects_collapse_missing_parameter_bad_units_and_coverage() {
        let mut e = sample();
        assert!(evaluate(&e, &HashMap::new(), &HashMap::new()).is_err());
        let values = HashMap::from([("width".into(), Value::from(0.1))]);
        assert!(evaluate(&e, &values, &HashMap::new()).is_err());
        let units = HashMap::from([("width".into(), Unit::Mm)]);
        assert!(evaluate(&e, &values, &units).is_err());
        e.axes[0].stations[0][0] = 0.;
        assert!(validate(&e).is_err());
        e = sample();
        e.axes.push(e.axes[0].clone());
        assert!(validate(&e).is_err());
    }
}
