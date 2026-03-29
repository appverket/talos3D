/// generate_oracle — convert a DWG via ODA File Converter and write an ImportSummary oracle JSON.
///
/// Usage:
///   cargo run -p cadio-tools --bin generate_oracle -- <dwg-path> [output.json]
///
/// The output path defaults to:
///   <workspace-root>/tests/fixtures/cadio/survey_oracle.json
///
/// Requirements:
///   - ODA File Converter must be available.  The tool checks (in order):
///     1. $TALOS3D_DWG_CONVERTER env var (path to converter wrapper)
///     2. tools/dwg/ODAFileConverter.app (relative to workspace root)
///     3. /Applications/ODA File Converter.app
///
/// Once generated, commit the oracle JSON so comparison tests can run in CI
/// without needing ODA installed.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use cadio_ir::{
    Arc, Block, CadDocument, CadFormat, CadVersion, Circle, Entity, EntityCommon, Face3D,
    ImportSummary, Insert, Layer, Line, Point3, Polyline,
};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let dwg_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: generate_oracle <dwg-path> [output.json]");

    // CARGO_MANIFEST_DIR = …/crates/cadio-tools → parent → parent = workspace root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.ancestors().nth(2).unwrap_or(&manifest);
    let default_output = workspace.join("tests/fixtures/cadio/survey_oracle.json");
    let output_path = args.next().map(PathBuf::from).unwrap_or(default_output);

    eprintln!("converting {} via ODA…", dwg_path.display());
    let dxf_path = convert_via_oda(&dwg_path).expect("ODA conversion failed");
    eprintln!("converted DXF: {}", dxf_path.display());

    let drawing = dxf::Drawing::load_file(&dxf_path)
        .unwrap_or_else(|e| panic!("failed to read DXF {}: {e}", dxf_path.display()));

    let document = dxf_drawing_to_cad_document(&drawing);
    let summary = ImportSummary::from_document(&document);

    eprintln!(
        "oracle: {} layers, {} blocks, {} model entities, {} block entities",
        summary.layers.len(),
        summary.blocks.len(),
        summary.model_entity_counts.total(),
        summary.block_entity_counts.total(),
    );
    for layer in &summary.layers {
        eprintln!("  layer: {layer}");
    }

    fs::create_dir_all(output_path.parent().unwrap()).expect("failed to create output directory");
    let json = serde_json::to_string_pretty(&summary).expect("serialisation failed");
    fs::write(&output_path, &json).expect("failed to write oracle JSON");
    eprintln!("oracle written to {}", output_path.display());
}

// ── ODA conversion ────────────────────────────────────────────────────────────

