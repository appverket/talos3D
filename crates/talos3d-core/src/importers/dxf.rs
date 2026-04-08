use std::{
    env,
    f32::consts::TAU,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::math::{DVec2, DVec3, Vec3, Vec3Swizzles};
use cadio_dwg::{probe_file, read_document as read_dwg_document, DwgReadError};
use cadio_ir::{
    CadDocument, Entity as CadEntity, Face3D as CadFace3D, Insert as CadInsert, Point3 as CadPoint3,
};
use dxf::{
    entities::{Entity, EntityType, Insert},
    Block, Drawing, Point,
};
use serde_json::Value;

use crate::plugins::import::FormatImporter;

pub(crate) const DWG_CONVERSION_MESSAGE: &str =
    "DWG import requires conversion to DXF. Install ODA File Converter, install LibreDWG, or set TALOS3D_DWG_CONVERTER to a converter command, then retry.";
const DXF_EXTENSIONS: &[&str] = &["dxf", "dwg"];
const ARC_SEGMENTS_PER_REVOLUTION: usize = 64;
const MAX_ARC_SEGMENTS: usize = 4_096;
const CIRCLE_SEGMENTS: usize = 64;
const INSERT_EXPANSION_DEPTH_LIMIT: usize = 10;
const ODA_OUTPUT_VERSION: &str = "ACAD2018";
const ODA_RECURSE_FLAG: &str = "0";
const ODA_AUDIT_FLAG: &str = "1";
const ALLOW_DWG_CONVERTER_ENV: &str = "TALOS3D_ALLOW_DWG_CONVERTER";
const DWG_CONVERTER_ENV: &str = "TALOS3D_DWG_CONVERTER";
const ODA_MACOS_APP_PATH: &str =
    "/Applications/ODA File Converter.app/Contents/MacOS/ODAFileConverter";
const LIBREDWG_DWG2DXF: &str = "dwg2dxf";
const LIBREDWG_DWGREAD: &str = "dwgread";
const MAX_NATIVE_COORDINATE_ABS: f64 = 1.0e9;
const MIN_NATIVE_GEOMETRY_ABS: f32 = 1.0e-6;
const BUNDLED_CONVERTER_DIRS: &[&str] = &[
    "tools/bin",
    "tools/dwg",
    "third_party/bin",
    "third_party/dwg",
    "resources/bin",
];

pub struct DxfImporter;

impl FormatImporter for DxfImporter {
    fn format_name(&self) -> &'static str {
        "AutoCAD DXF/DWG"
    }

    fn extensions(&self) -> &'static [&'static str] {
        DXF_EXTENSIONS
    }

    fn import(&self, path: &Path) -> Result<Vec<Value>, String> {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref()
        {
            Some("dwg") => import_dwg(path),
            _ => {
                let drawing = Drawing::load_file(path)
                    .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
                parse_dxf_requests(&drawing)
            }
        }
    }
}

fn import_dwg(path: &Path) -> Result<Vec<Value>, String> {
    if dwg_converter_is_allowed() {
        let converted_result = converted_dwg_import(path);
        return match converted_result {
            Ok(requests) => Ok(requests),
            Err(conversion_error) => match probe_file(path) {
                Ok(probe) => Err(format!(
                    "{conversion_error} Native DWG probing recognized {} ({:?}), but native semantic decoding still failed.",
                    probe.sentinel, probe.version
                )),
                Err(DwgReadError::Io(error)) => Err(format!(
                    "{conversion_error} Native DWG probing also failed with an I/O error: {error}"
                )),
                Err(native_error) => Err(format!(
                    "{conversion_error} Native DWG probing also failed: {native_error}"
                )),
            },
        };
    }

    native_dwg_import(path)
}

fn native_dwg_import(path: &Path) -> Result<Vec<Value>, String> {
    match (probe_file(path), read_dwg_document(path)) {
        (Ok(_probe), Ok(document)) => parse_cad_document_requests(&document).map_err(|error| {
            format!(
                "{error} Native DWG decode recovered {} layers, {} blocks, and {} entities.",
                document.layers.len(),
                document.blocks.len(),
                document.entities.len(),
            )
        }),
        (Ok(probe), Err(error)) => Err(format!(
            "Native DWG parsing recognized {} ({:?}) but failed while decoding semantic entities: {error}",
            probe.sentinel, probe.version
        )),
        (Err(DwgReadError::Io(error)), _) => {
            Err(format!("Native DWG probing failed with an I/O error: {error}"))
        }
        (Err(error), _) => Err(format!("Native DWG probing failed: {error}")),
    }
}

fn converted_dwg_import(path: &Path) -> Result<Vec<Value>, String> {
    let temp_dir = create_temp_conversion_dir()?;
    let converted_path = convert_dwg_to_dxf(path, &temp_dir)?;
    let drawing = Drawing::load_file(&converted_path).map_err(|error| {
        format!(
            "Failed to read converted DXF {}: {error}",
            converted_path.display()
        )
    })?;
    parse_dxf_requests(&drawing)
}

fn dwg_converter_is_allowed() -> bool {
    env::var(ALLOW_DWG_CONVERTER_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn create_temp_conversion_dir() -> Result<PathBuf, String> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("Failed to create DWG conversion timestamp: {error}"))?
        .as_nanos();
    let directory = env::temp_dir().join(format!("talos3d-dwg-{unique}"));
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Failed to create temporary DWG conversion directory: {error}"))?;
    Ok(directory)
}

fn convert_dwg_to_dxf(path: &Path, temp_dir: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("DWG path '{}' does not have a file name", path.display()))?;
    let input_dir = temp_dir.join("input");
    let output_dir = temp_dir.join("output");
    fs::create_dir_all(&input_dir)
        .map_err(|error| format!("Failed to create DWG input directory: {error}"))?;
    fs::create_dir_all(&output_dir)
        .map_err(|error| format!("Failed to create DWG output directory: {error}"))?;
    let staged_input = input_dir.join(file_name);
    fs::copy(path, &staged_input).map_err(|error| {
        format!(
            "Failed to stage DWG file '{}' for conversion: {error}",
            path.display()
        )
    })?;

    if let Some(wrapper_command) = converter_wrapper_command() {
        run_converter_wrapper(&wrapper_command, &staged_input, &output_dir)?;
    } else if let Some(oda_converter) = find_oda_converter() {
        run_oda_converter(&oda_converter, &input_dir, &output_dir, file_name)?;
    } else if let Some(dwg2dxf) = find_libredwg_dwg2dxf() {
        run_libredwg_dwg2dxf(&dwg2dxf, &staged_input, &output_dir)?;
    } else if let Some(dwgread) = find_libredwg_dwgread() {
        run_libredwg_dwgread(&dwgread, &staged_input, &output_dir)?;
    } else {
        return Err(format!(
            "{DWG_CONVERSION_MESSAGE} Looked for {DWG_CONVERTER_ENV}, bundled converter binaries near the app/project, ODA File Converter, '{LIBREDWG_DWG2DXF}', and '{LIBREDWG_DWGREAD}'."
        ));
    }

    find_converted_dxf(&output_dir, file_name)
}

