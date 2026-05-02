//! Host charts and chart-space openings for Complex Authored Entities.
//!
//! A host chart maps a compact 2D authoring domain into 3D. Openings can then
//! be authored as loops in chart space instead of as overlapping 3D boxes. This
//! is the ADR-048 substrate that lets a planar wall and a cylindrical turret
//! share one opening representation.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::{
    identity::ElementId,
    modeling::geometry_health::{
        GeometryHealthIssue, GeometryHealthIssueKind, GeometryHealthReport,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChartDomain {
    pub min: f32,
    pub max: f32,
}

impl ChartDomain {
    pub fn new(min: f32, max: f32) -> Self {
        Self { min, max }
    }

    pub fn contains(&self, value: f32) -> bool {
        value >= self.min && value <= self.max
    }

    pub fn length(&self) -> f32 {
        self.max - self.min
    }

    pub fn is_valid(&self) -> bool {
        self.min.is_finite() && self.max.is_finite() && self.max > self.min
    }
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostChart {
    pub chart_id: String,
    pub kind: HostChartKind,
}

impl HostChart {
    pub fn planar(chart_id: impl Into<String>, chart: PlanarHostChart) -> Self {
        Self {
            chart_id: chart_id.into(),
            kind: HostChartKind::Planar(chart),
        }
    }

    pub fn cylindrical(chart_id: impl Into<String>, chart: CylindricalHostChart) -> Self {
        Self {
            chart_id: chart_id.into(),
            kind: HostChartKind::Cylindrical(chart),
        }
    }

    pub fn contains_point(&self, point: Vec2) -> bool {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.contains_point(point),
            HostChartKind::Cylindrical(chart) => chart.contains_point(point),
        }
    }

    pub fn point_at(&self, point: Vec2) -> Vec3 {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.point_at(point),
            HostChartKind::Cylindrical(chart) => chart.point_at(point),
        }
    }

    pub fn normal_at(&self, point: Vec2) -> Vec3 {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.normal_at(point),
            HostChartKind::Cylindrical(chart) => chart.normal_at(point),
        }
    }

    pub fn validate_opening(&self, opening: &ChartSpaceOpeningFeature) -> GeometryHealthReport {
        let mut issues = Vec::new();

        if opening.chart_ref != self.chart_id {
            issues.push(GeometryHealthIssue {
                kind: GeometryHealthIssueKind::HostFeatureOutsideHostDomain,
                message: format!(
                    "Opening references chart '{}' but was evaluated against '{}'",
                    opening.chart_ref, self.chart_id
                ),
                face_indices: Vec::new(),
                measured_area: None,
            });
        }

        if opening.profile_loop_2d.vertices.len() < 3 {
            issues.push(GeometryHealthIssue {
                kind: GeometryHealthIssueKind::HostFeatureOutsideHostDomain,
                message: "Opening profile must contain at least three vertices".to_string(),
                face_indices: Vec::new(),
                measured_area: None,
            });
        }

        for (index, point) in opening.profile_loop_2d.vertices.iter().enumerate() {
            if !self.contains_point(*point) {
                issues.push(GeometryHealthIssue {
                    kind: GeometryHealthIssueKind::HostFeatureOutsideHostDomain,
                    message: format!(
                        "Opening profile vertex {index} lies outside host chart '{}'",
                        self.chart_id
                    ),
                    face_indices: Vec::new(),
                    measured_area: None,
                });
            }
        }

        GeometryHealthReport { issues }
    }

    pub fn world_profile(&self, opening: &ChartSpaceOpeningFeature) -> Vec<Vec3> {
        opening
            .profile_loop_2d
            .vertices
            .iter()
            .map(|point| self.point_at(*point))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostChartKind {
    Planar(PlanarHostChart),
    Cylindrical(CylindricalHostChart),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlanarHostChart {
    pub origin: Vec3,
    pub u_axis: Vec3,
    pub v_axis: Vec3,
    pub u_domain: ChartDomain,
    pub v_domain: ChartDomain,
    pub thickness: f32,
}

impl PlanarHostChart {
    pub fn new(
        origin: Vec3,
        u_axis: Vec3,
        v_axis: Vec3,
        u_domain: ChartDomain,
        v_domain: ChartDomain,
        thickness: f32,
    ) -> Self {
        Self {
            origin,
            u_axis: u_axis.normalize_or_zero(),
            v_axis: v_axis.normalize_or_zero(),
            u_domain,
            v_domain,
            thickness,
        }
    }

    pub fn contains_point(&self, point: Vec2) -> bool {
        self.u_domain.contains(point.x) && self.v_domain.contains(point.y)
    }

    pub fn point_at(&self, point: Vec2) -> Vec3 {
        self.origin + self.u_axis * point.x + self.v_axis * point.y
    }

    pub fn normal_at(&self, _point: Vec2) -> Vec3 {
        self.u_axis.cross(self.v_axis).normalize_or_zero()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CylindricalHostChart {
    pub center: Vec3,
    pub radius: f32,
    pub theta_domain: ChartDomain,
    pub z_domain: ChartDomain,
    pub theta_zero_axis: Vec3,
    pub up_axis: Vec3,
    pub thickness: f32,
}

impl CylindricalHostChart {
    pub fn new(
        center: Vec3,
        radius: f32,
        theta_domain: ChartDomain,
        z_domain: ChartDomain,
        theta_zero_axis: Vec3,
        up_axis: Vec3,
        thickness: f32,
    ) -> Self {
        Self {
            center,
            radius,
            theta_domain,
            z_domain,
            theta_zero_axis: theta_zero_axis.normalize_or_zero(),
            up_axis: up_axis.normalize_or_zero(),
            thickness,
        }
    }

    pub fn contains_point(&self, point: Vec2) -> bool {
        self.theta_domain.contains(point.x) && self.z_domain.contains(point.y)
    }

    pub fn point_at(&self, point: Vec2) -> Vec3 {
        self.center + self.radial_at(point.x) * self.radius + self.up_axis * point.y
    }

    pub fn normal_at(&self, point: Vec2) -> Vec3 {
        self.radial_at(point.x)
    }

    fn radial_at(&self, theta: f32) -> Vec3 {
        let tangent_zero = self.up_axis.cross(self.theta_zero_axis).normalize_or_zero();
        (self.theta_zero_axis * theta.cos() + tangent_zero * theta.sin()).normalize_or_zero()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartSpaceProfileLoop {
    pub vertices: Vec<Vec2>,
}

impl ChartSpaceProfileLoop {
    pub fn new(vertices: Vec<Vec2>) -> Self {
        Self { vertices }
    }

    pub fn rectangle(min: Vec2, max: Vec2) -> Self {
        Self {
            vertices: vec![
                Vec2::new(min.x, min.y),
                Vec2::new(max.x, min.y),
                Vec2::new(max.x, max.y),
                Vec2::new(min.x, max.y),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpeningDepthPolicy {
    ThroughHost,
    SymmetricDepth { depth: f32 },
    OffsetDepth { start_offset: f32, end_offset: f32 },
}

impl Default for OpeningDepthPolicy {
    fn default() -> Self {
        Self::ThroughHost
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpeningClearancePolicy {
    None,
    Uniform { margin: f32 },
}

impl Default for OpeningClearancePolicy {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartSpaceOpeningFeature {
    pub host_ref: ElementId,
    pub chart_ref: String,
    pub profile_loop_2d: ChartSpaceProfileLoop,
    #[serde(default)]
    pub depth_policy: OpeningDepthPolicy,
    #[serde(default)]
    pub clearance_policy: OpeningClearancePolicy,
    pub hosted_fill_ref: Option<ElementId>,
    pub structural_role: Option<String>,
}

impl ChartSpaceOpeningFeature {
    pub fn new(
        host_ref: ElementId,
        chart_ref: impl Into<String>,
        profile_loop_2d: ChartSpaceProfileLoop,
    ) -> Self {
        Self {
            host_ref,
            chart_ref: chart_ref.into(),
            profile_loop_2d,
            depth_policy: OpeningDepthPolicy::ThroughHost,
            clearance_policy: OpeningClearancePolicy::None,
            hosted_fill_ref: None,
            structural_role: None,
        }
    }

    pub fn with_hosted_fill(mut self, hosted_fill_ref: ElementId) -> Self {
        self.hosted_fill_ref = Some(hosted_fill_ref);
        self
    }

    pub fn with_clearance(mut self, clearance_policy: OpeningClearancePolicy) -> Self {
        self.clearance_policy = clearance_policy;
        self
    }

    pub fn with_depth(mut self, depth_policy: OpeningDepthPolicy) -> Self {
        self.depth_policy = depth_policy;
        self
    }

    pub fn with_structural_role(mut self, structural_role: impl Into<String>) -> Self {
        self.structural_role = Some(structural_role.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_2, PI};

    use super::*;

    #[test]
    fn planar_chart_maps_wall_local_opening_profile() {
        let chart = HostChart::planar(
            "wall_face",
            PlanarHostChart::new(
                Vec3::ZERO,
                Vec3::X,
                Vec3::Y,
                ChartDomain::new(0.0, 4.0),
                ChartDomain::new(0.0, 3.0),
                0.2,
            ),
        );
        let opening = ChartSpaceOpeningFeature::new(
            ElementId(7),
            "wall_face",
            ChartSpaceProfileLoop::rectangle(Vec2::new(1.0, 0.8), Vec2::new(2.2, 2.0)),
        );

        let report = chart.validate_opening(&opening);
        let world_profile = chart.world_profile(&opening);

        assert!(report.is_clean(), "{report:#?}");
        assert_eq!(world_profile[0], Vec3::new(1.0, 0.8, 0.0));
        assert_eq!(world_profile[2], Vec3::new(2.2, 2.0, 0.0));
        assert_eq!(chart.normal_at(Vec2::new(1.0, 1.0)), Vec3::Z);
    }

    #[test]
    fn cylindrical_chart_maps_turret_opening_profile_radially() {
        let chart = HostChart::cylindrical(
            "turret_outer_shell",
            CylindricalHostChart::new(
                Vec3::ZERO,
                2.0,
                ChartDomain::new(-PI, PI),
                ChartDomain::new(0.0, 5.0),
                Vec3::X,
                Vec3::Y,
                0.25,
            ),
        );
        let opening = ChartSpaceOpeningFeature::new(
            ElementId(11),
            "turret_outer_shell",
            ChartSpaceProfileLoop::rectangle(Vec2::new(0.0, 1.0), Vec2::new(FRAC_PI_2, 2.5)),
        );

        let report = chart.validate_opening(&opening);
        let world_profile = chart.world_profile(&opening);

        assert!(report.is_clean(), "{report:#?}");
        assert!((world_profile[0] - Vec3::new(2.0, 1.0, 0.0)).length() < 1e-5);
        assert!((world_profile[1] - Vec3::new(0.0, 1.0, -2.0)).length() < 1e-5);
        assert!((chart.normal_at(Vec2::new(FRAC_PI_2, 2.0)) - Vec3::NEG_Z).length() < 1e-5);
    }

    #[test]
    fn out_of_domain_opening_reports_host_feature_issue() {
        let chart = HostChart::planar(
            "wall_face",
            PlanarHostChart::new(
                Vec3::ZERO,
                Vec3::X,
                Vec3::Y,
                ChartDomain::new(0.0, 4.0),
                ChartDomain::new(0.0, 3.0),
                0.2,
            ),
        );
        let opening = ChartSpaceOpeningFeature::new(
            ElementId(7),
            "wall_face",
            ChartSpaceProfileLoop::rectangle(Vec2::new(3.5, 1.0), Vec2::new(4.5, 2.0)),
        );

        let report = chart.validate_opening(&opening);

        assert_eq!(
            report
                .issues_by_kind(GeometryHealthIssueKind::HostFeatureOutsideHostDomain)
                .count(),
            2
        );
    }

    #[test]
    fn chart_space_opening_feature_round_trips_through_json() {
        let feature = ChartSpaceOpeningFeature::new(
            ElementId(7),
            "wall_face",
            ChartSpaceProfileLoop::rectangle(Vec2::new(1.0, 0.8), Vec2::new(2.2, 2.0)),
        )
        .with_hosted_fill(ElementId(9))
        .with_clearance(OpeningClearancePolicy::Uniform { margin: 0.015 })
        .with_depth(OpeningDepthPolicy::ThroughHost)
        .with_structural_role("window");

        let json = serde_json::to_string(&feature).unwrap();
        let round_trip: ChartSpaceOpeningFeature = serde_json::from_str(&json).unwrap();

        assert_eq!(round_trip, feature);
    }

    #[test]
    fn host_chart_round_trips_through_json() {
        let chart = HostChart::cylindrical(
            "turret_outer_shell",
            CylindricalHostChart::new(
                Vec3::new(1.0, 0.0, 2.0),
                2.0,
                ChartDomain::new(-PI, PI),
                ChartDomain::new(0.0, 5.0),
                Vec3::X,
                Vec3::Y,
                0.25,
            ),
        );

        let json = serde_json::to_string(&chart).unwrap();
        let round_trip: HostChart = serde_json::from_str(&json).unwrap();

        assert_eq!(round_trip, chart);
    }
}
