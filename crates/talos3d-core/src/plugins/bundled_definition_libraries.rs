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
    for library in bundled_definition_libraries()? {
        if libraries.get(&library.id).is_none() {
            libraries.insert(library);
        }
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
    use crate::plugins::modeling::definition::DefinitionLibraryId;

    #[test]
    fn bundled_libraries_use_bundled_scope() {
        let libraries = bundled_definition_libraries().expect("bundled libraries should parse");
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].scope, DefinitionLibraryScope::Bundled);
        assert_eq!(
            libraries[0].id,
            DefinitionLibraryId("architecture.double-european-window".to_string())
        );
        assert!(libraries[0].definitions.contains_key(
            &crate::plugins::modeling::definition::DefinitionId(
                "architecture.window.double-european".to_string(),
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
                    "architecture.double-european-window".to_string()
                ))
                .expect("library should exist")
                .name,
            "Custom Override"
        );
    }
}