fn converter_wrapper_command() -> Option<String> {
    env::var(DWG_CONVERTER_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_converter_wrapper(
    wrapper_command: &str,
    input_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    let output_path = output_dir
        .join(
            input_path
                .file_stem()
                .ok_or_else(|| "DWG input path is missing a file stem".to_string())?,
        )
        .with_extension("dxf");
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(wrapper_command)
        .arg("talos3d-dwg-wrapper");
    command.arg(input_path);
    command.arg(&output_path);
    let output = command
        .output()
        .map_err(|error| format!("Failed to launch DWG converter wrapper: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "DWG converter wrapper failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn find_oda_converter() -> Option<PathBuf> {
    if let Some(bundled) = find_bundled_converter("ODAFileConverter") {
        return Some(bundled);
    }
    if Path::new(ODA_MACOS_APP_PATH).is_file() {
        return Some(PathBuf::from(ODA_MACOS_APP_PATH));
    }
    find_executable_on_path("ODAFileConverter")
}

fn find_libredwg_dwg2dxf() -> Option<PathBuf> {
    find_bundled_converter(LIBREDWG_DWG2DXF).or_else(|| find_executable_on_path(LIBREDWG_DWG2DXF))
}

fn find_libredwg_dwgread() -> Option<PathBuf> {
    find_bundled_converter(LIBREDWG_DWGREAD).or_else(|| find_executable_on_path(LIBREDWG_DWGREAD))
}

fn find_bundled_converter(name: &str) -> Option<PathBuf> {
    bundled_search_roots()
        .into_iter()
        .flat_map(|root| bundled_candidates_for_root(&root, name))
        .find(|candidate| candidate.is_file())
}

fn bundled_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    roots.push(manifest_dir.clone());
    if let Some(parent) = manifest_dir.parent() {
        roots.push(parent.to_path_buf());
        if let Some(grandparent) = parent.parent() {
            roots.push(grandparent.to_path_buf());
        }
    }
    if let Ok(current_dir) = env::current_dir() {
        roots.push(current_dir);
    }
    if let Ok(current_exe) = env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            roots.push(exe_dir.to_path_buf());
            if let Some(parent) = exe_dir.parent() {
                roots.push(parent.to_path_buf());
                roots.push(parent.join("Resources"));
            }
        }
    }
    dedupe_paths(roots)
}

fn bundled_candidates_for_root(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        root.join(name),
        root.join(format!("{name}.app/Contents/MacOS/{name}")),
    ];
    for directory in BUNDLED_CONVERTER_DIRS {
        let directory = root.join(directory);
        candidates.push(directory.join(name));
        candidates.push(directory.join(format!("{name}.app/Contents/MacOS/{name}")));
    }
    candidates
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.iter().any(|existing| existing == &path) {
            unique.push(path);
        }
    }
    unique
}

fn find_executable_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for directory in env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn run_oda_converter(
    converter_path: &Path,
    input_dir: &Path,
    output_dir: &Path,
    _file_name: &std::ffi::OsStr,
) -> Result<(), String> {
    let output = Command::new(converter_path)
        .arg(input_dir)
        .arg(output_dir)
        .arg(ODA_OUTPUT_VERSION)
        .arg("DXF")
        .arg(ODA_RECURSE_FLAG)
        .arg(ODA_AUDIT_FLAG)
        .output()
        .map_err(|error| format!("Failed to launch ODA File Converter: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(format!("ODA File Converter failed: {detail}"))
    }
}

fn find_converted_dxf(
    output_dir: &Path,
    source_file_name: &std::ffi::OsStr,
) -> Result<PathBuf, String> {
    let expected_stem = Path::new(source_file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "Failed to derive converted DXF file stem".to_string())?;

    let mut matches = fs::read_dir(output_dir)
        .map_err(|error| format!("Failed to inspect DWG output directory: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dxf"))
        })
        .collect::<Vec<_>>();

    matches.sort();

    if let Some(exact) = matches.iter().find(|path| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem == expected_stem)
    }) {
        return Ok(exact.clone());
    }

    if let Some(single) = matches.into_iter().next() {
        return Ok(single);
    }

    Err(format!(
        "DWG conversion completed without producing a DXF in '{}'",
        output_dir.display()
    ))
}