fn convert_via_oda(dwg_path: &Path) -> Result<PathBuf, String> {
    let oda = find_oda_converter().ok_or("ODA File Converter not found")?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = env::temp_dir().join(format!("cadio-oracle-{unique}"));
    let input_dir = tmp.join("input");
    let output_dir = tmp.join("output");
    fs::create_dir_all(&input_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;

    let file_name = dwg_path.file_name().ok_or("no file name")?;
    let staged = input_dir.join(file_name);
    fs::copy(dwg_path, &staged).map_err(|e| e.to_string())?;

    let status = Command::new(&oda)
        .args([
            input_dir.to_str().unwrap(),
            output_dir.to_str().unwrap(),
            "ACAD2018",
            "DXF",
            "0", // recurse
            "1", // audit
        ])
        .status()
        .map_err(|e| format!("failed to run ODA: {e}"))?;

    if !status.success() {
        return Err(format!("ODA exited with status {status}"));
    }

    // Find the produced .dxf file.
    for entry in fs::read_dir(&output_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.path().extension().and_then(|e| e.to_str()) == Some("dxf") {
            return Ok(entry.path());
        }
    }
    Err("ODA produced no DXF file".to_string())
}

fn find_oda_converter() -> Option<PathBuf> {
    // 1. Custom env var.
    if let Ok(path) = env::var("TALOS3D_DWG_CONVERTER") {
        let p = PathBuf::from(path.trim());
        if p.exists() {
            return Some(p);
        }
    }

    // 2. Bundled in workspace (tools/dwg/ODAFileConverter.app).
    // CARGO_MANIFEST_DIR is absolute (e.g. /…/talos3d/crates/cadio-tools).
    // Three levels up lands at the workspace root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest.ancestors().nth(2).unwrap_or(&manifest);
    let bundled =
        workspace_root.join("tools/dwg/ODAFileConverter.app/Contents/MacOS/ODAFileConverter");
    if bundled.exists() {
        return Some(bundled);
    }

    // 3. System install.
    let system =
        PathBuf::from("/Applications/ODA File Converter.app/Contents/MacOS/ODAFileConverter");
    if system.exists() {
        return Some(system);
    }

    None
}

// ── DXF → CadDocument ────────────────────────────────────────────────────────

fn dxf_drawing_to_cad_document(drawing: &dxf::Drawing) -> CadDocument {
    let layers: Vec<Layer> = drawing
        .layers()
        .map(|l| Layer {
            name: l.name.clone(),
            visible: l.is_layer_on,
        })
        .collect();

    let mut blocks: Vec<Block> = Vec::new();
    let mut model_entities: Vec<Entity> = Vec::new();

    for entity in drawing.entities() {
        if let Some(cad) = dxf_entity_to_cad(entity) {
            model_entities.push(cad);
        }
    }

    for block in drawing.blocks() {
        if block.name.starts_with('*') {
            // paper space / model space pseudo-blocks — skip as block definitions
            if block.name == "*Model_Space" || block.name == "*MODEL_SPACE" {
                for entity in &block.entities {
                    if let Some(cad) = dxf_entity_to_cad(entity) {
                        model_entities.push(cad);
                    }
                }
            }
            continue;
        }
        let block_entities: Vec<Entity> = block
            .entities
            .iter()
            .filter_map(dxf_entity_to_cad)
            .collect();
        blocks.push(Block {
            name: block.name.clone(),
            base_point: dxf_point(&block.base_point),
            entities: block_entities,
        });
    }

    CadDocument {
        format: CadFormat::Dxf,
        version: CadVersion::Unknown,
        units: None,
        extents: None,
        layers,
        blocks,
        entities: model_entities,
    }
}

fn dxf_entity_to_cad(entity: &dxf::entities::Entity) -> Option<Entity> {
    let common = EntityCommon {
        handle: None,
        layer: Some(entity.common.layer.clone()),
    };
    match &entity.specific {
        dxf::entities::EntityType::Line(l) => Some(Entity::Line(Line {
            common,
            start: dxf_point(&l.p1),
            end: dxf_point(&l.p2),
        })),
        dxf::entities::EntityType::Arc(a) => Some(Entity::Arc(Arc {
            common,
            center: dxf_point(&a.center),
            radius: a.radius,
            start_angle_degrees: a.start_angle,
            end_angle_degrees: a.end_angle,
        })),
        dxf::entities::EntityType::Circle(c) => Some(Entity::Circle(Circle {
            common,
            center: dxf_point(&c.center),
            radius: c.radius,
        })),
        dxf::entities::EntityType::LwPolyline(p) => {
            let points = p
                .vertices
                .iter()
                .map(|v| Point3 {
                    x: v.x,
                    y: v.y,
                    z: 0.0,
                })
                .collect();
            Some(Entity::Polyline(Polyline {
                common,
                points,
                closed: p.is_closed(),
            }))
        }
        dxf::entities::EntityType::Polyline(p) => {
            let points = p.vertices().map(|v| dxf_point(&v.location)).collect();
            Some(Entity::Polyline(Polyline {
                common,
                points,
                closed: p.is_closed(),
            }))
        }
        dxf::entities::EntityType::Face3D(f) => Some(Entity::Face3D(Face3D {
            common,
            corners: [
                dxf_point(&f.first_corner),
                dxf_point(&f.second_corner),
                dxf_point(&f.third_corner),
                dxf_point(&f.fourth_corner),
            ],
        })),
        dxf::entities::EntityType::Insert(i) => Some(Entity::Insert(Insert {
            common,
            block_name: i.name.clone(),
            insertion_point: dxf_point(&i.location),
            scale: Point3 {
                x: i.x_scale_factor,
                y: i.y_scale_factor,
                z: i.z_scale_factor,
            },
            rotation_degrees: i.rotation,
            column_count: i.column_count as u16,
            row_count: i.row_count as u16,
            column_spacing: i.column_spacing,
            row_spacing: i.row_spacing,
        })),
        _ => None,
    }
}

fn dxf_point(p: &dxf::Point) -> Point3 {
    Point3 {
        x: p.x,
        y: p.y,
        z: p.z,
    }
}
