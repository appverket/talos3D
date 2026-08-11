//! [`DraftingPlugin`] — Bevy plugin that registers the drafting capability,
//! the `DimensionAnnotationFactory` authored-entity factory, the one Drafting
//! workspace transition/inspection commands, style commands, and the
//! persistence sync system.
//!
//! Follows the pattern established by `dimension_line.rs` but for the new
//! dimension type. Runs alongside the legacy plugin; legacy dims are
//! independently persisted under `dimension_annotations` while new dims go
//! under `drafting_annotations`.

use std::collections::HashMap;

use bevy::prelude::*;
use serde_json::Value;

use crate::{
    capability_registry::CapabilityRegistryAppExt,
    plugins::{
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        document_properties::DocumentProperties,
        drawing_export::DRAFTING_CAPABILITY_ID as DRAWING_EXPORT_CAPABILITY_ID,
        identity::ElementId,
    },
};

use super::{
    annotation::{
        DimensionAnnotationFactory, DimensionAnnotationNode, DimensionAnnotationSnapshot,
    },
    draft::{DraftFactory, DraftSyncState},
    style::DimensionStyleRegistry,
    visibility::DraftingVisibility,
};

/// Capability ID the drafting plugin's commands attach to. Re-exports the
/// existing id registered by `drawing_export.rs` so dimension authoring and
/// vector export share the "2D Drafting" workbench. The plugin does NOT
/// register its own capability descriptor to avoid the double-registration
/// panic in `CapabilityRegistry`.
pub const DRAFTING_CAPABILITY_ID: &str = DRAWING_EXPORT_CAPABILITY_ID;

/// Domain_defaults key under which drafting annotations persist. Distinct from
/// the legacy `dimension_annotations` key used by `dimension_line.rs`.
pub const DRAFTING_ANNOTATIONS_KEY: &str = "drafting_annotations";

pub struct DraftingPlugin;

#[derive(Resource, Default)]
struct DraftingAnnotationSyncState {
    last_serialized: Option<Value>,
}

