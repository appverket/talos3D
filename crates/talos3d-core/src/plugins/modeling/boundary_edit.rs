//! Representation-owned boundary editing shared by viewport and agent commands.
use crate::{
    authored_entity::BoxedEntity,
    capability_registry::{CapabilityRegistry, SelectableSubobjectRef, SubobjectSelection},
    plugins::{
        command_registry::{
            CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
        },
        commands::{capture_delete_snapshots, find_entity_by_element_id_readonly},
        history::{EditorCommand, PendingCommandQueue},
        identity::ElementId,
    },
};
use bevy::prelude::*;
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct BoundaryBinding {
    pub owner: ElementId,
    pub target: ElementId,
    pub key: String,
    pub origin: Vec3,
    pub normal: Vec3,
}
#[derive(Clone)]
pub struct BoundaryPlan {
    pub before: Vec<BoxedEntity>,
    pub after: Vec<BoxedEntity>,
}
#[derive(Clone)]
pub struct BoundaryGesture {
    pub binding: BoundaryBinding,
    pub original: BoxedEntity,
    pub deletion: BoundaryPlan,
    pub presented: Option<BoundaryPlan>,
}
#[derive(Resource, Default)]
pub struct BoundarySession(pub Option<BoundaryGesture>);

pub fn resolve(world: &World, reference: &SelectableSubobjectRef) -> Option<BoundaryBinding> {
    world
        .get_resource::<CapabilityRegistry>()?
        .factories()
        .iter()
        .find_map(|f| f.boundary_binding(world, reference))
}
pub fn capture(world: &World, binding: BoundaryBinding) -> Result<BoundaryGesture, String> {
    let registry = world.resource::<CapabilityRegistry>();
    let entity = find_entity_by_element_id_readonly(world, binding.target)
        .ok_or("Boundary owner no longer exists")?;
    let original = registry
        .capture_snapshot(&world.entity(entity), world)
        .ok_or("Boundary owner has no authored snapshot")?;
    // Validate erase semantics before deriving the shared dependency/aggregate repair.
    original.0.edit_boundary(&binding.key, None)?;
    let ids = registry.expand_delete_ids(world, &[binding.target]);
    let (mut before, repairs, after) = capture_delete_snapshots(world, &ids);
    before.extend(repairs);
    Ok(BoundaryGesture {
        binding,
        original,
        deletion: BoundaryPlan { before, after },
        presented: None,
    })
}
pub fn plan(gesture: &BoundaryGesture, distance: Option<f32>) -> Result<BoundaryPlan, String> {
    if distance.is_some_and(|v| !v.is_finite()) {
        return Err("Distance must be finite".into());
    }
    let after = gesture
        .original
        .0
        .edit_boundary(&gesture.binding.key, distance)?;
    if after.is_empty() {
        Ok(gesture.deletion.clone())
    } else {
        Ok(BoundaryPlan {
            before: vec![gesture.original.clone()],
            after,
        })
    }
}
/// Same complete snapshot transition for preview, history, undo and redo.
pub fn present(world: &mut World, target: &[BoxedEntity], previous: &[BoxedEntity]) {
    for before in previous.iter().rev() {
        if !target.iter().any(|s| s.element_id() == before.element_id()) {
            before.remove_from(world);
        }
    }
    for after in target {
        after.apply_with_previous(
            world,
            previous
                .iter()
                .find(|s| s.element_id() == after.element_id()),
        );
        super::dependency_graph::stamp_authored_entity_dependencies(world, after);
    }
}
impl EditorCommand for BoundaryPlan {
    fn label(&self) -> &'static str {
        "Edit semantic boundary"
    }
    fn apply(&mut self, world: &mut World) {
        present(world, &self.after, &self.before);
    }
    fn undo(&mut self, world: &mut World) {
        present(world, &self.before, &self.after);
    }
}
pub fn enqueue(world: &mut World, plan: BoundaryPlan) {
    world
        .resource_mut::<PendingCommandQueue>()
        .push_command(Box::new(plan));
}
pub fn restore(world: &mut World) {
    let current = world
        .get_resource::<BoundarySession>()
        .and_then(|s| s.0.as_ref())
        .and_then(|s| s.presented.clone());
    if let Some(p) = current {
        present(world, &p.before, &p.after);
    }
}
pub fn preview(world: &mut World, distance: f32) -> Result<Vec<BoxedEntity>, String> {
    let gesture = world
        .resource::<BoundarySession>()
        .0
        .as_ref()
        .ok_or("No boundary gesture")?
        .clone();
    let next = plan(&gesture, Some(distance))?;
    if gesture
        .presented
        .as_ref()
        .is_none_or(|p| p.after != next.after)
    {
        restore(world);
        present(world, &next.after, &next.before);
        world
            .resource_mut::<BoundarySession>()
            .0
            .as_mut()
            .unwrap()
            .presented = Some(next.clone());
    }
    Ok(next.after)
}
pub fn selected_reference(world: &World) -> Option<SelectableSubobjectRef> {
    let refs = &world.get_resource::<SubobjectSelection>()?.refs;
    refs.iter()
        .find(|r| matches!(r, SelectableSubobjectRef::Edge { .. }))
        .or_else(|| refs.first())
        .cloned()
}
fn reference(world: &World, args: &Value) -> Result<SelectableSubobjectRef, String> {
    args.get("reference")
        .map(|v| serde_json::from_value(v.clone()).map_err(|e| e.to_string()))
        .unwrap_or_else(|| {
            selected_reference(world).ok_or("Select a boundary or supply reference".into())
        })
}
pub fn execute(
    world: &mut World,
    reference: SelectableSubobjectRef,
    operation: &str,
    args: &Value,
) -> Result<CommandResult, String> {
    if matches!(operation, "hide" | "show") {
        let SelectableSubobjectRef::Edge { element_id, edge } = reference else {
            return Err("Select an edge".into());
        };
        let id = ElementId(element_id);
        let entity = find_entity_by_element_id_readonly(world, id).ok_or("Object missing")?;
        let before = world
            .resource::<CapabilityRegistry>()
            .capture_snapshot(&world.entity(entity), world)
            .ok_or("No authored object")?;
        let mut overrides = before.subobject_display_overrides().unwrap_or_default();
        overrides.set_edge_hidden(edge, operation == "hide");
        let after =
            before.set_subobject_display_overrides((!overrides.is_empty()).then_some(overrides))?;
        enqueue(
            world,
            BoundaryPlan {
                before: vec![before],
                after: vec![after],
            },
        );
        return Ok(CommandResult {
            modified: vec![id.0],
            ..Default::default()
        });
    }
    let binding = resolve(world, &reference).ok_or(
        "This edge has no unambiguous authored boundary edit; use its owning object's parameters",
    )?;
    let gesture = capture(world, binding)?;
    let distance = match operation {
        "erase" | "delete" | "close" => None,
        "move" | "push_pull" => Some(
            args.get("distance")
                .and_then(Value::as_f64)
                .map(|v| v as f32)
                .or_else(|| {
                    args.get("delta")
                        .and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok())
                        .map(|v| Vec3::from(v).dot(gesture.binding.normal))
                })
                .ok_or("Supply distance in metres or world delta [x,y,z]")?,
        ),
        _ => return Err(format!("Unsupported boundary operation: {operation}")),
    };
    let plan = plan(&gesture, distance)?;
    let result = CommandResult {
        modified: plan.after.iter().map(|s| s.element_id().0).collect(),
        deleted: plan
            .before
            .iter()
            .filter(|b| !plan.after.iter().any(|a| a.element_id() == b.element_id()))
            .map(|s| s.element_id().0)
            .collect(),
        ..Default::default()
    };
    enqueue(world, plan);
    Ok(result)
}
fn command(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let r = reference(world, args)?;
    let op = args
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("move");
    execute(world, r, op, args)
}
fn erase(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let mut a = args.clone();
    a["operation"] = json!("erase");
    command(world, &a)
}
fn hide(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let mut a = args.clone();
    a["operation"] = json!("hide");
    command(world, &a)
}
fn show(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let r = reference(world, args)?;
    let id = ElementId(r.element_id());
    let entity = find_entity_by_element_id_readonly(world, id).ok_or("Object missing")?;
    let before = world
        .resource::<CapabilityRegistry>()
        .capture_snapshot(&world.entity(entity), world)
        .ok_or("No authored object")?;
    let after = before.set_subobject_display_overrides(None)?;
    enqueue(
        world,
        BoundaryPlan {
            before: vec![before],
            after: vec![after],
        },
    );
    Ok(CommandResult::default())
}
fn inspect(world: &mut World, args: &Value) -> Result<CommandResult, String> {
    let r = reference(world, args)?;
    let b = resolve(world, &r).ok_or("No semantic boundary edit for this selection")?;
    let gesture = capture(world, b.clone());
    let erase_error = gesture.as_ref().err().cloned();
    let move_error = gesture
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|g| plan(g, Some(0.)).map(|_| ()))
        .err();
    Ok(CommandResult {
        output: Some(
            json!({"owner":b.owner.0,"target":b.target.0,"boundary":b.key,"origin":b.origin.to_array(),"normal":b.normal.to_array(),"can_erase":erase_error.is_none(),"erase_refusal":erase_error,"can_move":move_error.is_none(),"move_refusal":move_error,"edge_visibility_operations":["hide","show"]}),
        ),
        ..Default::default()
    })
}
pub fn register(app: &mut App) {
    use crate::plugins::command_registry::CommandHandler;
    app.init_resource::<BoundarySession>();
    for (id, label, handler) in [
        (
            "inspect_boundary",
            "Inspect Boundary",
            inspect as CommandHandler,
        ),
        ("edit_boundary", "Edit Boundary", command as CommandHandler),
        (
            "erase_boundary",
            "Close Opening / Erase Feature",
            erase as CommandHandler,
        ),
        ("hide_edge", "Hide Edge", hide as CommandHandler),
        ("show_edges", "Show Object Edges", show as CommandHandler),
    ] {
        app.register_command(CommandDescriptor{id:format!("modeling.{id}"),label:label.into(),description:"Edit the authored feature controlled by a selected generated boundary. Move/push_pull uses signed metres along its normal; erase removes the owning opening/region. Hide/show affect display only. Unsupported or hosted-product boundaries return an explanation.".into(),category:CommandCategory::Edit,parameters:Some(json!({"type":"object","properties":{"reference":{"type":"object"},"operation":{"type":"string","enum":["move","push_pull","erase","hide","show"]},"distance":{"type":"number"},"delta":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3}}})),default_shortcut:None,icon:None,hint:None,requires_selection:false,show_in_menu:!matches!(id,"edit_boundary"|"inspect_boundary"),version:1,activates_tool:None,capability_id:Some("modeling".into())},handler);
    }
}
