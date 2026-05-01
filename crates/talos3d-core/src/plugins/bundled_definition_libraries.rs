use bevy::prelude::*;

use crate::plugins::modeling::definition::{
    DefinitionLibrary, DefinitionLibraryFile, DefinitionLibraryRegistry, DefinitionLibraryScope,
};

pub struct BundledDefinitionLibrariesPlugin;

const ARCHITECTURE_DOUBLE_WINDOW_LIBRARY_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/definition_libraries/architecture_double_window.json"
));

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
    Ok(vec![parse_bundled_definition_library(
        "architecture_double_window.json",
        ARCHITECTURE_DOUBLE_WINDOW_LIBRARY_JSON,
    )?])
}

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
    use crate::plugins::modeling::definition::{DefinitionId, DefinitionLibraryId};

    #[test]
    fn bundled_libraries_use_bundled_scope() {
        let libraries = bundled_definition_libraries().expect("bundled libraries should parse");
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].scope, DefinitionLibraryScope::Bundled);
        assert_eq!(
            libraries[0].id,
            DefinitionLibraryId("architecture.european-window-library".to_string())
        );
        assert!(libraries[0].definitions.contains_key(
            &crate::plugins::modeling::definition::DefinitionId(
                "architecture.window.double-european".to_string(),
            )
        ));
        assert!(libraries[0].definitions.contains_key(
            &crate::plugins::modeling::definition::DefinitionId(
                "architecture.window.european-single".to_string(),
            )
        ));
    }

    #[test]
    fn bundled_libraries_do_not_override_existing_entries() {
        let mut registry = DefinitionLibraryRegistry::default();
        let mut library = bundled_definition_libraries()
            .expect("bundled libraries should parse")
            .into_iter()
            .next()
            .expect("one bundled library");
        library.name = "Custom Override".to_string();
        registry.insert(library);

        apply_bundled_definition_libraries(&mut registry)
            .expect("reapplying bundled libraries should succeed");

        assert_eq!(
            registry
                .get(&DefinitionLibraryId(
                    "architecture.european-window-library".to_string()
                ))
                .expect("library should exist")
                .name,
            "Custom Override"
        );
    }

    #[test]
    fn bundled_window_material_parameters_are_not_geometry_affecting() {
        let library = bundled_definition_libraries()
            .expect("bundled libraries should parse")
            .into_iter()
            .next()
            .expect("one bundled library");
        let mut checked = 0;

        for definition_id in [
            "architecture.window.double-european",
            "architecture.window.double-european.greyline",
            "architecture.window.european-single",
            "architecture.window.european-single.greyline",
        ] {
            let definition = library
                .definitions
                .get(&DefinitionId(definition_id.to_string()))
                .expect("bundled window definition should exist");
            for parameter in &definition.interface.parameters.0 {
                if parameter.name == "finish_color"
                    || parameter.name.starts_with("material_")
                    || parameter.name.contains("material_assignment")
                {
                    assert!(
                        !parameter.geometry_affecting,
                        "{definition_id} parameter '{}' should be material-only",
                        parameter.name
                    );
                    checked += 1;
                }
            }
        }

        assert_eq!(
            checked, 4,
            "expected four bundled finish/material parameters"
        );
    }
}
