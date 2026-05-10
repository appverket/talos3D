use bevy::prelude::*;

use crate::plugins::modeling::definition::{
    DefinitionLibrary, DefinitionLibraryFile, DefinitionLibraryRegistry, DefinitionLibraryScope,
};

pub struct BundledDefinitionLibrariesPlugin;

impl Plugin for BundledDefinitionLibrariesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, seed_bundled_definition_libraries);
    }
}

fn seed_bundled_definition_libraries(mut libraries: ResMut<DefinitionLibraryRegistry>) {
    if let Err(error) = apply_bundled_definition_libraries(&mut libraries) {
        error!("Failed to load bundled definition libraries: {error}");
    }
}

pub fn apply_bundled_definition_libraries(
    libraries: &mut DefinitionLibraryRegistry,
) -> Result<(), String> {
    for mut library in bundled_definition_libraries()? {
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
    Ok(())
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
}