fn run_libredwg_dwg2dxf(
    converter_path: &Path,
    input_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    let output = Command::new(converter_path)
        .arg("-y")
        .arg(input_path)
        .current_dir(output_dir)
        .output()
        .map_err(|error| format!("Failed to launch LibreDWG dwg2dxf: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(format!("LibreDWG dwg2dxf failed: {detail}"))
    }
}

fn run_libredwg_dwgread(
    converter_path: &Path,
    input_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    let output_path = output_dir
        .join(
            input_path
                .file_stem()
                .ok_or_else(|| "DWG input path is missing a file stem".to_string())?,
        )
        .with_extension("dxf");
    let output = Command::new(converter_path)
        .arg("-O")
        .arg("DXF")
        .arg("-o")
        .arg(&output_path)
        .arg(input_path)
        .output()
        .map_err(|error| format!("Failed to launch LibreDWG dwgread: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(format!("LibreDWG dwgread failed: {detail}"))
    }
}

pub fn parse_dxf_requests(drawing: &Drawing) -> Result<Vec<Value>, String> {
    let mut requests = Vec::new();
    let transforms = Vec::new();
    let mut diagnostics = ImportDiagnostics::default();
    for entity in drawing.entities() {
        requests.extend(entity_requests(
            drawing,
            entity,
            &transforms,
            INSERT_EXPANSION_DEPTH_LIMIT,
            &mut diagnostics,
        )?);
    }

    if requests.is_empty() {
        return Err(diagnostics.empty_import_error());
    }

    Ok(filter_outlier_requests(requests))
}

#[derive(Clone, Copy)]
struct InsertTransform {
    base_point: DVec3,
    location: DVec3,
    rotation_radians: f64,
    scale: DVec3,
}

#[derive(Default)]
struct ImportDiagnostics {
    unsupported_types: Vec<String>,
    missing_blocks: Vec<String>,
}

impl ImportDiagnostics {
    fn record_unsupported(&mut self, entity_type: &str) {
        if !self
            .unsupported_types
            .iter()
            .any(|existing| existing == entity_type)
        {
            self.unsupported_types.push(entity_type.to_string());
        }
    }

    fn record_missing_block(&mut self, block_name: &str) {
        if !self
            .missing_blocks
            .iter()
            .any(|existing| existing == block_name)
        {
            self.missing_blocks.push(block_name.to_string());
        }
    }

    fn empty_import_error(&self) -> String {
        let mut message = "DXF file did not contain any supported import entities".to_string();
        if !self.unsupported_types.is_empty() {
            message.push_str(". Found unsupported entity types: ");
            message.push_str(&self.unsupported_types.join(", "));
        }
        if !self.missing_blocks.is_empty() {
            message.push_str(". Missing block definitions: ");
            message.push_str(&self.missing_blocks.join(", "));
        }
        message
    }
}

fn entity_requests(
    drawing: &Drawing,
    entity: &Entity,
    transforms: &[InsertTransform],
    remaining_depth: usize,
    diagnostics: &mut ImportDiagnostics,
) -> Result<Vec<Value>, String> {
    if entity.common.is_in_paper_space {
        return Ok(Vec::new());
    }

    match &entity.specific {
        EntityType::Line(line) => Ok(vec![polyline_request(
            vec![
                map_point(line.p1.clone(), transforms),
                map_point(line.p2.clone(), transforms),
            ],
            Some(entity.common.layer.as_str()),
        )]),
        EntityType::Polyline(polyline) => {
            let vertices = polyline.vertices().cloned().collect::<Vec<_>>();
            let points = tessellate_polyline_vertices(
                &vertices,
                polyline.is_closed(),
                transforms,
                |vertex| vertex.location,
                |vertex| vertex.bulge,
            );
            polyline_request_if_valid(points, Some(entity.common.layer.as_str()))
        }
        EntityType::LwPolyline(polyline) => {
            let points = tessellate_polyline_vertices(
                &polyline.vertices,
                polyline.is_closed(),
                transforms,
                |vertex| Point::new(vertex.x, vertex.y, entity.common.elevation),
                |vertex| vertex.bulge,
            );
            polyline_request_if_valid(points, Some(entity.common.layer.as_str()))
        }
        EntityType::Arc(arc) => {
            let start_radians = arc.start_angle.to_radians();
            let end_radians = arc.end_angle.to_radians();
            let sweep = normalized_arc_sweep(start_radians, end_radians);
            let segment_count = ((sweep / TAU as f64) * ARC_SEGMENTS_PER_REVOLUTION as f64)
                .ceil()
                .max(2.0)
                .min(MAX_ARC_SEGMENTS as f64) as usize;
            let points = (0..=segment_count)
                .map(|index| {
                    let t = index as f64 / segment_count as f64;
                    let angle = start_radians + sweep * t;
                    let point = Point::new(
                        arc.center.x + arc.radius * angle.cos(),
                        arc.center.y + arc.radius * angle.sin(),
                        arc.center.z,
                    );
                    map_point(point, transforms)
                })
                .collect::<Vec<_>>();
            Ok(vec![polyline_request(
                points,
                Some(entity.common.layer.as_str()),
            )])
        }
        EntityType::Circle(circle) => {
            let points = (0..=CIRCLE_SEGMENTS)
                .map(|index| {
                    let angle = index as f64 / CIRCLE_SEGMENTS as f64 * std::f64::consts::TAU;
                    let point = Point::new(
                        circle.center.x + circle.radius * angle.cos(),
                        circle.center.y + circle.radius * angle.sin(),
                        circle.center.z,
                    );
                    map_point(point, transforms)
                })
                .collect::<Vec<_>>();
            Ok(vec![polyline_request(
                points,
                Some(entity.common.layer.as_str()),
            )])
        }
        EntityType::Face3D(face) => Ok(vec![triangle_mesh_request(
            vec![
                map_point(face.first_corner.clone(), transforms),
                map_point(face.second_corner.clone(), transforms),
                map_point(face.third_corner.clone(), transforms),
                map_point(face.fourth_corner.clone(), transforms),
            ],
            face_fourth_corner_is_distinct(face, transforms),
            Some(entity.common.layer.as_str()),
        )]),
        EntityType::Solid(solid) => Ok(vec![triangle_mesh_request(
            vec![
                map_point(solid.first_corner.clone(), transforms),
                map_point(solid.second_corner.clone(), transforms),
                map_point(solid.third_corner.clone(), transforms),
                map_point(solid.fourth_corner.clone(), transforms),
            ],
            true,
            Some(entity.common.layer.as_str()),
        )]),
        EntityType::Trace(trace) => Ok(vec![triangle_mesh_request(
            vec![
                map_point(trace.first_corner.clone(), transforms),
                map_point(trace.second_corner.clone(), transforms),
                map_point(trace.third_corner.clone(), transforms),
                map_point(trace.fourth_corner.clone(), transforms),
            ],
            true,
            Some(entity.common.layer.as_str()),
        )]),
        EntityType::Insert(insert) => {
            expand_insert(drawing, insert, transforms, remaining_depth, diagnostics)
        }
        _ => {
            diagnostics.record_unsupported(entity_type_name(entity));
            Ok(Vec::new())
        }
    }
}

fn expand_insert(
    drawing: &Drawing,
    insert: &Insert,
    transforms: &[InsertTransform],
    remaining_depth: usize,
    diagnostics: &mut ImportDiagnostics,
) -> Result<Vec<Value>, String> {
    if remaining_depth == 0 {
        return Err(format!(
            "DXF block expansion exceeded the depth limit while expanding '{}'",
            insert.name
        ));
    }

    let Some(block) = drawing
        .blocks()
        .find(|block| block.name.eq_ignore_ascii_case(&insert.name))
    else {
        diagnostics.record_missing_block(&insert.name);
        return Ok(Vec::new());
    };

    let mut requests = Vec::new();
    for insert_transform in insert_transforms(block, insert) {
        let mut next_transforms = transforms.to_vec();
        next_transforms.push(insert_transform);
        for entity in &block.entities {
            requests.extend(entity_requests(
                drawing,
                entity,
                &next_transforms,
                remaining_depth - 1,
                diagnostics,
            )?);
        }
    }
    Ok(requests)
}

fn entity_type_name(entity: &Entity) -> &'static str {
    match entity.specific {
        EntityType::Line(_) => "LINE",
        EntityType::Polyline(_) => "POLYLINE",
        EntityType::LwPolyline(_) => "LWPOLYLINE",
        EntityType::Arc(_) => "ARC",
        EntityType::Circle(_) => "CIRCLE",
        EntityType::Face3D(_) => "3DFACE",
        EntityType::Insert(_) => "INSERT",
        EntityType::ModelPoint(_) => "POINT",
        EntityType::Solid(_) => "SOLID",
        EntityType::Trace(_) => "TRACE",
        _ => "UNKNOWN",
    }
}

fn insert_transform(block: &Block, insert: &Insert) -> InsertTransform {
    InsertTransform {
        base_point: dxf_point_to_vec3(block.base_point.clone()),
        location: dxf_point_to_vec3(insert.location.clone()),
        rotation_radians: insert.rotation.to_radians(),
        scale: DVec3::new(
            insert.x_scale_factor,
            insert.y_scale_factor,
            insert.z_scale_factor,
        ),
    }
}

fn insert_transforms(block: &Block, insert: &Insert) -> Vec<InsertTransform> {
    let base = insert_transform(block, insert);
    let column_count = u16::try_from(insert.column_count.max(1)).unwrap_or(1);
    let row_count = u16::try_from(insert.row_count.max(1)).unwrap_or(1);
    let mut transforms = Vec::with_capacity(usize::from(column_count) * usize::from(row_count));
    for row in 0..row_count {
        for column in 0..column_count {
            let mut transform = base;
            let cell_offset = DVec3::new(
                f64::from(column) * insert.column_spacing,
                f64::from(row) * insert.row_spacing,
                0.0,
            );
            transform.location +=
                rotate_insert_offset(cell_offset * transform.scale, transform.rotation_radians);
            transforms.push(transform);
        }
    }
    transforms
}

fn map_point(point: Point, transforms: &[InsertTransform]) -> [f32; 3] {
    let mut point = dxf_point_to_vec3(point);
    for transform in transforms {
        point = apply_insert_transform(point, transform);
    }
    let mapped = Vec3::new(point.x as f32, point.z as f32, point.y as f32);
    [mapped.x, mapped.y, mapped.z]
}

fn dxf_point_to_vec3(point: Point) -> DVec3 {
    DVec3::new(point.x, point.y, point.z)
}

fn apply_insert_transform(point: DVec3, transform: &InsertTransform) -> DVec3 {
    let mut local = point - transform.base_point;
    local *= transform.scale;
    let rotated = rotate_insert_offset(local, transform.rotation_radians);
    rotated + transform.location
}

fn rotate_insert_offset(local: DVec3, rotation_radians: f64) -> DVec3 {
    let (sin, cos) = rotation_radians.sin_cos();
    DVec3::new(
        local.x * cos - local.y * sin,
        local.x * sin + local.y * cos,
        local.z,
    )
}

fn normalized_arc_sweep(start_radians: f64, end_radians: f64) -> f64 {
    let sweep = (end_radians - start_radians).rem_euclid(std::f64::consts::TAU);
    if sweep <= f64::EPSILON {
        std::f64::consts::TAU
    } else {
        sweep
    }
}

fn tessellate_polyline_vertices<T: Clone>(
    vertices: &[T],
    is_closed: bool,
    transforms: &[InsertTransform],
    point_for: impl Fn(T) -> Point,
    bulge_for: impl Fn(T) -> f64,
) -> Vec<[f32; 3]> {
    if vertices.len() < 2 {
        return Vec::new();
    }

    let segment_count = if is_closed {
        vertices.len()
    } else {
        vertices.len() - 1
    };
    let mut points = Vec::new();
    for index in 0..segment_count {
        let start = vertices[index].clone();
        let end = vertices[(index + 1) % vertices.len()].clone();
        let segment_points = tessellate_bulge_segment(
            point_for(start.clone()),
            point_for(end),
            bulge_for(start),
            transforms,
        );
        if index == 0 {
            points.extend(segment_points);
        } else {
            points.extend(segment_points.into_iter().skip(1));
        }
    }
    points
}

fn tessellate_bulge_segment(
    start: Point,
    end: Point,
    bulge: f64,
    transforms: &[InsertTransform],
) -> Vec<[f32; 3]> {
    const BULGE_EPSILON: f64 = 1.0e-9;

    let start_vec = dxf_point_to_vec3(start.clone());
    let end_vec = dxf_point_to_vec3(end.clone());
    let chord = end_vec - start_vec;
    let chord_length = chord.xy().length();
    if bulge.abs() <= BULGE_EPSILON || chord_length <= BULGE_EPSILON {
        return vec![map_point(start, transforms), map_point(end, transforms)];
    }

    let included_angle = 4.0 * bulge.atan();
    let segment_count = ((included_angle.abs() / std::f64::consts::TAU)
        * ARC_SEGMENTS_PER_REVOLUTION as f64)
        .ceil()
        .max(2.0) as usize;
    let center = bulge_arc_center(start_vec, end_vec, bulge, chord_length);
    let radius_vector = start_vec.xy() - center;
    let radius = radius_vector.length();
    if radius <= BULGE_EPSILON {
        return vec![map_point(start, transforms), map_point(end, transforms)];
    }

    let start_angle = radius_vector.y.atan2(radius_vector.x);
    let mut points = Vec::with_capacity(segment_count + 1);
    for index in 0..=segment_count {
        let t = index as f64 / segment_count as f64;
        let angle = start_angle + included_angle * t;
        let xy = center + DVec3::new(radius * angle.cos(), radius * angle.sin(), 0.0).xy();
        let point = Point::new(xy.x, xy.y, start.z + (end.z - start.z) * t);
        points.push(map_point(point, transforms));
    }
    points
}

fn bulge_arc_center(start: DVec3, end: DVec3, bulge: f64, chord_length: f64) -> DVec2 {
    let midpoint = (start.xy() + end.xy()) * 0.5;
    let direction = (end.xy() - start.xy()) / chord_length;
    let left_normal = DVec2::new(-direction.y, direction.x);
    let offset = chord_length * (1.0 - bulge * bulge) / (4.0 * bulge);
    midpoint + left_normal * offset
}

fn polyline_request(points: Vec<[f32; 3]>, layer: Option<&str>) -> Value {
    let layer = layer.filter(|layer| !layer.is_empty());
    let elevation_metadata = elevation_metadata_json(&points, layer);
    serde_json::json!({
        "type": "polyline",
        "points": points,
        "layer": layer,
        "elevation_metadata": elevation_metadata,
    })
}

fn polyline_request_if_valid(
    points: Vec<[f32; 3]>,
    layer: Option<&str>,
) -> Result<Vec<Value>, String> {
    if points.len() < 2 {
        return Ok(Vec::new());
    }
    Ok(vec![polyline_request(points, layer)])
}

fn triangle_mesh_request(
    vertices: Vec<[f32; 3]>,
    fourth_corner_is_distinct: bool,
    layer: Option<&str>,
) -> Value {
    let faces = if fourth_corner_is_distinct {
        vec![[0_u32, 1, 2], [0_u32, 2, 3]]
    } else {
        vec![[0_u32, 1, 2]]
    };
    serde_json::json!({
        "type": "triangle_mesh",
        "vertices": vertices,
        "faces": faces,
        "normals": serde_json::Value::Null,
        "name": serde_json::Value::Null,
        "layer": layer.filter(|layer| !layer.is_empty()),
    })
}

fn face_fourth_corner_is_distinct(
    face: &dxf::entities::Face3D,
    transforms: &[InsertTransform],
) -> bool {
    let third = map_point(face.third_corner.clone(), transforms);
    let fourth = map_point(face.fourth_corner.clone(), transforms);
    third != fourth
}

fn parse_cad_document_requests(document: &CadDocument) -> Result<Vec<Value>, String> {
    let model_entities = filter_model_space_entities(&document.entities);
    let mut requests = Vec::new();
    let mut diagnostics = ImportDiagnostics::default();
    let transforms = Vec::new();
    for entity in &model_entities {
        match cad_entity_requests(
            document,
            entity,
            &transforms,
            INSERT_EXPANSION_DEPTH_LIMIT,
            &mut diagnostics,
        ) {
            Ok(entity_requests) => requests.extend(entity_requests),
            Err(_) => diagnostics.record_unsupported("INVALID_GEOMETRY"),
        }
    }
    if requests.is_empty() {
        return Err(
            "Native DWG file did not contain any supported, geometrically valid import entities."
                .to_string(),
        );
    }
    Ok(filter_outlier_requests(requests))
}

/// Filter out entities that are likely paper-space or title-block geometry.
///
/// The native DWG decoder may include paper-space entities when the entity
/// mode bits are misaligned. These entities have coordinates near the page
/// origin (small values) while model-space survey entities have large
/// real-world coordinates. Detect and exclude outliers by comparing each
/// entity's representative point to the median of all entity points.
fn filter_model_space_entities(entities: &[CadEntity]) -> Vec<&CadEntity> {
    let centers: Vec<(usize, DVec2)> = entities
        .iter()
        .enumerate()
        .filter_map(|(i, entity)| {
            let point = entity_representative_point(entity)?;
            Some((i, DVec2::new(point.x, point.y)))
        })
        .collect();

    if centers.len() < 3 {
        return entities.iter().collect();
    }

    let mut xs: Vec<f64> = centers.iter().map(|(_, p)| p.x).collect();
    let mut ys: Vec<f64> = centers.iter().map(|(_, p)| p.y).collect();
    xs.sort_unstable_by(f64::total_cmp);
    ys.sort_unstable_by(f64::total_cmp);
    let mid = xs.len() / 2;
    let median = DVec2::new(xs[mid], ys[mid]);

    // Use MAD (median absolute deviation) for robust spread estimation.
    // Unlike IQR, MAD correctly handles bimodal distributions where
    // paper-space entities (near origin) form a separate cluster from
    // model-space entities (at real-world coordinates).
    let mut deviations: Vec<f64> = centers
        .iter()
        .map(|(_, center)| (*center - median).length())
        .collect();
    deviations.sort_unstable_by(f64::total_cmp);
    let mad = deviations[deviations.len() / 2].max(100.0);
    let threshold = (mad * 10.0).max(1_000.0);

    let mut included: Vec<bool> = vec![true; entities.len()];
    for &(i, center) in &centers {
        let distance = (center - median).length();
        if distance > threshold {
            included[i] = false;
        }
    }

    entities
        .iter()
        .enumerate()
        .filter(|(i, _)| included[*i])
        .map(|(_, entity)| entity)
        .collect()
}

/// Filter out import requests whose center is far from the bulk of the model.
///
/// Works on the final JSON requests so it applies to both DXF and native DWG
/// paths. Uses MAD-based outlier detection on the 2D center of each request.
fn filter_outlier_requests(requests: Vec<Value>) -> Vec<Value> {
    let centers: Vec<(usize, DVec2)> = requests
        .iter()
        .enumerate()
        .filter_map(|(i, request)| request_representative_point(request).map(|point| (i, point)))
        .collect();

    if centers.len() < 3 {
        return requests;
    }

    let mut xs: Vec<f64> = centers.iter().map(|(_, p)| p.x).collect();
    let mut ys: Vec<f64> = centers.iter().map(|(_, p)| p.y).collect();
    xs.sort_unstable_by(f64::total_cmp);
    ys.sort_unstable_by(f64::total_cmp);
    let mid = xs.len() / 2;
    let median = DVec2::new(xs[mid], ys[mid]);

    let mut deviations: Vec<f64> = centers
        .iter()
        .map(|(_, center)| (*center - median).length())
        .collect();
    deviations.sort_unstable_by(f64::total_cmp);
    let mad = deviations[deviations.len() / 2].max(100.0);
    let threshold = (mad * 10.0).max(1_000.0);

    let mut included: Vec<bool> = vec![true; requests.len()];
    for &(i, center) in &centers {
        if (center - median).length() > threshold {
            included[i] = false;
        }
    }

    requests
        .into_iter()
        .enumerate()
        .filter(|(i, _)| included[*i])
        .map(|(_, request)| request)
        .collect()
}

/// Extract a 2D representative point from a JSON import request.
fn request_representative_point(request: &Value) -> Option<DVec2> {
    let points = request.get("points").and_then(|p| p.as_array());
    if let Some(points) = points {
        if let Some(first) = points.first().and_then(|p| p.as_array()) {
            let x = first.first().and_then(|v| v.as_f64())?;
            let y = first.get(1).and_then(|v| v.as_f64())?;
            return Some(DVec2::new(x, y));
        }
    }
    let vertices = request.get("vertices").and_then(|v| v.as_array());
    if let Some(vertices) = vertices {
        if let Some(first) = vertices.first().and_then(|v| v.as_array()) {
            let x = first.first().and_then(|v| v.as_f64())?;
            let y = first.get(1).and_then(|v| v.as_f64())?;
            return Some(DVec2::new(x, y));
        }
    }
    None
}

fn entity_representative_point(entity: &CadEntity) -> Option<CadPoint3> {
    match entity {
        CadEntity::Line(line) => Some(line.start),
        CadEntity::Polyline(polyline) => polyline.points.first().copied(),
        CadEntity::Arc(arc) => Some(arc.center),
        CadEntity::Circle(circle) => Some(circle.center),
        CadEntity::Face3D(face) => face.corners.first().copied(),
        CadEntity::Insert(insert) => Some(insert.insertion_point),
        CadEntity::Unknown(_) => None,
    }
}

#[derive(Clone, Copy)]
struct CadInsertTransform {
    base_point: DVec3,
    location: DVec3,
    rotation_radians: f64,
    scale: DVec3,
}

fn cad_entity_requests(
    document: &CadDocument,
    entity: &CadEntity,
    transforms: &[CadInsertTransform],
    remaining_depth: usize,
    diagnostics: &mut ImportDiagnostics,
) -> Result<Vec<Value>, String> {
    match entity {
        CadEntity::Line(line) => {
            let points = vec![
                map_cad_point(line.start, transforms)?,
                map_cad_point(line.end, transforms)?,
            ];
            native_polyline_request(points, line.common.layer.as_deref())
        }
        CadEntity::Polyline(polyline) => {
            let mut points = Vec::with_capacity(polyline.points.len());
            for point in &polyline.points {
                points.push(map_cad_point(*point, transforms)?);
            }
            native_polyline_request(points, polyline.common.layer.as_deref())
        }
        CadEntity::Arc(arc) => {
            let start_radians = arc.start_angle_degrees.to_radians();
            let end_radians = arc.end_angle_degrees.to_radians();
            let sweep = normalized_arc_sweep(start_radians, end_radians);
            let segment_count = ((sweep / TAU as f64) * ARC_SEGMENTS_PER_REVOLUTION as f64)
                .ceil()
                .max(2.0) as usize;
            let mut points = Vec::with_capacity(segment_count + 1);
            for index in 0..=segment_count {
                let t = index as f64 / segment_count as f64;
                let angle = start_radians + sweep * t;
                let point = CadPoint3 {
                    x: arc.center.x + arc.radius * angle.cos(),
                    y: arc.center.y + arc.radius * angle.sin(),
                    z: arc.center.z,
                };
                points.push(map_cad_point(point, transforms)?);
            }
            native_polyline_request(points, arc.common.layer.as_deref())
        }
        CadEntity::Circle(circle) => {
            let mut points = Vec::with_capacity(CIRCLE_SEGMENTS + 1);
            for index in 0..=CIRCLE_SEGMENTS {
                let angle = index as f64 / CIRCLE_SEGMENTS as f64 * std::f64::consts::TAU;
                let point = CadPoint3 {
                    x: circle.center.x + circle.radius * angle.cos(),
                    y: circle.center.y + circle.radius * angle.sin(),
                    z: circle.center.z,
                };
                points.push(map_cad_point(point, transforms)?);
            }
            native_polyline_request(points, circle.common.layer.as_deref())
        }
        CadEntity::Face3D(face) => cad_face3d_request(face, transforms),
        CadEntity::Insert(insert) => {
            expand_cad_insert(document, insert, transforms, remaining_depth, diagnostics)
        }
        CadEntity::Unknown(_common) => {
            diagnostics.record_unsupported("UNKNOWN");
            Ok(Vec::new())
        }
    }
}

fn cad_face3d_request(
    face: &CadFace3D,
    transforms: &[CadInsertTransform],
) -> Result<Vec<Value>, String> {
    let vertices = face
        .corners
        .iter()
        .map(|point| map_cad_point(*point, transforms))
        .collect::<Result<Vec<_>, _>>()?;
    if vertices.len() != 4 {
        return Ok(Vec::new());
    }
    if !native_points_are_reasonable(&vertices) {
        return Err("Native DWG face produced implausible geometry.".to_string());
    }
    let third = vertices[2];
    let fourth = vertices[3];
    Ok(vec![triangle_mesh_request(
        vertices,
        third != fourth,
        face.common.layer.as_deref(),
    )])
}

fn expand_cad_insert(
    document: &CadDocument,
    insert: &CadInsert,
    transforms: &[CadInsertTransform],
    remaining_depth: usize,
    diagnostics: &mut ImportDiagnostics,
) -> Result<Vec<Value>, String> {
    if remaining_depth == 0 {
        return Err(format!(
            "DWG block expansion exceeded the depth limit while expanding '{}'",
            insert.block_name
        ));
    }

    let Some(block) = document
        .blocks
        .iter()
        .find(|block| block.name.eq_ignore_ascii_case(&insert.block_name))
    else {
        diagnostics.record_missing_block(&insert.block_name);
        return Ok(Vec::new());
    };

    let mut requests = Vec::new();
    for insert_transform in cad_insert_transforms(block, insert) {
        let mut next_transforms = transforms.to_vec();
        next_transforms.push(insert_transform);
        for entity in &block.entities {
            if block_entity_has_implausible_local_coords(entity, &block.base_point) {
                continue;
            }
            requests.extend(cad_entity_requests(
                document,
                entity,
                &next_transforms,
                remaining_depth - 1,
                diagnostics,
            )?);
        }
    }
    Ok(requests)
}

fn cad_insert_transform(block: &cadio_ir::Block, insert: &CadInsert) -> CadInsertTransform {
    CadInsertTransform {
        base_point: cad_point_to_vec3(block.base_point),
        location: cad_point_to_vec3(insert.insertion_point),
        rotation_radians: insert.rotation_degrees.to_radians(),
        scale: DVec3::new(insert.scale.x, insert.scale.z, insert.scale.y),
    }
}

fn cad_insert_transforms(block: &cadio_ir::Block, insert: &CadInsert) -> Vec<CadInsertTransform> {
    let base = cad_insert_transform(block, insert);
    let column_count = insert.column_count.max(1);
    let row_count = insert.row_count.max(1);
    let mut transforms = Vec::with_capacity(usize::from(column_count) * usize::from(row_count));
    for row in 0..row_count {
        for column in 0..column_count {
            let mut transform = base;
            let cell_offset = DVec3::new(
                f64::from(column) * insert.column_spacing,
                0.0,
                f64::from(row) * insert.row_spacing,
            );
            transform.location +=
                rotate_cad_insert_offset(cell_offset * transform.scale, transform.rotation_radians);
            transforms.push(transform);
        }
    }
    transforms
}

fn map_cad_point(point: CadPoint3, transforms: &[CadInsertTransform]) -> Result<[f32; 3], String> {
    let mut point = cad_point_to_vec3(point);
    for transform in transforms {
        point = apply_cad_insert_transform(point, transform);
    }
    validate_native_vec3(point)
}

fn cad_point_to_vec3(point: CadPoint3) -> DVec3 {
    // CAD uses Z-up (x=easting, y=northing, z=elevation).
    // Bevy uses Y-up, so swap Y↔Z.
    DVec3::new(point.x, point.z, point.y)
}

fn apply_cad_insert_transform(point: DVec3, transform: &CadInsertTransform) -> DVec3 {
    let mut local = point - transform.base_point;
    local *= transform.scale;
    let rotated = rotate_cad_insert_offset(local, transform.rotation_radians);
    rotated + transform.location
}

fn rotate_cad_insert_offset(local: DVec3, rotation_radians: f64) -> DVec3 {
    let (sin, cos) = rotation_radians.sin_cos();
    DVec3::new(
        local.x * cos - local.z * sin,
        local.y,
        local.x * sin + local.z * cos,
    )
}

/// Block entities should have local coordinates relative to the block's base
/// point. If the coordinates are very large (global/world-space), the entity
/// was likely decoded with a misaligned bit stream and would produce
/// implausible geometry when expanded via INSERT.
const MAX_BLOCK_LOCAL_COORDINATE_ABS: f64 = 10_000.0;

fn block_entity_has_implausible_local_coords(entity: &CadEntity, base_point: &CadPoint3) -> bool {
    let points: &[CadPoint3] = match entity {
        CadEntity::Line(line) => {
            return [line.start, line.end]
                .iter()
                .any(|p| block_point_too_far(p, base_point))
        }
        CadEntity::Polyline(polyline) => &polyline.points,
        CadEntity::Arc(arc) => return block_point_too_far(&arc.center, base_point),
        CadEntity::Circle(circle) => return block_point_too_far(&circle.center, base_point),
        CadEntity::Face3D(face) => &face.corners,
        CadEntity::Insert(_) | CadEntity::Unknown(_) => return false,
    };
    points.iter().any(|p| block_point_too_far(p, base_point))
}

fn block_point_too_far(point: &CadPoint3, base: &CadPoint3) -> bool {
    (point.x - base.x).abs() > MAX_BLOCK_LOCAL_COORDINATE_ABS
        || (point.y - base.y).abs() > MAX_BLOCK_LOCAL_COORDINATE_ABS
}

fn validate_native_vec3(point: DVec3) -> Result<[f32; 3], String> {
    if !point.x.is_finite() || !point.y.is_finite() || !point.z.is_finite() {
        return Err("Native DWG entity produced non-finite coordinates.".to_string());
    }
    if point.x.abs() > MAX_NATIVE_COORDINATE_ABS
        || point.y.abs() > MAX_NATIVE_COORDINATE_ABS
        || point.z.abs() > MAX_NATIVE_COORDINATE_ABS
    {
        return Err("Native DWG entity produced implausible coordinates.".to_string());
    }
    Ok([point.x as f32, point.y as f32, point.z as f32])
}

fn native_polyline_request(
    points: Vec<[f32; 3]>,
    layer: Option<&str>,
) -> Result<Vec<Value>, String> {
    if !native_points_are_reasonable(&points) {
        return Err("Native DWG polyline produced implausible geometry.".to_string());
    }
    polyline_request_if_valid(points, layer)
}

fn native_points_are_reasonable(points: &[[f32; 3]]) -> bool {
    if points.len() < 2 {
        return false;
    }
    let mut has_distinct_points = false;
    let mut max_abs = 0.0f32;
    let first = points[0];
    for point in points {
        for component in point {
            if !component.is_finite() {
                return false;
            }
            max_abs = max_abs.max(component.abs());
        }
        if !has_distinct_points
            && ((point[0] - first[0]).abs() > MIN_NATIVE_GEOMETRY_ABS
                || (point[1] - first[1]).abs() > MIN_NATIVE_GEOMETRY_ABS
                || (point[2] - first[2]).abs() > MIN_NATIVE_GEOMETRY_ABS)
        {
            has_distinct_points = true;
        }
    }
    has_distinct_points && max_abs > MIN_NATIVE_GEOMETRY_ABS
}

fn elevation_metadata_json(points: &[[f32; 3]], layer: Option<&str>) -> serde_json::Value {
    let Some(elevation) = infer_elevation(points, layer) else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "source_layer": layer.unwrap_or_default(),
        "elevation": elevation,
        "survey_source_id": serde_json::Value::Null,
    })
}

