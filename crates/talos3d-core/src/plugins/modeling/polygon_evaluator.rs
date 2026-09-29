//! Pure evaluation at the canonical body's geometry boundary. Evaluation runs
//! before authored state or derived render entities change.
use super::{
    definition::{BodyExpr, PolygonExtrusionEvaluator},
    profile::{Profile2d, ProfileExtrusion, ProfileSegment},
};
use crate::plugins::units::Unit;
use bevy::prelude::*;
use serde_json::Value;
use std::collections::HashMap;

pub(super) fn length_scale(unit: Unit) -> Result<f64, String> {
    match unit {
        Unit::Mm => Ok(0.001),
        Unit::Cm => Ok(0.01),
        Unit::M => Ok(1.0),
        Unit::Ft => Ok(0.3048),
        Unit::In => Ok(0.0254),
        _ => Err(format!("Expected a length unit, found {unit}")),
    }
}

pub(super) fn evaluate(
    evaluator: &PolygonExtrusionEvaluator,
    values: &HashMap<String, Value>,
    units: &HashMap<String, Unit>,
    centre: Vec3,
) -> Result<ProfileExtrusion, String> {
    length_scale(evaluator.coordinate_unit)?;
    // Bounds keep malformed imported bodies from creating unbounded geometry
    // work. The existing GPU mesh path consumes the resulting extrusion.
    if !(3..=256).contains(&evaluator.profile_xz.len()) {
        return Err("Polygon extrusion requires 3..=256 vertices".into());
    }
    let coordinate = |expr: &BodyExpr| -> Result<f32, String> {
        evaluate_coordinate(expr, values, units, evaluator.coordinate_unit)
    };
    let height = coordinate(&evaluator.height)?;
    if height <= 0.0 {
        return Err("Polygon extrusion height must be positive".into());
    }
    let mut points = evaluator
        .profile_xz
        .iter()
        .map(|point| Ok(Vec2::new(coordinate(&point[0])?, coordinate(&point[1])?)))
        .collect::<Result<Vec<_>, String>>()?;
    validate_contour(&points)?;
    let area = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64)
        .sum::<f64>();
    if area.abs() <= 1e-12 {
        return Err("Polygon extrusion profile has no area".into());
    }
    if area < 0.0 {
        points.reverse();
    }
    Ok(ProfileExtrusion {
        centre,
        profile: Profile2d {
            start: points[0],
            segments: points[1..]
                .iter()
                .map(|point| ProfileSegment::LineTo { to: *point })
                .collect(),
        },
        height,
    })
}

pub(super) fn evaluate_coordinate(
    expr: &BodyExpr,
    values: &HashMap<String, Value>,
    units: &HashMap<String, Unit>,
    coordinate_unit: Unit,
) -> Result<f32, String> {
    if let Some(unit) = expr.evaluated_unit(values, units)? {
        if unit != coordinate_unit && unit != Unit::Dimensionless {
            return Err(format!(
                "Coordinate expected {coordinate_unit}, found {unit}"
            ));
        }
    }
    let value = expr
        .evaluate(values, units)?
        .as_f64()
        .ok_or("Coordinate must be numeric")?
        * length_scale(coordinate_unit)?;
    let result = value as f32;
    if !result.is_finite() {
        return Err("Coordinate must be finite in world units".into());
    }
    Ok(result)
}

fn validate_contour(points: &[Vec2]) -> Result<(), String> {
    // Use double precision for predicates on the finite world-space vertices.
    fn cross(a: Vec2, b: Vec2, c: Vec2) -> f64 {
        (b.x as f64 - a.x as f64) * (c.y as f64 - a.y as f64)
            - (b.y as f64 - a.y as f64) * (c.x as f64 - a.x as f64)
    }
    fn on_segment(a: Vec2, b: Vec2, p: Vec2) -> bool {
        p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)
    }
    let n = points.len();
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        if a == b {
            return Err("Polygon profile has a zero-length edge".into());
        }
        for j in i + 1..n {
            if j == i + 1 || (i == 0 && j == n - 1) {
                continue;
            }
            let (c, d) = (points[j], points[(j + 1) % n]);
            let (ab_c, ab_d, cd_a, cd_b) = (
                cross(a, b, c),
                cross(a, b, d),
                cross(c, d, a),
                cross(c, d, b),
            );
            let crosses = ab_c * ab_d < 0.0 && cd_a * cd_b < 0.0;
            let touches = (ab_c == 0.0 && on_segment(a, b, c))
                || (ab_d == 0.0 && on_segment(a, b, d))
                || (cd_a == 0.0 && on_segment(c, d, a))
                || (cd_b == 0.0 && on_segment(c, d, b));
            if crosses || touches {
                return Err("Polygon profile self-intersects".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn evaluator(points: &[[f64; 2]]) -> PolygonExtrusionEvaluator {
        PolygonExtrusionEvaluator {
            coordinate_unit: Unit::Mm,
            profile_xz: points
                .iter()
                .map(|p| p.map(|v| BodyExpr::Literal { value: json!(v) }))
                .collect(),
            height: BodyExpr::Reference {
                path: "depth".into(),
            },
        }
    }
    #[test]
    fn typed_polygon_evaluation_converts_units_and_preserves_winding() {
        let e = evaluator(&[[0.0, 0.0], [0.0, 1000.0], [2000.0, 0.0]]);
        let geometry = evaluate(
            &e,
            &HashMap::from([("depth".into(), json!(200.0))]),
            &HashMap::from([("depth".into(), Unit::Mm)]),
            Vec3::new(5.0, 2.0, 3.0),
        )
        .unwrap();
        assert_eq!(geometry.height, 0.2);
        assert!(geometry.profile.is_ccw());
        assert_eq!(geometry.centre, Vec3::new(5.0, 2.0, 3.0));
        assert!(geometry
            .profile
            .tessellate(1)
            .contains(&Vec2::new(2.0, 0.0)));
    }
    #[test]
    fn malformed_polygons_and_mismatched_units_are_refused() {
        let values = HashMap::from([("depth".into(), json!(100.0))]);
        let units = HashMap::from([("depth".into(), Unit::Mm)]);
        for points in [
            vec![[0.0, 0.0], [1.0, 1.0], [0.0, 1.0], [1.0, 0.0]],
            vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]],
            vec![[0.0, 0.0], [0.0, 0.0], [1.0, 1.0]],
        ] {
            assert!(evaluate(&evaluator(&points), &values, &units, Vec3::ZERO).is_err());
        }
        let e = evaluator(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
        assert!(evaluate(
            &e,
            &values,
            &HashMap::from([("depth".into(), Unit::Deg)]),
            Vec3::ZERO
        )
        .is_err());
        assert!(evaluate(
            &e,
            &HashMap::from([("depth".into(), json!(-1.0))]),
            &units,
            Vec3::ZERO
        )
        .is_err());
    }
}
