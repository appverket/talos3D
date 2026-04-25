//! Optional storage-backed cache for session draft registries.
//!
//! This cache is intentionally non-authoritative. It lets standalone desktop
//! builds warm-start consultable session drafts from local storage while
//! keeping the runtime model independent of any backend assumptions.

use std::{
    env,
    path::{Path, PathBuf},
};

use bevy::{log::warn, prelude::*};
use serde::{Deserialize, Serialize};

use crate::plugins::{
    assembly_pattern_drafts::{AssemblyPatternDraftArtifact, AssemblyPatternDraftRegistry},
    recipe_drafts::{RecipeDraftArtifact, RecipeDraftRegistry},
    storage::Storage,
};

#[derive(Resource, Debug, Clone)]
pub struct SessionDraftCacheSettings {
    pub storage_key: Option<String>,
}

impl Default for SessionDraftCacheSettings {
    fn default() -> Self {
        Self {
            storage_key: default_session_draft_cache_key(),
        }
    }
}

#[derive(Resource, Debug, Default, Clone)]
struct SessionDraftCacheState {
    loaded: bool,
    skip_next_sync: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionDraftCacheFile {
    version: u32,
    #[serde(default)]
    recipe_drafts: Vec<RecipeDraftArtifact>,
    #[serde(default)]
    assembly_pattern_drafts: Vec<AssemblyPatternDraftArtifact>,
}

pub struct SessionDraftCachePlugin;

impl Plugin for SessionDraftCachePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RecipeDraftRegistry>()
            .init_resource::<AssemblyPatternDraftRegistry>()
            .init_resource::<SessionDraftCacheSettings>()
            .init_resource::<SessionDraftCacheState>()
            .add_systems(Startup, load_session_draft_cache)
            .add_systems(Update, sync_session_draft_cache);
    }
}

fn load_session_draft_cache(world: &mut World) {
    let storage_key = world
        .get_resource::<SessionDraftCacheSettings>()
        .and_then(|settings| settings.storage_key.clone());
    let Some(storage_key) = storage_key else {
        if let Some(mut state) = world.get_resource_mut::<SessionDraftCacheState>() {
            state.loaded = true;
        }
        return;
    };

    let Some(storage) = world.get_resource::<Storage>() else {
        if let Some(mut state) = world.get_resource_mut::<SessionDraftCacheState>() {
            state.loaded = true;
        }
        return;
    };

    let exists = storage.0.exists(&storage_key).unwrap_or(false);
    if !exists {
        if let Some(mut state) = world.get_resource_mut::<SessionDraftCacheState>() {
            state.loaded = true;
        }
        return;
    }

    match storage.0.load(&storage_key) {
        Ok(bytes) => match serde_json::from_slice::<SessionDraftCacheFile>(&bytes) {
            Ok(cache) => {
                world.init_resource::<RecipeDraftRegistry>();
                world.init_resource::<AssemblyPatternDraftRegistry>();
                world
                    .resource_mut::<RecipeDraftRegistry>()
                    .restore(cache.recipe_drafts);
                world
                    .resource_mut::<AssemblyPatternDraftRegistry>()
                    .restore(cache.assembly_pattern_drafts);
                if let Some(mut state) = world.get_resource_mut::<SessionDraftCacheState>() {
                    state.loaded = true;
                    state.skip_next_sync = true;
                }
            }
            Err(error) => {
                warn!(
                    "failed to parse session draft cache '{}': {error}",
                    storage_key
                );
                if let Some(mut state) = world.get_resource_mut::<SessionDraftCacheState>() {
                    state.loaded = true;
                }
            }
        },
        Err(error) => {
            warn!(
                "failed to load session draft cache '{}': {error}",
                storage_key
            );
            if let Some(mut state) = world.get_resource_mut::<SessionDraftCacheState>() {
                state.loaded = true;
            }
        }
    }
}

fn sync_session_draft_cache(
    recipe_registry: Option<Res<RecipeDraftRegistry>>,
    pattern_registry: Option<Res<AssemblyPatternDraftRegistry>>,
    storage: Option<Res<Storage>>,
    settings: Res<SessionDraftCacheSettings>,
    mut state: ResMut<SessionDraftCacheState>,
) {
    if !state.loaded {
        return;
    }

    let recipe_changed = recipe_registry
        .as_ref()
        .is_some_and(|resource| resource.is_changed());
    let pattern_changed = pattern_registry
        .as_ref()
        .is_some_and(|resource| resource.is_changed());
    if !recipe_changed && !pattern_changed {
        return;
    }

    if state.skip_next_sync {
        state.skip_next_sync = false;
        return;
    }

    let Some(storage_key) = settings.storage_key.as_deref() else {
        return;
    };
    let Some(storage) = storage else {
        return;
    };

    if Path::new(storage_key).is_absolute() {
        if let Some(parent) = Path::new(storage_key).parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                warn!(
                    "failed to create session draft cache directory '{}': {error}",
                    parent.display()
                );
                return;
            }
        }
    }

    let cache = SessionDraftCacheFile {
        version: 1,
        recipe_drafts: recipe_registry
            .as_ref()
            .map(|resource| resource.snapshot())
            .unwrap_or_default(),
        assembly_pattern_drafts: pattern_registry
            .as_ref()
            .map(|resource| resource.snapshot())
            .unwrap_or_default(),
    };

    match serde_json::to_vec_pretty(&cache) {
        Ok(bytes) => {
            if let Err(error) = storage.0.save(&bytes, storage_key) {
                warn!(
                    "failed to save session draft cache '{}': {error}",
                    storage_key
                );
            }
        }
        Err(error) => {
            warn!(
                "failed to serialize session draft cache '{}': {error}",
                storage_key
            );
        }
    }
}