impl Plugin for DraftingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DimensionStyleRegistry>()
            .init_resource::<DraftingVisibility>()
            .init_resource::<super::workspace::DraftingWorkspaceState>()
            .init_resource::<DraftingAnnotationSyncState>()
            .init_resource::<DraftSyncState>()
            .register_authored_entity_factory(DimensionAnnotationFactory)
            .register_authored_entity_factory(DraftFactory)
            // The "drafting" capability descriptor is owned by
            // drawing_export.rs. This plugin attaches commands to that same
            // capability rather than registering a duplicate, which would
            // panic at startup.
            .register_command(
                CommandDescriptor {
                    id: "drafting.toggle".to_string(),
                    label: "Drafting".to_string(),
                    description: "Enter or exit the unified Drafting workspace".to_string(),
                    category: CommandCategory::View,
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "enabled": {
                                "type": "boolean",
                                "description": "Set Drafting active or inactive. Omit to toggle."
                            }
                        },
                        "additionalProperties": false
                    })),
                    default_shortcut: None,
                    icon: Some("icon.dimensions".to_string()),
                    hint: Some(
                        "Toggle the orthographic black-on-white Drafting workspace".to_string(),
                    ),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                super::workspace::execute_toggle_drafting_workspace,
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.inspect".to_string(),
                    label: "Inspect Drafting".to_string(),
                    description: "Inspect the authoritative Drafting workspace state".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                super::workspace::execute_inspect_drafting_workspace,
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.create_draft".to_string(),
                    label: "Create Draft".to_string(),
                    description: "Create and select a durable Draft metadata container. Members remain referenced authored entities; no geometry is copied.".to_string(),
                    category: CommandCategory::Create,
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "default": "Draft"},
                            "origin": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                            "normal": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                            "tangent": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3},
                            "scale_denominator": {"type": "number", "exclusiveMinimum": 0, "default": 50},
                            "paper_width_mm": {"type": "number", "exclusiveMinimum": 0, "default": 297},
                            "paper_height_mm": {"type": "number", "exclusiveMinimum": 0, "default": 210},
                            "margin_mm": {"type": "number", "minimum": 0, "default": 10},
                            "default_layer": {"type": "string", "default": "Default"},
                            "default_dimension_style": {"type": "string", "default": "architectural_metric"},
                            "members": {"type": "array", "items": {"type": "integer", "minimum": 0}, "default": []}
                        },
                        "additionalProperties": false
                    })),
                    default_shortcut: None,
                    icon: None,
                    hint: Some("Create the semantic container for one drawing".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                super::draft::execute_create_draft,
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.select_draft".to_string(),
                    label: "Select Draft".to_string(),
                    description: "Select the Draft consumed by the one Drafting workspace and DrawingScene pipeline.".to_string(),
                    category: CommandCategory::View,
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "required": ["draft_id"],
                        "properties": {"draft_id": {"type": "integer", "minimum": 0}},
                        "additionalProperties": false
                    })),
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                super::draft::execute_select_draft,
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.update_membership".to_string(),
                    label: "Update Draft Membership".to_string(),
                    description: "Add, remove, or replace stable authored-entity references in a Draft without taking geometry ownership.".to_string(),
                    category: CommandCategory::Edit,
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "required": ["member_ids"],
                        "properties": {
                            "draft_id": {"type": "integer", "minimum": 0, "description": "Omit to use the active Draft"},
                            "mode": {"type": "string", "enum": ["add", "remove", "replace"], "default": "add"},
                            "member_ids": {"type": "array", "items": {"type": "integer", "minimum": 0}}
                        },
                        "additionalProperties": false
                    })),
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                super::draft::execute_update_membership,
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.inspect_drafts".to_string(),
                    label: "Inspect Drafts".to_string(),
                    description: "Inspect all Drafts, active selection, resolved 3D/2D members, and stale references.".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: false,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                super::draft::execute_inspect_drafts,
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.set_preset_arch_imperial".to_string(),
                    label: "Arch (imperial)".to_string(),
                    description: "Set architectural imperial as the default drafting style"
                        .to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                |world, _| execute_set_preset(world, "architectural_imperial"),
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.set_preset_arch_metric".to_string(),
                    label: "Arch (metric)".to_string(),
                    description: "Set architectural metric as the default drafting style"
                        .to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                |world, _| execute_set_preset(world, "architectural_metric"),
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.set_preset_eng_mm".to_string(),
                    label: "Eng (mm)".to_string(),
                    description: "Set engineering mm as the default drafting style".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                |world, _| execute_set_preset(world, "engineering_mm"),
            )
            .register_command(
                CommandDescriptor {
                    id: "drafting.set_preset_eng_inch".to_string(),
                    label: "Eng (inch)".to_string(),
                    description: "Set engineering inch as the default drafting style".to_string(),
                    category: CommandCategory::View,
                    parameters: None,
                    default_shortcut: None,
                    icon: None,
                    hint: None,
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: Some(DRAFTING_CAPABILITY_ID.to_string()),
                },
                |world, _| execute_set_preset(world, "engineering_inch"),
            )
            // Run the legacy migration BEFORE the sync system each frame:
            // if a project has just been loaded carrying legacy
            // dimension_annotations, this seeds the new key so the sync
            // system picks it up in the same frame. The migration is
            // idempotent, and tolerates missing DocumentProperties gracefully.
            .add_systems(
                Update,
                (
                    super::migration::migrate_legacy_dimensions,
                    sync_drafting_annotations,
                    super::draft::sync_drafts,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                super::workspace::enforce_drafting_workspace_invariants,
            )
            .add_systems(
                Update,
                super::workspace::enforce_active_draft_camera_alignment
                    .after(crate::plugins::camera::CameraViewUpdateSet),
            );
        app.add_systems(
            Update,
            super::workspace::sync_active_draft_plane_on_change.after(super::draft::sync_drafts),
        );
        super::primitive::register_draft_primitive_support(app);
        app.add_plugins(super::primitive_tool::DraftPrimitiveToolPlugin);
    }
}

