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
    use crate::plugins::materials::{
        BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80, BUILTIN_MATERIAL_WINDOW_DARK_GASKET,
        BUILTIN_MATERIAL_WINDOW_HARDWARE_STEEL, BUILTIN_MATERIAL_WINDOW_WHITE_FRAME,
    };
    use crate::plugins::modeling::definition::{
        DefinitionId, DefinitionLibraryId, ParameterScaleBehavior,
    };

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

    #[test]
    fn bundled_european_double_window_defaults_to_white_hollow_parts() {
        let library = bundled_definition_libraries()
            .expect("bundled libraries should parse")
            .into_iter()
            .next()
            .expect("one bundled library");

        for definition_id in [
            "architecture.window.double-european",
            "architecture.window.european-single",
        ] {
            let definition = library
                .definitions
                .get(&DefinitionId(definition_id.to_string()))
                .expect("window definition should exist");
            assert!(
                definition.evaluators.is_empty(),
                "{definition_id} must not emit a solid slab over its compound parts"
            );
            assert_eq!(
                definition
                    .material_assignment
                    .as_ref()
                    .and_then(|assignment| assignment.render_material_id(None))
                    .as_deref(),
                Some(BUILTIN_MATERIAL_WINDOW_WHITE_FRAME),
                "{definition_id} should default to white frame material"
            );
        }

        let double = library
            .definitions
            .get(&DefinitionId(
                "architecture.window.double-european".to_string(),
            ))
            .expect("double window definition should exist");
        let double_slots = &double
            .compound
            .as_ref()
            .expect("double window should be compound")
            .child_slots;
        assert!(
            double_slots
                .iter()
                .all(|slot| slot.definition_id.0 != "architecture.window.european-single"),
            "the default double window should be one two-leaf casement, not two nested single-window units"
        );
        for required_slot in ["left_sash", "right_sash", "left_handle", "right_handle"] {
            assert!(
                double_slots
                    .iter()
                    .any(|slot| slot.slot_id == required_slot),
                "double window should include {required_slot}"
            );
        }
        let scale_behavior = |name: &str| {
            double
                .interface
                .parameters
                .0
                .iter()
                .find(|parameter| parameter.name == name)
                .and_then(|parameter| parameter.metadata.scale_behavior)
        };
        assert_eq!(
            scale_behavior("overall_width"),
            Some(ParameterScaleBehavior::ScaleWithOccurrence)
        );
        assert_eq!(
            scale_behavior("overall_height"),
            Some(ParameterScaleBehavior::ScaleWithOccurrence)
        );
        assert_eq!(
            scale_behavior("frame_face_width"),
            Some(ParameterScaleBehavior::FixedWorld),
            "frame face width is a construction invariant, not a generic scale target"
        );
        assert_eq!(
            scale_behavior("glazing_thickness"),
            Some(ParameterScaleBehavior::FixedWorld)
        );
        assert_eq!(
            scale_behavior("sash_split"),
            Some(ParameterScaleBehavior::Ratio)
        );
        assert_eq!(
            scale_behavior("finish_color"),
            Some(ParameterScaleBehavior::Semantic)
        );

        for definition_id in ["architecture.window.frame", "architecture.window.sash"] {
            let definition = library
                .definitions
                .get(&DefinitionId(definition_id.to_string()))
                .expect("component definition should exist");
            assert!(definition.evaluators.is_empty());
            assert_eq!(
                definition
                    .compound
                    .as_ref()
                    .map(|compound| compound.child_slots.len()),
                Some(4),
                "{definition_id} should be composed from two stiles and two rails"
            );
            assert_eq!(
                definition
                    .material_assignment
                    .as_ref()
                    .and_then(|assignment| assignment.render_material_id(None))
                    .as_deref(),
                Some(BUILTIN_MATERIAL_WINDOW_WHITE_FRAME)
            );
        }

        let glazing = library
            .definitions
            .get(&DefinitionId("architecture.window.glazing".to_string()))
            .expect("glazing definition should exist");
        assert_eq!(
            glazing
                .material_assignment
                .as_ref()
                .and_then(|assignment| assignment.render_material_id(None))
                .as_deref(),
            Some(BUILTIN_MATERIAL_BLUE_TINT_GLAZING_80)
        );

        let muntin = library
            .definitions
            .get(&DefinitionId("architecture.window.muntin".to_string()))
            .expect("muntin definition should exist");
        assert_eq!(
            muntin
                .material_assignment
                .as_ref()
                .and_then(|assignment| assignment.render_material_id(None))
                .as_deref(),
            Some(BUILTIN_MATERIAL_WINDOW_WHITE_FRAME),
            "muntins are frame members, not glazing"
        );

        for (definition_id, material_id) in [
            (
                "architecture.window.handle",
                BUILTIN_MATERIAL_WINDOW_HARDWARE_STEEL,
            ),
            (
                "architecture.window.hinge",
                BUILTIN_MATERIAL_WINDOW_HARDWARE_STEEL,
            ),
            (
                "architecture.window.gasket",
                BUILTIN_MATERIAL_WINDOW_DARK_GASKET,
            ),
        ] {
            let definition = library
                .definitions
                .get(&DefinitionId(definition_id.to_string()))
                .expect("required window subpart definition should exist");
            assert_eq!(
                definition
                    .material_assignment
                    .as_ref()
                    .and_then(|assignment| assignment.render_material_id(None))
                    .as_deref(),
                Some(material_id)
            );
        }
    }
}