fn default_session_draft_cache_key() -> Option<String> {
    if let Ok(explicit) = env::var("TALOS3D_SESSION_DRAFT_CACHE_KEY") {
        if !explicit.trim().is_empty() {
            return Some(explicit);
        }
    }

    let instance = env::var("TALOS3D_INSTANCE_ID").unwrap_or_else(|_| "default".to_string());
    let base = default_session_cache_base_dir()?;
    Some(
        base.join(format!("session-drafts-{instance}.json"))
            .to_string_lossy()
            .to_string(),
    )
}

fn default_session_cache_base_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("Talos3D"))
    }

    #[cfg(target_os = "macos")]
    {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join("Library/Application Support/Talos3D"))
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Some(path) = env::var_os("XDG_STATE_HOME") {
            return Some(PathBuf::from(path).join("talos3d"));
        }
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join(".local/state/talos3d"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        assembly_pattern_drafts::{AssemblyPatternDraftArtifact, AssemblyPatternDraftStatus},
        recipe_drafts::{RecipeDraftArtifact, RecipeDraftParameter, RecipeDraftStatus},
        storage::{LocalFileBackend, Storage},
    };

    fn temp_cache_path(name: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir()
            .join(format!("{name}-{nanos}.json"))
            .to_string_lossy()
            .to_string()
    }

    #[test]
    fn session_draft_cache_round_trips_recipe_and_pattern_drafts() {
        let cache_path = temp_cache_path("talos3d-session-drafts");

        let mut save_app = App::new();
        save_app.insert_resource(Storage(Box::new(LocalFileBackend)));
        save_app.insert_resource(SessionDraftCacheSettings {
            storage_key: Some(cache_path.clone()),
        });
        save_app.add_plugins(SessionDraftCachePlugin);
        save_app.update();

        save_app
            .world_mut()
            .resource_mut::<RecipeDraftRegistry>()
            .save(RecipeDraftArtifact {
                id: String::new(),
                label: "Roof Recipe".into(),
                description: "cached recipe draft".into(),
                target_class: "roof_system".into(),
                supported_refinement_levels: vec!["Constructible".into()],
                parameters: vec![RecipeDraftParameter {
                    name: "span_mm".into(),
                    value_schema: serde_json::json!({ "type": "number" }),
                    default: Some(serde_json::json!(6000)),
                }],
                jurisdiction: Some("SE".into()),
                gap_id: None,
                source_passage_refs: vec!["SE/mono_truss".into()],
                acquisition_context: serde_json::json!({ "source": "test" }),
                draft_script: serde_json::json!({ "steps": [] }),
                notes: vec!["draft".into()],
                status: RecipeDraftStatus::Installed,
                created_at: 0,
                updated_at: 0,
            });
        save_app
            .world_mut()
            .resource_mut::<AssemblyPatternDraftRegistry>()
            .save(AssemblyPatternDraftArtifact {
                id: String::new(),
                label: "Wall Pattern".into(),
                description: "cached pattern draft".into(),
                target_types: vec!["wall_assembly".into()],
                axis: "exterior_to_interior".into(),
                layers: Vec::new(),
                relation_rules: Vec::new(),
                root_layer_ids: vec!["stud_frame".into()],
                requires_support_path: true,
                tags: vec!["wall".into()],
                parameter_schema: serde_json::json!({}),
                jurisdiction: Some("SE".into()),
                gap_id: None,
                source_passage_refs: vec!["SE/wall".into()],
                acquisition_context: serde_json::json!({ "source": "test" }),
                notes: vec!["pattern".into()],
                status: AssemblyPatternDraftStatus::Installed,
                created_at: 0,
                updated_at: 0,
            });
        save_app.update();

        let mut load_app = App::new();
        load_app.insert_resource(Storage(Box::new(LocalFileBackend)));
        load_app.insert_resource(SessionDraftCacheSettings {
            storage_key: Some(cache_path.clone()),
        });
        load_app.add_plugins(SessionDraftCachePlugin);
        load_app.update();

        assert_eq!(
            load_app
                .world()
                .resource::<RecipeDraftRegistry>()
                .snapshot()
                .len(),
            1
        );
        assert_eq!(
            load_app
                .world()
                .resource::<AssemblyPatternDraftRegistry>()
                .snapshot()
                .len(),
            1
        );

        let _ = std::fs::remove_file(cache_path);
    }
}