fn execute_set_preset(world: &mut World, preset: &str) -> Result<CommandResult, String> {
    let mut registry = world.resource_mut::<DimensionStyleRegistry>();
    if registry.get(preset).is_none() {
        return Err(format!("style '{preset}' not registered"));
    }
    registry.set_default(preset);
    Ok(CommandResult::empty())
}

/// Bidirectional sync between ECS entities (`ElementId` + `DimensionAnnotationNode`)
/// and `DocumentProperties.domain_defaults[DRAFTING_ANNOTATIONS_KEY]`.
/// Mirrors the pattern in `dimension_line.rs::sync_dimension_annotations`.
fn sync_drafting_annotations(world: &mut World) {
    // DocumentProperties is owned by the persistence/document_state plugins;
    // during early boot before those init, skip quietly.
    if !world.contains_resource::<DocumentProperties>() {
        return;
    }
    let saved = {
        let doc_props = world.resource::<DocumentProperties>();
        doc_props
            .domain_defaults
            .get(DRAFTING_ANNOTATIONS_KEY)
            .cloned()
    };
    let saved_changed = {
        let sync_state = world.resource::<DraftingAnnotationSyncState>();
        saved != sync_state.last_serialized
    };

    if saved_changed {
        match saved.as_ref() {
            Some(value) => {
                let Some(snapshots) = deserialize_annotations(value) else {
                    world
                        .resource_mut::<DraftingAnnotationSyncState>()
                        .last_serialized = saved.clone();
                    return;
                };
                apply_annotations_to_world(world, &snapshots);
            }
            None => apply_annotations_to_world(world, &[]),
        }
        world
            .resource_mut::<DraftingAnnotationSyncState>()
            .last_serialized = saved.clone();
    }

    let serialized = serialize_annotations_from_world(world);
    {
        let mut doc_props = world.resource_mut::<DocumentProperties>();
        match &serialized {
            Some(value) => {
                if doc_props.domain_defaults.get(DRAFTING_ANNOTATIONS_KEY) != Some(value) {
                    doc_props
                        .domain_defaults
                        .insert(DRAFTING_ANNOTATIONS_KEY.to_string(), value.clone());
                }
            }
            None => {
                doc_props.domain_defaults.remove(DRAFTING_ANNOTATIONS_KEY);
            }
        }
    }
    world
        .resource_mut::<DraftingAnnotationSyncState>()
        .last_serialized = serialized;
}

fn serialize_annotations_from_world(world: &mut World) -> Option<Value> {
    let mut query = world.query::<(&ElementId, &DimensionAnnotationNode)>();
    let mut snapshots: Vec<DimensionAnnotationSnapshot> = query
        .iter(world)
        .map(|(element_id, node)| DimensionAnnotationSnapshot {
            element_id: *element_id,
            kind: node.kind.clone(),
            a: node.a,
            b: node.b,
            offset: node.offset,
            style_name: node.style_name.clone(),
            text_override: node.text_override.clone(),
            visible: node.visible,
        })
        .collect();
    if snapshots.is_empty() {
        return None;
    }
    snapshots.sort_by_key(|s| s.element_id.0);
    serde_json::to_value(snapshots).ok()
}

fn deserialize_annotations(value: &Value) -> Option<Vec<DimensionAnnotationSnapshot>> {
    let mut snapshots: Vec<DimensionAnnotationSnapshot> =
        serde_json::from_value(value.clone()).ok()?;
    snapshots.sort_by_key(|s| s.element_id.0);
    Some(snapshots)
}

