//! Capability-owned request schemas and pure planners. These are transient
//! edit requests; AuthoringScript remains the durable authored representation.
use super::{modifiers::EditPlanDraft, *};

type Planner = dyn Fn(&World, Value) -> Result<EditPlanDraft, String> + Send + Sync;

pub struct EditRequestDescriptor {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub schema: Value,
    planner: Arc<Planner>,
}
impl EditRequestDescriptor {
    pub fn new(
        id: impl Into<String>,
        version: u32,
        description: impl Into<String>,
        schema: Value,
        planner: impl Fn(&World, Value) -> Result<EditPlanDraft, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            version,
            description: description.into(),
            schema,
            planner: Arc::new(planner),
        }
    }
}
#[derive(Resource, Default)]
pub struct EditRequestRegistry {
    entries: BTreeMap<String, EditRequestDescriptor>,
}
impl EditRequestRegistry {
    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }
    pub fn register(&mut self, descriptor: EditRequestDescriptor) -> Result<(), String> {
        if self.entries.contains_key(&descriptor.id) {
            return Err(format!("Duplicate edit request: {}", descriptor.id));
        }
        self.entries.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }
    pub fn descriptors(&self) -> Vec<Value> {
        self.entries.values().map(|d| json!({"request_kind":d.id, "version":d.version,
            "description":d.description, "input_schema":d.schema,
            "preview_tool":"preview_edit_plan", "inspect_tool":"inspect_edit_plan", "apply_tool":"apply_edit_plan"})).collect()
    }
    pub fn prepare(
        &self,
        world: &World,
        kind: &str,
        parameters: Value,
    ) -> Result<EditPlanDraft, String> {
        let descriptor = self
            .entries
            .get(kind)
            .ok_or_else(|| format!("Unknown edit request {kind}; call list_edit_requests"))?;
        (descriptor.planner)(world, parameters)
    }
}

pub fn ensure_authored_base(world: &World) -> Result<(), String> {
    if world
        .get_resource::<crate::plugins::transform::TransformState>()
        .is_some_and(|s| !s.is_idle())
    {
        return Err("Interactive edit active; finish or cancel the gesture before reading an authored base or changing the model.".into());
    }
    if world
        .get_resource::<PendingCommandQueue>()
        .is_some_and(|q| !q.is_empty())
    {
        return Err("Pending history work; retry after the current edit completes.".into());
    }
    Ok(())
}

pub fn preview(
    world: &mut World,
    kind: &str,
    parameters: Value,
) -> Result<Arc<AuthoredEditPlan>, String> {
    ensure_authored_base(world)?;
    let draft = world
        .resource::<EditRequestRegistry>()
        .prepare(world, kind, parameters)?;
    let plan = draft
        .capture(world, None, world.resource::<History>().revision_token())
        .map_err(|e| e.to_string())?;
    world
        .resource_mut::<AuthoredEditPlanRegistry>()
        .publish(plan)
        .map_err(|e| e.to_string())
}

pub fn describe(plan: &AuthoredEditPlan) -> Value {
    json!({"plan_id":plan.plan_id(), "interaction_id":plan.interaction_id(),
        "digest":plan.digest(), "base_model_revision":plan.base_model_revision(),
        "can_commit":plan.can_commit(), "created_ids":plan.created_ids(),
        "removed_ids":plan.removed_ids(), "captured":plan.content(),
        "geometry_reviewed":false})
}

/// Current eligibility is separate from the immutable capture-time verdict.
pub fn inspect(world: &World, plan: &AuthoredEditPlan) -> Value {
    let mut value = describe(plan);
    let current = world.resource::<History>().revision_token();
    value["current_model_revision"] = json!(current);
    value["stale"] = json!(current != *plan.base_model_revision());
    let refusal = ensure_authored_base(world)
        .err()
        .or_else(|| plan.preflight(world).map(|r| r.summary()));
    value["can_apply_now"] = json!(refusal.is_none());
    value["apply_refusal"] = json!(refusal);
    value
}
