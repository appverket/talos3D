use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    capability_registry::{CapabilityRegistry, DefaultsRegistry},
    curation::{Nomination, NominationQueue, SourceRegistry, SourceRegistryEntry, SourceTier},
    plugins::{
        bundled_definition_libraries::apply_bundled_definition_libraries,
        commands::snapshot_dependency_order,
        definition_preview_scene::PreviewOnly,
        document_properties::DocumentProperties,
        document_state::DocumentState,
        history::{History, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        layers::LayerRegistry,
        lighting::{ensure_default_lighting_scene, SceneLightingSettings},
        materials::{
            ensure_builtin_materials, is_builtin_material_id, material_texture_asset_ids,
            normalize_material_textures, MaterialRegistry, TextureRegistry,
        },
        modeling::{
            definition::{DefinitionLibraryRegistry, DefinitionRegistry},
            dependency_graph::stamp_authored_entity_dependencies,
        },
        named_views::NamedViewRegistry,
        property_edit::PropertyEditState,
        selection::Selected,
        storage::Storage,
        tools::{ActiveTool, Preview},
        transform::TransformState,
        ui::StatusBarData,
    },
};

const PROJECT_FILE_VERSION: u32 = 1;
const FEEDBACK_DURATION_SECONDS: f32 = 2.0;
const FILE_EXTENSION: &str = "talos3d";

pub struct PersistencePlugin;

impl Plugin for PersistencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OpaquePersistedEntities>().add_systems(
            Update,
            (
                new_document_shortcut,
                save_project_shortcut,
                save_as_shortcut,
                load_project_shortcut,
            ),
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersistedEntityRecord {
    #[serde(rename = "type")]
    pub type_name: String,
    pub data: Value,
}

#[derive(Resource, Debug, Default, Clone, PartialEq)]
pub struct OpaquePersistedEntities(pub Vec<PersistedEntityRecord>);

#[derive(Debug, Serialize, Deserialize)]
struct ProjectFile {
    version: u32,
    next_element_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    document_properties: Option<DocumentProperties>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layers: Option<LayerRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    materials: Option<MaterialRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    textures: Option<TextureRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    definitions: Option<DefinitionRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    definition_libraries: Option<DefinitionLibraryRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    named_views: Option<NamedViewRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lighting: Option<SceneLightingSettings>,
    /// Project-scope `SourceRegistry` entries (ADR-040 / PP80). Only
    /// `SourceTier::Project` entries persist here; Canonical and
    /// Jurisdictional live in code/packs and are rebuilt on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sources: Option<Vec<SourceRegistryEntry>>,
    /// Pending source-registry nominations (ADR-040 / PP80). Persist so
    /// an agent's proposed additions survive across authoring sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nominations: Option<Vec<Nomination>>,
    entities: Vec<PersistedEntityRecord>,
}

// --- Shortcut handlers ---

fn new_document_shortcut(world: &mut World) {
    let (pressed, shift_pressed) = primary_shortcut_state(world, KeyCode::KeyN);
    if !pressed || shift_pressed {
        return;
    }
    new_document(world);
    set_feedback(world, "New project".to_string());
}

fn save_project_shortcut(world: &mut World) {
    let (pressed, shift_pressed) = primary_shortcut_state(world, KeyCode::KeyS);
    if !pressed || shift_pressed {
        return;
    }
    match save_project_now(world) {
        Ok(()) => {
            let name = world.resource::<DocumentState>().display_name();
            set_feedback(world, format!("Saved {name}"));
        }
        Err(error) => set_feedback(world, format!("Save failed: {error}")),
    }
}

fn save_as_shortcut(world: &mut World) {
    let (pressed, shift_pressed) = primary_shortcut_state(world, KeyCode::KeyS);
    if !pressed || !shift_pressed {
        return;
    }
    match save_as_now(world) {
        Ok(Some(())) => {
            let name = world.resource::<DocumentState>().display_name();
            set_feedback(world, format!("Saved {name}"));
        }
        Ok(None) => {} // cancelled
        Err(error) => set_feedback(world, format!("Save failed: {error}")),
    }
}

fn load_project_shortcut(world: &mut World) {
    let (pressed, shift_pressed) = primary_shortcut_state(world, KeyCode::KeyO);
    if !pressed || shift_pressed {
        return;
    }
    match open_project_dialog(world) {
        Ok(Some(())) => {
            let name = world.resource::<DocumentState>().display_name();
            set_feedback(world, format!("Loaded {name}"));
        }
        Ok(None) => {} // cancelled
        Err(error) => set_feedback(world, format!("Load failed: {error}")),
    }
}

// --- Public API ---

/// Save to the current path, or open a Save As dialog if no path is set.
pub fn save_project_now(world: &mut World) -> Result<(), String> {
    let current_path = world.resource::<DocumentState>().current_path.clone();
    match current_path {
        Some(path) => save_to_path(world, &path),
        None => save_as_now(world).map(|_| ()),
    }
}

pub fn save_project_to_path(world: &mut World, path: PathBuf) -> Result<PathBuf, String> {
    let path = ensure_extension(path);
    save_to_path(world, &path)?;
    Ok(path)
}

pub fn load_project_from_path(world: &mut World, path: PathBuf) -> Result<PathBuf, String> {
    load_from_path(world, &path)?;
    Ok(path)
}

/// Open a Save As dialog, save to the chosen path.
pub fn save_as_now(world: &mut World) -> Result<Option<()>, String> {
    let current_path = world.resource::<DocumentState>().current_path.clone();
    let mut dialog = rfd::FileDialog::new()
        .add_filter("Talos3D Project", &[FILE_EXTENSION])
        .set_file_name("project.talos3d");
    if let Some(ref path) = current_path {
        if let Some(parent) = path.parent() {
            dialog = dialog.set_directory(parent);
        }
        if let Some(name) = path.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy().to_string());
        }
    }
    match dialog.save_file() {
        Some(path) => {
            let path = ensure_extension(path);
            save_to_path(world, &path)?;
            Ok(Some(()))
        }
        None => Ok(None),
    }
}

/// Open a file dialog and load the chosen project.
pub fn open_project_dialog(world: &mut World) -> Result<Option<()>, String> {
    let dialog = rfd::FileDialog::new().add_filter("Talos3D Project", &[FILE_EXTENSION]);
    match dialog.pick_file() {
        Some(path) => {
            load_from_path(world, &path)?;
            Ok(Some(()))
        }
        None => Ok(None),
    }
}

