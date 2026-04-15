use std::collections::HashMap;

use bevy::{ecs::world::EntityRef, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability_registry::CapabilityRegistry;
use crate::plugins::{
    camera::focus_orbit_camera_on_bounds, commands::DeleteEntitiesCommand,
    history::PendingCommandQueue, identity::ElementId, lighting::scene_light_object_exposed,
    palette::PaletteState, selection::Selected, tools::ActiveTool, transform::PivotPoint,
};

use crate::plugins::icons;

pub struct CommandRegistryPlugin;

impl Plugin for CommandRegistryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CommandRegistry>()
            .init_resource::<IconRegistry>()
            .init_resource::<PendingCommandInvocations>()
            .add_systems(Startup, setup_core_icons)
            .add_systems(
                Update,
                (detect_command_shortcuts, execute_pending_commands).chain(),
            );
        register_core_commands(app);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandCategory {
    File,
    Create,
    Edit,
    View,
    Custom(String),
}

impl CommandCategory {
    pub fn label(&self) -> &str {
        match self {
            Self::File => "File",
            Self::Create => "Create",
            Self::Edit => "Edit",
            Self::View => "View",
            Self::Custom(label) => label.as_str(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    pub category: CommandCategory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_shortcut: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub requires_selection: bool,
    pub show_in_menu: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activates_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
}

fn default_version() -> u32 {
    1
}

/// Structured result from command execution, suitable for AI inspection and
/// programmatic callers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandResult {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub modified: Vec<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

impl CommandResult {
    pub fn empty() -> Self {
        Self::default()
    }
}

pub type CommandHandler = fn(&mut World, &Value) -> Result<CommandResult, String>;

#[derive(Clone)]
struct RegisteredCommand {
    descriptor: CommandDescriptor,
    handler: CommandHandler,
}

#[derive(Resource, Default)]
pub struct CommandRegistry {
    commands: Vec<RegisteredCommand>,
    index_by_id: HashMap<String, usize>,
}

impl CommandRegistry {
    pub fn commands(&self) -> impl Iterator<Item = &CommandDescriptor> {
        self.commands.iter().map(|command| &command.descriptor)
    }

    pub fn get(&self, id: &str) -> Option<&CommandDescriptor> {
        self.index_by_id
            .get(id)
            .and_then(|index| self.commands.get(*index))
            .map(|command| &command.descriptor)
    }

    fn register(&mut self, descriptor: CommandDescriptor, handler: CommandHandler) {
        let index = self.commands.len();
        self.index_by_id.insert(descriptor.id.clone(), index);
        self.commands.push(RegisteredCommand {
            descriptor,
            handler,
        });
    }

    pub fn handler_for(&self, id: &str) -> Option<CommandHandler> {
        self.index_by_id
            .get(id)
            .and_then(|index| self.commands.get(*index))
            .map(|command| command.handler)
    }

    /// Export all registered command descriptors as a JSON value.
    /// Used by MCP tool discovery and the command schema API.
    pub fn export_schema(&self) -> Value {
        let descriptors: Vec<&CommandDescriptor> = self.commands().collect();
        serde_json::to_value(descriptors).unwrap_or_default()
    }
}

#[derive(Resource, Default, Clone)]
pub struct IconRegistry {
    icons: HashMap<String, Handle<Image>>,
}

impl IconRegistry {
    pub fn register(&mut self, id: impl Into<String>, handle: Handle<Image>) {
        self.icons.insert(id.into(), handle);
    }

    pub fn get(&self, id: &str) -> Option<Handle<Image>> {
        self.icons.get(id).cloned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCommandInvocation {
    pub id: String,
    pub parameters: Value,
}

/// Serialized form of a command invocation, suitable for replay, macro capture,
/// and AI tooling. Matches the canonical JSON shape defined in COMMAND_SYSTEM.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedCommand {
    pub command: String,
    pub version: u32,
    pub parameters: Value,
}

impl SerializedCommand {
    pub fn from_invocation(
        invocation: &PendingCommandInvocation,
        registry: &CommandRegistry,
    ) -> Self {
        let version = registry.get(&invocation.id).map(|d| d.version).unwrap_or(1);
        Self {
            command: invocation.id.clone(),
            version,
            parameters: invocation.parameters.clone(),
        }
    }

    pub fn to_invocation(&self) -> PendingCommandInvocation {
        PendingCommandInvocation {
            id: self.command.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// Serializable interaction event for direct interaction injection
/// (future: AI agent direct control layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "interaction")]
pub enum SerializedInteraction {
    #[serde(rename = "pointer_drag")]
    PointerDrag {
        button: String,
        start: [f64; 2],
        end: [f64; 2],
        #[serde(default)]
        modifiers: Vec<String>,
    },
    #[serde(rename = "keyboard_input")]
    KeyboardInput {
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
}

#[derive(Resource, Default)]
pub struct PendingCommandInvocations {
    invocations: Vec<PendingCommandInvocation>,
}

pub trait CommandRegistryAppExt {
    fn register_command(
        &mut self,
        descriptor: CommandDescriptor,
        handler: CommandHandler,
    ) -> &mut Self;
}

impl CommandRegistryAppExt for App {
    fn register_command(
        &mut self,
        descriptor: CommandDescriptor,
        handler: CommandHandler,
    ) -> &mut Self {
        if !self.world().contains_resource::<CommandRegistry>() {
            self.init_resource::<CommandRegistry>();
        }
        self.world_mut()
            .resource_mut::<CommandRegistry>()
            .register(descriptor, handler);
        self
    }
}

pub fn queue_command_invocation(world: &mut World, id: impl Into<String>, parameters: Value) {
    queue_command_invocation_resource(
        &mut world.resource_mut::<PendingCommandInvocations>(),
        id,
        parameters,
    );
}

pub fn queue_command_invocation_resource(
    pending: &mut PendingCommandInvocations,
    id: impl Into<String>,
    parameters: Value,
) {
    pending.invocations.push(PendingCommandInvocation {
        id: id.into(),
        parameters,
    });
}

pub fn command_enabled(world: &World, descriptor: &CommandDescriptor) -> bool {
    !descriptor.requires_selection || selection_count(world) > 0
}

pub fn selection_count(world: &World) -> usize {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .filter(|entity_ref| entity_ref.contains::<Selected>())
        .count()
}

pub fn activate_tool_command(world: &mut World, tool: ActiveTool) -> Result<CommandResult, String> {
    world.resource_mut::<NextState<ActiveTool>>().set(tool);
    Ok(CommandResult::empty())
}

pub fn ordered_menu_categories(registry: &CommandRegistry) -> Vec<CommandCategory> {
    let mut categories = Vec::new();
    for descriptor in registry
        .commands()
        .filter(|descriptor| descriptor.show_in_menu)
    {
        if categories
            .iter()
            .all(|category| category != &descriptor.category)
        {
            categories.push(descriptor.category.clone());
        }
    }
    categories
}

pub fn register_core_commands(app: &mut App) {
    app.register_command(
        CommandDescriptor {
            id: "core.select_tool".to_string(),
            label: "Select Tool".to_string(),
            description: "Activate the select tool".to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            default_shortcut: Some("Esc".to_string()),
            icon: Some("icon.select".to_string()),
            hint: Some("Activate the select tool".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: Some("Select".to_string()),
            capability_id: None,
        },
        execute_select_tool,
    )
    .register_command(
        CommandDescriptor {
            id: "core.undo".to_string(),
            label: "Undo".to_string(),
            description: "Undo the last command".to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            default_shortcut: Some("Ctrl/Cmd+Z".to_string()),
            icon: Some("icon.undo".to_string()),
            hint: Some("Undo the most recent change".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_undo,
    )
    .register_command(
        CommandDescriptor {
            id: "core.redo".to_string(),
            label: "Redo".to_string(),
            description: "Redo the last undone command".to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            default_shortcut: Some("Ctrl/Cmd+Shift+Z".to_string()),
            icon: Some("icon.redo".to_string()),
            hint: Some("Redo the last undone command".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_redo,
    )
    .register_command(
        CommandDescriptor {
            id: "core.delete".to_string(),
            label: "Delete".to_string(),
            description: "Delete the selected entities".to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            default_shortcut: Some("Delete".to_string()),
            icon: Some("icon.delete".to_string()),
            hint: Some("Delete the current selection".to_string()),
            requires_selection: true,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_delete,
    )
    .register_command(
        CommandDescriptor {
            id: "core.select_all".to_string(),
            label: "Select All".to_string(),
            description: "Select all authored entities".to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            default_shortcut: Some("Ctrl/Cmd+A".to_string()),
            icon: Some("icon.select_all".to_string()),
            hint: Some("Select all authored entities".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_select_all,
    )
    .register_command(
        CommandDescriptor {
            id: "core.deselect".to_string(),
            label: "Deselect".to_string(),
            description: "Clear the current selection".to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            default_shortcut: Some("Esc, Ctrl/Cmd+D".to_string()),
            icon: Some("icon.deselect".to_string()),
            hint: Some("Clear the current selection".to_string()),
            requires_selection: true,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_deselect,
    )
    .register_command(
        CommandDescriptor {
            id: "core.zoom_to_extents".to_string(),
            label: "Zoom To Extents".to_string(),
            description: "Frame the camera on all authored entities.".to_string(),
            category: CommandCategory::View,
            parameters: None,
            default_shortcut: Some("Home".to_string()),
            icon: Some("icon.zoom_extents".to_string()),
            hint: Some("Frame the camera on the full model extents".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_zoom_to_extents,
    )
    .register_command(
        CommandDescriptor {
            id: "core.zoom_to_selection".to_string(),
            label: "Zoom To Selection".to_string(),
            description: "Frame the camera on the current selection.".to_string(),
            category: CommandCategory::View,
            parameters: None,
            default_shortcut: Some("Shift+Home".to_string()),
            icon: Some("icon.zoom_selection".to_string()),
            hint: Some("Frame the camera on the current selection".to_string()),
            requires_selection: true,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_zoom_to_selection,
    )
    .register_command(
        CommandDescriptor {
            id: "core.show_command_palette".to_string(),
            label: "Show Command Palette".to_string(),
            description: "Open the command panel.".to_string(),
            category: CommandCategory::View,
            parameters: None,
            default_shortcut: Some(if cfg!(target_os = "macos") {
                "Cmd+K".to_string()
            } else {
                "Ctrl+K".to_string()
            }),
            icon: None,
            hint: Some("Search commands, tools, and categories".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_show_command_palette,
    )
    .register_command(
        CommandDescriptor {
            id: "core.set_pivot".to_string(),
            label: "Set Pivot".to_string(),
            description: "Set the active transform pivot to explicit X Y Z coordinates".to_string(),
            category: CommandCategory::Edit,
            parameters: Some(serde_json::json!({
                "type": "object",
                "required": ["x", "y", "z"],
                "properties": {
                    "x": {"type": "number"},
                    "y": {"type": "number"},
                    "z": {"type": "number"}
                }
            })),
            default_shortcut: None,
            icon: Some("icon.crosshair".to_string()),
            hint: Some("Type `Set Pivot x y z` in the palette to set a custom pivot".to_string()),
            requires_selection: true,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_set_pivot,
    )
    .register_command(
        CommandDescriptor {
            id: "core.clear_pivot".to_string(),
            label: "Clear Pivot".to_string(),
            description: "Clear the active transform pivot and return to the selection centroid"
                .to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            default_shortcut: None,
            icon: Some("icon.crosshair".to_string()),
            hint: Some("Clear the active transform pivot".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_clear_pivot,
    )
    .register_command(
        CommandDescriptor {
            id: "core.new".to_string(),
            label: "New".to_string(),
            description: "Create a new empty project".to_string(),
            category: CommandCategory::File,
            parameters: None,
            default_shortcut: Some("Ctrl/Cmd+N".to_string()),
            icon: Some("icon.new".to_string()),
            hint: Some("Create a new empty Talos3D project".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_new,
    )
    .register_command(
        CommandDescriptor {
            id: "core.open".to_string(),
            label: "Open...".to_string(),
            description: "Open a project file".to_string(),
            category: CommandCategory::File,
            parameters: None,
            default_shortcut: Some("Ctrl/Cmd+O".to_string()),
            icon: Some("icon.open".to_string()),
            hint: Some("Open a Talos3D project file".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_open,
    )
    .register_command(
        CommandDescriptor {
            id: "core.save".to_string(),
            label: "Save".to_string(),
            description: "Save the current project".to_string(),
            category: CommandCategory::File,
            parameters: None,
            default_shortcut: Some("Ctrl/Cmd+S".to_string()),
            icon: Some("icon.save".to_string()),
            hint: Some("Save the current Talos3D project".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_save,
    )
    .register_command(
        CommandDescriptor {
            id: "core.save_as".to_string(),
            label: "Save As...".to_string(),
            description: "Save the project to a new file".to_string(),
            category: CommandCategory::File,
            parameters: None,
            default_shortcut: Some("Ctrl/Cmd+Shift+S".to_string()),
            icon: Some("icon.save".to_string()),
            hint: Some("Save the Talos3D project to a new file".to_string()),
            requires_selection: false,
            show_in_menu: true,
            version: 1,
            activates_tool: None,
            capability_id: None,
        },
        execute_save_as,
    );
}

fn setup_core_icons(mut images: ResMut<Assets<Image>>, mut icon_registry: ResMut<IconRegistry>) {
    let icon_names = [
        ("icon.select", "mouse_pointer"),
        ("icon.undo", "undo"),
        ("icon.redo", "redo"),
        ("icon.delete", "trash"),
        ("icon.move", "move"),
        ("icon.rotate", "rotate"),
        ("icon.scale", "scale"),
        ("icon.save", "save"),
        ("icon.export", "export"),
        ("icon.load", "folder_open"),
        ("icon.create", "plus"),
        ("icon.view", "scan"),
        ("icon.new", "file_plus"),
        ("icon.open", "folder_open"),
        ("icon.import", "import"),
        ("icon.select_all", "box_select"),
        ("icon.deselect", "deselect"),
        ("icon.crosshair", "crosshair"),
        ("icon.zoom_extents", "zoom_extents"),
        ("icon.zoom_selection", "zoom_selection"),
        ("icon.group", "group"),
        ("icon.ungroup", "ungroup"),
        ("icon.create_box", "create_box"),
        ("icon.create_cylinder", "create_cylinder"),
        ("icon.create_sphere", "create_sphere"),
        ("icon.create_plane", "create_plane"),
        ("icon.create_polyline", "create_polyline"),
        ("icon.create_fillet", "create_fillet"),
        ("icon.create_chamfer", "create_chamfer"),
        ("icon.dimension", "dimension"),
        ("icon.dimensions", "dimensions"),
        ("icon.guide_line", "guide_line"),
        ("icon.guide_lines", "guide_lines"),
        ("icon.view_perspective", "view_perspective"),
        ("icon.view_orthographic", "view_orthographic"),
        ("icon.view_isometric", "view_isometric"),
        ("icon.view_front", "view_front"),
        ("icon.view_back", "view_back"),
        ("icon.view_top", "view_top"),
        ("icon.view_bottom", "view_bottom"),
        ("icon.view_left", "view_left"),
        ("icon.view_right", "view_right"),
        ("icon.view_wireframe", "view_wireframe"),
        ("icon.view_outline", "view_outline"),
        ("icon.view_grid", "view_grid"),
        ("icon.view_paper", "view_paper"),
    ];

    let size = bevy::render::render_resource::Extent3d {
        width: icons::ICON_SIZE,
        height: icons::ICON_SIZE,
        depth_or_array_layers: 1,
    };

    for (id, icon_name) in icon_names {
        let rgba = icons::render_icon(icon_name);
        let image = Image::new(
            size,
            bevy::render::render_resource::TextureDimension::D2,
            rgba,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::default(),
        );
        icon_registry.register(id, images.add(image));
    }
}

fn detect_command_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut pending: ResMut<PendingCommandInvocations>,
    egui_wants_input: Res<crate::plugins::egui_chrome::EguiWantsInput>,
) {
    if egui_wants_input.keyboard {
        return;
    }

    let primary_modifier = if cfg!(target_os = "macos") {
        keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
    } else {
        keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
    };

    if primary_modifier && keys.just_pressed(KeyCode::KeyA) {
        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        if !shift {
            queue_command_invocation_resource(&mut pending, "core.select_all", Value::Null);
        }
    }

    if primary_modifier && keys.just_pressed(KeyCode::KeyD) {
        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        if !shift {
            queue_command_invocation_resource(&mut pending, "core.deselect", Value::Null);
        }
    }

    if primary_modifier && keys.just_pressed(KeyCode::KeyG) {
        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        if shift {
            queue_command_invocation_resource(&mut pending, "modeling.ungroup", Value::Null);
        } else {
            queue_command_invocation_resource(&mut pending, "modeling.group", Value::Null);
        }
    }
}

fn execute_pending_commands(world: &mut World) {
    let invocations = std::mem::take(
        &mut world
            .resource_mut::<PendingCommandInvocations>()
            .invocations,
    );

    for invocation in invocations {
        let handler = {
            let registry = world.resource::<CommandRegistry>();
            registry.handler_for(&invocation.id)
        };
        let result = handler
            .ok_or_else(|| format!("Unknown command: {}", invocation.id))
            .and_then(|handler| handler(world, &invocation.parameters));
        if let Err(error) = result {
            if let Some(mut status_bar_data) =
                world.get_resource_mut::<crate::plugins::ui::StatusBarData>()
            {
                status_bar_data.set_feedback(error, 2.0);
            }
        }
    }
}

fn execute_undo(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    world.resource_mut::<PendingCommandQueue>().queue_undo();
    Ok(CommandResult::empty())
}

fn execute_select_tool(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    activate_tool_command(world, ActiveTool::Select)
}

fn execute_redo(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    world.resource_mut::<PendingCommandQueue>().queue_redo();
    Ok(CommandResult::empty())
}

fn execute_delete(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    let element_ids: Vec<ElementId> = q
        .iter(world)
        .filter(|entity_ref| entity_ref.contains::<Selected>())
        .filter_map(|entity_ref| entity_ref.get::<ElementId>().copied())
        .collect();
    if element_ids.is_empty() {
        return Err("No selection to delete".to_string());
    }
    world
        .resource_mut::<Messages<DeleteEntitiesCommand>>()
        .write(DeleteEntitiesCommand { element_ids });
    Ok(CommandResult::empty())
}

fn execute_select_all(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    use crate::plugins::layers::{LayerAssignment, LayerRegistry};

    let registry = world.resource::<LayerRegistry>();
    let mut q = world.try_query::<EntityRef>().unwrap();
    let entities: Vec<Entity> = q
        .iter(world)
        .filter(|entity_ref| {
            if !entity_ref.contains::<ElementId>() {
                return false;
            }
            // Skip entities on hidden or locked layers
            let layer_name = entity_ref
                .get::<LayerAssignment>()
                .map(|a| a.layer.as_str())
                .unwrap_or(crate::plugins::layers::DEFAULT_LAYER_NAME);
            registry.is_visible(layer_name) && !registry.is_locked(layer_name)
        })
        .filter(|entity_ref| {
            // Skip hidden entities
            entity_ref.get::<Visibility>().copied() != Some(Visibility::Hidden)
        })
        .filter(|entity_ref| scene_light_object_exposed(entity_ref, world))
        .map(|entity_ref| entity_ref.id())
        .collect();
    for entity in entities {
        world.entity_mut(entity).insert(Selected);
    }
    Ok(CommandResult::empty())
}

fn execute_deselect(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    let entities: Vec<Entity> = q
        .iter(world)
        .filter(|entity_ref| entity_ref.contains::<Selected>())
        .map(|entity_ref| entity_ref.id())
        .collect();
    for entity in entities {
        world.entity_mut(entity).remove::<Selected>();
    }
    world.insert_resource(PivotPoint::default());
    Ok(CommandResult::empty())
}

fn execute_zoom_to_extents(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    frame_camera_for_entities(world, false)
}

fn execute_zoom_to_selection(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    frame_camera_for_entities(world, true)
}

fn execute_show_command_palette(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    world.resource_mut::<PaletteState>().show();
    Ok(CommandResult::empty())
}

fn frame_camera_for_entities(
    world: &mut World,
    selected_only: bool,
) -> Result<CommandResult, String> {
    let Some(bounds) = snapshot_bounds_for_entities(world, selected_only) else {
        return Err(if selected_only {
            "No selected geometry to frame".to_string()
        } else {
            "No authored geometry to frame".to_string()
        });
    };
    if !focus_orbit_camera_on_bounds(world, bounds) {
        return Err("No orbit camera is available".to_string());
    }
    if let Some(mut status_bar_data) = world.get_resource_mut::<crate::plugins::ui::StatusBarData>()
    {
        status_bar_data.set_feedback(
            if selected_only {
                "Camera framed on selection".to_string()
            } else {
                "Camera framed on model extents".to_string()
            },
            2.0,
        );
    }
    Ok(CommandResult::empty())
}

fn snapshot_bounds_for_entities(
    world: &World,
    selected_only: bool,
) -> Option<crate::authored_entity::EntityBounds> {
    let registry = world.get_resource::<CapabilityRegistry>()?;
    let mut q = world.try_query::<EntityRef>().unwrap();
    let snapshots = q
        .iter(world)
        .filter(|entity_ref| entity_ref.contains::<ElementId>())
        .filter(|entity_ref| !selected_only || entity_ref.contains::<Selected>())
        .filter(|entity_ref| scene_light_object_exposed(entity_ref, world))
        .filter_map(|entity_ref| registry.capture_snapshot(&entity_ref, world))
        .collect::<Vec<_>>();

    let model_snapshots = snapshots
        .iter()
        .filter(|snapshot| snapshot.scope() == crate::authored_entity::EntityScope::AuthoredModel)
        .collect::<Vec<_>>();
    let bounds_source = if selected_only && model_snapshots.is_empty() {
        snapshots.iter().collect::<Vec<_>>()
    } else {
        model_snapshots
    };

    bounds_source
        .into_iter()
        .filter_map(|snapshot| snapshot.bounds())
        .reduce(|acc, bounds| crate::authored_entity::EntityBounds {
            min: acc.min.min(bounds.min),
            max: acc.max.max(bounds.max),
        })
}

fn execute_set_pivot(world: &mut World, parameters: &Value) -> Result<CommandResult, String> {
    let x = parameters
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| "Set Pivot requires numeric x, y, z parameters".to_string())?
        as f32;
    let y = parameters
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| "Set Pivot requires numeric x, y, z parameters".to_string())?
        as f32;
    let z = parameters
        .get("z")
        .and_then(Value::as_f64)
        .ok_or_else(|| "Set Pivot requires numeric x, y, z parameters".to_string())?
        as f32;
    if selection_count(world) == 0 {
        return Err("Select an entity before setting a pivot".to_string());
    }

    world.resource_mut::<PivotPoint>().position = Some(Vec3::new(x, y, z));
    if let Some(mut status_bar_data) = world.get_resource_mut::<crate::plugins::ui::StatusBarData>()
    {
        status_bar_data.set_feedback(format!("Pivot set to ({x:.2}, {y:.2}, {z:.2})"), 2.0);
    }
    Ok(CommandResult::empty())
}

fn execute_clear_pivot(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    world.resource_mut::<PivotPoint>().position = None;
    if let Some(mut status_bar_data) = world.get_resource_mut::<crate::plugins::ui::StatusBarData>()
    {
        status_bar_data.set_feedback("Pivot cleared".to_string(), 2.0);
    }
    Ok(CommandResult::empty())
}

fn execute_new(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    crate::plugins::persistence::new_document(world);
    Ok(CommandResult::empty())
}

fn execute_open(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    match crate::plugins::persistence::open_project_dialog(world) {
        Ok(Some(())) => Ok(CommandResult::empty()),
        Ok(None) => Ok(CommandResult::empty()),
        Err(e) => Err(e),
    }
}

fn execute_save(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    crate::plugins::persistence::save_project_now(world).map(|()| CommandResult::empty())
}

fn execute_save_as(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    match crate::plugins::persistence::save_as_now(world) {
        Ok(Some(())) => Ok(CommandResult::empty()),
        Ok(None) => Ok(CommandResult::empty()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{
        lighting::{SceneLightNode, SceneLightObjectVisibility},
        palette::filtered_commands,
        transform::PivotPoint,
        ui::StatusBarData,
    };

    struct TestCommandPlugin;

    impl Plugin for TestCommandPlugin {
        fn build(&self, app: &mut App) {
            app.register_command(
                CommandDescriptor {
                    id: "test.custom".to_string(),
                    label: "Custom Test Command".to_string(),
                    description: "Command registered by a test capability".to_string(),
                    category: CommandCategory::Custom("Test".to_string()),
                    parameters: None,
                    default_shortcut: Some("T".to_string()),
                    icon: None,
                    hint: Some("Exercise custom command registration".to_string()),
                    requires_selection: false,
                    show_in_menu: true,
                    version: 1,
                    activates_tool: None,
                    capability_id: None,
                },
                execute_noop,
            );
        }
    }

    fn execute_noop(_: &mut World, _: &Value) -> Result<CommandResult, String> {
        Ok(CommandResult::empty())
    }

    #[test]
    fn custom_commands_appear_in_palette_and_menu_views() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin)
            .add_plugins(TestCommandPlugin);

        let registry = app.world().resource::<CommandRegistry>();
        let palette_entries = filtered_commands(registry, "custom");
        assert_eq!(palette_entries.len(), 1);
        assert_eq!(palette_entries[0].id, "test.custom");

        let categories = ordered_menu_categories(registry);
        assert!(categories.contains(&CommandCategory::Custom("Test".to_string())));
    }

    #[test]
    fn core_pivot_commands_update_the_pivot_resource() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin)
            .init_resource::<Assets<Image>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(crate::plugins::egui_chrome::EguiWantsInput::default())
            .insert_resource(PivotPoint::default())
            .insert_resource(StatusBarData::default());

        queue_command_invocation(
            app.world_mut(),
            "core.set_pivot",
            serde_json::json!({"x": 1.0, "y": 2.0, "z": 3.0}),
        );
        app.world_mut().spawn(Selected);
        app.update();
        assert_eq!(
            app.world().resource::<PivotPoint>().position,
            Some(Vec3::new(1.0, 2.0, 3.0)),
        );

        queue_command_invocation(app.world_mut(), "core.clear_pivot", serde_json::json!({}));
        app.update();
        assert_eq!(app.world().resource::<PivotPoint>().position, None);
    }

    #[test]
    fn serialized_command_round_trips_through_json() {
        let cmd = SerializedCommand {
            command: "architectural.create_wall".to_string(),
            version: 1,
            parameters: serde_json::json!({
                "start": [0.0, 0.0],
                "end": [5.0, 0.0],
                "height": 3.0,
                "thickness": 0.2
            }),
        };

        let json = serde_json::to_string(&cmd).unwrap();
        let deserialized: SerializedCommand = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.command, "architectural.create_wall");
        assert_eq!(deserialized.version, 1);
        assert_eq!(deserialized.parameters["height"], 3.0);
    }

    #[test]
    fn command_descriptor_serializes_to_json() {
        let descriptor = CommandDescriptor {
            id: "core.delete".to_string(),
            label: "Delete".to_string(),
            description: "Delete selected entities".to_string(),
            category: CommandCategory::Edit,
            parameters: None,
            version: 1,
            default_shortcut: Some("Delete".to_string()),
            icon: None,
            hint: None,
            requires_selection: true,
            show_in_menu: true,
            activates_tool: None,
            capability_id: None,
        };

        let json = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(json["id"], "core.delete");
        assert_eq!(json["category"], "Edit");
        assert_eq!(json["version"], 1);
        assert!(json.get("parameters").is_none());
    }

    #[test]
    fn serialized_command_from_invocation_uses_registry_version() {
        let mut registry = CommandRegistry::default();
        registry.register(
            CommandDescriptor {
                id: "test.v2".to_string(),
                label: "Test".to_string(),
                description: "Test".to_string(),
                category: CommandCategory::Edit,
                parameters: None,
                version: 2,
                default_shortcut: None,
                icon: None,
                hint: None,
                requires_selection: false,
                show_in_menu: false,
                activates_tool: None,
                capability_id: None,
            },
            execute_noop,
        );

        let invocation = PendingCommandInvocation {
            id: "test.v2".to_string(),
            parameters: serde_json::json!({}),
        };

        let serialized = SerializedCommand::from_invocation(&invocation, &registry);
        assert_eq!(serialized.version, 2);
    }

    #[test]
    fn export_schema_includes_all_registered_commands() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin)
            .add_plugins(TestCommandPlugin);

        let registry = app.world().resource::<CommandRegistry>();
        let schema = registry.export_schema();
        let commands = schema.as_array().unwrap();

        // Should contain core commands + the test command
        assert!(commands.len() > 1);

        let test_cmd = commands.iter().find(|c| c["id"] == "test.custom");
        assert!(test_cmd.is_some());
        let test_cmd = test_cmd.unwrap();
        assert_eq!(test_cmd["label"], "Custom Test Command");
        assert_eq!(test_cmd["version"], 1);
    }

    #[test]
    fn select_all_skips_hidden_light_objects_by_default() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin)
            .init_resource::<Assets<Image>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(crate::plugins::layers::LayerRegistry::default())
            .insert_resource(SceneLightObjectVisibility::default())
            .insert_resource(crate::plugins::egui_chrome::EguiWantsInput::default());

        app.world_mut().spawn(ElementId(1));
        app.world_mut()
            .spawn((ElementId(2), SceneLightNode::default()));

        queue_command_invocation(app.world_mut(), "core.select_all", Value::Null);
        app.update();

        let selected_ids: Vec<u64> = {
            let mut query = app.world_mut().query::<(&ElementId, &Selected)>();
            query.iter(app.world()).map(|(id, _)| id.0).collect()
        };
        assert_eq!(selected_ids, vec![1]);
    }

    #[test]
    fn select_all_includes_light_objects_when_exposed() {
        let mut app = App::new();
        app.add_plugins(CommandRegistryPlugin)
            .init_resource::<Assets<Image>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(crate::plugins::layers::LayerRegistry::default())
            .insert_resource(SceneLightObjectVisibility { visible: true })
            .insert_resource(crate::plugins::egui_chrome::EguiWantsInput::default());

        app.world_mut().spawn(ElementId(1));
        app.world_mut()
            .spawn((ElementId(2), SceneLightNode::default()));

        queue_command_invocation(app.world_mut(), "core.select_all", Value::Null);
        app.update();

        let mut selected_ids: Vec<u64> = {
            let mut query = app.world_mut().query::<(&ElementId, &Selected)>();
            query.iter(app.world()).map(|(id, _)| id.0).collect()
        };
        selected_ids.sort();
        assert_eq!(selected_ids, vec![1, 2]);
    }
}