fn apply_annotations_to_world(world: &mut World, snapshots: &[DimensionAnnotationSnapshot]) {
    let mut existing_query = world.query::<(Entity, &ElementId, &DimensionAnnotationNode)>();
    let mut existing = existing_query
        .iter(world)
        .map(|(entity, element_id, node)| (element_id.0, (entity, node.clone())))
        .collect::<HashMap<_, _>>();

    for snapshot in snapshots {
        let node = DimensionAnnotationNode {
            kind: snapshot.kind.clone(),
            a: snapshot.a,
            b: snapshot.b,
            offset: snapshot.offset,
            style_name: snapshot.style_name.clone(),
            text_override: snapshot.text_override.clone(),
            visible: snapshot.visible,
        };
        if let Some((entity, existing_node)) = existing.remove(&snapshot.element_id.0) {
            if existing_node != node {
                world.entity_mut(entity).insert(node);
            }
        } else {
            world.spawn((snapshot.element_id, node));
        }
    }

    for (_, (entity, _)) in existing {
        let _ = world.despawn(entity);
    }
}

/// Query helper: collect (node, ElementId) pairs for all currently-visible
/// drafting annotations, honouring both the per-annotation `visible` flag and
/// the global `DraftingVisibility` resource.
pub fn visible_annotations(world: &World) -> Vec<(ElementId, DimensionAnnotationNode)> {
    let visibility = world
        .get_resource::<DraftingVisibility>()
        .cloned()
        .unwrap_or_default();
    let Some(mut query) = world.try_query::<(&ElementId, &DimensionAnnotationNode)>() else {
        return Vec::new();
    };
    query
        .iter(world)
        .filter(|(_, node)| node.visible)
        .filter(|(_, node)| visibility.is_visible(&node.style_name, node.kind.tag()))
        .map(|(id, node)| (*id, node.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{command_registry::CommandRegistry, drafting::kind::DimensionKind};

    #[test]
    fn sync_writes_and_reads_back() {
        let mut app = App::new();
        app.init_resource::<DocumentProperties>()
            .init_resource::<DraftingAnnotationSyncState>()
            .init_resource::<crate::plugins::identity::ElementIdAllocator>()
            .init_resource::<DimensionStyleRegistry>()
            .init_resource::<DraftingVisibility>();

        let element_id = ElementId(1);
        app.world_mut().spawn((
            element_id,
            DimensionAnnotationNode {
                kind: DimensionKind::Linear { direction: Vec3::X },
                a: Vec3::ZERO,
                b: Vec3::new(4.572, 0.0, 0.0),
                offset: Vec3::new(0.0, 0.5, 0.0),
                style_name: "architectural_imperial".into(),
                text_override: None,
                visible: true,
            },
        ));

        // Run the sync system once.
        app.add_systems(Update, sync_drafting_annotations);
        app.update();

        let props = app.world().resource::<DocumentProperties>();
        let stored = props
            .domain_defaults
            .get(DRAFTING_ANNOTATIONS_KEY)
            .expect("persisted");
        assert!(stored.is_array());
    }

    #[test]
    fn draft_commands_publish_typed_agent_schemas() {
        let mut app = App::new();
        app.add_plugins(DraftingPlugin);
        let registry = app.world().resource::<CommandRegistry>();
        for id in [
            "drafting.create_draft",
            "drafting.select_draft",
            "drafting.update_membership",
            "drafting.inspect_drafts",
            "drafting.create_line",
            "drafting.create_polyline",
            "drafting.create_rectangle",
            "drafting.create_circle",
            "drafting.create_text",
            "drafting.inspect_primitives",
        ] {
            let descriptor = registry.get(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(
                descriptor.capability_id.as_deref(),
                Some(DRAFTING_CAPABILITY_ID)
            );
            if !id.starts_with("drafting.inspect") {
                assert!(descriptor.parameters.is_some(), "{id} needs a typed schema");
            }
            if id.starts_with("drafting.create_") && id != "drafting.create_draft" {
                assert!(descriptor.activates_tool.is_some(), "{id} needs a UI tool");
            }
        }
        assert!(registry.duplicate_ids().is_empty());
    }
}
