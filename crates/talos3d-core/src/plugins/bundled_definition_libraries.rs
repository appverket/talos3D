use std::sync::Arc;

use bevy::prelude::*;

use crate::plugins::modeling::definition::{
    DefinitionLibrary, DefinitionLibraryFile, DefinitionLibraryRegistry, DefinitionLibraryScope,
};

pub struct BundledDefinitionLibrariesPlugin;

impl Plugin for BundledDefinitionLibrariesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BundledDefinitionLibraryProviderRegistry>()
            .add_systems(Startup, seed_bundled_definition_libraries);
    }
}

pub type BundledDefinitionLibraryProvider =
    Arc<dyn Fn() -> Result<Vec<DefinitionLibrary>, String> + Send + Sync>;

#[derive(Clone)]
struct RegisteredBundledDefinitionLibraryProvider {
    id: String,
    provider: BundledDefinitionLibraryProvider,
}

/// App-composed product libraries that must be restored after every project
/// load. Bundled libraries are intentionally absent from project persistence.
#[derive(Resource, Clone, Default)]
pub struct BundledDefinitionLibraryProviderRegistry {
    providers: Vec<RegisteredBundledDefinitionLibraryProvider>,
}

impl BundledDefinitionLibraryProviderRegistry {
    pub fn register(&mut self, id: impl Into<String>, provider: BundledDefinitionLibraryProvider) {
        let id = id.into();
        assert!(
            !self.providers.iter().any(|entry| entry.id == id),
            "Bundled DefinitionLibrary provider '{id}' was registered more than once"
        );
        self.providers
            .push(RegisteredBundledDefinitionLibraryProvider { id, provider });
    }

    fn load_all(&self) -> Result<Vec<DefinitionLibrary>, String> {
        let mut libraries = bundled_definition_libraries()?;
        for entry in &self.providers {
            libraries.extend(
                (entry.provider)().map_err(|error| format!("provider '{}': {error}", entry.id))?,
            );
        }
        Ok(libraries)
    }
}

pub trait BundledDefinitionLibraryAppExt {
    fn register_bundled_definition_library_provider<F>(
        &mut self,
        id: impl Into<String>,
        provider: F,
    ) -> &mut Self
    where
        F: Fn() -> Result<Vec<DefinitionLibrary>, String> + Send + Sync + 'static;
}

impl BundledDefinitionLibraryAppExt for App {
    fn register_bundled_definition_library_provider<F>(
        &mut self,
        id: impl Into<String>,
        provider: F,
    ) -> &mut Self
    where
        F: Fn() -> Result<Vec<DefinitionLibrary>, String> + Send + Sync + 'static,
    {
        self.init_resource::<BundledDefinitionLibraryProviderRegistry>();
        self.world_mut()
            .resource_mut::<BundledDefinitionLibraryProviderRegistry>()
            .register(id, Arc::new(provider));
        self
    }
}

fn seed_bundled_definition_libraries(world: &mut World) {
    if let Err(error) = apply_registered_bundled_definition_libraries(world) {
        error!("Failed to load bundled definition libraries: {error}");
    }
}

pub fn apply_registered_bundled_definition_libraries(world: &mut World) -> Result<(), String> {
    let loaded = world
        .get_resource::<BundledDefinitionLibraryProviderRegistry>()
        .cloned()
        .unwrap_or_default()
        .load_all()?;
    let mut libraries = world
        .get_resource_mut::<DefinitionLibraryRegistry>()
        .ok_or_else(|| "DefinitionLibraryRegistry is unavailable".to_string())?;
    apply_definition_libraries(&mut libraries, loaded);
    Ok(())
}

pub fn apply_bundled_definition_libraries(
    libraries: &mut DefinitionLibraryRegistry,
) -> Result<(), String> {
    apply_definition_libraries(libraries, bundled_definition_libraries()?);
    Ok(())
}

fn apply_definition_libraries(
    libraries: &mut DefinitionLibraryRegistry,
    bundled: Vec<DefinitionLibrary>,
) {
    for mut library in bundled {
        if libraries.get(&library.id).is_some() {
            continue;
        }
        // PP-099 / PP-MATREL-1 slice 2: migrate any legacy
        // domain_data.architectural.material_assignment.material_id
        // poke on bundled definitions into the new
        // Definition.material_assignment slot before insert. Idempotent
        // when a bundled JSON has already been rewritten (slice 3) to
        // populate the new field directly.
        let migrated = library.migrate_legacy_material_assignments();
        if !migrated.is_empty() {
            info!(
                "PP-099: migrated {} bundled definition(s) in '{}' from legacy \
                 domain_data material_assignment: {}",
                migrated.len(),
                library.id.0,
                migrated
                    .iter()
                    .map(|id| id.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        libraries.insert(library);
    }
}

fn bundled_definition_libraries() -> Result<Vec<DefinitionLibrary>, String> {
    Ok(Vec::new())
}

#[allow(dead_code)]
fn parse_bundled_definition_library(
    file_name: &str,
    contents: &str,
) -> Result<DefinitionLibrary, String> {
    let mut file: DefinitionLibraryFile =
        serde_json::from_str(contents).map_err(|error| error.to_string())?;
    if file.version != DefinitionLibraryFile::VERSION {
        return Err(format!(
            "Bundled definition library '{file_name}' has unsupported version {} (expected {})",
            file.version,
            DefinitionLibraryFile::VERSION
        ));
    }
    file.library.scope = DefinitionLibraryScope::Bundled;
    file.library.source_path = None;
    Ok(file.library)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::modeling::definition::{DefinitionLibraryId, DefinitionLibraryScope};

    fn test_library() -> DefinitionLibrary {
        DefinitionLibrary {
            id: DefinitionLibraryId("test.capability-products".to_string()),
            name: "Capability Products".to_string(),
            scope: DefinitionLibraryScope::Bundled,
            source_path: None,
            tags: vec!["test".to_string()],
            definitions: default(),
            draft_status: default(),
        }
    }

    #[test]
    fn core_ships_no_domain_bundled_libraries() {
        let libraries = bundled_definition_libraries().expect("bundled libraries should parse");
        assert!(libraries.is_empty());
    }

    #[test]
    fn applying_core_bundled_libraries_is_a_noop() {
        let mut registry = DefinitionLibraryRegistry::default();

        apply_bundled_definition_libraries(&mut registry)
            .expect("reapplying bundled libraries should succeed");

        assert!(registry.list().is_empty());
    }

    #[test]
    fn capability_provider_is_seeded_and_reapplied_after_document_reset() {
        let mut app = App::new();
        app.init_resource::<DefinitionLibraryRegistry>()
            .add_plugins(BundledDefinitionLibrariesPlugin)
            .register_bundled_definition_library_provider("test.products", || {
                Ok(vec![test_library()])
            });

        app.update();
        let id = DefinitionLibraryId("test.capability-products".to_string());
        assert!(app
            .world()
            .resource::<DefinitionLibraryRegistry>()
            .get(&id)
            .is_some());

        app.world_mut()
            .insert_resource(DefinitionLibraryRegistry::default());
        apply_registered_bundled_definition_libraries(app.world_mut())
            .expect("registered provider should survive a document reset");
        assert!(app
            .world()
            .resource::<DefinitionLibraryRegistry>()
            .get(&id)
            .is_some());
    }
}
