use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::plugins::{
    commands::{enqueue_create_definition, enqueue_update_definition},
    history::apply_pending_history_commands,
    modeling::definition::{
        AnchorDef, AxisRef, ChildSlotDef, CompoundDefinition, ConstraintDef, Definition,
        DefinitionId, DefinitionKind, DefinitionLibraryId, DefinitionLibraryRegistry,
        DefinitionRegistry, DerivedParameterDef, EvaluatorDecl, ExprNode, Interface,
        OverridePolicy, ParameterBinding, ParameterDef, ParameterMetadata, ParameterSchema,
        RepresentationDecl, SlotCount, SlotLayout, SlotMultiplicity, TransformBinding,
    },
};

static DRAFT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn allocate_draft_id() -> String {
    let counter = DRAFT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    format!("draft-{timestamp_ms}-{counter}")
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DefinitionDraftId(pub String);

impl DefinitionDraftId {
    pub fn new() -> Self {
        Self(allocate_draft_id())
    }
}

impl Default for DefinitionDraftId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for DefinitionDraftId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionDraft {
    pub draft_id: DefinitionDraftId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_definition_id: Option<DefinitionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_library_id: Option<DefinitionLibraryId>,
    pub working_copy: Definition,
    #[serde(default)]
    pub dirty: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Resource)]
pub struct DefinitionDraftRegistry {
    drafts: HashMap<DefinitionDraftId, DefinitionDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_draft_id: Option<DefinitionDraftId>,
}

impl DefinitionDraftRegistry {
    pub fn list(&self) -> Vec<&DefinitionDraft> {
        self.drafts.values().collect()
    }

    pub fn get(&self, draft_id: &DefinitionDraftId) -> Option<&DefinitionDraft> {
        self.drafts.get(draft_id)
    }

    pub fn get_mut(&mut self, draft_id: &DefinitionDraftId) -> Option<&mut DefinitionDraft> {
        self.drafts.get_mut(draft_id)
    }

    pub fn insert(&mut self, draft: DefinitionDraft) -> DefinitionDraftId {
        let draft_id = draft.draft_id.clone();
        self.drafts.insert(draft_id.clone(), draft);
        self.active_draft_id = Some(draft_id.clone());
        draft_id
    }