/// Create a new empty document.
pub fn new_document(world: &mut World) {
    clear_scene(world);

    let mut props = DocumentProperties::default();
    if let Some(defaults_registry) = world.get_resource::<DefaultsRegistry>() {
        defaults_registry.apply_all(&mut props);
    }
    world.insert_resource(props);
    world.insert_resource(OpaquePersistedEntities::default());
    world.insert_resource(LayerRegistry::default());
    world.insert_resource(MaterialRegistry::default());
    world.insert_resource(TextureRegistry::default());
    if let Some(mut materials) = world.get_resource_mut::<MaterialRegistry>() {
        ensure_builtin_materials(&mut materials);
    }
    world.insert_resource(DefinitionRegistry::default());
    world.insert_resource(DefinitionLibraryRegistry::default());
    if let Some(mut libraries) = world.get_resource_mut::<DefinitionLibraryRegistry>() {
        if let Err(error) = apply_bundled_definition_libraries(&mut libraries) {
            error!("Failed to restore bundled definition libraries for new document: {error}");
        }
    }
    world.insert_resource(NamedViewRegistry::default());
    world.insert_resource(SceneLightingSettings::default());
    // Curation substrate (ADR-040 / PP80): reset the SourceRegistry's
    // Project-tier entries on New Document. Canonical seeds and any
    // pack-loaded Jurisdictional entries stay put.
    if let Some(mut registry) = world.get_resource_mut::<SourceRegistry>() {
        registry.replace_project_scope(std::iter::empty());
    }
    if let Some(mut queue) = world.get_resource_mut::<NominationQueue>() {
        *queue = NominationQueue::default();
    }
    world.resource_mut::<ElementIdAllocator>().set_next(1);
    ensure_default_lighting_scene(world);
    world.resource_mut::<History>().clear();
    world.resource_mut::<PendingCommandQueue>().clear();
    world.resource_mut::<PropertyEditState>().clear();
    world.resource_mut::<TransformState>().clear();
    world
        .resource_mut::<NextState<ActiveTool>>()
        .set(ActiveTool::Select);
    world.resource_mut::<DocumentState>().reset();
}

// --- Internal ---

fn save_to_path(world: &mut World, path: &PathBuf) -> Result<(), String> {
    let project = build_project_file(world)?;
    let json = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
    let key = path.to_string_lossy().into_owned();
    world.resource::<Storage>().0.save(json.as_bytes(), &key)?;

    world.resource_mut::<History>().mark_save_point();
    world
        .resource_mut::<DocumentState>()
        .mark_saved(path.clone());

    Ok(())
}

fn load_from_path(world: &mut World, path: &PathBuf) -> Result<(), String> {
    let key = path.to_string_lossy().into_owned();
    let contents = String::from_utf8(world.resource::<Storage>().0.load(&key)?)
        .map_err(|error| error.to_string())?;
    let project: ProjectFile = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
    load_project(world, project)?;

    let mut doc_state = world.resource_mut::<DocumentState>();
    doc_state.mark_saved(path.clone());

    Ok(())
}

fn ensure_extension(mut path: PathBuf) -> PathBuf {
    match path.extension() {
        Some(ext) if ext == FILE_EXTENSION => path,
        _ => {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(".");
            name.push(FILE_EXTENSION);
            path.set_file_name(name);
            path
        }
    }
}

fn build_project_file(world: &mut World) -> Result<ProjectFile, String> {
    // `Without<PreviewOnly>` is defense-in-depth: the definition preview scene
    // spawns an occurrence root with `ElementId(u64::MAX - 1)` that must never
    // appear in a saved project file.  `PreviewOnly` is the single gate.
    let mut query = world.query_filtered::<Entity, (With<ElementId>, Without<PreviewOnly>)>();
    let entity_ids: Vec<Entity> = query.iter(world).collect();
    let registry = world.resource::<CapabilityRegistry>();

    let mut entities = entity_ids
        .into_iter()
        .filter_map(|entity| world.get_entity(entity).ok())
        .filter_map(|entity_ref| registry.capture_snapshot(&entity_ref, world))
        .filter(|snapshot| snapshot.scope() == crate::authored_entity::EntityScope::AuthoredModel)
        .map(|snapshot| PersistedEntityRecord {
            type_name: snapshot.type_name().to_string(),
            data: snapshot.to_persisted_json(),
        })
        .collect::<Vec<_>>();
    entities.sort_by_key(entity_record_sort_key);
    entities.extend(
        world
            .resource::<OpaquePersistedEntities>()
            .0
            .iter()
            .cloned(),
    );

    let doc_props = world.resource::<DocumentProperties>().clone();
    let layer_registry = world.resource::<LayerRegistry>().clone();
    let layers = if layer_registry.layers.len() > 1 {
        Some(layer_registry)
    } else {
        None
    };
    let mut material_registry = MaterialRegistry::default();
    let mut referenced_texture_ids = std::collections::BTreeSet::new();
    for material in world.resource::<MaterialRegistry>().all() {
        if !is_builtin_material_id(&material.id) {
            referenced_texture_ids.extend(material_texture_asset_ids(material));
            material_registry.upsert(material.clone());
        }
    }
    let materials = if material_registry.count() > 0 {
        Some(material_registry)
    } else {
        None
    };
    let texture_registry = world.resource::<TextureRegistry>();
    let textures = if referenced_texture_ids.is_empty() {
        None
    } else {
        Some(texture_registry.referenced_subset(&referenced_texture_ids))
    };
    let definition_registry = world.resource::<DefinitionRegistry>().clone();
    let definitions = if definition_registry.list().is_empty() {
        None
    } else {
        Some(definition_registry)
    };
    let mut definition_library_registry = DefinitionLibraryRegistry::default();
    for library in world.resource::<DefinitionLibraryRegistry>().list() {
        if library.scope != crate::plugins::modeling::definition::DefinitionLibraryScope::Bundled {
            definition_library_registry.insert(library.clone());
        }
    }
    let definition_libraries = if definition_library_registry.list().is_empty() {
        None
    } else {
        Some(definition_library_registry)
    };
    let named_view_registry = world.resource::<NamedViewRegistry>().clone();
    let named_views = if named_view_registry.views.is_empty() {
        None
    } else {
        Some(named_view_registry)
    };
    let lighting = world.get_resource::<SceneLightingSettings>().cloned();
    let sources = world.get_resource::<SourceRegistry>().and_then(|reg| {
        let project_entries: Vec<SourceRegistryEntry> =
            reg.project_scope_entries().cloned().collect();
        if project_entries.is_empty() {
            None
        } else {
            Some(project_entries)
        }
    });
    let nominations = world.get_resource::<NominationQueue>().and_then(|q| {
        if q.is_empty() {
            None
        } else {
            Some(q.list().iter().cloned().collect::<Vec<_>>())
        }
    });

    Ok(ProjectFile {
        version: PROJECT_FILE_VERSION,
        next_element_id: world.resource::<ElementIdAllocator>().next_value(),
        document_properties: Some(doc_props),
        layers,
        materials,
        textures,
        definitions,
        definition_libraries,
        named_views,
        lighting,
        sources,
        nominations,
        entities,
    })
}

