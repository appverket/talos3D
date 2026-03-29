use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CadFormat {
    Dwg,
    Dxf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CadVersion {
    Unknown,
    AcadR13,
    AcadR14,
    Acad2000,
    Acad2004,
    Acad2007,
    Acad2010,
    Acad2013,
    Acad2018,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CadDocument {
    pub format: CadFormat,
    pub version: CadVersion,
    pub units: Option<String>,
    pub extents: Option<Extents3>,
    pub layers: Vec<Layer>,
    pub blocks: Vec<Block>,
    pub entities: Vec<Entity>,
}

impl CadDocument {
    pub fn empty(format: CadFormat, version: CadVersion) -> Self {
        Self {
            format,
            version,
            units: None,
            extents: None,
            layers: Vec::new(),
            blocks: Vec::new(),
            entities: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extents3 {
    pub min: Point3,
    pub max: Point3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub name: String,
    pub base_point: Point3,
    pub entities: Vec<Entity>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityCommon {
    pub handle: Option<String>,
    pub layer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Entity {
    Line(Line),
    Arc(Arc),
    Circle(Circle),
    Polyline(Polyline),
    Face3D(Face3D),
    Insert(Insert),
    Unknown(EntityCommon),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub common: EntityCommon,
    pub start: Point3,
    pub end: Point3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arc {
    pub common: EntityCommon,
    pub center: Point3,
    pub radius: f64,
    pub start_angle_degrees: f64,
    pub end_angle_degrees: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Circle {
    pub common: EntityCommon,
    pub center: Point3,
    pub radius: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Polyline {
    pub common: EntityCommon,
    pub points: Vec<Point3>,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Face3D {
    pub common: EntityCommon,
    pub corners: [Point3; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insert {
    pub common: EntityCommon,
    pub block_name: String,
    pub insertion_point: Point3,
    pub scale: Point3,
    pub rotation_degrees: f64,
    pub column_count: u16,
    pub row_count: u16,
    pub column_spacing: f64,
    pub row_spacing: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Point3 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
}

/// A compact, serializable summary of a `CadDocument` used for oracle comparison.
///
/// Both the ODA-converted path and the native path produce a `CadDocument`.
/// Serialising both to `ImportSummary` and diffing gives a clear convergence metric
/// without requiring exact byte-for-byte equality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportSummary {
    /// All layer names, sorted.
    pub layers: Vec<String>,
    /// All block names, sorted (excluding paper-space pseudo-blocks).
    pub blocks: Vec<String>,
    /// Count of each entity kind in model space (not inside blocks).
    pub model_entity_counts: EntityCounts,
    /// Count of each entity kind summed across all block definitions.
    pub block_entity_counts: EntityCounts,
    /// Axis-aligned bounding box of all model-space coordinates.
    /// `None` if no geometry was decoded.
    pub model_bounds: Option<Bounds3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EntityCounts {
    pub lines: usize,
    pub arcs: usize,
    pub circles: usize,
    pub polylines: usize,
    pub faces: usize,
    pub inserts: usize,
    pub unknown: usize,
}

impl EntityCounts {
    pub fn total(&self) -> usize {
        self.lines
            + self.arcs
            + self.circles
            + self.polylines
            + self.faces
            + self.inserts
            + self.unknown
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bounds3 {
    pub min: Point3,
    pub max: Point3,
}

impl ImportSummary {
    pub fn from_document(doc: &CadDocument) -> Self {
        let layers = doc
            .layers
            .iter()
            .map(|l| l.name.clone())
            .collect::<Vec<_>>();
        let blocks = doc
            .blocks
            .iter()
            .filter(|b| !b.name.starts_with('*'))
            .map(|b| b.name.clone())
            .collect::<Vec<_>>();

        let model_entity_counts = count_entities(&doc.entities);
        let block_entity_counts =
            doc.blocks
                .iter()
                .fold(EntityCounts::default(), |mut acc, block| {
                    let c = count_entities(&block.entities);
                    acc.lines += c.lines;
                    acc.arcs += c.arcs;
                    acc.circles += c.circles;
                    acc.polylines += c.polylines;
                    acc.faces += c.faces;
                    acc.inserts += c.inserts;
                    acc.unknown += c.unknown;
                    acc
                });

        let model_bounds = bounds_of(&doc.entities);

        Self {
            layers,
            blocks,
            model_entity_counts,
            block_entity_counts,
            model_bounds,
        }
    }

    /// Returns a human-readable diff against `other`.
    /// Empty string means the two summaries are equivalent.
    pub fn diff_report(&self, other: &ImportSummary) -> String {
        let mut lines = Vec::new();

        let missing_layers: Vec<_> = self
            .layers
            .iter()
            .filter(|l| !other.layers.contains(l))
            .collect();
        let extra_layers: Vec<_> = other
            .layers
            .iter()
            .filter(|l| !self.layers.contains(l))
            .collect();
        if !missing_layers.is_empty() {
            lines.push(format!("layers missing in native: {:?}", missing_layers));
        }
        if !extra_layers.is_empty() {
            lines.push(format!("extra layers in native: {:?}", extra_layers));
        }

        let missing_blocks: Vec<_> = self
            .blocks
            .iter()
            .filter(|b| !other.blocks.contains(b))
            .collect();
        if !missing_blocks.is_empty() {
            lines.push(format!("blocks missing in native: {:?}", missing_blocks));
        }

        diff_count(
            &mut lines,
            "model lines",
            self.model_entity_counts.lines,
            other.model_entity_counts.lines,
        );
        diff_count(
            &mut lines,
            "model arcs",
            self.model_entity_counts.arcs,
            other.model_entity_counts.arcs,
        );
        diff_count(
            &mut lines,
            "model circles",
            self.model_entity_counts.circles,
            other.model_entity_counts.circles,
        );
        diff_count(
            &mut lines,
            "model polylines",
            self.model_entity_counts.polylines,
            other.model_entity_counts.polylines,
        );
        diff_count(
            &mut lines,
            "model faces",
            self.model_entity_counts.faces,
            other.model_entity_counts.faces,
        );
        diff_count(
            &mut lines,
            "model inserts",
            self.model_entity_counts.inserts,
            other.model_entity_counts.inserts,
        );

        if let (Some(oracle_b), Some(native_b)) = (&self.model_bounds, &other.model_bounds) {
            let overlap = bounds_overlap(oracle_b, native_b);
            if !overlap {
                lines.push(format!(
                    "bounds do not overlap: oracle [{:?}..{:?}] native [{:?}..{:?}]",
                    oracle_b.min, oracle_b.max, native_b.min, native_b.max
                ));
            }
        } else if self.model_bounds.is_some() && other.model_bounds.is_none() {
            lines.push("native produced no geometry (oracle has bounds)".to_string());
        }

        lines.join("\n")
    }
}

fn count_entities(entities: &[Entity]) -> EntityCounts {
    let mut counts = EntityCounts::default();
    for e in entities {
        match e {
            Entity::Line(_) => counts.lines += 1,
            Entity::Arc(_) => counts.arcs += 1,
            Entity::Circle(_) => counts.circles += 1,
            Entity::Polyline(_) => counts.polylines += 1,
            Entity::Face3D(_) => counts.faces += 1,
            Entity::Insert(_) => counts.inserts += 1,
            Entity::Unknown(_) => counts.unknown += 1,
        }
    }
    counts
}

fn bounds_of(entities: &[Entity]) -> Option<Bounds3> {
    let mut min = Point3 {
        x: f64::MAX,
        y: f64::MAX,
        z: f64::MAX,
    };
    let mut max = Point3 {
        x: f64::MIN,
        y: f64::MIN,
        z: f64::MIN,
    };
    let mut any = false;
    for e in entities {
        let pts: Vec<Point3> = match e {
            Entity::Line(l) => vec![l.start, l.end],
            Entity::Arc(a) => vec![a.center],
            Entity::Circle(c) => vec![c.center],
            Entity::Polyline(p) => p.points.clone(),
            Entity::Face3D(f) => f.corners.to_vec(),
            Entity::Insert(i) => vec![i.insertion_point],
            Entity::Unknown(_) => vec![],
        };
        for p in pts {
            if p.x.is_finite() && p.y.is_finite() && p.z.is_finite() {
                min.x = min.x.min(p.x);
                min.y = min.y.min(p.y);
                min.z = min.z.min(p.z);
                max.x = max.x.max(p.x);
                max.y = max.y.max(p.y);
                max.z = max.z.max(p.z);
                any = true;
            }
        }
    }
    if any {
        Some(Bounds3 { min, max })
    } else {
        None
    }
}

fn bounds_overlap(a: &Bounds3, b: &Bounds3) -> bool {
    a.max.x >= b.min.x && b.max.x >= a.min.x && a.max.y >= b.min.y && b.max.y >= a.min.y
}

fn diff_count(lines: &mut Vec<String>, label: &str, oracle: usize, native: usize) {
    if oracle == 0 {
        return;
    }
    let ratio = native as f64 / oracle as f64;
    if ratio < 0.5 {
        lines.push(format!(
            "{label}: oracle={oracle} native={native} ({:.0}%)",
            ratio * 100.0
        ));
    }
}
