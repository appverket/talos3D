use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    capability_registry::{CapabilityRegistry, DefaultsRegistry},
    plugins::{
        bundled_definition_libraries::apply_bundled_definition_libraries,
        commands::snapshot_dependency_order,
        document_properties::DocumentProperties,
        document_state::DocumentState,
        history::{History, PendingCommandQueue},
        identity::{ElementId, ElementIdAllocator},
        layers::LayerRegistry,
        materials::{ensure_builtin_materials, is_builtin_material_id, MaterialRegistry},
        modeling::definition::{DefinitionLibraryRegistry, DefinitionRegistry},
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
    definitions: Option<DefinitionRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    definition_libraries: Option<DefinitionLibraryRegistry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    named_views: Option<NamedViewRegistry>,
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
    world.resource_mut::<ElementIdAllocator>().set_next(1);
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
    let mut query = world.query_filtered::<Entity, With<ElementId>>();
    let entity_ids: Vec<Entity> = query.iter(world).collect();
    let registry = world.resource::<CapabilityRegistry>();

    let mut entities = entity_ids
        .into_iter()
        .filter_map(|entity| world.get_entity(entity).ok())
        .filter_map(|entity_ref| registry.capture_snapshot(&entity_ref, world))
        .map(|snapshot| PersistedEntityRecord {
            type_name: snapshot.type_name().to_string(),
            data: snapshot.to_json(),
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
    for material in world.resource::<MaterialRegistry>().all() {
        if !is_builtin_material_id(&material.id) {
            material_registry.upsert(material.clone());
        }
    }
    let materials = if material_registry.count() > 0 {
        Some(material_registry)
    } else {
        None
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

    Ok(ProjectFile {
        version: PROJECT_FILE_VERSION,
        next_element_id: world.resource::<ElementIdAllocator>().next_value(),
        document_properties: Some(doc_props),
        layers,
        materials,
        definitions,
        definition_libraries,
        named_views,
        entities,
    })
}

fn load_project(world: &mut World, project: ProjectFile) -> Result<(), String> {
    if project.version != PROJECT_FILE_VERSION {
        return Err(format!(
            "Unsupported project version {} (expected {})",
            project.version, PROJECT_FILE_VERSION
        ));
    }

    clear_scene(world);

    let doc_props = match project.document_properties {
        Some(props) => props,
        None => {
            let mut props = DocumentProperties::default();
            if let Some(defaults_registry) = world.get_resource::<DefaultsRegistry>() {
                defaults_registry.apply_all(&mut props);
            }
            props
        }
    };
    world.insert_resource(doc_props);
    world.insert_resource(project.layers.unwrap_or_default());
    world.insert_resource(project.materials.unwrap_or_default());
    if let Some(mut materials) = world.get_resource_mut::<MaterialRegistry>() {
        ensure_builtin_materials(&mut materials);
    }
    world.insert_resource(project.definitions.unwrap_or_default());
    world.insert_resource(project.definition_libraries.unwrap_or_default());
    if let Some(mut libraries) = world.get_resource_mut::<DefinitionLibraryRegistry>() {
        if let Err(error) = apply_bundled_definition_libraries(&mut libraries) {
            error!("Failed to restore bundled definition libraries after project load: {error}");
        }
    }
    world.insert_resource(project.named_views.unwrap_or_default());

    let registry = world.resource::<CapabilityRegistry>();
    let mut recognized = Vec::new();
    let mut opaque = Vec::new();

    for record in project.entities {
        if let Some(factory) = registry.factory_for(&record.type_name) {
            let snapshot = factory.from_persisted_json(&record.data)?;
            recognized.push(snapshot);
        } else {
            opaque.push(record);
        }
    }

    recognized.sort_by_key(snapshot_dependency_order);
    for snapshot in recognized {
        snapshot.apply_to(world);
    }

    world.insert_resource(OpaquePersistedEntities(opaque));
    world
        .resource_mut::<ElementIdAllocator>()
        .set_next(project.next_element_id);
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
        .as_object()
        .and_then(|obj| {
            obj.values()
                .find_map(|v| v.get("element_id").and_then(Value::as_u64))
        })
        .unwrap_or(u64::MAX);
    (type_order, element_id)
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
        layers::LayerRegistry,
        materials::{
            ensure_builtin_materials, BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80,
            BUILTIN_MATERIAL_MAIBEC_RED_CEDAR_LIGHT_H2BO,
        },
        modeling::definition::{DefinitionLibrary, DefinitionLibraryId, DefinitionLibraryScope},
    };

    #[test]
    fn build_project_file_omits_bundled_definition_libraries() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(DocumentProperties::default());
        world.insert_resource(LayerRegistry::default());
        world.insert_resource(MaterialRegistry::default());
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
    fn build_project_file_omits_bundled_materials() {
        let mut world = World::new();
        world.insert_resource(CapabilityRegistry::default());
        world.insert_resource(DocumentProperties::default());
        world.insert_resource(LayerRegistry::default());
        let mut materials = MaterialRegistry::default();
        ensure_builtin_materials(&mut materials);
        materials.create("Project Material");
        world.insert_resource(materials);
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