fn load_project(world: &mut World, project: ProjectFile) -> Result<(), String> {
    let ProjectFile {
        version,
        mut next_element_id,
        document_properties,
        layers,
        materials,
        textures,
        definitions,
        definition_libraries,
        named_views,
        lighting,
        sources,
        nominations,
        entities,
    } = project;

    if version != PROJECT_FILE_VERSION {
        return Err(format!(
            "Unsupported project version {} (expected {})",
            version, PROJECT_FILE_VERSION
        ));
    }

    let had_lighting = lighting.is_some();
    let registry = world.resource::<CapabilityRegistry>();
    let mut recognized = Vec::new();
    let mut opaque = Vec::new();
    let mut legacy_dimension_annotations = Vec::new();
    let mut legacy_section_views = Vec::new();

    for mut record in entities {
        upgrade_legacy_entity_record(&mut record, &mut next_element_id);
        if matches!(record.type_name.as_str(), "dimension_line" | "clip_plane") {
            ensure_metadata_record_element_id(&mut record.data, &mut next_element_id);
        }
        if record.type_name == "dimension_line" {
            legacy_dimension_annotations.push(record.data);
            continue;
        }
        if record.type_name == "clip_plane" {
            legacy_section_views.push(record.data);
            continue;
        }
        if let Some(factory) = registry.factory_for(&record.type_name) {
            let snapshot = factory.from_persisted_json(&record.data)?;
            recognized.push(snapshot);
        } else {
            opaque.push(record);
        }
    }

    clear_scene(world);

    let doc_props = match document_properties {
        Some(props) => props,
        None => {
            let mut props = DocumentProperties::default();
            if let Some(defaults_registry) = world.get_resource::<DefaultsRegistry>() {
                defaults_registry.apply_all(&mut props);
            }
            props
        }
    };
    let mut doc_props = doc_props;
    if !legacy_dimension_annotations.is_empty()
        && !doc_props
            .domain_defaults
            .contains_key(crate::plugins::dimension_line::DIMENSION_ANNOTATIONS_KEY)
    {
        doc_props.domain_defaults.insert(
            crate::plugins::dimension_line::DIMENSION_ANNOTATIONS_KEY.to_string(),
            Value::Array(legacy_dimension_annotations),
        );
    }
    if !legacy_section_views.is_empty()
        && !doc_props
            .domain_defaults
            .contains_key(crate::plugins::clipping_planes::SECTION_VIEW_METADATA_KEY)
    {
        doc_props.domain_defaults.insert(
            crate::plugins::clipping_planes::SECTION_VIEW_METADATA_KEY.to_string(),
            Value::Array(legacy_section_views),
        );
    }
    world.insert_resource(doc_props);
    world.insert_resource(layers.unwrap_or_default());
    world.insert_resource(textures.unwrap_or_default());
    world.insert_resource(materials.unwrap_or_default());
    if let Some(mut materials) = world.get_resource_mut::<MaterialRegistry>() {
        ensure_builtin_materials(&mut materials);
    }
    let mut normalized_materials = {
        let materials = world.resource::<MaterialRegistry>();
        materials.all().cloned().collect::<Vec<_>>()
    };
    if let Some(mut textures) = world.get_resource_mut::<TextureRegistry>() {
        for material in &mut normalized_materials {
            normalize_material_textures(material, &mut textures);
        }
    }
    if let Some(mut materials) = world.get_resource_mut::<MaterialRegistry>() {
        for material in normalized_materials {
            if let Some(existing) = materials.get_mut(&material.id) {
                *existing = material;
            }
        }
    }
    world.insert_resource(definitions.unwrap_or_default());
    world.insert_resource(definition_libraries.unwrap_or_default());
    if let Some(mut libraries) = world.get_resource_mut::<DefinitionLibraryRegistry>() {
        if let Err(error) = apply_bundled_definition_libraries(&mut libraries) {
            error!("Failed to restore bundled definition libraries after project load: {error}");
        }
    }
    world.insert_resource(named_views.unwrap_or_default());
    world.insert_resource(lighting.unwrap_or_default());

    // Curation substrate (ADR-040 / PP80): reload Project-tier sources
    // into the registry. The registry itself is installed by
    // `CurationPlugin` and already holds Canonical seeds; here we only
    // restore the project-persisted tier. Jurisdictional/Organizational
    // entries live in packs and aren't part of the project file.
    if let Some(mut registry) = world.get_resource_mut::<SourceRegistry>() {
        let project_entries: Vec<SourceRegistryEntry> = sources
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.tier == SourceTier::Project)
            .collect();
        registry.replace_project_scope(project_entries);
    }
    if let Some(mut queue) = world.get_resource_mut::<NominationQueue>() {
        queue.restore(nominations.unwrap_or_default());
    }

    recognized.sort_by_key(snapshot_dependency_order);
    for snapshot in &recognized {
        snapshot.apply_to(world);
        stamp_authored_entity_dependencies(world, snapshot);
    }

    world.insert_resource(OpaquePersistedEntities(opaque));
    if !had_lighting {
        ensure_default_lighting_scene(world);
    }
    world
        .resource_mut::<ElementIdAllocator>()
        .set_next(next_element_id);
    world.resource_mut::<History>().clear();
    world.resource_mut::<PendingCommandQueue>().clear();
    world.resource_mut::<PropertyEditState>().clear();
    world.resource_mut::<TransformState>().clear();
    world
        .resource_mut::<NextState<ActiveTool>>()
        .set(ActiveTool::Select);

    Ok(())
}

fn entity_record_sort_key(record: &PersistedEntityRecord) -> (u8, u64) {
    let type_order = snapshot_dependency_order_by_name(&record.type_name);
    let element_id = record
        .data
        .get("element_id")
        .and_then(Value::as_u64)
        .or_else(|| {
            record.data.as_object().and_then(|obj| {
                obj.values()
                    .find_map(|v| v.get("element_id").and_then(Value::as_u64))
            })
        })
        .unwrap_or(u64::MAX);
    (type_order, element_id)
}

fn upgrade_legacy_entity_record(record: &mut PersistedEntityRecord, next_element_id: &mut u64) {
    if !is_legacy_primitive_record_type(&record.type_name) {
        return;
    }
    let Some(object) = record.data.as_object_mut() else {
        return;
    };
    if !object.contains_key("element_id") {
        object.insert("element_id".to_string(), Value::from(*next_element_id));
        *next_element_id += 1;
    }
    object.entry("rotation".to_string()).or_insert_with(|| {
        serde_json::to_value(crate::plugins::modeling::primitives::ShapeRotation::default())
            .unwrap_or(Value::Null)
    });
}

fn ensure_metadata_record_element_id(data: &mut Value, next_element_id: &mut u64) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    if !object.contains_key("element_id") {
        object.insert("element_id".to_string(), Value::from(*next_element_id));
        *next_element_id += 1;
    }
}

fn is_legacy_primitive_record_type(type_name: &str) -> bool {
    matches!(
        type_name,
        "box"
            | "cylinder"
            | "sphere"
            | "plane"
            | "profile_extrusion"
            | "profile_sweep"
            | "profile_revolve"
    )
}