fn infer_elevation(points: &[[f32; 3]], layer: Option<&str>) -> Option<f32> {
    let first = points.first()?;
    let consistent_non_zero = points
        .iter()
        .all(|point| (point[1] - first[1]).abs() <= f32::EPSILON)
        && first[1].abs() > f32::EPSILON;
    if consistent_non_zero {
        return Some(first[1]);
    }
    layer.and_then(parse_elevation_from_layer_name)
}

fn parse_elevation_from_layer_name(layer: &str) -> Option<f32> {
    let mut best = None;
    let mut current = String::new();
    for character in layer.chars() {
        if character.is_ascii_digit() || character == '.' || character == '-' {
            current.push(character);
        } else if !current.is_empty() {
            best = current.parse::<f32>().ok().or(best);
            current.clear();
        }
    }
    if !current.is_empty() {
        best = current.parse::<f32>().ok().or(best);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadio_ir::{
        Block as CadBlock, CadDocument, CadFormat, CadVersion, Entity as CadEntity,
        EntityCommon as CadEntityCommon, Insert as CadInsert, Line as CadLine, Point3 as CadPoint3,
        Polyline as CadPolyline,
    };

    use dxf::entities::{Arc, Circle, Face3D, Line, LwPolyline, ModelPoint, Polyline, Vertex};
    use dxf::LwPolylineVertex;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn importer_lists_dxf_and_dwg_extensions() {
        let importer = DxfImporter;
        assert_eq!(importer.extensions(), &["dxf", "dwg"]);
    }

    #[test]
    fn importer_uses_native_path_by_default_for_dwg() {
        let importer = DxfImporter;
        let prior_allow = env::var(ALLOW_DWG_CONVERTER_ENV).ok();
        let prior_wrapper = env::var(DWG_CONVERTER_ENV).ok();
        unsafe {
            env::remove_var(ALLOW_DWG_CONVERTER_ENV);
            env::remove_var(DWG_CONVERTER_ENV);
        }
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = env::temp_dir().join(format!("talos3d-test-{unique}.dwg"));
        fs::write(&path, b"not a real dwg").expect("temp dwg should be created");
        let error = importer
            .import(&path)
            .expect_err("native dwg import should fail on invalid test data");
        let _ = fs::remove_file(&path);
        match prior_allow {
            Some(value) => unsafe { env::set_var(ALLOW_DWG_CONVERTER_ENV, value) },
            None => unsafe { env::remove_var(ALLOW_DWG_CONVERTER_ENV) },
        }
        match prior_wrapper {
            Some(value) => unsafe { env::set_var(DWG_CONVERTER_ENV, value) },
            None => unsafe { env::remove_var(DWG_CONVERTER_ENV) },
        }
        assert!(error.contains("Native DWG probing failed"));
    }

    #[test]
    fn find_converted_dxf_accepts_normalized_unicode_name_mismatch() {
        let base = std::env::temp_dir().join(format!(
            "talos3d-dxf-output-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&base).expect("output directory should be created");
        let converted = base.join("Västra Lagnö 1_156 revA.dxf");
        fs::write(&converted, "0\nSECTION\n2\nEOF\n").expect("converted dxf should be written");

        let resolved = find_converted_dxf(&base, "Västra Lagnö 1_156 revA.dwg".as_ref())
            .expect("single converted file should resolve");
        assert_eq!(resolved, converted);
    }

    #[test]
    #[ignore = "requires local DWG sample and bundled ODA converter"]
    fn importer_converts_local_sample_dwg_when_bundled_converter_is_available() {
        let Some(sample_path) = local_sample_dwg() else {
            eprintln!("skipping: no local sample DWG found");
            return;
        };
        let Some(converter_path) = find_oda_converter() else {
            eprintln!("skipping: no bundled/system ODA converter found");
            return;
        };
        assert!(converter_path.is_file(), "converter path should exist");
        let prior_allow = env::var(ALLOW_DWG_CONVERTER_ENV).ok();
        unsafe {
            env::set_var(ALLOW_DWG_CONVERTER_ENV, "1");
        }

        let importer = DxfImporter;
        let requests = importer
            .import(&sample_path)
            .expect("sample DWG should convert and import");
        let polyline_count = requests
            .iter()
            .filter(|request| request["type"] == "polyline")
            .count();
        let mesh_count = requests
            .iter()
            .filter(|request| request["type"] == "triangle_mesh")
            .count();
        println!(
            "sample converted import requests: total={} polylines={} meshes={}",
            requests.len(),
            polyline_count,
            mesh_count,
        );
        match prior_allow {
            Some(value) => unsafe { env::set_var(ALLOW_DWG_CONVERTER_ENV, value) },
            None => unsafe { env::remove_var(ALLOW_DWG_CONVERTER_ENV) },
        }
        assert!(
            !requests.is_empty(),
            "sample DWG should produce at least one import request"
        );
    }

    #[test]
    #[ignore = "requires local DWG sample and bundled ODA converter"]
    fn local_sample_converted_dxf_lists_block_names() {
        let Some(sample_path) = local_sample_dwg() else {
            eprintln!("skipping: no local sample DWG found");
            return;
        };
        if find_oda_converter().is_none() {
            eprintln!("skipping: no bundled/system ODA converter found");
            return;
        }

        let temp_dir = create_temp_conversion_dir().expect("temp dir should be created");
        let converted =
            convert_dwg_to_dxf(&sample_path, &temp_dir).expect("sample should convert to dxf");
        let drawing = Drawing::load_file(&converted).expect("converted dxf should parse");

        let mut block_names = drawing
            .blocks()
            .map(|block| block.name.clone())
            .collect::<Vec<_>>();
        block_names.sort();
        println!("sample converted dxf blocks: {block_names:?}");
        assert!(
            !block_names.is_empty(),
            "converted sample should expose block names"
        );
    }

    #[test]
    fn native_dwg_import_reads_local_sample_when_available() {
        let Some(sample_path) = local_sample_dwg() else {
            return;
        };
        let prior_allow = env::var(ALLOW_DWG_CONVERTER_ENV).ok();
        let prior_wrapper = env::var(DWG_CONVERTER_ENV).ok();
        unsafe {
            env::remove_var(ALLOW_DWG_CONVERTER_ENV);
            env::remove_var(DWG_CONVERTER_ENV);
        }

        let requests = native_dwg_import(&sample_path).expect("sample DWG should import natively");
        let polyline_count = requests
            .iter()
            .filter(|request| request["type"] == "polyline")
            .count();
        let mesh_count = requests
            .iter()
            .filter(|request| request["type"] == "triangle_mesh")
            .count();
        println!(
            "sample native import requests: total={} polylines={} meshes={}",
            requests.len(),
            polyline_count,
            mesh_count,
        );

        match prior_allow {
            Some(value) => unsafe { env::set_var(ALLOW_DWG_CONVERTER_ENV, value) },
            None => unsafe { env::remove_var(ALLOW_DWG_CONVERTER_ENV) },
        }
        match prior_wrapper {
            Some(value) => unsafe { env::set_var(DWG_CONVERTER_ENV, value) },
            None => unsafe { env::remove_var(DWG_CONVERTER_ENV) },
        }

        assert!(
            !requests.is_empty(),
            "native DWG import should surface at least some authored requests from the local sample"
        );
        assert!(
            requests.iter().any(|request| request["type"] == "polyline"),
            "native DWG import should recover polyline geometry from the local sample"
        );
    }

    #[test]
    fn parse_dxf_requests_maps_supported_entities() {
        let mut drawing = Drawing::new();
        drawing.add_entity(Entity::new(EntityType::Line(Line::new(
            Point::new(1.0, 2.0, 3.0),
            Point::new(4.0, 5.0, 6.0),
        ))));
        drawing.add_entity(Entity::new(EntityType::Circle(Circle::new(
            Point::new(0.0, 0.0, 2.0),
            1.0,
        ))));
        drawing.add_entity(Entity::new(EntityType::Arc(Arc::new(
            Point::new(0.0, 0.0, 0.0),
            2.0,
            0.0,
            90.0,
        ))));
        drawing.add_entity(Entity::new(EntityType::Face3D(Face3D::new(
            Point::new(0.0, 0.0, 0.0),
            Point::new(1.0, 0.0, 0.0),
            Point::new(1.0, 1.0, 0.0),
            Point::new(0.0, 1.0, 0.0),
        ))));

        let requests = parse_dxf_requests(&drawing).expect("requests should parse");

        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0]["type"], "polyline");
        assert_eq!(requests[0]["points"][0], serde_json::json!([1.0, 3.0, 2.0]));
        assert_eq!(requests[3]["type"], "triangle_mesh");
        assert_eq!(
            requests[3]["faces"],
            serde_json::json!([[0, 1, 2], [0, 2, 3]])
        );
    }

    #[test]
    fn parse_dxf_requests_expands_insert_blocks() {
        let mut drawing = Drawing::new();
        let mut block = Block {
            name: "ContourBlock".to_string(),
            base_point: Point::new(1.0, 1.0, 0.0),
            ..Default::default()
        };
        block.entities.push(Entity::new(EntityType::Line(Line::new(
            Point::new(1.0, 1.0, 0.0),
            Point::new(3.0, 1.0, 0.0),
        ))));
        drawing.add_block(block);

        let mut insert = Insert {
            name: "ContourBlock".to_string(),
            ..Default::default()
        };
        insert.location = Point::new(10.0, 5.0, 2.0);
        drawing.add_entity(Entity::new(EntityType::Insert(insert)));

        let requests = parse_dxf_requests(&drawing).expect("insert should expand");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["type"], "polyline");
        assert_eq!(
            requests[0]["points"][0],
            serde_json::json!([10.0, 2.0, 5.0])
        );
        assert_eq!(
            requests[0]["points"][1],
            serde_json::json!([12.0, 2.0, 5.0])
        );
    }

    #[test]
    fn parse_dxf_requests_reads_polyline_vertices() {
        let mut drawing = Drawing::new();
        let mut polyline = Polyline::default();
        polyline.add_vertex(
            &mut drawing,
            Vertex {
                location: Point::new(0.0, 0.0, 1.0),
                ..Default::default()
            },
        );
        polyline.add_vertex(
            &mut drawing,
            Vertex {
                location: Point::new(2.0, 0.0, 1.0),
                ..Default::default()
            },
        );
        drawing.add_entity(Entity::new(EntityType::Polyline(polyline)));

        let requests = parse_dxf_requests(&drawing).expect("polyline should import");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["type"], "polyline");
        assert_eq!(requests[0]["points"][1], serde_json::json!([2.0, 1.0, 0.0]));
    }

    #[test]
    fn parse_dxf_requests_tags_non_zero_z_polylines_as_elevation() {
        let mut drawing = Drawing::new();
        let mut polyline = Polyline::default();
        polyline.add_vertex(
            &mut drawing,
            Vertex {
                location: Point::new(0.0, 0.0, 12.5),
                ..Default::default()
            },
        );
        polyline.add_vertex(
            &mut drawing,
            Vertex {
                location: Point::new(5.0, 0.0, 12.5),
                ..Default::default()
            },
        );
        let mut entity = Entity::new(EntityType::Polyline(polyline));
        entity.common.layer = "Contour_12.5".to_string();
        drawing.add_entity(entity);

        let requests = parse_dxf_requests(&drawing).expect("polyline should import");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["elevation_metadata"],
            serde_json::json!({
                "source_layer": "Contour_12.5",
                "elevation": 12.5,
                "survey_source_id": serde_json::Value::Null,
            })
        );
    }

    #[test]
    fn parse_dxf_requests_falls_back_to_layer_name_for_elevation() {
        let mut drawing = Drawing::new();
        let polyline = LwPolyline {
            vertices: vec![
                LwPolylineVertex {
                    x: 0.0,
                    y: 0.0,
                    ..Default::default()
                },
                LwPolylineVertex {
                    x: 2.0,
                    y: 0.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut entity = Entity::new(EntityType::LwPolyline(polyline));
        entity.common.layer = "ELEV_100.0".to_string();
        drawing.add_entity(entity);

        let requests = parse_dxf_requests(&drawing).expect("polyline should import");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["elevation_metadata"]["elevation"],
            serde_json::json!(100.0)
        );
        assert_eq!(
            requests[0]["elevation_metadata"]["source_layer"],
            "ELEV_100.0"
        );
    }

    #[test]
    fn parse_dxf_requests_leaves_non_contour_polylines_without_elevation_metadata() {
        let mut drawing = Drawing::new();
        let polyline = LwPolyline {
            vertices: vec![
                LwPolylineVertex {
                    x: 0.0,
                    y: 0.0,
                    ..Default::default()
                },
                LwPolylineVertex {
                    x: 2.0,
                    y: 0.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut entity = Entity::new(EntityType::LwPolyline(polyline));
        entity.common.layer = "RoadEdge".to_string();
        drawing.add_entity(entity);

        let requests = parse_dxf_requests(&drawing).expect("polyline should import");
        assert_eq!(requests.len(), 1);
        assert!(requests[0]["elevation_metadata"].is_null());
    }

    #[test]
    fn parse_dxf_requests_tessellates_lwpolyline_bulges() {
        let mut drawing = Drawing::new();
        let polyline = LwPolyline {
            vertices: vec![
                LwPolylineVertex {
                    x: 0.0,
                    y: 0.0,
                    bulge: 1.0,
                    ..Default::default()
                },
                LwPolylineVertex {
                    x: 2.0,
                    y: 0.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        drawing.add_entity(Entity::new(EntityType::LwPolyline(polyline)));

        let requests = parse_dxf_requests(&drawing).expect("bulged polyline should import");
        let points = requests[0]["points"]
            .as_array()
            .expect("polyline points should be an array");
        assert!(points.len() > 2, "bulged segment should be tessellated");
        assert_point_close(
            points.first().expect("first point should exist"),
            [0.0, 0.0, 0.0],
        );
        assert_point_close(
            points.last().expect("last point should exist"),
            [2.0, 0.0, 0.0],
        );
    }

    #[test]
    fn parse_dxf_requests_ignores_paper_space_entities() {
        let mut drawing = Drawing::new();
        let mut paper_line = Entity::new(EntityType::Line(Line::new(
            Point::new(0.0, 0.0, 0.0),
            Point::new(1.0, 0.0, 0.0),
        )));
        paper_line.common.is_in_paper_space = true;
        drawing.add_entity(paper_line);
        drawing.add_entity(Entity::new(EntityType::Line(Line::new(
            Point::new(1.0, 0.0, 0.0),
            Point::new(2.0, 0.0, 0.0),
        ))));

        let requests = parse_dxf_requests(&drawing).expect("model space entities should import");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["points"][0], serde_json::json!([1.0, 0.0, 0.0]));
        assert_eq!(requests[0]["points"][1], serde_json::json!([2.0, 0.0, 0.0]));
    }

    #[test]
    fn parse_dxf_requests_reports_unsupported_entity_types() {
        let mut drawing = Drawing::new();
        drawing.add_entity(Entity::new(EntityType::ModelPoint(ModelPoint::default())));

        let error = parse_dxf_requests(&drawing).expect_err("unsupported DXF should fail");
        assert!(error.contains("did not contain any supported import entities"));
        assert!(error.contains("POINT"));
    }

    #[test]
    fn parse_dxf_requests_reports_missing_insert_blocks() {
        let mut drawing = Drawing::new();
        let insert = Insert {
            name: "MissingBlock".to_string(),
            ..Default::default()
        };
        drawing.add_entity(Entity::new(EntityType::Insert(insert)));

        let error = parse_dxf_requests(&drawing).expect_err("missing block should fail");
        assert!(error.contains("did not contain any supported import entities"));
        assert!(error.contains("Missing block definitions: MissingBlock"));
    }

    #[test]
    fn parse_cad_document_requests_maps_native_entities() {
        let document = CadDocument {
            format: CadFormat::Dwg,
            version: CadVersion::Acad2018,
            units: None,
            extents: None,
            layers: Vec::new(),
            blocks: Vec::new(),
            entities: vec![
                CadEntity::Line(CadLine {
                    common: CadEntityCommon {
                        handle: Some("10".to_string()),
                        layer: Some("LAYER_A".to_string()),
                    },
                    start: CadPoint3 {
                        x: 1.0,
                        y: 2.0,
                        z: 3.0,
                    },
                    end: CadPoint3 {
                        x: 4.0,
                        y: 5.0,
                        z: 6.0,
                    },
                }),
                CadEntity::Polyline(CadPolyline {
                    common: CadEntityCommon {
                        handle: Some("11".to_string()),
                        layer: Some("ELEV_100".to_string()),
                    },
                    points: vec![
                        CadPoint3 {
                            x: 0.0,
                            y: 100.0,
                            z: 0.0,
                        },
                        CadPoint3 {
                            x: 1.0,
                            y: 100.0,
                            z: 0.0,
                        },
                    ],
                    closed: false,
                }),
            ],
        };

        let requests = parse_cad_document_requests(&document).expect("native document should map");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["points"][0], serde_json::json!([1.0, 3.0, 2.0]));
        assert_eq!(requests[0]["points"][1], serde_json::json!([4.0, 6.0, 5.0]));
        assert_eq!(requests[0]["layer"], "LAYER_A");
        assert_eq!(
            requests[1]["elevation_metadata"]["elevation"],
            serde_json::json!(100.0)
        );
        assert_eq!(requests[1]["layer"], "ELEV_100");
    }

    #[test]
    fn parse_cad_document_requests_expands_native_blocks() {
        let document = CadDocument {
            format: CadFormat::Dwg,
            version: CadVersion::Acad2018,
            units: None,
            extents: None,
            layers: Vec::new(),
            blocks: vec![CadBlock {
                name: "marker".to_string(),
                base_point: CadPoint3::ZERO,
                entities: vec![CadEntity::Line(CadLine {
                    common: CadEntityCommon {
                        handle: Some("20".to_string()),
                        layer: Some("SYMBOL".to_string()),
                    },
                    start: CadPoint3::ZERO,
                    end: CadPoint3 {
                        x: 2.0,
                        y: 0.0,
                        z: 0.0,
                    },
                })],
            }],
            entities: vec![CadEntity::Insert(CadInsert {
                common: CadEntityCommon {
                    handle: Some("21".to_string()),
                    layer: Some("SYMBOL".to_string()),
                },
                block_name: "marker".to_string(),
                insertion_point: CadPoint3 {
                    x: 10.0,
                    y: 0.0,
                    z: 5.0,
                },
                scale: CadPoint3 {
                    x: 2.0,
                    y: 1.0,
                    z: 1.0,
                },
                rotation_degrees: 0.0,
                column_count: 1,
                row_count: 1,
                column_spacing: 0.0,
                row_spacing: 0.0,
            })],
        };

        let requests = parse_cad_document_requests(&document).expect("native insert should expand");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["points"][0],
            serde_json::json!([10.0, 5.0, 0.0])
        );
        assert_eq!(
            requests[0]["points"][1],
            serde_json::json!([14.0, 5.0, 0.0])
        );
        assert_eq!(requests[0]["layer"], "SYMBOL");
    }

    fn local_sample_dwg() -> Option<PathBuf> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let samples_dir = repo_root.join("tmp/dwg-samples");
        let mut entries = fs::read_dir(samples_dir).ok()?;
        entries
            .find_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("dwg"))
            })
    }

    #[test]
    fn filter_model_space_excludes_paper_space_cluster() {
        // Simulate a DWG with model-space entities at large coords and paper-space near origin.
        let model_entity = |x, y| {
            CadEntity::Polyline(CadPolyline {
                common: CadEntityCommon {
                    handle: None,
                    layer: Some("MODEL".to_string()),
                },
                points: vec![
                    CadPoint3 { x, y, z: 0.0 },
                    CadPoint3 {
                        x: x + 1.0,
                        y: y + 1.0,
                        z: 0.0,
                    },
                ],
                closed: false,
            })
        };
        let paper_entity = |x, y| {
            CadEntity::Polyline(CadPolyline {
                common: CadEntityCommon {
                    handle: None,
                    layer: Some("RAM".to_string()),
                },
                points: vec![
                    CadPoint3 { x, y, z: 0.0 },
                    CadPoint3 {
                        x: x + 1.0,
                        y: y + 1.0,
                        z: 0.0,
                    },
                ],
                closed: false,
            })
        };

        let mut entities = Vec::new();
        // 7 model-space entities clustered at (190000, 6600000)
        for i in 0..7 {
            entities.push(model_entity(
                190000.0 + i as f64 * 10.0,
                6_600_000.0 + i as f64 * 10.0,
            ));
        }
        // 3 paper-space entities near origin
        for i in 0..3 {
            entities.push(paper_entity(100.0 + i as f64, 200.0 + i as f64));
        }

        let filtered = filter_model_space_entities(&entities);
        assert_eq!(filtered.len(), 7, "paper-space entities should be excluded");
        for entity in &filtered {
            let point = entity_representative_point(entity).unwrap();
            assert!(
                point.x > 100_000.0,
                "only model-space entities should remain"
            );
        }
    }

    #[test]
    fn filter_outlier_requests_removes_distant_entity() {
        let make_request = |x: f64, y: f64| {
            serde_json::json!({
                "type": "polyline",
                "points": [[x, y, 0.0], [x + 1.0, y + 1.0, 0.0]],
            })
        };
        let mut requests = Vec::new();
        // 10 entities clustered around (100, 200)
        for i in 0..10 {
            requests.push(make_request(100.0 + i as f64, 200.0 + i as f64));
        }
        // 1 outlier far away
        requests.push(make_request(999_999.0, 999_999.0));

        let filtered = filter_outlier_requests(requests);
        assert_eq!(filtered.len(), 10, "outlier should be removed");
        for request in &filtered {
            let x = request["points"][0][0].as_f64().unwrap();
            assert!(x < 200.0, "only clustered entities should remain");
        }
    }

    fn assert_point_close(value: &Value, expected: [f64; 3]) {
        let point = value.as_array().expect("point should be an array");
        for (component, expected) in point.iter().zip(expected) {
            let actual = component
                .as_f64()
                .expect("point component should be numeric");
            assert!(
                (actual - expected).abs() <= 1.0e-5,
                "expected {expected}, got {actual}"
            );
        }
    }
}
