use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use talos3d_core::{plugins::identity::ElementId, plugins::modeling::primitives::TriangleMesh};

pub const DEFAULT_TERRAIN_MAX_TRIANGLE_AREA: f32 = 25.0;
pub const DEFAULT_TERRAIN_MINIMUM_ANGLE: f32 = 10.0;
pub const DEFAULT_TERRAIN_CONTOUR_INTERVAL: f32 = 1.0;
pub const DEFAULT_TERRAIN_CONTOUR_JOIN_TOLERANCE: f32 = 1.5;
pub const DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING: f32 = 2.0;

fn default_drape_sample_spacing() -> f32 {
    DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ElevationCurveType {
    Major,
    Minor,
    Index,
    #[default]
    Supplementary,
}

impl ElevationCurveType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Index => "index",
            Self::Supplementary => "supplementary",
        }
    }
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElevationCurve {
    pub points: Vec<Vec3>,
    pub elevation: f32,
    pub source_layer: String,
    pub curve_type: ElevationCurveType,
    pub survey_source_id: Option<String>,
}

#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainSurface {
    pub name: String,
    pub source_curve_ids: Vec<ElementId>,
    pub datum_elevation: f32,
    pub boundary: Vec<Vec2>,
    pub max_triangle_area: f32,
    pub minimum_angle: f32,
    pub contour_interval: f32,
    #[serde(default = "default_drape_sample_spacing")]
    pub drape_sample_spacing: f32,
    pub offset: Vec3,
}

impl TerrainSurface {
    pub fn new(name: String, source_curve_ids: Vec<ElementId>) -> Self {
        Self {
            name,
            source_curve_ids,
            datum_elevation: 0.0,
            boundary: Vec::new(),
            max_triangle_area: DEFAULT_TERRAIN_MAX_TRIANGLE_AREA,
            minimum_angle: DEFAULT_TERRAIN_MINIMUM_ANGLE,
            contour_interval: DEFAULT_TERRAIN_CONTOUR_INTERVAL,
            drape_sample_spacing: DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING,
            offset: Vec3::ZERO,
        }
    }
}

#[derive(Component, Debug, Clone, PartialEq)]
pub struct TerrainMeshCache {
    pub mesh: TriangleMesh,
    pub contour_segments: Vec<[Vec3; 2]>,
}

impl Default for TerrainMeshCache {
    fn default() -> Self {
        Self {
            mesh: TriangleMesh {
                vertices: Vec::new(),
                faces: Vec::new(),
                normals: Some(Vec::new()),
                name: None,
            },
            contour_segments: Vec::new(),
        }
    }
}

#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NeedsTerrainMesh;