fn snapshot_dependency_order_by_name(type_name: &str) -> u8 {
    match type_name {
        "wall" => 0,
        "opening" => 1,
        "face_profile_feature" | "csg" => 3,
        "semantic_assembly" => 4,
        "semantic_relation" => 5,
        _ => 2,
    }
}

fn clear_scene(world: &mut World) {
    // Flush deferred commands so mesh-generation inserts on about-to-be-despawned
    // entities don't fire after the entities are gone (causing a panic).
    world.flush();

    let mut entities_to_despawn = Vec::new();
    let mut meshes_to_remove = Vec::new();

    {
        let mut query = world.query::<(
            Entity,
            Option<&Mesh3d>,
            Option<&ElementId>,
            Has<Preview>,
            Has<Selected>,
        )>();
        for (entity, mesh, element_id, is_preview, _) in query.iter(world) {
            if element_id.is_some() || is_preview {
                entities_to_despawn.push(entity);
                if let Some(mesh) = mesh {
                    meshes_to_remove.push(mesh.id());
                }
            }
        }
    }

    for mesh_id in meshes_to_remove {
        world.resource_mut::<Assets<Mesh>>().remove(mesh_id);
    }

    for entity in entities_to_despawn {
        let _ = world.despawn(entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        bundled_definition_libraries::apply_bundled_definition_libraries,
        definition_preview_scene::PreviewOnly,
        history::{History, PendingCommandQueue},
        layers::LayerRegistry,
        lighting::SceneLightingSettings,
        materials::{
            ensure_builtin_materials, normalize_material_textures, MaterialDef,
            TextureChannelIntent, TextureColorSpace, TextureRef, TextureRegistry,
            BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80, BUILTIN_MATERIAL_MAIBEC_RED_CEDAR_LIGHT_H2BO,
        },
        modeling::{
            definition::{
                Definition, DefinitionId, DefinitionKind, DefinitionLibrary, DefinitionLibraryId,
                DefinitionLibraryScope, Interface, OverridePolicy, ParamType, ParameterDef,
                ParameterMetadata, ParameterSchema,
            },
            occurrence::{OccurrenceFactory, OccurrenceIdentity},
        },
        property_edit::PropertyEditState,
        tools::ActiveTool,
        transform::TransformState,
    };

    #[test]
    fn build_project_file_omits_bundled_definition_libraries() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(DocumentProperties::default());
        world.insert_resource(LayerRegistry::default());
        world.insert_resource(MaterialRegistry::default());
        world.insert_resource(TextureRegistry::default());
        world.insert_resource(DefinitionRegistry::default());
        world.insert_resource(NamedViewRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(OpaquePersistedEntities::default());

        let mut libraries = DefinitionLibraryRegistry::default();
        apply_bundled_definition_libraries(&mut libraries)
            .expect("bundled definition libraries should load");
        libraries.insert(DefinitionLibrary {
            id: DefinitionLibraryId("test.document-library".to_string()),
            name: "Document Library".to_string(),
            scope: DefinitionLibraryScope::DocumentLocal,
            source_path: None,
            tags: vec!["test".to_string()],
            definitions: default(),
        });
        world.insert_resource(libraries);

        let project = build_project_file(&mut world).expect("project should serialize");
        let libraries = project
            .definition_libraries
            .expect("document-scoped library should be persisted");
        assert_eq!(libraries.list().len(), 1);
        assert_eq!(
            libraries.list()[0].id,
            DefinitionLibraryId("test.document-library".to_string())
        );
    }

    #[test]
    fn build_project_file_excludes_preview_only_entities() {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(OccurrenceFactory);
        world.insert_resource(registry);
        world.insert_resource(DocumentProperties::default());
        world.insert_resource(LayerRegistry::default());
        world.insert_resource(MaterialRegistry::default());
        world.insert_resource(TextureRegistry::default());
        world.insert_resource(DefinitionRegistry::default());
        world.insert_resource(DefinitionLibraryRegistry::default());
        world.insert_resource(NamedViewRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(OpaquePersistedEntities::default());

        let definition_id = DefinitionId("test.definition".to_string());
        world.spawn((
            ElementId(1),
            OccurrenceIdentity::new(definition_id.clone(), 1),
            Transform::default(),
            Name::new("Authored occurrence"),
        ));
        world.spawn((
            ElementId(2),
            OccurrenceIdentity::new(definition_id, 1),
            Transform::default(),
            Name::new("Preview occurrence"),
            PreviewOnly,
        ));

        let project = build_project_file(&mut world).expect("project should serialize");

        assert_eq!(project.entities.len(), 1);
        assert_eq!(project.entities[0].type_name, "occurrence");
        assert_eq!(
            project.entities[0]
                .data
                .get("element_id")
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn load_project_restores_definition_occurrences_as_editable_roots() {
        let definition_id = DefinitionId("test.window.definition".to_string());
        let definition = Definition {
            id: definition_id.clone(),
            base_definition_id: None,
            name: "Test Window".to_string(),
            definition_kind: DefinitionKind::Solid,
            definition_version: 1,
            interface: Interface {
                parameters: ParameterSchema(vec![ParameterDef {
                    name: "overall_width".to_string(),
                    param_type: ParamType::Numeric,
                    default_value: serde_json::json!(1.2),
                    override_policy: OverridePolicy::Overridable,
                    metadata: ParameterMetadata::default(),
                }]),
                void_declaration: None,
                external_context_requirements: Vec::new(),
            },
            evaluators: Vec::new(),
            representations: Vec::new(),
            compound: None,
            domain_data: Value::Null,
        };

        let mut source = World::new();
        let mut source_factories = CapabilityRegistry::default();
        source_factories.register_factory(OccurrenceFactory);
        source.insert_resource(source_factories);
        source.insert_resource(DocumentProperties::default());
        source.insert_resource(LayerRegistry::default());
        source.insert_resource(MaterialRegistry::default());
        source.insert_resource(TextureRegistry::default());
        let mut source_definitions = DefinitionRegistry::default();
        source_definitions.insert(definition);
        source.insert_resource(source_definitions);
        source.insert_resource(DefinitionLibraryRegistry::default());
        source.insert_resource(NamedViewRegistry::default());
        source.insert_resource(ElementIdAllocator::default());
        source.insert_resource(OpaquePersistedEntities::default());

        let mut identity = OccurrenceIdentity::new(definition_id.clone(), 1);
        identity
            .overrides
            .set("overall_width".to_string(), serde_json::json!(1.68));
        source.spawn((
            ElementId(42),
            identity,
            Transform::from_xyz(2.0, 0.0, 0.0),
            Name::new("Saved reusable window"),
        ));

        let project = build_project_file(&mut source).expect("project should serialize");
        assert!(project.entities.iter().any(|record| {
            record.type_name == "occurrence"
                && record.data.get("element_id").and_then(Value::as_u64) == Some(42)
        }));

        let mut target = World::new();
        let mut target_factories = CapabilityRegistry::default();
        target_factories.register_factory(OccurrenceFactory);
        target.insert_resource(target_factories);
        target.insert_resource(bevy::prelude::Assets::<bevy::prelude::Mesh>::default());
        target.insert_resource(MaterialRegistry::default());
        target.insert_resource(DefinitionRegistry::default());
        target.insert_resource(DefinitionLibraryRegistry::default());
        target.insert_resource(NamedViewRegistry::default());
        target.insert_resource(ElementIdAllocator::default());
        target.insert_resource(OpaquePersistedEntities::default());
        target.insert_resource(History::default());
        target.insert_resource(PendingCommandQueue::default());
        target.insert_resource(PropertyEditState::default());
        target.insert_resource(TransformState::default());
        target.insert_resource(State::new(ActiveTool::Select));
        target.insert_resource(NextState::<ActiveTool>::default());

        load_project(&mut target, project).expect("project should load");

        let mut query = target.query::<(&ElementId, &OccurrenceIdentity, &Name, &Transform)>();
        let restored = query
            .iter(&target)
            .find(|(element_id, _, _, _)| **element_id == ElementId(42))
            .expect("saved occurrence root should be restored");
        assert_eq!(restored.1.definition_id, definition_id);
        assert_eq!(
            restored
                .1
                .overrides
                .get("overall_width")
                .and_then(Value::as_f64),
            Some(1.68)
        );
        assert_eq!(restored.2.as_str(), "Saved reusable window");
        assert_eq!(restored.3.translation, Vec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn build_project_file_omits_bundled_materials() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(DocumentProperties::default());
        world.insert_resource(LayerRegistry::default());
        let mut materials = MaterialRegistry::default();
        ensure_builtin_materials(&mut materials);
        materials.create("Project Material");
        world.insert_resource(materials);
        world.insert_resource(TextureRegistry::default());
        world.insert_resource(DefinitionRegistry::default());
        world.insert_resource(DefinitionLibraryRegistry::default());
        world.insert_resource(NamedViewRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(OpaquePersistedEntities::default());

        let project = build_project_file(&mut world).expect("project should serialize");
        let materials = project.materials.expect("project material should persist");
        assert_eq!(materials.count(), 1);
        assert!(materials
            .get(BUILTIN_MATERIAL_MAIBEC_RED_CEDAR_LIGHT_H2BO)
            .is_none());
        assert!(materials
            .get(BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80)
            .is_none());
        assert_eq!(
            materials
                .all()
                .next()
                .expect("one project material should remain")
                .name,
            "Project Material"
        );
    }

    #[test]
    fn build_project_file_persists_only_referenced_texture_assets() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(DocumentProperties::default());
        world.insert_resource(LayerRegistry::default());

        let mut materials = MaterialRegistry::default();
        let mut textures = TextureRegistry::default();
        let mut material = MaterialDef::new("Project Material");
        material.base_color_texture = Some(TextureRef::Embedded {
            data: "referenced-bytes".to_string(),
            mime: "image/png".to_string(),
        });
        normalize_material_textures(&mut material, &mut textures);
        let referenced_texture_id = match material.base_color_texture.as_ref() {
            Some(TextureRef::TextureAsset { id }) => id.clone(),
            other => panic!("expected normalized texture asset, got {other:?}"),
        };
        let unreferenced_texture_id = textures.intern_embedded(
            "unreferenced-bytes".to_string(),
            "image/png".to_string(),
            TextureColorSpace::Srgb,
            TextureChannelIntent::BaseColor,
        );
        materials.upsert(material);
        world.insert_resource(materials);
        world.insert_resource(textures);
        world.insert_resource(DefinitionRegistry::default());
        world.insert_resource(DefinitionLibraryRegistry::default());
        world.insert_resource(NamedViewRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(OpaquePersistedEntities::default());

        let project = build_project_file(&mut world).expect("project should serialize");
        let textures = project
            .textures
            .expect("referenced textures should persist");
        assert!(textures.get(&referenced_texture_id).is_some());
        assert!(textures.get(&unreferenced_texture_id).is_none());
    }

    #[test]
    fn load_project_promotes_legacy_drawing_metadata_entities() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(bevy::prelude::Assets::<bevy::prelude::Mesh>::default());
        world.insert_resource(MaterialRegistry::default());
        world.insert_resource(DefinitionRegistry::default());
        world.insert_resource(DefinitionLibraryRegistry::default());
        world.insert_resource(NamedViewRegistry::default());
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(OpaquePersistedEntities::default());
        world.insert_resource(History::default());
        world.insert_resource(PendingCommandQueue::default());
        world.insert_resource(PropertyEditState::default());
        world.insert_resource(TransformState::default());
        world.insert_resource(State::new(ActiveTool::Select));
        world.insert_resource(NextState::<ActiveTool>::default());

        let project = ProjectFile {
            version: PROJECT_FILE_VERSION,
            next_element_id: 100,
            document_properties: Some(DocumentProperties::default()),
            layers: None,
            materials: None,
            textures: None,
            definitions: None,
            definition_libraries: None,
            named_views: None,
            lighting: Some(SceneLightingSettings::default()),
            sources: None,
            nominations: None,
            entities: vec![
                PersistedEntityRecord {
                    type_name: "dimension_line".to_string(),
                    data: serde_json::json!({
                        "start": [0.0, 0.0, 0.0],
                        "end": [2.0, 0.0, 0.0],
                        "line_point": [1.0, 0.0, -0.5],
                        "visible": true
                    }),
                },
                PersistedEntityRecord {
                    type_name: "clip_plane".to_string(),
                    data: serde_json::json!({
                        "name": "Section",
                        "origin": [0.0, 2.0, 0.0],
                        "normal": [0.0, 1.0, 0.0],
                        "active": true
                    }),
                },
            ],
        };

        load_project(&mut world, project).expect("project should load");

        let doc_props = world.resource::<DocumentProperties>();
        assert!(doc_props
            .domain_defaults
            .contains_key(crate::plugins::dimension_line::DIMENSION_ANNOTATIONS_KEY));
        assert!(doc_props
            .domain_defaults
            .contains_key(crate::plugins::clipping_planes::SECTION_VIEW_METADATA_KEY));
    }

    /// PP-A2DB-1 slice B3c: a project containing the post-commit
    /// state of a flat-assembly Make Reusable round-trips through
    /// save/load. Verifies that:
    ///
    /// - the surviving SemanticAssembly preserves its `realization`
    ///   member shape on reload;
    /// - the agreement's metadata block (promoted_definition_id,
    ///   promoted_occurrence_id, source_member_snapshot_ref,
    ///   source_relation_snapshot_ref) round-trips intact;
    /// - the new Occurrence at the preserved ElementId reloads with
    ///   the right OccurrenceIdentity;
    /// - the promoted Definition (as registered in
    ///   `DefinitionRegistry`) round-trips.
    ///
    /// The test assembles the post-commit state by running the full
    /// PP-A2DB-1 pipeline directly: gather → adapter →
    /// commit_assembly_promotion. The Definition is registered into
    /// `DefinitionRegistry` directly (slice B3b will route this
    /// through `publish_draft` once the MCP entry point lands).
    #[test]
    fn assembly_promotion_post_commit_state_round_trips_through_save_load() {
        use crate::capability_registry::CapabilityRegistry;
        use crate::plugins::modeling::assembly::{
            AssemblyFactory, AssemblyMemberRef, RelationFactory, SemanticAssembly,
        };
        use crate::plugins::modeling::definition::{ChildSlotDef, CompoundDefinition};
        use crate::plugins::promotion::{PromotionOutputShape, SemanticAssemblyPromotionSource};
        use crate::plugins::promotion_world::{
            apply_assembly_migration_diff, commit_assembly_promotion,
            gather_semantic_assembly_input, spawn_promoted_occurrence, AssemblyCommitConfig,
            AssemblyPromotionMetadata,
        };

        // === Build the source world ===========================================
        let mut source = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(AssemblyFactory);
        registry.register_factory(RelationFactory);
        registry.register_factory(OccurrenceFactory);
        source.insert_resource(registry);
        source.insert_resource(DocumentProperties::default());
        source.insert_resource(LayerRegistry::default());
        source.insert_resource(MaterialRegistry::default());
        source.insert_resource(TextureRegistry::default());
        source.insert_resource(DefinitionRegistry::default());
        source.insert_resource(DefinitionLibraryRegistry::default());
        source.insert_resource(NamedViewRegistry::default());
        source.insert_resource(ElementIdAllocator::default());
        source.insert_resource(OpaquePersistedEntities::default());
        // Bump the allocator past the test ids we hand-pick to avoid
        // colliding with the new occurrence id when allocating.
        source.resource_mut::<ElementIdAllocator>().set_next(900);

        // Two leaf members and a flat assembly grouping them.
        let wall_a = ElementId(10);
        let wall_b = ElementId(11);
        let assembly_id = ElementId(1);
        source.spawn(wall_a);
        source.spawn(wall_b);
        source.spawn((
            assembly_id,
            SemanticAssembly {
                assembly_type: "house".into(),
                label: "Test House".into(),
                members: vec![
                    AssemblyMemberRef {
                        target: wall_a,
                        role: "wall".into(),
                    },
                    AssemblyMemberRef {
                        target: wall_b,
                        role: "wall".into(),
                    },
                ],
                parameters: serde_json::Value::Null,
                metadata: serde_json::Value::Null,
            },
        ));

        // === Run the full PP-A2DB-1 pipeline =================================
        let input = gather_semantic_assembly_input(&source, assembly_id).unwrap();
        let adapter = SemanticAssemblyPromotionSource {
            name: "House".into(),
            replace_source: true,
            provenance: Default::default(),
        };
        let out = adapter.build_plan_and_diff(input).unwrap();

        // Register a published Definition in the registry so the
        // round-trip exercises the registry persistence path. The
        // body mirrors the plan's compound shape.
        let promoted_definition_id = DefinitionId(format!("test.promoted.{}", out.plan.draft_id.0));
        let mut promoted_def = crate::plugins::definition_authoring::blank_definition("House");
        promoted_def.id = promoted_definition_id.clone();
        if let PromotionOutputShape::Compound { child_slots } = &out.plan.output_shape {
            promoted_def.compound = Some(CompoundDefinition {
                child_slots: child_slots.iter().cloned().collect::<Vec<ChildSlotDef>>(),
                ..Default::default()
            });
        }
        promoted_def.definition_version = 1;
        source
            .resource_mut::<DefinitionRegistry>()
            .insert(promoted_def.clone());

        // Apply the world commit cycle, preserving wall_a's id as the
        // new Occurrence id.
        let committed = commit_assembly_promotion(
            &mut source,
            &out.migration_diff,
            AssemblyCommitConfig {
                source_assembly_id: assembly_id,
                source_member_ids: vec![wall_a, wall_b],
                promoted_definition_id: promoted_definition_id.clone(),
                promoted_definition_version: 1,
                preserved_element_id: Some(wall_a),
                source_member_snapshot_ref: Some("snap-mem-1".into()),
                source_relation_snapshot_ref: Some("snap-rel-1".into()),
            },
        )
        .expect("commit succeeds");
        assert_eq!(committed.new_occurrence_id, wall_a);

        // === Save and reload =================================================
        let project = build_project_file(&mut source).expect("project should serialize");
        let json = serde_json::to_string(&project).expect("project should serialize to JSON");
        let project: ProjectFile =
            serde_json::from_str(&json).expect("project should deserialize from JSON");

        let mut target = World::new();
        let mut target_registry = CapabilityRegistry::default();
        target_registry.register_factory(AssemblyFactory);
        target_registry.register_factory(RelationFactory);
        target_registry.register_factory(OccurrenceFactory);
        target.insert_resource(target_registry);
        target.insert_resource(bevy::prelude::Assets::<bevy::prelude::Mesh>::default());
        target.insert_resource(MaterialRegistry::default());
        target.insert_resource(DefinitionRegistry::default());
        target.insert_resource(DefinitionLibraryRegistry::default());
        target.insert_resource(NamedViewRegistry::default());
        target.insert_resource(ElementIdAllocator::default());
        target.insert_resource(OpaquePersistedEntities::default());
        target.insert_resource(History::default());
        target.insert_resource(PendingCommandQueue::default());
        target.insert_resource(PropertyEditState::default());
        target.insert_resource(TransformState::default());
        target.insert_resource(State::new(ActiveTool::Select));
        target.insert_resource(NextState::<ActiveTool>::default());

        load_project(&mut target, project).expect("project should load");

        // === Verify the surviving SemanticAssembly ===========================
        let mut q = target.query::<(&ElementId, &SemanticAssembly)>();
        let (_, survivor) = q
            .iter(&target)
            .find(|(eid, _)| **eid == assembly_id)
            .expect("source assembly survives reload");
        assert_eq!(survivor.members.len(), 1);
        assert_eq!(survivor.members[0].target, wall_a);
        assert_eq!(survivor.members[0].role, "realization");

        let metadata = survivor
            .metadata
            .as_object()
            .expect("metadata block survives reload as JSON object");
        assert_eq!(
            metadata["promoted_definition_id"],
            serde_json::Value::String(promoted_definition_id.0.clone())
        );
        assert_eq!(
            metadata["promoted_occurrence_id"]
                .get("0")
                .and_then(Value::as_u64)
                .or_else(|| metadata["promoted_occurrence_id"].as_u64()),
            Some(wall_a.0),
            "promoted_occurrence_id round-trips with the preserved ElementId",
        );
        assert_eq!(
            metadata["source_member_snapshot_ref"],
            serde_json::Value::String("snap-mem-1".into()),
        );
        assert_eq!(
            metadata["source_relation_snapshot_ref"],
            serde_json::Value::String("snap-rel-1".into()),
        );

        // === Verify the new Occurrence reload ================================
        let mut occ_q = target.query::<(&ElementId, &OccurrenceIdentity)>();
        let (_, occurrence) = occ_q
            .iter(&target)
            .find(|(eid, _)| **eid == wall_a)
            .expect("new occurrence reloads at the preserved id");
        assert_eq!(occurrence.definition_id, promoted_definition_id);
        assert_eq!(occurrence.definition_version, 1);

        // === Verify the promoted Definition reload ===========================
        let registry = target.resource::<DefinitionRegistry>();
        let restored_def = registry
            .get(&promoted_definition_id)
            .expect("promoted definition reloads in DefinitionRegistry");
        assert_eq!(restored_def.id, promoted_definition_id);
        assert_eq!(restored_def.name, "House");
        let compound = restored_def
            .compound
            .as_ref()
            .expect("compound body reloads on the promoted definition");
        // The flat assembly had 2 wall members → indexed slots
        // wall_1, wall_2 in the plan → same slot ids on the
        // round-tripped Definition's compound body.
        assert_eq!(compound.child_slots.len(), 2);
        let slot_ids: Vec<&str> = compound
            .child_slots
            .iter()
            .map(|s| s.slot_id.as_str())
            .collect();
        assert_eq!(slot_ids, vec!["wall_1", "wall_2"]);

        // === Sanity: the despawned non-preserved member is gone ==============
        let mut id_q = target.query::<&ElementId>();
        let count = id_q.iter(&target).filter(|eid| **eid == wall_b).count();
        assert_eq!(
            count, 0,
            "wall_b was despawned during commit; reload preserves the absence"
        );

        // Suppress unused-import warning for helpers used only inside
        // the `commit_assembly_promotion` orchestrator path; keep them
        // imported so the test reads as a single-stop reference.
        let _ = (apply_assembly_migration_diff, spawn_promoted_occurrence);
        let _ = AssemblyPromotionMetadata {
            promoted_definition_id: promoted_definition_id.clone(),
            promoted_occurrence_id: wall_a,
            source_member_snapshot_ref: None,
            source_relation_snapshot_ref: None,
        };
    }

    /// PP-A2DB-2 slice C4a: a project containing the post-commit AND
    /// post-materialize state of a flat-assembly Make Reusable
    /// promotion that includes both internal relation templates and
    /// boundary-spanning external relations round-trips through
    /// save/load. Verifies all the slice A/B/C1/C3 surfaces survive
    /// together:
    ///
    /// - Internal `SemanticRelation` between two source members
    ///   becomes a `SemanticRelationTemplate` on the promoted
    ///   `CompoundDefinition.relation_templates`.
    /// - Boundary-spanning `SemanticRelation` from a source member
    ///   to an outsider classifies as `RequiredContext` and lands on
    ///   `Definition.interface.external_context_requirements`.
    /// - Materialized authored `SemanticRelation` (spawned by
    ///   `materialize_relation_templates`) survives reload with both
    ///   endpoints intact.
    /// - Surviving SemanticAssembly + metadata block + new Occurrence
    ///   (preserved id) all behave per PP-A2DB-1.
    #[test]
    fn relation_templates_and_external_context_requirements_round_trip_through_save_load() {
        use crate::capability_registry::CapabilityRegistry;
        use crate::plugins::modeling::assembly::{
            AssemblyFactory, AssemblyMemberRef, RelationFactory, SemanticAssembly, SemanticRelation,
        };
        use crate::plugins::modeling::definition::CompoundDefinition;
        use crate::plugins::promotion::{
            ExternalRelationClassification, RelationClassificationRules,
            SemanticAssemblyPromotionSource,
        };
        use crate::plugins::promotion_world::{
            apply_assembly_migration_diff, commit_assembly_promotion,
            gather_semantic_assembly_input, materialize_relation_templates,
            spawn_promoted_occurrence, AssemblyCommitConfig, AssemblyPromotionMetadata,
        };
        use std::collections::HashMap;

        // === Build the source world ===========================================
        let mut source = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(AssemblyFactory);
        registry.register_factory(RelationFactory);
        registry.register_factory(OccurrenceFactory);
        source.insert_resource(registry);
        source.insert_resource(DocumentProperties::default());
        source.insert_resource(LayerRegistry::default());
        source.insert_resource(MaterialRegistry::default());
        source.insert_resource(TextureRegistry::default());
        source.insert_resource(DefinitionRegistry::default());
        source.insert_resource(DefinitionLibraryRegistry::default());
        source.insert_resource(NamedViewRegistry::default());
        source.insert_resource(ElementIdAllocator::default());
        source.insert_resource(OpaquePersistedEntities::default());
        source.resource_mut::<ElementIdAllocator>().set_next(900);

        // Two leaf members, one outsider (for the boundary-spanning
        // relation), one assembly grouping the two leaves.
        let frame_id = ElementId(10);
        let leaf_id = ElementId(11);
        let outsider_id = ElementId(20);
        let assembly_id = ElementId(1);
        source.spawn(frame_id);
        source.spawn(leaf_id);
        source.spawn(outsider_id);
        source.spawn((
            assembly_id,
            SemanticAssembly {
                assembly_type: "door".into(),
                label: "Front Door".into(),
                members: vec![
                    AssemblyMemberRef {
                        target: frame_id,
                        role: "frame".into(),
                    },
                    AssemblyMemberRef {
                        target: leaf_id,
                        role: "leaf".into(),
                    },
                ],
                parameters: serde_json::Value::Null,
                metadata: serde_json::Value::Null,
            },
        ));
        // Internal relation: frame <-> leaf (both endpoints inside source).
        source.spawn((
            ElementId(30),
            SemanticRelation {
                source: frame_id,
                target: leaf_id,
                relation_type: "hinges_on".into(),
                parameters: serde_json::json!({ "hinge_count": 3 }),
            },
        ));
        // Boundary-spanning relation: frame -> outsider.
        source.spawn((
            ElementId(31),
            SemanticRelation {
                source: frame_id,
                target: outsider_id,
                relation_type: "needs_room".into(),
                parameters: serde_json::Value::Null,
            },
        ));

        // === Run the full PP-A2DB-2 pipeline ==================================
        let mut input = gather_semantic_assembly_input(&source, assembly_id).unwrap();
        // Slice B: provide rules so `needs_room` classifies as
        // RequiredContext. Slice C4 will populate this from the
        // capability registry; here we set it explicitly.
        let mut rules = HashMap::new();
        rules.insert(
            "needs_room".into(),
            ExternalRelationClassification::RequiredContext,
        );
        input.relation_classification = RelationClassificationRules {
            by_descriptor: rules,
            default_unknown: None,
        };

        let adapter = SemanticAssemblyPromotionSource {
            name: "Door".into(),
            replace_source: true,
            provenance: Default::default(),
        };
        let out = adapter.build_plan_and_diff(input).unwrap();

        // Spot-check the diff before we commit.
        assert_eq!(out.migration_diff.candidate_relation_templates.len(), 1);
        assert_eq!(out.plan.external_context_requirements.len(), 1);

        // Body builder stamps the slice A templates AND slice B
        // requirements onto the Definition. Slice C1's
        // `Interface::with_external_context_requirements` and slice
        // C3's `Definition::with_relation_templates` make this a
        // single-line operation per surface.
        let promoted_definition_id = DefinitionId(format!("test.promoted.{}", out.plan.draft_id.0));
        let mut promoted_def = crate::plugins::definition_authoring::blank_definition("Door");
        promoted_def.id = promoted_definition_id.clone();
        promoted_def.interface = promoted_def
            .interface
            .with_external_context_requirements(out.plan.external_context_requirements.clone());
        if let crate::plugins::promotion::PromotionOutputShape::Compound { child_slots } =
            &out.plan.output_shape
        {
            promoted_def.compound = Some(CompoundDefinition {
                child_slots: child_slots.clone(),
                ..Default::default()
            });
        }
        promoted_def = promoted_def
            .with_relation_templates(out.migration_diff.candidate_relation_templates.clone());
        promoted_def.definition_version = 1;
        source
            .resource_mut::<DefinitionRegistry>()
            .insert(promoted_def.clone());

        // Commit the assembly migration.
        let committed = commit_assembly_promotion(
            &mut source,
            &out.migration_diff,
            AssemblyCommitConfig {
                source_assembly_id: assembly_id,
                source_member_ids: vec![frame_id, leaf_id],
                promoted_definition_id: promoted_definition_id.clone(),
                promoted_definition_version: 1,
                preserved_element_id: Some(frame_id),
                source_member_snapshot_ref: Some("snap-mem".into()),
                source_relation_snapshot_ref: Some("snap-rel".into()),
            },
        )
        .expect("commit succeeds");
        assert_eq!(committed.new_occurrence_id, frame_id);

        // Slice C3: materialize accepted templates as authored
        // SemanticRelation entities. The realization map binds slot
        // ids to the live ECS ElementIds. Since both slot members
        // were despawned (the preserved one became the Occurrence;
        // the other was despawned), `frame` and `leaf` both resolve
        // to the new Occurrence id for this minimal test. Slice C4
        // will plumb a richer realization map; here we just exercise
        // that materialization spawns the right number of relations
        // and they round-trip.
        let mut realizations = HashMap::new();
        realizations.insert("frame".into(), committed.new_occurrence_id);
        realizations.insert("leaf".into(), committed.new_occurrence_id);
        let materialized = materialize_relation_templates(
            &mut source,
            &promoted_def,
            committed.new_occurrence_id,
            &realizations,
        )
        .expect("materialize succeeds");
        assert_eq!(materialized, 1, "one template -> one spawned relation");

        // === Save and reload =================================================
        let project = build_project_file(&mut source).expect("project should serialize");
        let json = serde_json::to_string(&project).expect("project should serialize to JSON");
        let project: ProjectFile =
            serde_json::from_str(&json).expect("project should deserialize from JSON");

        let mut target = World::new();
        let mut target_registry = CapabilityRegistry::default();
        target_registry.register_factory(AssemblyFactory);
        target_registry.register_factory(RelationFactory);
        target_registry.register_factory(OccurrenceFactory);
        target.insert_resource(target_registry);
        target.insert_resource(bevy::prelude::Assets::<bevy::prelude::Mesh>::default());
        target.insert_resource(MaterialRegistry::default());
        target.insert_resource(DefinitionRegistry::default());
        target.insert_resource(DefinitionLibraryRegistry::default());
        target.insert_resource(NamedViewRegistry::default());
        target.insert_resource(ElementIdAllocator::default());
        target.insert_resource(OpaquePersistedEntities::default());
        target.insert_resource(History::default());
        target.insert_resource(PendingCommandQueue::default());
        target.insert_resource(PropertyEditState::default());
        target.insert_resource(TransformState::default());
        target.insert_resource(State::new(ActiveTool::Select));
        target.insert_resource(NextState::<ActiveTool>::default());

        load_project(&mut target, project).expect("project should load");

        // === Verify the surviving SemanticAssembly ===========================
        let mut q = target.query::<(&ElementId, &SemanticAssembly)>();
        let (_, survivor) = q
            .iter(&target)
            .find(|(eid, _)| **eid == assembly_id)
            .expect("source assembly survives reload");
        assert_eq!(survivor.members.len(), 1);
        assert_eq!(survivor.members[0].role, "realization");

        // === Verify the new Occurrence reload ================================
        let mut occ_q = target.query::<(&ElementId, &OccurrenceIdentity)>();
        let (_, occurrence) = occ_q
            .iter(&target)
            .find(|(eid, _)| **eid == frame_id)
            .expect("new occurrence reloads at the preserved id");
        assert_eq!(occurrence.definition_id, promoted_definition_id);

        // === Verify the promoted Definition reload ===========================
        let registry = target.resource::<DefinitionRegistry>();
        let restored_def = registry
            .get(&promoted_definition_id)
            .expect("promoted definition reloads in DefinitionRegistry");
        // Slice C1: external context requirements survive on the
        // Interface.
        assert_eq!(
            restored_def.interface.external_context_requirements.len(),
            1
        );
        let req = &restored_def.interface.external_context_requirements[0];
        assert_eq!(req.relation_type, "needs_room");
        assert_eq!(
            req.classification,
            ExternalRelationClassification::RequiredContext
        );
        // Slice C3: relation templates survive on the compound body.
        let compound = restored_def
            .compound
            .as_ref()
            .expect("compound body reloads");
        assert_eq!(compound.relation_templates.len(), 1);
        let template = &compound.relation_templates[0];
        assert_eq!(template.relation_type, "hinges_on");

        // === Verify the materialized SemanticRelation reload =================
        let mut rel_q = target.query::<&SemanticRelation>();
        let materialized: Vec<&SemanticRelation> = rel_q
            .iter(&target)
            .filter(|r| r.relation_type == "hinges_on")
            .collect();
        assert!(
            !materialized.is_empty(),
            "materialized hinges_on SemanticRelation survives reload"
        );

        // Sanity: silence unused-import warnings for helpers used
        // only inside `commit_assembly_promotion`.
        let _ = (apply_assembly_migration_diff, spawn_promoted_occurrence);
        let _ = AssemblyPromotionMetadata {
            promoted_definition_id: promoted_definition_id.clone(),
            promoted_occurrence_id: frame_id,
            source_member_snapshot_ref: None,
            source_relation_snapshot_ref: None,
        };
    }
}

fn primary_shortcut_state(world: &mut World, key: KeyCode) -> (bool, bool) {
    let keys = world.resource::<ButtonInput<KeyCode>>();
    let primary_modifier_pressed = if cfg!(target_os = "macos") {
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
    } else {
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    };
    let shift_pressed = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    (
        primary_modifier_pressed && keys.just_pressed(key),
        shift_pressed,
    )
}

fn set_feedback(world: &mut World, message: String) {
    let mut status_bar_data = world.resource_mut::<StatusBarData>();
    status_bar_data.set_feedback(message, FEEDBACK_DURATION_SECONDS);
}