    pub fn remove(&mut self, draft_id: &DefinitionDraftId) -> Option<DefinitionDraft> {
        let removed = self.drafts.remove(draft_id);
        if self.active_draft_id.as_ref() == Some(draft_id) {
            self.active_draft_id = self.drafts.keys().next().cloned();
        }
        removed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DefinitionPatch {
    SetName {
        name: String,
    },
    SetDefinitionKind {
        definition_kind: DefinitionKind,
    },
    SetBaseDefinition {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_definition_id: Option<DefinitionId>,
    },
    SetDomainData {
        value: Value,
    },
    SetParameter {
        parameter: ParameterDef,
    },
    SetParameterDefault {
        name: String,
        default_value: Value,
    },
    SetParameterMetadata {
        name: String,
        metadata: ParameterMetadata,
    },
    SetParameterOverridePolicy {
        name: String,
        override_policy: OverridePolicy,
    },
    RemoveParameter {
        name: String,
    },
    SetEvaluators {
        evaluators: Vec<EvaluatorDecl>,
    },
    SetRepresentations {
        representations: Vec<RepresentationDecl>,
    },
    SetDerivedParameter {
        derived_parameter: DerivedParameterDef,
    },
    RemoveDerivedParameter {
        name: String,
    },
    SetConstraint {
        constraint: ConstraintDef,
    },
    RemoveConstraint {
        id: String,
    },
    SetAnchor {
        anchor: AnchorDef,
    },
    RemoveAnchor {
        id: String,
    },
    SetChildSlot {
        child_slot: ChildSlotDef,
    },
    RemoveChildSlot {
        slot_id: String,
    },
    SetChildSlotBinding {
        slot_id: String,
        binding: ParameterBinding,
    },
    RemoveChildSlotBinding {
        slot_id: String,
        target_param: String,
    },
    SetChildSlotTransform {
        slot_id: String,
        transform_binding: TransformBinding,
    },
    SetChildSlotSuppression {
        slot_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suppression_expr: Option<ExprNode>,
    },
    SetChildSlotMultiplicity {
        slot_id: String,
        multiplicity: SlotMultiplicity,
    },
    RemoveChildSlotMultiplicity {
        slot_id: String,
    },
    /// Typed patch that writes (or clears) the architectural material assignment
    /// stored at `domain_data.architectural.material_assignment.material_id`.
    ///
    /// `None` removes the `material_assignment` key entirely.
    /// This replaces the former orphan `Use Glass Material` button and the
    /// free-form `SetDomainData` calls that accompanied it.
    SetDomainDataMaterial {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        material_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionDependencyEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionCompileSummary {
    pub target_id: String,
    pub nodes: Vec<String>,
    pub edges: Vec<DefinitionDependencyEdge>,
    pub child_slot_count: usize,
    #[serde(default)]
    pub collection_slots: Vec<DefinitionCollectionSlotSummary>,
    pub derived_parameter_count: usize,
    pub constraint_count: usize,
    pub anchor_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionCollectionSlotSummary {
    pub slot_id: String,
    pub count: Value,
    pub layout: Value,
}

pub fn create_definition_draft(
    drafts: &mut DefinitionDraftRegistry,
    definition: Definition,
    source_definition_id: Option<DefinitionId>,
    source_library_id: Option<DefinitionLibraryId>,
) -> DefinitionDraftId {
    let draft = DefinitionDraft {
        draft_id: DefinitionDraftId::new(),
        source_definition_id,
        source_library_id,
        working_copy: definition,
        dirty: true,
    };
    drafts.insert(draft)
}

pub fn blank_definition(name: impl Into<String>) -> Definition {
    Definition {
        id: DefinitionId::new(),
        base_definition_id: None,
        name: name.into(),
        definition_kind: DefinitionKind::Solid,
        definition_version: 1,
        interface: Interface {
            parameters: ParameterSchema::default(),
            void_declaration: None,
            external_context_requirements: Vec::new(),
        },
        evaluators: Vec::new(),
        representations: Vec::new(),
        compound: None,
        material_assignment: None,
        domain_data: Value::Null,
    }
}

pub fn resolve_definition_for_authoring(
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    definition_id: &str,
    library_id: Option<&str>,
) -> Result<
    (
        Definition,
        Option<DefinitionId>,
        Option<DefinitionLibraryId>,
        Definition,
    ),
    String,
> {
    let definition_id = DefinitionId(definition_id.to_string());
    if let Some(library_id) = library_id {
        let library_id = DefinitionLibraryId(library_id.to_string());
        let library = libraries
            .get(&library_id)
            .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
        let definition = library.get(&definition_id).cloned().ok_or_else(|| {
            format!(
                "Definition '{}' not found in '{}'",
                definition_id, library_id
            )
        })?;
        let mut preview = definitions.clone();
        for library_definition in library.definitions.values() {
            preview.insert(library_definition.clone());
        }
        let effective = preview.effective_definition(&definition_id)?;
        Ok((definition, None, Some(library_id), effective))
    } else {
        let definition = definitions
            .get(&definition_id)
            .cloned()
            .ok_or_else(|| format!("Definition '{}' not found", definition_id))?;
        let effective = definitions.effective_definition(&definition_id)?;
        Ok((definition, Some(definition_id), None, effective))
    }
}

pub fn derive_definition_from_base(
    base: &Definition,
    effective_base: &Definition,
    name: String,
) -> Definition {
    Definition {
        id: DefinitionId::new(),
        base_definition_id: Some(base.id.clone()),
        name,
        definition_kind: effective_base.definition_kind.clone(),
        definition_version: 1,
        interface: Interface {
            parameters: ParameterSchema::default(),
            void_declaration: None,
            external_context_requirements: Vec::new(),
        },
        evaluators: Vec::new(),
        representations: Vec::new(),
        compound: None,
        material_assignment: None,
        domain_data: Value::Null,
    }
}

pub fn preview_registry_for_draft(
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    draft: &DefinitionDraft,
) -> Result<DefinitionRegistry, String> {
    let mut preview = definitions.clone();
    if let Some(library_id) = draft.source_library_id.as_ref() {
        let library = libraries
            .get(library_id)
            .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;
        for definition in library.definitions.values() {
            preview.insert(definition.clone());
        }
    }
    preview.insert(draft.working_copy.clone());
    Ok(preview)
}

pub fn draft_effective_definition(
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    draft: &DefinitionDraft,
) -> Result<Definition, String> {
    let preview = preview_registry_for_draft(definitions, libraries, draft)?;
    preview.effective_definition(&draft.working_copy.id)
}

pub fn validate_draft(
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    draft: &DefinitionDraft,
) -> Result<Definition, String> {
    let preview = preview_registry_for_draft(definitions, libraries, draft)?;
    preview.validate_definition(&draft.working_copy)?;
    preview.effective_definition(&draft.working_copy.id)
}

pub fn compile_definition_summary(
    preview_registry: &DefinitionRegistry,
    definition: &Definition,
) -> Result<DefinitionCompileSummary, String> {
    let mut preview = preview_registry.clone();
    preview.insert(definition.clone());
    let effective = preview.effective_definition(&definition.id)?;

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for parameter in &effective.interface.parameters.0 {
        nodes.push(format!("param:{}", parameter.name));
    }

    if let Some(compound) = &effective.compound {
        for derived in &compound.derived_parameters {
            let derived_node = format!("derived:{}", derived.name);
            nodes.push(derived_node.clone());
            for dependency in collect_expression_dependencies(&derived.expr, &derived.dependencies)
            {
                edges.push(DefinitionDependencyEdge {
                    from: dependency,
                    to: derived_node.clone(),
                });
            }
        }

        for constraint in &compound.constraints {
            let constraint_node = format!("constraint:{}", constraint.id);
            nodes.push(constraint_node.clone());
            for dependency in
                collect_expression_dependencies(&constraint.expr, &constraint.dependencies)
            {
                edges.push(DefinitionDependencyEdge {
                    from: dependency,
                    to: constraint_node.clone(),
                });
            }
        }

        for anchor in &compound.anchors {
            nodes.push(format!("anchor:{}", anchor.id));
        }

        for slot in &compound.child_slots {
            let slot_node = format!("slot:{}", slot.slot_id);
            nodes.push(slot_node.clone());
            if let Some(translation) = &slot.transform_binding.translation {
                for expr in translation {
                    for dependency in collect_expression_dependencies(expr, &[]) {
                        edges.push(DefinitionDependencyEdge {
                            from: dependency,
                            to: slot_node.clone(),
                        });
                    }
                }
            }
            if let Some(suppression_expr) = &slot.suppression_expr {
                for dependency in collect_expression_dependencies(suppression_expr, &[]) {
                    edges.push(DefinitionDependencyEdge {
                        from: dependency,
                        to: slot_node.clone(),
                    });
                }
            }
            for binding in &slot.parameter_bindings {
                let binding_node =
                    format!("slot:{}:binding:{}", slot.slot_id, binding.target_param);
                nodes.push(binding_node.clone());
                edges.push(DefinitionDependencyEdge {
                    from: binding_node.clone(),
                    to: slot_node.clone(),
                });
                for dependency in collect_expression_dependencies(&binding.expr, &[]) {
                    edges.push(DefinitionDependencyEdge {
                        from: dependency,
                        to: binding_node.clone(),
                    });
                }
            }
        }

        let collection_slots = compound
            .child_slots
            .iter()
            .filter_map(collection_slot_summary)
            .collect::<Vec<_>>();

        nodes.sort();
        nodes.dedup();
        dedup_edges(&mut edges);

        return Ok(DefinitionCompileSummary {
            target_id: definition.id.to_string(),
            nodes,
            edges,
            child_slot_count: compound.child_slots.len(),
            collection_slots,
            derived_parameter_count: compound.derived_parameters.len(),
            constraint_count: compound.constraints.len(),
            anchor_count: compound.anchors.len(),
        });
    }

    nodes.sort();
    nodes.dedup();
    Ok(DefinitionCompileSummary {
        target_id: definition.id.to_string(),
        nodes,
        edges,
        child_slot_count: 0,
        collection_slots: Vec::new(),
        derived_parameter_count: 0,
        constraint_count: 0,
        anchor_count: 0,
    })
}

fn collection_slot_summary(slot: &ChildSlotDef) -> Option<DefinitionCollectionSlotSummary> {
    match &slot.multiplicity {
        SlotMultiplicity::Single => None,
        SlotMultiplicity::Collection { layout, count } => Some(DefinitionCollectionSlotSummary {
            slot_id: slot.slot_id.clone(),
            count: serde_json::to_value(count).unwrap_or(Value::Null),
            layout: serde_json::to_value(layout).unwrap_or(Value::Null),
        }),
    }
}

fn dedup_edges(edges: &mut Vec<DefinitionDependencyEdge>) {
    let mut seen = HashSet::new();
    edges.retain(|edge| seen.insert((edge.from.clone(), edge.to.clone())));
}

fn collect_expression_dependencies(expr: &ExprNode, declared: &[String]) -> Vec<String> {
    let mut dependencies = declared.iter().cloned().collect::<HashSet<_>>();
    collect_expression_dependencies_into(expr, &mut dependencies);
    dependencies.into_iter().collect()
}

fn collect_expression_dependencies_into(expr: &ExprNode, dependencies: &mut HashSet<String>) {
    match expr {
        ExprNode::Literal { .. } => {}
        ExprNode::ParamRef { path } => {
            dependencies.insert(path.clone());
        }
        ExprNode::Add { left, right }
        | ExprNode::Sub { left, right }
        | ExprNode::Mul { left, right }
        | ExprNode::Div { left, right }
        | ExprNode::Min { left, right }
        | ExprNode::Max { left, right }
        | ExprNode::Eq { left, right }
        | ExprNode::Gt { left, right }
        | ExprNode::Lt { left, right } => {
            collect_expression_dependencies_into(left, dependencies);
            collect_expression_dependencies_into(right, dependencies);
        }
        ExprNode::And { nodes } => {
            for node in nodes {
                collect_expression_dependencies_into(node, dependencies);
            }
        }
        ExprNode::IfElse {
            condition,
            when_true,
            when_false,
        } => {
            collect_expression_dependencies_into(condition, dependencies);
            collect_expression_dependencies_into(when_true, dependencies);
            collect_expression_dependencies_into(when_false, dependencies);
        }
    }
}

pub fn explain_definition(
    preview_registry: &DefinitionRegistry,
    raw_definition: &Definition,
) -> Result<Value, String> {
    let mut preview = preview_registry.clone();
    preview.insert(raw_definition.clone());
    let effective = preview.effective_definition(&raw_definition.id)?;
    let compile = compile_definition_summary(preview_registry, raw_definition)?;

    let local_parameters = raw_definition
        .interface
        .parameters
        .0
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>();
    let inherited_parameters = effective
        .interface
        .parameters
        .0
        .iter()
        .map(|parameter| parameter.name.clone())
        .filter(|name| !local_parameters.contains(name))
        .collect::<Vec<_>>();
    let local_slots = raw_definition
        .compound
        .as_ref()
        .map(|compound| {
            compound
                .child_slots
                .iter()
                .map(|slot| slot.slot_id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let inherited_slots = effective
        .compound
        .as_ref()
        .map(|compound| {
            compound
                .child_slots
                .iter()
                .map(|slot| slot.slot_id.clone())
                .filter(|slot_id| !local_slots.contains(slot_id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let resolved_collection_slots = explain_resolved_collection_slots(&effective);

    Ok(json!({
        "raw_full": raw_definition,
        "effective_full": effective,
        "local_parameter_names": local_parameters,
        "inherited_parameter_names": inherited_parameters,
        "local_child_slot_ids": local_slots,
        "inherited_child_slot_ids": inherited_slots,
        "resolved_collection_slots": resolved_collection_slots,
        "compile": compile,
    }))
}

fn explain_resolved_collection_slots(definition: &Definition) -> Vec<Value> {
    let Some(compound) = &definition.compound else {
        return Vec::new();
    };
    let mut values = definition
        .interface
        .parameters
        .0
        .iter()
        .map(|param| (param.name.clone(), param.default_value.clone()))
        .collect::<HashMap<_, _>>();
    for derived in &compound.derived_parameters {
        if let Ok(value) = evaluate_authoring_expr(&derived.expr, &values) {
            values.insert(derived.name.clone(), value);
        }
    }

    compound
        .child_slots
        .iter()
        .filter_map(|slot| match &slot.multiplicity {
            SlotMultiplicity::Single => None,
            SlotMultiplicity::Collection { layout, count } => Some(
                match explain_collection_slot(slot, layout, count, &values) {
                    Ok(value) => value,
                    Err(error) => json!({
                        "slot_id": slot.slot_id,
                        "resolution_error": error,
                    }),
                },
            ),
        })
        .collect()
}

fn explain_collection_slot(
    slot: &ChildSlotDef,
    layout: &SlotLayout,
    count: &SlotCount,
    values: &HashMap<String, Value>,
) -> Result<Value, String> {
    let instances = resolve_collection_instance_translations(slot, layout, count, values)?;
    Ok(json!({
        "slot_id": slot.slot_id,
        "count": instances.len(),
        "layout": serde_json::to_value(layout).unwrap_or(Value::Null),
        "instances": instances
            .into_iter()
            .map(|(index, translation)| json!({
                "index": index,
                "slot_path": format!("{}[{}]", slot.slot_id, index),
                "translation": [translation[0], translation[1], translation[2]],
            }))
            .collect::<Vec<_>>(),
    }))
}

fn resolve_collection_instance_translations(
    slot: &ChildSlotDef,
    layout: &SlotLayout,
    count: &SlotCount,
    values: &HashMap<String, Value>,
) -> Result<Vec<(usize, [f32; 3])>, String> {
    let requested_count = resolve_authoring_slot_count(&slot.slot_id, count, values)?;
    match layout {
        SlotLayout::Linear {
            axis,
            spacing,
            origin,
        } => {
            let axis = authoring_axis_vector(axis)?;
            let spacing = evaluate_authoring_expr_f32(spacing, values)?;
            let origin = evaluate_authoring_transform_binding(origin, &slot.slot_id, values)?;
            Ok((0..requested_count)
                .map(|index| {
                    (
                        index,
                        add_vec3(origin, scale_vec3(axis, spacing * (index as f32 + 1.0))),
                    )
                })
                .collect())
        }
        SlotLayout::Grid {
            axis_u,
            count_u,
            spacing_u,
            axis_v,
            count_v,
            spacing_v,
            origin,
        } => {
            let u_count = resolve_authoring_expr_count(&slot.slot_id, "count_u", count_u, values)?;
            let v_count = resolve_authoring_expr_count(&slot.slot_id, "count_v", count_v, values)?;
            let grid_count = u_count.checked_mul(v_count).ok_or_else(|| {
                format!(
                    "Collection slot '{}' grid count overflows usize",
                    slot.slot_id
                )
            })?;
            if requested_count != grid_count {
                return Err(format!(
                    "Collection slot '{}' count ({requested_count}) must match layout count ({grid_count})",
                    slot.slot_id
                ));
            }
            let axis_u = authoring_axis_vector(axis_u)?;
            let axis_v = authoring_axis_vector(axis_v)?;
            let spacing_u = evaluate_authoring_expr_f32(spacing_u, values)?;
            let spacing_v = evaluate_authoring_expr_f32(spacing_v, values)?;
            let origin = evaluate_authoring_transform_binding(origin, &slot.slot_id, values)?;
            let mut instances = Vec::with_capacity(grid_count);
            for v in 0..v_count {
                for u in 0..u_count {
                    let index = v * u_count + u;
                    let translation = add_vec3(
                        add_vec3(origin, scale_vec3(axis_u, spacing_u * (u as f32 + 1.0))),
                        scale_vec3(axis_v, spacing_v * (v as f32 + 1.0)),
                    );
                    instances.push((index, translation));
                }
            }
            Ok(instances)
        }
        SlotLayout::BySpacingFromHost { host_param, axis } => {
            let axis = authoring_axis_vector(axis)?;
            let spacing = values
                .get(&host_param.name)
                .and_then(Value::as_f64)
                .ok_or_else(|| {
                    format!(
                        "Collection slot '{}' host spacing parameter '{}' must resolve to a number",
                        slot.slot_id, host_param.name
                    )
                })? as f32;
            Ok((0..requested_count)
                .map(|index| (index, scale_vec3(axis, spacing * (index as f32 + 1.0))))
                .collect())
        }
        SlotLayout::LitePattern { pattern } => {
            let pattern = evaluate_authoring_expr(pattern, values)?;
            let pattern = pattern.as_str().ok_or_else(|| {
                format!(
                    "Collection slot '{}' lite pattern must resolve to a string",
                    slot.slot_id
                )
            })?;
            let (u_count, v_count) = parse_authoring_lite_pattern_dims(&slot.slot_id, pattern)?;
            let pattern_count = u_count.checked_mul(v_count).ok_or_else(|| {
                format!(
                    "Collection slot '{}' lite pattern count overflows usize",
                    slot.slot_id
                )
            })?;
            if requested_count != pattern_count {
                return Err(format!(
                    "Collection slot '{}' count ({requested_count}) must match layout count ({pattern_count})",
                    slot.slot_id
                ));
            }
            let mut instances = Vec::with_capacity(pattern_count);
            for v in 0..v_count {
                for u in 0..u_count {
                    let index = v * u_count + u;
                    instances.push((index, [u as f32, v as f32, 0.0]));
                }
            }
            Ok(instances)
        }
    }
}

fn resolve_authoring_slot_count(
    slot_id: &str,
    count: &SlotCount,
    values: &HashMap<String, Value>,
) -> Result<usize, String> {
    match count {
        SlotCount::Fixed(count) => usize::try_from(*count).map_err(|_| {
            format!("Collection slot '{slot_id}' fixed count does not fit this platform")
        }),
        SlotCount::DerivedFromExpr(expr) => {
            resolve_authoring_expr_count(slot_id, "count", expr, values)
        }
    }
}

fn resolve_authoring_expr_count(
    slot_id: &str,
    field: &str,
    expr: &ExprNode,
    values: &HashMap<String, Value>,
) -> Result<usize, String> {
    let value = evaluate_authoring_expr_f64(expr, values)?;
    if value < 0.0 || value.fract().abs() > f64::EPSILON {
        return Err(format!(
            "Collection slot '{slot_id}' {field} must resolve to a non-negative integer"
        ));
    }
    if value > usize::MAX as f64 {
        return Err(format!(
            "Collection slot '{slot_id}' {field} is too large for this platform"
        ));
    }
    Ok(value as usize)
}

fn evaluate_authoring_transform_binding(
    binding: &TransformBinding,
    slot_id: &str,
    values: &HashMap<String, Value>,
) -> Result<[f32; 3], String> {
    let Some(translation) = &binding.translation else {
        return Ok([0.0, 0.0, 0.0]);
    };
    if translation.len() != 3 {
        return Err(format!(
            "Child slot '{slot_id}' collection origin must contain exactly 3 expressions"
        ));
    }
    Ok([
        evaluate_authoring_expr_f32(&translation[0], values)?,
        evaluate_authoring_expr_f32(&translation[1], values)?,
        evaluate_authoring_expr_f32(&translation[2], values)?,
    ])
}

fn authoring_axis_vector(axis: &AxisRef) -> Result<[f32; 3], String> {
    match axis.0.as_str() {
        "x" | "X" | "u" | "U" | "horizontal" => Ok([1.0, 0.0, 0.0]),
        "y" | "Y" | "v" | "V" | "vertical" => Ok([0.0, 1.0, 0.0]),
        "z" | "Z" | "w" | "W" | "depth" => Ok([0.0, 0.0, 1.0]),
        other => Err(format!(
            "Unknown collection slot axis '{other}'; expected x/y/z or u/v/w"
        )),
    }
}

fn parse_authoring_lite_pattern_dims(
    slot_id: &str,
    pattern: &str,
) -> Result<(usize, usize), String> {
    let normalized = pattern.trim().replace(['X', '*'], "x").replace('×', "x");
    let Some((u, v)) = normalized.split_once('x') else {
        return Err(format!(
            "Collection slot '{slot_id}' lite pattern '{pattern}' must use '<columns>x<rows>'"
        ));
    };
    let u = u.trim().parse::<usize>().map_err(|_| {
        format!("Collection slot '{slot_id}' lite pattern '{pattern}' has invalid column count")
    })?;
    let v = v.trim().parse::<usize>().map_err(|_| {
        format!("Collection slot '{slot_id}' lite pattern '{pattern}' has invalid row count")
    })?;
    if u == 0 || v == 0 {
        return Err(format!(
            "Collection slot '{slot_id}' lite pattern '{pattern}' must have non-zero dimensions"
        ));
    }
    Ok((u, v))
}

fn evaluate_authoring_expr(
    expr: &ExprNode,
    values: &HashMap<String, Value>,
) -> Result<Value, String> {
    match expr {
        ExprNode::Literal { value } => Ok(value.clone()),
        ExprNode::ParamRef { path } => lookup_authoring_expr_value(path, values),
        ExprNode::Add { left, right } => Ok(Value::from(
            evaluate_authoring_expr_f64(left, values)?
                + evaluate_authoring_expr_f64(right, values)?,
        )),
        ExprNode::Sub { left, right } => Ok(Value::from(
            evaluate_authoring_expr_f64(left, values)?
                - evaluate_authoring_expr_f64(right, values)?,
        )),
        ExprNode::Mul { left, right } => Ok(Value::from(
            evaluate_authoring_expr_f64(left, values)?
                * evaluate_authoring_expr_f64(right, values)?,
        )),
        ExprNode::Div { left, right } => Ok(Value::from(
            evaluate_authoring_expr_f64(left, values)?
                / evaluate_authoring_expr_f64(right, values)?,
        )),
        ExprNode::Min { left, right } => Ok(Value::from(
            evaluate_authoring_expr_f64(left, values)?
                .min(evaluate_authoring_expr_f64(right, values)?),
        )),
        ExprNode::Max { left, right } => Ok(Value::from(
            evaluate_authoring_expr_f64(left, values)?
                .max(evaluate_authoring_expr_f64(right, values)?),
        )),
        ExprNode::Eq { left, right } => Ok(Value::Bool(
            evaluate_authoring_expr(left, values)? == evaluate_authoring_expr(right, values)?,
        )),
        ExprNode::Gt { left, right } => Ok(Value::Bool(
            evaluate_authoring_expr_f64(left, values)?
                > evaluate_authoring_expr_f64(right, values)?,
        )),
        ExprNode::Lt { left, right } => Ok(Value::Bool(
            evaluate_authoring_expr_f64(left, values)?
                < evaluate_authoring_expr_f64(right, values)?,
        )),
        ExprNode::And { nodes } => {
            for node in nodes {
                if !evaluate_authoring_expr(node, values)?
                    .as_bool()
                    .ok_or_else(|| "expression must evaluate to a boolean value".to_string())?
                {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        ExprNode::IfElse {
            condition,
            when_true,
            when_false,
        } => {
            if evaluate_authoring_expr(condition, values)?
                .as_bool()
                .ok_or_else(|| "expression must evaluate to a boolean value".to_string())?
            {
                evaluate_authoring_expr(when_true, values)
            } else {
                evaluate_authoring_expr(when_false, values)
            }
        }
    }
}

fn evaluate_authoring_expr_f64(
    expr: &ExprNode,
    values: &HashMap<String, Value>,
) -> Result<f64, String> {
    evaluate_authoring_expr(expr, values)?
        .as_f64()
        .ok_or_else(|| "expression must evaluate to a numeric value".to_string())
}

fn evaluate_authoring_expr_f32(
    expr: &ExprNode,
    values: &HashMap<String, Value>,
) -> Result<f32, String> {
    Ok(evaluate_authoring_expr_f64(expr, values)? as f32)
}

fn lookup_authoring_expr_value(
    path: &str,
    values: &HashMap<String, Value>,
) -> Result<Value, String> {
    if let Some(value) = values.get(path) {
        return Ok(value.clone());
    }
    if let Some(last_segment) = path.rsplit('.').next() {
        if let Some(value) = values.get(last_segment) {
            return Ok(value.clone());
        }
    }
    Err(format!("Expression references unknown parameter '{path}'"))
}

fn add_vec3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn scale_vec3(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

pub fn apply_patch_to_draft(
    definitions: &DefinitionRegistry,
    libraries: &DefinitionLibraryRegistry,
    drafts: &mut DefinitionDraftRegistry,
    draft_id: &DefinitionDraftId,
    patch: DefinitionPatch,
) -> Result<(), String> {
    let (effective_before, working_copy_id) = {
        let draft = drafts
            .get(draft_id)
            .ok_or_else(|| format!("Definition draft '{}' not found", draft_id))?;
        (
            draft_effective_definition(definitions, libraries, draft)?,
            draft.working_copy.id.clone(),
        )
    };

    let draft = drafts
        .get_mut(draft_id)
        .ok_or_else(|| format!("Definition draft '{}' not found", draft_id))?;
    if draft.working_copy.id != working_copy_id {
        return Err("Draft identity changed unexpectedly".to_string());
    }

    apply_patch_to_definition(&mut draft.working_copy, &effective_before, patch)?;
    draft.dirty = true;
    Ok(())
}

pub fn publish_draft(
    world: &mut World,
    draft_id: &DefinitionDraftId,
) -> Result<Definition, String> {
    let draft = world
        .resource::<DefinitionDraftRegistry>()
        .get(draft_id)
        .cloned()
        .ok_or_else(|| format!("Definition draft '{}' not found", draft_id))?;

    ensure_draft_dependencies_available(world, &draft)?;

    let effective = {
        let registry = world.resource::<DefinitionRegistry>();
        let libraries = world.resource::<DefinitionLibraryRegistry>();
        validate_draft(registry, libraries, &draft)?
    };

    if let Some(source_definition_id) = &draft.source_definition_id {
        let before = world
            .resource::<DefinitionRegistry>()
            .get(source_definition_id)
            .cloned()
            .ok_or_else(|| format!("Definition '{}' not found", source_definition_id))?;
        let mut after = draft.working_copy.clone();
        after.definition_version = before.definition_version + 1;
        enqueue_update_definition(world, before, after.clone());
        apply_pending_history_commands(world);
        if let Some(mut drafts) = world.get_resource_mut::<DefinitionDraftRegistry>() {
            if let Some(existing) = drafts.get_mut(draft_id) {
                existing.working_copy = after.clone();
                existing.dirty = false;
            }
        }
        Ok(after)
    } else {
        let created = draft.working_copy.clone();
        enqueue_create_definition(world, created.clone());
        apply_pending_history_commands(world);
        if let Some(mut drafts) = world.get_resource_mut::<DefinitionDraftRegistry>() {
            if let Some(existing) = drafts.get_mut(draft_id) {
                existing.source_definition_id = Some(created.id.clone());
                existing.working_copy = created.clone();
                existing.dirty = false;
            }
        }
        Ok(effective)
    }
}

fn ensure_draft_dependencies_available(
    world: &mut World,
    draft: &DefinitionDraft,
) -> Result<(), String> {
    let Some(library_id) = draft.source_library_id.as_ref() else {
        return Ok(());
    };
    let library = world
        .resource::<DefinitionLibraryRegistry>()
        .get(library_id)
        .cloned()
        .ok_or_else(|| format!("Definition library '{}' not found", library_id))?;

    let mut to_import = Vec::new();
    collect_definition_dependencies(
        draft.working_copy.base_definition_id.as_ref(),
        draft.working_copy.compound.as_ref(),
        &library,
        &mut to_import,
    );

    for dependency in to_import {
        let already_present = {
            let definitions = world.resource::<DefinitionRegistry>();
            definitions.get(&dependency.id).is_some()
        };
        if !already_present && dependency.id != draft.working_copy.id {
            enqueue_create_definition(world, dependency);
        }
    }
    apply_pending_history_commands(world);
    Ok(())
}

fn collect_definition_dependencies(
    base_definition_id: Option<&DefinitionId>,
    compound: Option<&CompoundDefinition>,
    library: &crate::plugins::modeling::definition::DefinitionLibrary,
    output: &mut Vec<Definition>,
) {
    let mut stack = Vec::new();
    if let Some(base_definition_id) = base_definition_id {
        if let Some(definition) = library.get(base_definition_id).cloned() {
            stack.push(definition);
        }
    }
    if let Some(compound) = compound {
        for slot in &compound.child_slots {
            if let Some(definition) = library.get(&slot.definition_id).cloned() {
                stack.push(definition);
            }
        }
    }
    let mut seen = HashSet::new();
    while let Some(definition) = stack.pop() {
        if !seen.insert(definition.id.clone()) {
            continue;
        }
        if let Some(base_definition_id) = &definition.base_definition_id {
            if let Some(base_definition) = library.get(base_definition_id).cloned() {
                stack.push(base_definition);
            }
        }
        if let Some(compound) = &definition.compound {
            for slot in &compound.child_slots {
                if let Some(child) = library.get(&slot.definition_id).cloned() {
                    stack.push(child);
                }
            }
        }
        output.push(definition);
    }
}

fn apply_patch_to_definition(
    definition: &mut Definition,
    effective_before: &Definition,
    patch: DefinitionPatch,
) -> Result<(), String> {
    match patch {
        DefinitionPatch::SetName { name } => {
            definition.name = name;
        }
        DefinitionPatch::SetDefinitionKind { definition_kind } => {
            definition.definition_kind = definition_kind;
        }
        DefinitionPatch::SetBaseDefinition { base_definition_id } => {
            definition.base_definition_id = base_definition_id;
        }
        DefinitionPatch::SetDomainData { value } => {
            definition.domain_data = value;
        }
        DefinitionPatch::SetParameter { parameter } => {
            upsert_parameter(&mut definition.interface.parameters.0, parameter);
        }
        DefinitionPatch::SetParameterDefault {
            name,
            default_value,
        } => {
            let parameter = ensure_local_parameter(definition, effective_before, &name)?;
            parameter.default_value = default_value;
        }
        DefinitionPatch::SetParameterMetadata { name, metadata } => {
            let parameter = ensure_local_parameter(definition, effective_before, &name)?;
            parameter.metadata = metadata;
        }
        DefinitionPatch::SetParameterOverridePolicy {
            name,
            override_policy,
        } => {
            let parameter = ensure_local_parameter(definition, effective_before, &name)?;
            parameter.override_policy = override_policy;
        }
        DefinitionPatch::RemoveParameter { name } => {
            definition
                .interface
                .parameters
                .0
                .retain(|entry| entry.name != name);
        }
        DefinitionPatch::SetEvaluators { evaluators } => {
            definition.evaluators = evaluators;
        }
        DefinitionPatch::SetRepresentations { representations } => {
            definition.representations = representations;
        }
        DefinitionPatch::SetDerivedParameter { derived_parameter } => {
            let compound = ensure_local_compound(definition);
            upsert_named(
                &mut compound.derived_parameters,
                derived_parameter,
                |entry| entry.name.clone(),
            );
        }
        DefinitionPatch::RemoveDerivedParameter { name } => {
            if let Some(compound) = definition.compound.as_mut() {
                compound
                    .derived_parameters
                    .retain(|entry| entry.name != name);
            }
        }
        DefinitionPatch::SetConstraint { constraint } => {
            let compound = ensure_local_compound(definition);
            upsert_named(&mut compound.constraints, constraint, |entry| {
                entry.id.clone()
            });
        }
        DefinitionPatch::RemoveConstraint { id } => {
            if let Some(compound) = definition.compound.as_mut() {
                compound.constraints.retain(|entry| entry.id != id);
            }
        }
        DefinitionPatch::SetAnchor { anchor } => {
            let compound = ensure_local_compound(definition);
            upsert_named(&mut compound.anchors, anchor, |entry| entry.id.clone());
        }
        DefinitionPatch::RemoveAnchor { id } => {
            if let Some(compound) = definition.compound.as_mut() {
                compound.anchors.retain(|entry| entry.id != id);
            }
        }
        DefinitionPatch::SetChildSlot { child_slot } => {
            let compound = ensure_local_compound(definition);
            upsert_named(&mut compound.child_slots, child_slot, |entry| {
                entry.slot_id.clone()
            });
        }
        DefinitionPatch::RemoveChildSlot { slot_id } => {
            if let Some(compound) = definition.compound.as_mut() {
                compound
                    .child_slots
                    .retain(|entry| entry.slot_id != slot_id);
            }
        }
        DefinitionPatch::SetChildSlotBinding { slot_id, binding } => {
            let slot = ensure_local_child_slot(definition, effective_before, &slot_id)?;
            upsert_named(&mut slot.parameter_bindings, binding, |entry| {
                entry.target_param.clone()
            });
        }
        DefinitionPatch::RemoveChildSlotBinding {
            slot_id,
            target_param,
        } => {
            let slot = ensure_local_child_slot(definition, effective_before, &slot_id)?;
            slot.parameter_bindings
                .retain(|binding| binding.target_param != target_param);
        }
        DefinitionPatch::SetChildSlotTransform {
            slot_id,
            transform_binding,
        } => {
            let slot = ensure_local_child_slot(definition, effective_before, &slot_id)?;
            slot.transform_binding = transform_binding;
        }
        DefinitionPatch::SetChildSlotSuppression {
            slot_id,
            suppression_expr,
        } => {
            let slot = ensure_local_child_slot(definition, effective_before, &slot_id)?;
            slot.suppression_expr = suppression_expr;
        }
        DefinitionPatch::SetChildSlotMultiplicity {
            slot_id,
            multiplicity,
        } => {
            let slot = ensure_local_child_slot(definition, effective_before, &slot_id)?;
            slot.multiplicity = multiplicity;
            slot.validate_multiplicity()
                .map_err(|error| error.to_string())?;
        }
        DefinitionPatch::RemoveChildSlotMultiplicity { slot_id } => {
            let slot = ensure_local_child_slot(definition, effective_before, &slot_id)?;
            slot.multiplicity = SlotMultiplicity::Single;
        }
        DefinitionPatch::SetDomainDataMaterial { material_id } => {
            // Ensure domain_data is an object.
            if !definition.domain_data.is_object() {
                definition.domain_data = json!({});
            }
            let root = definition
                .domain_data
                .as_object_mut()
                .expect("domain_data is an object");

            match material_id {
                Some(id) => {
                    // Ensure domain_data.architectural exists and is an object.
                    let arch = root
                        .entry("architectural")
                        .or_insert_with(|| json!({}));
                    if !arch.is_object() {
                        *arch = json!({});
                    }
                    arch.as_object_mut()
                        .expect("architectural is an object")
                        .insert("material_assignment".to_string(), json!({ "material_id": id }));
                }
                None => {
                    // Remove the key from architectural if it exists.
                    if let Some(arch) = root.get_mut("architectural") {
                        if let Some(obj) = arch.as_object_mut() {
                            obj.remove("material_assignment");
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn upsert_parameter(parameters: &mut Vec<ParameterDef>, parameter: ParameterDef) {
    if let Some(existing) = parameters
        .iter_mut()
        .find(|entry| entry.name == parameter.name)
    {
        *existing = parameter;
    } else {
        parameters.push(parameter);
    }
}

fn upsert_named<T, F>(entries: &mut Vec<T>, value: T, key: F)
where
    F: Fn(&T) -> String,
{
    let value_key = key(&value);
    if let Some(existing) = entries.iter_mut().find(|entry| key(entry) == value_key) {
        *existing = value;
    } else {
        entries.push(value);
    }
}

fn ensure_local_compound(definition: &mut Definition) -> &mut CompoundDefinition {
    definition
        .compound
        .get_or_insert_with(CompoundDefinition::default)
}

fn ensure_local_parameter<'a>(
    definition: &'a mut Definition,
    effective_before: &Definition,
    name: &str,
) -> Result<&'a mut ParameterDef, String> {
    if definition.interface.parameters.get(name).is_none() {
        let parameter = effective_before
            .interface
            .parameters
            .get(name)
            .cloned()
            .ok_or_else(|| format!("Parameter '{}' not found", name))?;
        definition.interface.parameters.0.push(parameter);
    }
    definition
        .interface
        .parameters
        .0
        .iter_mut()
        .find(|entry| entry.name == name)
        .ok_or_else(|| format!("Parameter '{}' not found", name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a standalone draft from a blank definition and apply a
    /// single patch, returning the resulting `Definition`.
    fn apply_single_patch(patch: DefinitionPatch) -> Result<Definition, String> {
        let definition = blank_definition("Test");
        let draft = DefinitionDraft {
            draft_id: DefinitionDraftId::new(),
            source_definition_id: None,
            source_library_id: None,
            working_copy: definition,
            dirty: false,
        };
        let mut drafts = DefinitionDraftRegistry::default();
        let draft_id = drafts.insert(draft);
        let definitions = DefinitionRegistry::default();
        let libraries = DefinitionLibraryRegistry::default();
        apply_patch_to_draft(&definitions, &libraries, &mut drafts, &draft_id, patch)?;
        Ok(drafts
            .get(&draft_id)
            .expect("draft still present after patch")
            .working_copy
            .clone())
    }

    #[test]
    fn set_domain_data_material_patch_round_trips() {
        // Set a non-None material_id.
        let result = apply_single_patch(DefinitionPatch::SetDomainDataMaterial {
            material_id: Some("builtin.glass.blue_tint_glazing_80".to_string()),
        })
        .expect("patch should succeed");

        let material_id = result
            .domain_data
            .get("architectural")
            .and_then(|a| a.get("material_assignment"))
            .and_then(|ma| ma.get("material_id"))
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            material_id,
            Some("builtin.glass.blue_tint_glazing_80"),
            "material_id should round-trip through the patch"
        );

        // Now clear it with None.
        let mut drafts = DefinitionDraftRegistry::default();
        let definitions = DefinitionRegistry::default();
        let libraries = DefinitionLibraryRegistry::default();
        let draft = DefinitionDraft {
            draft_id: DefinitionDraftId::new(),
            source_definition_id: None,
            source_library_id: None,
            working_copy: result,
            dirty: false,
        };
        let draft_id = drafts.insert(draft);
        apply_patch_to_draft(
            &definitions,
            &libraries,
            &mut drafts,
            &draft_id,
            DefinitionPatch::SetDomainDataMaterial { material_id: None },
        )
        .expect("clearing patch should succeed");

        let cleared = &drafts.get(&draft_id).unwrap().working_copy;
        let removed = cleared
            .domain_data
            .get("architectural")
            .and_then(|a| a.get("material_assignment"));
        assert!(
            removed.is_none(),
            "material_assignment should be removed after None patch"
        );
    }

    #[test]
    fn glass_material_orphan_is_gone() {
        // Structural assertion: the source text of definition_browser.rs must
        // not contain the literal "Use Glass Material".  This is the cheapest
        // enforcement of the DEFINITION_BROWSER_UX_AGREEMENT.md hard rule:
        // "The orphan `Use Glass Material` affordance is removed."
        //
        // The test reads the compiled-in source path via env! so it works in
        // CI without any runtime filesystem assumptions beyond the normal cargo
        // source tree.
        let source = include_str!("./definition_browser.rs");
        assert!(
            !source.contains("Use Glass Material"),
            "definition_browser.rs must not contain \"Use Glass Material\" (PP-DBUX5 agreement)"
        );
    }
}

fn ensure_local_child_slot<'a>(
    definition: &'a mut Definition,
    effective_before: &Definition,
    slot_id: &str,
) -> Result<&'a mut ChildSlotDef, String> {
    let has_local = definition
        .compound
        .as_ref()
        .map(|compound| {
            compound
                .child_slots
                .iter()
                .any(|slot| slot.slot_id == slot_id)
        })
        .unwrap_or(false);
    if !has_local {
        let slot = effective_before
            .compound
            .as_ref()
            .and_then(|compound| {
                compound
                    .child_slots
                    .iter()
                    .find(|slot| slot.slot_id == slot_id)
                    .cloned()
            })
            .ok_or_else(|| format!("Child slot '{}' not found", slot_id))?;
        ensure_local_compound(definition).child_slots.push(slot);
    }
    ensure_local_compound(definition)
        .child_slots
        .iter_mut()
        .find(|slot| slot.slot_id == slot_id)
        .ok_or_else(|| format!("Child slot '{}' not found", slot_id))
}
