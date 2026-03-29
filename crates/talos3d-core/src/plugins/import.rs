use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    authored_entity::EntityBounds,
    capability_registry::CapabilityRegistry,
    plugins::{
        camera::focus_orbit_camera_on_bounds,
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        commands::{BeginCommandGroup, CreateEntityCommand, EndCommandGroup},
        history::HistorySet,
        ui::StatusBarData,
    },
};

pub struct ImportPlugin;

const IMPORT_STATUS_DURATION_SECONDS: f32 = 2.5;

impl Plugin for ImportPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ImportRegistry>()
            .init_resource::<PendingImportJobs>()
            .init_resource::<ImportProgressState>()
            .init_resource::<ImportReviewState>()
            .init_resource::<DocumentImportPlacementState>()
            .init_resource::<ImportedLayerPanelState>()
            .init_resource::<PendingImportCommit>()
            .register_command(
                CommandDescriptor {
                    id: "core.import".to_string(),
                    label: "Import...".to_string(),
                    description: "Import external geometry from a supported file format."
                        .to_string(),
                    category: CommandCategory::File,
                    parameters: None,
                    default_shortcut: None,
                    icon: Some("icon.import".to_string()),
                    hint: Some("Import supported file formats into the current model".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_import_dialog,
            )
            .add_systems(
                Update,
                (
                    poll_import_jobs,
                    commit_reviewed_import.before(HistorySet::Queue),
                    sync_imported_layer_visibility_to_registry,
                ),
            );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportOriginMode {
    PreserveSourceCoordinates,
    CenterAtWorldOrigin,
    #[default]
    DocumentLocalOrigin,
}

impl ImportOriginMode {
    pub const ALL: [Self; 3] = [
        Self::DocumentLocalOrigin,
        Self::PreserveSourceCoordinates,
        Self::CenterAtWorldOrigin,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::PreserveSourceCoordinates => "Preserve source coordinates",
            Self::CenterAtWorldOrigin => "Center at world origin",
            Self::DocumentLocalOrigin => "Document local origin",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::PreserveSourceCoordinates => {
                "Keep the source coordinates and only apply manual offset."
            }
            Self::CenterAtWorldOrigin => {
                "Shift this import so its bounds center lands at the world origin."
            }
            Self::DocumentLocalOrigin => {
                "Keep imported geometry near the grid by reusing one stable document offset."
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportTransformSettings {
    pub unit_scale: f32,
    pub origin_mode: ImportOriginMode,
    pub origin_offset: Vec3,
}

const LOCAL_ORIGIN_RECENTER_DISTANCE: f32 = 10_000.0;
const LOCAL_ORIGIN_RECENTER_SCALE_MULTIPLIER: f32 = 100.0;

impl Default for ImportTransformSettings {
    fn default() -> Self {
        Self {
            unit_scale: 1.0,
            origin_mode: ImportOriginMode::default(),
            origin_offset: Vec3::ZERO,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportLayerEntry {
    pub name: String,
    pub count: usize,
    pub include: bool,
    pub visible: bool,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct ImportReviewState {
    pub source_name: Option<String>,
    pub requests: Vec<Value>,
    pub settings: ImportTransformSettings,
    pub layers: Vec<ImportLayerEntry>,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct DocumentImportPlacementState {
    pub local_origin_offset: Option<Vec3>,
}

#[derive(Resource, Debug, Default)]
pub struct ImportProgressState {
    pub started_at: Option<Instant>,
    pub source_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportedLayerEntryState {
    pub count: usize,
    pub visible: bool,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct ImportedLayerPanelState {
    pub entries: BTreeMap<String, ImportedLayerEntryState>,
}

#[derive(Resource, Debug, Default)]
pub struct PendingImportCommit {
    source_name: Option<String>,
    requests: Vec<Value>,
}

pub trait FormatImporter: Send + Sync + 'static {
    fn format_name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn import(&self, path: &Path) -> Result<Vec<Value>, String>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImporterDescriptor {
    pub format_name: String,
    pub extensions: Vec<String>,
}

#[derive(Resource, Default)]
pub struct ImportRegistry {
    ordered_importers: Vec<Arc<dyn FormatImporter>>,
    importers_by_extension: HashMap<String, Arc<dyn FormatImporter>>,
}

impl ImportRegistry {
    pub fn register_importer<I>(&mut self, importer: I)
    where
        I: FormatImporter,
    {
        let importer = Arc::new(importer);
        for extension in importer.extensions() {
            self.importers_by_extension
                .insert(extension.to_ascii_lowercase(), importer.clone());
        }
        self.ordered_importers.push(importer);
    }

    pub fn importers(&self) -> &[Arc<dyn FormatImporter>] {
        &self.ordered_importers
    }

    pub fn list_importers(&self) -> Vec<ImporterDescriptor> {
        self.ordered_importers
            .iter()
            .map(|importer| ImporterDescriptor {
                format_name: importer.format_name().to_string(),
                extensions: importer
                    .extensions()
                    .iter()
                    .map(|extension| extension.to_string())
                    .collect(),
            })
            .collect()
    }

    pub fn importer_for_extension(&self, extension: &str) -> Option<Arc<dyn FormatImporter>> {
        self.importers_by_extension
            .get(&extension.to_ascii_lowercase())
            .cloned()
    }

    pub fn resolve_importer(
        &self,
        path: &Path,
        format_hint: Option<&str>,
    ) -> Result<Arc<dyn FormatImporter>, String> {
        if let Some(format_hint) = format_hint {
            if let Some(importer) = self.importer_for_extension(format_hint) {
                return Ok(importer);
            }

            if let Some(importer) = self
                .ordered_importers
                .iter()
                .find(|importer| importer.format_name().eq_ignore_ascii_case(format_hint))
            {
                return Ok(importer.clone());
            }

            return Err(format!(
                "No importer registered for format hint '{format_hint}'"
            ));
        }

        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .ok_or_else(|| format!("File '{}' does not have a usable extension", path.display()))?;
        self.importer_for_extension(extension)
            .ok_or_else(|| format!("No importer registered for '.{extension}' files"))
    }
}

pub trait ImportRegistryAppExt {
    fn register_format_importer<I>(&mut self, importer: I) -> &mut Self
    where
        I: FormatImporter;
}

impl ImportRegistryAppExt for App {
    fn register_format_importer<I>(&mut self, importer: I) -> &mut Self
    where
        I: FormatImporter,
    {
        if !self.world().contains_resource::<ImportRegistry>() {
            self.init_resource::<ImportRegistry>();
        }

        self.world_mut()
            .resource_mut::<ImportRegistry>()
            .register_importer(importer);
        self
    }
}

#[derive(Resource, Default)]
struct PendingImportJobs {
    jobs: Mutex<Vec<mpsc::Receiver<ImportJobResult>>>,
}

struct ImportJobResult {
    display_path: PathBuf,
    requests: Result<Vec<Value>, String>,
}

fn execute_import_dialog(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let Some(path) = open_import_file_dialog(world.resource::<ImportRegistry>().importers()) else {
        return Ok(CommandResult::empty());
    };
    start_import_job(world, path, None)
}

pub fn start_import_job(
    world: &mut World,
    path: PathBuf,
    format_hint: Option<String>,
) -> Result<CommandResult, String> {
    let staged_path = stage_import_source(&path)?;
    let display_path = path.clone();
    let importer = world
        .resource::<ImportRegistry>()
        .resolve_importer(&staged_path, format_hint.as_deref())?;
    let (sender, receiver) = mpsc::channel();

    thread::Builder::new()
        .name("talos3d-import".to_string())
        .spawn(move || {
            let requests = importer.import(&staged_path);
            let _ = sender.send(ImportJobResult {
                display_path,
                requests,
            });
        })
        .map_err(|error| format!("Failed to start import thread: {error}"))?;

    world
        .resource::<PendingImportJobs>()
        .jobs
        .lock()
        .map_err(|_| "Failed to store pending import job".to_string())?
        .push(receiver);
    {
        let mut progress = world.resource_mut::<ImportProgressState>();
        progress.started_at = Some(Instant::now());
        progress.source_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .map(ToOwned::to_owned);
    }
    set_import_feedback(
        world,
        format!(
            "Importing {}...",
            path.file_name()
                .and_then(|file_name| file_name.to_str())
                .unwrap_or("file")
        ),
    );
    Ok(CommandResult::empty())
}

pub fn import_file_now(
    world: &mut World,
    path: &Path,
    format_hint: Option<&str>,
) -> Result<Vec<u64>, String> {
    let staged_path = stage_import_source(path)?;
    let importer = world
        .resource::<ImportRegistry>()
        .resolve_importer(&staged_path, format_hint)?;
    let requests = importer.import(&staged_path)?;
    let snapshots = import_requests_to_snapshots(world, requests)?;
    let element_ids = snapshots
        .iter()
        .map(|snapshot| snapshot.element_id().0)
        .collect::<Vec<_>>();
    focus_camera_on_imported_snapshots(world, &snapshots);
    let group_name = path.file_name().map(|n| n.to_string_lossy().to_string());
    queue_import_group(world, snapshots, group_name);
    Ok(element_ids)
}

pub fn apply_import_review_to_pending(
    review: &mut ImportReviewState,
    pending: &mut PendingImportCommit,
    placement: &mut DocumentImportPlacementState,
    layers: &mut ImportedLayerPanelState,
) -> Result<(), String> {
    eprintln!(
        "[import] apply_import_review_to_pending called, {} requests",
        review.requests.len()
    );
    let review_snapshot = review.clone();
    if review_snapshot.requests.is_empty() {
        eprintln!("[import] ERROR: no requests pending");
        return Err("No import review is pending".to_string());
    }

    let (requests, visibility) = apply_import_review(&review_snapshot, placement);
    if requests.is_empty() {
        return Err("Import review excluded all entities".to_string());
    }

    pending.source_name = review_snapshot.source_name;
    pending.requests = requests;
    layers.entries = visibility;
    *review = ImportReviewState::default();
    Ok(())
}

fn poll_import_jobs(world: &mut World) {
    let mut completed = Vec::new();
    let mut pending = Vec::new();

    {
        let import_jobs = world.resource::<PendingImportJobs>();
        let mut jobs = match import_jobs.jobs.lock() {
            Ok(jobs) => jobs,
            Err(_) => return,
        };
        for receiver in std::mem::take(&mut *jobs) {
            match receiver.try_recv() {
                Ok(result) => completed.push(result),
                Err(mpsc::TryRecvError::Empty) => pending.push(receiver),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        *jobs = pending;
    }

    for result in completed {
        {
            let mut progress = world.resource_mut::<ImportProgressState>();
            progress.started_at = None;
            progress.source_name = None;
        }
        match result.requests {
            Ok(requests) => {
                eprintln!("[import] poll: received {} requests", requests.len());
                let source_name = result
                    .display_path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| "file".to_string());
                *world.resource_mut::<ImportReviewState>() =
                    build_import_review_state(Some(source_name), requests);
                set_import_feedback(
                    world,
                    "Review import settings and layer selection, then click Import".to_string(),
                );
            }
            Err(error) => set_import_feedback(world, format!("Import failed: {error}")),
        }
    }
}

fn commit_reviewed_import(world: &mut World) {
    let (source_name, requests) = {
        let pending = world.resource::<PendingImportCommit>();
        if pending.requests.is_empty() {
            return;
        }
        (pending.source_name.clone(), pending.requests.clone())
    };

    eprintln!("[import] commit: {} requests", requests.len());
    match import_requests_to_snapshots(world, requests) {
        Ok(snapshots) => {
            let count = snapshots.len();
            if let Some(bounds) = combined_snapshot_bounds(&snapshots) {
                eprintln!("[import] bounds: min={:?} max={:?}", bounds.min, bounds.max);
                let extent = bounds.max - bounds.min;
                eprintln!("[import] extent: {:?}", extent);
            }
            focus_camera_on_imported_snapshots(world, &snapshots);
            queue_import_group(world, snapshots, source_name.clone());
            set_import_feedback(
                world,
                format!(
                    "Imported {count} entities from {}",
                    source_name.as_deref().unwrap_or("file")
                ),
            );
        }
        Err(error) => {
            eprintln!("[import] ERROR: {error}");
            set_import_feedback(world, format!("Import failed: {error}"));
        }
    }

    let mut pending = world.resource_mut::<PendingImportCommit>();
    pending.source_name = None;
    pending.requests.clear();
}

fn stage_import_source(path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("Import path '{}' does not have a file name", path.display()))?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("Failed to create import staging timestamp: {error}"))?
        .as_nanos();
    let stage_dir = std::env::temp_dir().join(format!("talos3d-import-{unique}"));
    fs::create_dir_all(&stage_dir)
        .map_err(|error| format!("Failed to create import staging directory: {error}"))?;
    let staged_path = stage_dir.join(file_name);
    fs::copy(path, &staged_path)
        .map_err(|error| format!("Failed to stage '{}' for import: {error}", path.display()))?;
    Ok(staged_path)
}

fn import_requests_to_snapshots(
    world: &mut World,
    requests: Vec<Value>,
) -> Result<Vec<crate::authored_entity::BoxedEntity>, String> {
    let mut snapshots = Vec::with_capacity(requests.len());
    for request in requests {
        let entity_type = request
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "Import request is missing a string 'type' field".to_string())?;
        let snapshot = world
            .resource::<CapabilityRegistry>()
            .factory_for(entity_type)
            .ok_or_else(|| format!("No authored entity factory registered for '{entity_type}'"))?
            .from_create_request(world, &request)?;
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

fn queue_import_group(
    world: &mut World,
    snapshots: Vec<crate::authored_entity::BoxedEntity>,
    group_name: Option<String>,
) {
    let mut begin_events = world.resource_mut::<Messages<BeginCommandGroup>>();
    begin_events.write(BeginCommandGroup {
        label: "Import file",
    });
    let _ = begin_events;

    // Collect member element IDs before sending create events
    let member_ids: Vec<crate::plugins::identity::ElementId> =
        snapshots.iter().map(|s| s.element_id()).collect();

    // Register imported layer names in the LayerRegistry
    {
        let mut registry = world.resource_mut::<crate::plugins::layers::LayerRegistry>();
        for snapshot in &snapshots {
            for field in snapshot.property_fields() {
                if field.name == "layer" {
                    if let Some(crate::authored_entity::PropertyValue::Text(name)) = &field.value {
                        registry.ensure_layer(name);
                    }
                }
            }
        }
    }

    let mut create_events = world.resource_mut::<Messages<CreateEntityCommand>>();
    for snapshot in snapshots {
        create_events.write(CreateEntityCommand { snapshot });
    }

    // Create a group wrapping all imported entities
    if !member_ids.is_empty() {
        let group_id = world
            .resource::<crate::plugins::identity::ElementIdAllocator>()
            .next_id();
        let group_snapshot = crate::plugins::modeling::group::GroupSnapshot {
            element_id: group_id,
            name: group_name.unwrap_or_else(|| "Import".to_string()),
            member_ids,
            composite: None,
            cached_bounds: None,
        };
        create_events = world.resource_mut::<Messages<CreateEntityCommand>>();
        create_events.write(CreateEntityCommand {
            snapshot: group_snapshot.into(),
        });
    }

    let _ = create_events;

    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);
}

fn focus_camera_on_imported_snapshots(
    world: &mut World,
    snapshots: &[crate::authored_entity::BoxedEntity],
) {
    let Some(bounds) = combined_snapshot_bounds(snapshots) else {
        return;
    };
    let _ = focus_orbit_camera_on_bounds(world, bounds);
}

fn combined_snapshot_bounds(
    snapshots: &[crate::authored_entity::BoxedEntity],
) -> Option<EntityBounds> {
    snapshots
        .iter()
        .filter_map(|snapshot| snapshot.bounds())
        .reduce(|acc, bounds| EntityBounds {
            min: acc.min.min(bounds.min),
            max: acc.max.max(bounds.max),
        })
}

fn build_import_review_state(
    source_name: Option<String>,
    requests: Vec<Value>,
) -> ImportReviewState {
    let layers = summarize_import_layers(&requests)
        .into_iter()
        .map(|(name, count)| ImportLayerEntry {
            name,
            count,
            include: true,
            visible: true,
        })
        .collect();
    ImportReviewState {
        source_name,
        requests,
        settings: ImportTransformSettings::default(),
        layers,
    }
}

fn summarize_import_layers(requests: &[Value]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for request in requests {
        let layer = request_layer_name(request).unwrap_or_else(|| "(Unlayered)".to_string());
        *counts.entry(layer).or_insert(0) += 1;
    }
    counts
}

fn request_layer_name(request: &Value) -> Option<String> {
    request
        .get("layer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn apply_import_review(
    review: &ImportReviewState,
    placement: &mut DocumentImportPlacementState,
) -> (Vec<Value>, BTreeMap<String, ImportedLayerEntryState>) {
    let mut visibility = BTreeMap::new();
    let included_layers = review
        .layers
        .iter()
        .filter(|entry| entry.include)
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    for layer in &review.layers {
        visibility.insert(
            layer.name.clone(),
            ImportedLayerEntryState {
                count: layer.count,
                visible: layer.visible,
            },
        );
    }

    let included_requests = review
        .requests
        .iter()
        .filter(|request| {
            let layer = request_layer_name(request).unwrap_or_else(|| "(Unlayered)".to_string());
            included_layers.iter().any(|included| *included == layer)
        })
        .collect::<Vec<_>>();
    let effective_origin_offset =
        resolve_import_origin_offset(&included_requests, &review.settings, placement);
    eprintln!(
        "[import] origin_mode={:?} offset={:?} included={}",
        review.settings.origin_mode,
        effective_origin_offset,
        included_requests.len()
    );
    let requests = included_requests
        .into_iter()
        .map(|request| transform_import_request(request, &review.settings, effective_origin_offset))
        .collect::<Vec<_>>();
    (requests, visibility)
}

fn resolve_import_origin_offset(
    requests: &[&Value],
    settings: &ImportTransformSettings,
    placement: &mut DocumentImportPlacementState,
) -> Vec3 {
    let placement_offset = match settings.origin_mode {
        ImportOriginMode::PreserveSourceCoordinates => Vec3::ZERO,
        ImportOriginMode::CenterAtWorldOrigin => {
            scaled_request_bounds_center(requests, settings.unit_scale)
                .map_or(Vec3::ZERO, |center| -center)
        }
        ImportOriginMode::DocumentLocalOrigin => {
            if let Some((min, max)) = scaled_request_bounds(requests, settings.unit_scale) {
                let center = (min + max) * 0.5;
                let extent = (max - min).length().max(1.0);
                let mut offset = placement.local_origin_offset.unwrap_or(-center);
                let transformed_center = center + offset;
                let recenter_threshold = LOCAL_ORIGIN_RECENTER_DISTANCE
                    .max(extent * LOCAL_ORIGIN_RECENTER_SCALE_MULTIPLIER);
                if transformed_center.length() > recenter_threshold {
                    offset = -center;
                }
                placement.local_origin_offset = Some(offset);
                offset
            } else {
                Vec3::ZERO
            }
        }
    };

    placement_offset + settings.origin_offset
}

fn scaled_request_bounds_center(requests: &[&Value], unit_scale: f32) -> Option<Vec3> {
    let (min, max) = scaled_request_bounds(requests, unit_scale)?;
    Some((min + max) * 0.5)
}

fn scaled_request_bounds(requests: &[&Value], unit_scale: f32) -> Option<(Vec3, Vec3)> {
    let mut min = Vec3::ZERO;
    let mut max = Vec3::ZERO;
    let mut found = false;

    for request in requests {
        accumulate_request_bounds(request, unit_scale, &mut min, &mut max, &mut found);
    }

    found.then_some((min, max))
}

fn accumulate_request_bounds(
    request: &Value,
    unit_scale: f32,
    min: &mut Vec3,
    max: &mut Vec3,
    found: &mut bool,
) {
    for field in ["points", "vertices"] {
        let Some(points) = request.get(field).and_then(Value::as_array) else {
            continue;
        };
        for point in points {
            let Some(point) = point_from_value(point) else {
                continue;
            };
            let point = point * unit_scale;
            if *found {
                *min = min.min(point);
                *max = max.max(point);
            } else {
                *min = point;
                *max = point;
                *found = true;
            }
        }
    }
}

fn point_from_value(value: &Value) -> Option<Vec3> {
    let point = value.as_array()?;
    if point.len() != 3 {
        return None;
    }
    Some(Vec3::new(
        point[0].as_f64().unwrap_or_default() as f32,
        point[1].as_f64().unwrap_or_default() as f32,
        point[2].as_f64().unwrap_or_default() as f32,
    ))
}

fn transform_import_request(
    request: &Value,
    settings: &ImportTransformSettings,
    origin_offset: Vec3,
) -> Value {
    let mut request = request.clone();
    for field in ["points", "vertices"] {
        if let Some(points) = request.get_mut(field).and_then(Value::as_array_mut) {
            for point in points {
                transform_point_value(point, settings, origin_offset);
            }
        }
    }

    if let Some(metadata) = request
        .get_mut("elevation_metadata")
        .and_then(Value::as_object_mut)
    {
        if let Some(elevation_value) = metadata.get_mut("elevation") {
            if let Some(elevation) = elevation_value.as_f64() {
                let transformed = elevation as f32 * settings.unit_scale + origin_offset.y;
                *elevation_value = Value::from(transformed);
            }
        }
    }

    request
}

fn transform_point_value(
    value: &mut Value,
    settings: &ImportTransformSettings,
    origin_offset: Vec3,
) {
    let Some(point) = value.as_array() else {
        return;
    };
    if point.len() != 3 {
        return;
    }
    let x = point[0].as_f64().unwrap_or_default() as f32;
    let y = point[1].as_f64().unwrap_or_default() as f32;
    let z = point[2].as_f64().unwrap_or_default() as f32;
    let transformed = Vec3::new(x, y, z) * settings.unit_scale + origin_offset;
    *value = serde_json::json!([transformed.x, transformed.y, transformed.z]);
}

fn sync_imported_layer_visibility_to_registry(
    mut layer_state: ResMut<ImportedLayerPanelState>,
    mut registry: ResMut<crate::plugins::layers::LayerRegistry>,
) {
    if !layer_state.is_changed() || layer_state.entries.is_empty() {
        return;
    }
    // Apply import review visibility settings to the layer registry once
    for (name, entry) in &layer_state.entries {
        if let Some(def) = registry.layers.get_mut(name) {
            def.visible = entry.visible;
        }
    }
    // Clear the import layer state so it doesn't keep overriding
    layer_state.entries.clear();
}

fn open_import_file_dialog(importers: &[Arc<dyn FormatImporter>]) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    for importer in importers {
        dialog = dialog.add_filter(importer.format_name(), importer.extensions());
    }
    dialog.pick_file()
}

fn set_import_feedback(world: &mut World, message: String) {
    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(message, IMPORT_STATUS_DURATION_SECONDS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestImporter;

    impl FormatImporter for TestImporter {
        fn format_name(&self) -> &'static str {
            "Test"
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["test"]
        }

        fn import(&self, _path: &Path) -> Result<Vec<Value>, String> {
            Ok(vec![serde_json::json!({"type": "triangle_mesh"})])
        }
    }

    #[test]
    fn registry_lists_and_resolves_importers() {
        let mut registry = ImportRegistry::default();
        registry.register_importer(TestImporter);

        let descriptors = registry.list_importers();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].format_name, "Test");
        assert_eq!(descriptors[0].extensions, vec!["test"]);
        assert!(registry
            .resolve_importer(Path::new("mesh.test"), None)
            .is_ok());
    }

    #[test]
    fn stage_import_source_copies_file_to_temp_location() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let source = std::env::temp_dir().join(format!("talos3d-import-source-{unique}.txt"));
        fs::write(&source, "payload").expect("source file should be written");

        let staged = stage_import_source(&source).expect("file should stage");
        assert_ne!(staged, source);
        assert_eq!(
            fs::read_to_string(&staged).expect("staged file should be readable"),
            "payload"
        );

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(staged);
    }

    #[test]
    fn apply_import_review_filters_layers_and_transforms_points() {
        let mut placement = DocumentImportPlacementState::default();
        let review = ImportReviewState {
            source_name: Some("sample.dxf".to_string()),
            requests: vec![
                serde_json::json!({
                    "type": "polyline",
                    "points": [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
                    "layer": "Contours",
                    "elevation_metadata": {
                        "source_layer": "Contours",
                        "elevation": 2.0,
                        "survey_source_id": serde_json::Value::Null,
                    }
                }),
                serde_json::json!({
                    "type": "triangle_mesh",
                    "vertices": [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                    "faces": [[0, 1, 2]],
                    "layer": "Road"
                }),
            ],
            settings: ImportTransformSettings {
                unit_scale: 2.0,
                origin_mode: ImportOriginMode::PreserveSourceCoordinates,
                origin_offset: Vec3::new(10.0, -1.0, 4.0),
            },
            layers: vec![
                ImportLayerEntry {
                    name: "Contours".to_string(),
                    count: 1,
                    include: true,
                    visible: false,
                },
                ImportLayerEntry {
                    name: "Road".to_string(),
                    count: 1,
                    include: false,
                    visible: true,
                },
            ],
        };

        let (requests, visibility) = apply_import_review(&review, &mut placement);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["points"][0],
            serde_json::json!([12.0, 3.0, 10.0])
        );
        assert_eq!(
            requests[0]["elevation_metadata"]["elevation"],
            serde_json::json!(3.0)
        );
        assert!(!visibility["Contours"].visible);
        assert!(visibility["Road"].visible);
    }

    #[test]
    fn apply_import_review_can_center_geometry_at_world_origin() {
        let mut placement = DocumentImportPlacementState::default();
        let review = ImportReviewState {
            source_name: Some("sample.dxf".to_string()),
            requests: vec![serde_json::json!({
                "type": "polyline",
                "points": [[100.0, 0.0, 100.0], [102.0, 0.0, 104.0]],
                "layer": "Contours",
            })],
            settings: ImportTransformSettings {
                unit_scale: 1.0,
                origin_mode: ImportOriginMode::CenterAtWorldOrigin,
                origin_offset: Vec3::ZERO,
            },
            layers: vec![ImportLayerEntry {
                name: "Contours".to_string(),
                count: 1,
                include: true,
                visible: true,
            }],
        };

        let (requests, _) = apply_import_review(&review, &mut placement);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["points"][0],
            serde_json::json!([-1.0, 0.0, -2.0])
        );
        assert_eq!(requests[0]["points"][1], serde_json::json!([1.0, 0.0, 2.0]));
        assert!(placement.local_origin_offset.is_none());
    }

    #[test]
    fn apply_import_review_reuses_document_local_origin_offset() {
        let mut placement = DocumentImportPlacementState::default();
        let review_a = ImportReviewState {
            source_name: Some("site-a.dxf".to_string()),
            requests: vec![serde_json::json!({
                "type": "polyline",
                "points": [[100.0, 0.0, 100.0], [102.0, 0.0, 104.0]],
                "layer": "Contours",
            })],
            settings: ImportTransformSettings::default(),
            layers: vec![ImportLayerEntry {
                name: "Contours".to_string(),
                count: 1,
                include: true,
                visible: true,
            }],
        };

        let (requests_a, _) = apply_import_review(&review_a, &mut placement);
        assert_eq!(
            requests_a[0]["points"][0],
            serde_json::json!([-1.0, 0.0, -2.0])
        );
        assert_eq!(
            requests_a[0]["points"][1],
            serde_json::json!([1.0, 0.0, 2.0])
        );
        assert_eq!(
            placement.local_origin_offset,
            Some(Vec3::new(-101.0, -0.0, -102.0))
        );

        let review_b = ImportReviewState {
            source_name: Some("site-b.dxf".to_string()),
            requests: vec![serde_json::json!({
                "type": "polyline",
                "points": [[110.0, 0.0, 110.0], [112.0, 0.0, 114.0]],
                "layer": "Contours",
            })],
            settings: ImportTransformSettings::default(),
            layers: vec![ImportLayerEntry {
                name: "Contours".to_string(),
                count: 1,
                include: true,
                visible: true,
            }],
        };

        let (requests_b, _) = apply_import_review(&review_b, &mut placement);
        assert_eq!(
            requests_b[0]["points"][0],
            serde_json::json!([9.0, 0.0, 8.0])
        );
        assert_eq!(
            requests_b[0]["points"][1],
            serde_json::json!([11.0, 0.0, 12.0])
        );
    }

    #[test]
    fn build_import_review_state_summarizes_layers() {
        let review = build_import_review_state(
            Some("sample.dxf".to_string()),
            vec![
                serde_json::json!({"type": "polyline", "points": [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], "layer": "Contours"}),
                serde_json::json!({"type": "triangle_mesh", "vertices": [[0.0, 0.0, 0.0]], "faces": [], "layer": "Contours"}),
                serde_json::json!({"type": "polyline", "points": [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], "layer": serde_json::Value::Null}),
            ],
        );

        assert_eq!(review.layers.len(), 2);
        assert_eq!(review.layers[0].name, "(Unlayered)");
        assert_eq!(review.layers[0].count, 1);
        assert_eq!(review.layers[1].name, "Contours");
        assert_eq!(review.layers[1].count, 2);
    }
}
