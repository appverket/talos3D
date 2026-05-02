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
    modeling::primitives::TriangleMesh,
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

    pub fn sampled(chart_id: impl Into<String>, chart: SampledHostChart) -> Self {
        Self {
            chart_id: chart_id.into(),
            kind: HostChartKind::Sampled(chart),
        }
    }

    pub fn contains_point(&self, point: Vec2) -> bool {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.contains_point(point),
            HostChartKind::Cylindrical(chart) => chart.contains_point(point),
            HostChartKind::Sampled(chart) => chart.contains_point(point),
        }
    }

    pub fn point_at(&self, point: Vec2) -> Vec3 {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.point_at(point),
            HostChartKind::Cylindrical(chart) => chart.point_at(point),
            HostChartKind::Sampled(chart) => chart.point_at(point),
        }
    }

    pub fn normal_at(&self, point: Vec2) -> Vec3 {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.normal_at(point),
            HostChartKind::Cylindrical(chart) => chart.normal_at(point),
            HostChartKind::Sampled(chart) => chart.normal_at(point),
        }
    }

    pub fn u_domain(&self) -> ChartDomain {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.u_domain,
            HostChartKind::Cylindrical(chart) => chart.theta_domain,
            HostChartKind::Sampled(chart) => chart.u_domain,
        }
    }

    pub fn v_domain(&self) -> ChartDomain {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.v_domain,
            HostChartKind::Cylindrical(chart) => chart.z_domain,
            HostChartKind::Sampled(chart) => chart.v_domain,
        }
    }

    pub fn thickness(&self) -> f32 {
        match &self.kind {
            HostChartKind::Planar(chart) => chart.thickness,
            HostChartKind::Cylindrical(chart) => chart.thickness,
            HostChartKind::Sampled(chart) => chart.thickness,
        }
    }

    pub fn wraps_u(&self) -> bool {
        match &self.kind {
            HostChartKind::Cylindrical(chart) => {
                (chart.theta_domain.length() - std::f32::consts::TAU).abs() <= 1e-4
            }
            HostChartKind::Planar(_) | HostChartKind::Sampled(_) => false,
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

    fn offset_point_at(&self, point: Vec2, side: f32) -> Vec3 {
        self.point_at(point) + self.normal_at(point) * (self.thickness() * 0.5 * side)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostChartKind {
    Planar(PlanarHostChart),
    Cylindrical(CylindricalHostChart),
    Sampled(SampledHostChart),
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
pub struct SampledHostChart {
    pub u_domain: ChartDomain,
    pub v_domain: ChartDomain,
    pub thickness: f32,
    pub u_count: usize,
    pub v_count: usize,
    pub samples: Vec<SampledHostChartPoint>,
}

impl SampledHostChart {
    pub fn new(
        u_domain: ChartDomain,
        v_domain: ChartDomain,
        thickness: f32,
        u_count: usize,
        v_count: usize,
        samples: Vec<SampledHostChartPoint>,
    ) -> Self {
        Self {
            u_domain,
            v_domain,
            thickness,
            u_count,
            v_count,
            samples,
        }
    }

    pub fn contains_point(&self, point: Vec2) -> bool {
        self.u_domain.contains(point.x) && self.v_domain.contains(point.y)
    }

    pub fn point_at(&self, point: Vec2) -> Vec3 {
        self.sample_at(point, |sample| sample.position)
    }

    pub fn normal_at(&self, point: Vec2) -> Vec3 {
        self.sample_at(point, |sample| sample.normal)
            .normalize_or_zero()
    }

    fn sample_at(&self, point: Vec2, field: impl Fn(SampledHostChartPoint) -> Vec3) -> Vec3 {
        if self.u_count < 2 || self.v_count < 2 || self.samples.len() != self.u_count * self.v_count
        {
            return Vec3::ZERO;
        }

        let u = grid_coordinate(self.u_domain, point.x, self.u_count);
        let v = grid_coordinate(self.v_domain, point.y, self.v_count);
        let u0 = u.floor() as usize;
        let v0 = v.floor() as usize;
        let u1 = (u0 + 1).min(self.u_count - 1);
        let v1 = (v0 + 1).min(self.v_count - 1);
        let u_t = u - u0 as f32;
        let v_t = v - v0 as f32;

        let p00 = field(self.samples[sampled_index(u0, v0, self.v_count)]);
        let p10 = field(self.samples[sampled_index(u1, v0, self.v_count)]);
        let p01 = field(self.samples[sampled_index(u0, v1, self.v_count)]);
        let p11 = field(self.samples[sampled_index(u1, v1, self.v_count)]);

        p00.lerp(p10, u_t).lerp(p01.lerp(p11, u_t), v_t)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SampledHostChartPoint {
    pub position: Vec3,
    pub normal: Vec3,
}

fn grid_coordinate(domain: ChartDomain, value: f32, count: usize) -> f32 {
    if !domain.is_valid() || count < 2 {
        return 0.0;
    }
    (((value - domain.min) / domain.length()) * (count as f32 - 1.0)).clamp(0.0, count as f32 - 1.0)
}

fn sampled_index(u_index: usize, v_index: usize, v_count: usize) -> usize {
    u_index * v_count + v_index
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

    pub fn bounds(&self) -> Option<ChartSpaceBounds> {
        let first = *self.vertices.first()?;
        let mut min = first;
        let mut max = first;
        for point in &self.vertices {
            min = min.min(*point);
            max = max.max(*point);
        }
        Some(ChartSpaceBounds { min, max })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChartSpaceBounds {
    pub min: Vec2,
    pub max: Vec2,
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

pub fn evaluate_chart_host_mesh_with_openings(
    chart: &HostChart,
    openings: &[ChartSpaceOpeningFeature],
) -> Result<TriangleMesh, GeometryHealthReport> {
    let mut validation_issues = Vec::new();
    let mut opening_rects = Vec::new();

    for opening in openings {
        let report = chart.validate_opening(opening);
        validation_issues.extend(report.issues);
        if let Some(bounds) = opening.profile_loop_2d.bounds() {
            opening_rects.push(OpeningRect {
                u_min: bounds.min.x,
                u_max: bounds.max.x,
                v_min: bounds.min.y,
                v_max: bounds.max.y,
            });
        }
    }

    if !validation_issues.is_empty() {
        return Err(GeometryHealthReport {
            issues: validation_issues,
        });
    }

    let u_domain = chart.u_domain();
    let v_domain = chart.v_domain();
    let mut u_breakpoints = vec![u_domain.min, u_domain.max];
    let mut v_breakpoints = vec![v_domain.min, v_domain.max];

    for opening in &opening_rects {
        u_breakpoints.push(opening.u_min);
        u_breakpoints.push(opening.u_max);
        v_breakpoints.push(opening.v_min);
        v_breakpoints.push(opening.v_max);
    }

    sort_dedup_breakpoints(&mut u_breakpoints);
    sort_dedup_breakpoints(&mut v_breakpoints);

    let u_cells = u_breakpoints.len().saturating_sub(1);
    let v_cells = v_breakpoints.len().saturating_sub(1);
    let wraps_u = chart.wraps_u();
    let mut solid_cells = vec![false; u_cells * v_cells];

    for u_index in 0..u_cells {
        for v_index in 0..v_cells {
            let center = Vec2::new(
                (u_breakpoints[u_index] + u_breakpoints[u_index + 1]) * 0.5,
                (v_breakpoints[v_index] + v_breakpoints[v_index + 1]) * 0.5,
            );
            let inside_opening = opening_rects.iter().any(|opening| opening.contains(center));
            solid_cells[cell_index(u_index, v_index, v_cells)] = !inside_opening;
        }
    }

    let mut builder = MeshBuilder::default();
    for u_index in 0..u_cells {
        for v_index in 0..v_cells {
            if !solid_cells[cell_index(u_index, v_index, v_cells)] {
                continue;
            }

            let u0 = u_breakpoints[u_index];
            let u1 = u_breakpoints[u_index + 1];
            let v0 = v_breakpoints[v_index];
            let v1 = v_breakpoints[v_index + 1];

            builder.add_quad(
                chart.offset_point_at(Vec2::new(u0, v0), 1.0),
                chart.offset_point_at(Vec2::new(u1, v0), 1.0),
                chart.offset_point_at(Vec2::new(u1, v1), 1.0),
                chart.offset_point_at(Vec2::new(u0, v1), 1.0),
            );
            builder.add_quad(
                chart.offset_point_at(Vec2::new(u0, v0), -1.0),
                chart.offset_point_at(Vec2::new(u0, v1), -1.0),
                chart.offset_point_at(Vec2::new(u1, v1), -1.0),
                chart.offset_point_at(Vec2::new(u1, v0), -1.0),
            );

            if !u_neighbor_is_solid(
                &solid_cells,
                u_index,
                v_index,
                u_cells,
                v_cells,
                -1,
                wraps_u,
            ) {
                builder.add_quad(
                    chart.offset_point_at(Vec2::new(u0, v0), 1.0),
                    chart.offset_point_at(Vec2::new(u0, v1), 1.0),
                    chart.offset_point_at(Vec2::new(u0, v1), -1.0),
                    chart.offset_point_at(Vec2::new(u0, v0), -1.0),
                );
            }
            if !u_neighbor_is_solid(&solid_cells, u_index, v_index, u_cells, v_cells, 1, wraps_u) {
                builder.add_quad(
                    chart.offset_point_at(Vec2::new(u1, v0), 1.0),
                    chart.offset_point_at(Vec2::new(u1, v0), -1.0),
                    chart.offset_point_at(Vec2::new(u1, v1), -1.0),
                    chart.offset_point_at(Vec2::new(u1, v1), 1.0),
                );
            }
            if v_index == 0 || !solid_cells[cell_index(u_index, v_index - 1, v_cells)] {
                builder.add_quad(
                    chart.offset_point_at(Vec2::new(u0, v0), 1.0),
                    chart.offset_point_at(Vec2::new(u0, v0), -1.0),
                    chart.offset_point_at(Vec2::new(u1, v0), -1.0),
                    chart.offset_point_at(Vec2::new(u1, v0), 1.0),
                );
            }
            if v_index + 1 == v_cells || !solid_cells[cell_index(u_index, v_index + 1, v_cells)] {
                builder.add_quad(
                    chart.offset_point_at(Vec2::new(u0, v1), 1.0),
                    chart.offset_point_at(Vec2::new(u1, v1), 1.0),
                    chart.offset_point_at(Vec2::new(u1, v1), -1.0),
                    chart.offset_point_at(Vec2::new(u0, v1), -1.0),
                );
            }
        }
    }

    Ok(builder.into_triangle_mesh("chart-host-with-openings"))
}

#[derive(Debug, Clone, Copy)]
struct OpeningRect {
    u_min: f32,
    u_max: f32,
    v_min: f32,
    v_max: f32,
}

impl OpeningRect {
    fn contains(&self, point: Vec2) -> bool {
        point.x > self.u_min && point.x < self.u_max && point.y > self.v_min && point.y < self.v_max
    }
}

#[derive(Default)]
struct MeshBuilder {
    vertices: Vec<Vec3>,
    faces: Vec<[u32; 3]>,
}

impl MeshBuilder {
    fn add_quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3) {
        let base = self.vertices.len() as u32;
        self.vertices.extend([a, b, c, d]);
        self.faces.push([base, base + 1, base + 2]);
        self.faces.push([base, base + 2, base + 3]);
    }

    fn into_triangle_mesh(self, name: impl Into<String>) -> TriangleMesh {
        TriangleMesh {
            vertices: self.vertices,
            faces: self.faces,
            normals: None,
            name: Some(name.into()),
        }
    }
}

fn sort_dedup_breakpoints(values: &mut Vec<f32>) {
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    values.dedup_by(|left, right| (*left - *right).abs() <= 1e-6);
}

fn cell_index(u_index: usize, v_index: usize, v_cells: usize) -> usize {
    u_index * v_cells + v_index
}

fn u_neighbor_is_solid(
    solid_cells: &[bool],
    u_index: usize,
    v_index: usize,
    u_cells: usize,
    v_cells: usize,
    offset: isize,
    wraps_u: bool,
) -> bool {
    let neighbor = u_index as isize + offset;
    if neighbor < 0 {
        return wraps_u && solid_cells[cell_index(u_cells - 1, v_index, v_cells)];
    }
    if neighbor >= u_cells as isize {
        return wraps_u && solid_cells[cell_index(0, v_index, v_cells)];
    }
    solid_cells[cell_index(neighbor as usize, v_index, v_cells)]
}

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_2, PI};

    use super::*;
    use crate::plugins::modeling::geometry_health::check_triangle_mesh_health;

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

    #[test]
    fn sampled_chart_maps_without_special_case_evaluator_logic() {
        let chart = HostChart::sampled(
            "freeform_panel",
            SampledHostChart::new(
                ChartDomain::new(0.0, 2.0),
                ChartDomain::new(0.0, 2.0),
                0.12,
                2,
                2,
                vec![
                    SampledHostChartPoint {
                        position: Vec3::new(0.0, 0.0, 0.0),
                        normal: Vec3::Z,
                    },
                    SampledHostChartPoint {
                        position: Vec3::new(0.0, 2.0, 0.1),
                        normal: Vec3::Z,
                    },
                    SampledHostChartPoint {
                        position: Vec3::new(2.0, 0.0, 0.2),
                        normal: Vec3::Z,
                    },
                    SampledHostChartPoint {
                        position: Vec3::new(2.0, 2.0, 0.3),
                        normal: Vec3::Z,
                    },
                ],
            ),
        );
        let opening = ChartSpaceOpeningFeature::new(
            ElementId(17),
            "freeform_panel",
            ChartSpaceProfileLoop::rectangle(Vec2::new(0.5, 0.5), Vec2::new(1.5, 1.5)),
        );

        let mesh = evaluate_chart_host_mesh_with_openings(&chart, &[opening]).unwrap();
        let report = check_triangle_mesh_health(&mesh);

        assert!(report.is_clean(), "{report:#?}");
        assert!(!mesh.faces.is_empty());
    }

    #[test]
    fn planar_chart_host_mesh_with_opening_has_clean_health() {
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
            ChartSpaceProfileLoop::rectangle(Vec2::new(1.2, 0.9), Vec2::new(2.2, 2.1)),
        );

        let mesh = evaluate_chart_host_mesh_with_openings(&chart, &[opening]).unwrap();
        let report = check_triangle_mesh_health(&mesh);

        assert!(report.is_clean(), "{report:#?}");
        assert!(!mesh.faces.is_empty());
    }

    #[test]
    fn cylindrical_chart_host_mesh_with_opening_uses_same_evaluator() {
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
            ChartSpaceProfileLoop::rectangle(Vec2::new(-0.35, 1.0), Vec2::new(0.35, 2.5)),
        );

        let mesh = evaluate_chart_host_mesh_with_openings(&chart, &[opening]).unwrap();
        let report = check_triangle_mesh_health(&mesh);

        assert!(report.is_clean(), "{report:#?}");
        assert!(!mesh.faces.is_empty());
    }
}
