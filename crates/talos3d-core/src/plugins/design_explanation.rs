//! Bounded read projection shared by the property inspector and MCP. No stored
//! explanation graph, prose-to-edit parser, geometry, or provider dependency.
use crate::{
    authored_entity::BoxedEntity,
    capability_registry::{CapabilityRegistry, ElementClassAssignment},
    plugins::{
        corpus_gap::CorpusPassageRegistry,
        history::{History, ModelRevision},
        identity::ElementId,
        modeling::{
            definition::{DefinitionRegistry, OverridePolicy, ParameterMutability},
            dependency_graph::EntityDependencies,
            group::GroupMembers,
            occurrence::OccurrenceIdentity,
        },
        refinement::{
            AuthoringProvenance, ClaimGrounding, Grounding, ObligationSet, ObligationStatus,
            PassageRef, RefinementStateComponent, SemanticIntent,
        },
        validation::Findings,
    },
};
use bevy::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};

pub const MAX_ROWS: usize = 24;
const MAX_TEXT: usize = 512;
#[derive(Debug, Clone, Serialize)]
pub struct ExplanationRow {
    pub label: String,
    pub text: String,
    pub details: Value,
}
#[derive(Debug, Clone, Serialize)]
pub struct ExplanationSection {
    pub title: String,
    pub rows: Vec<ExplanationRow>,
    pub omitted: usize,
}
impl ExplanationSection {
    fn new(title: &str) -> Self {
        Self {
            title: title.into(),
            rows: vec![],
            omitted: 0,
        }
    }
    fn add(&mut self, label: impl AsRef<str>, text: impl AsRef<str>, details: Value) {
        if self.rows.len() == MAX_ROWS {
            self.omitted += 1;
            return;
        }
        self.rows.push(ExplanationRow {
            label: clip(label.as_ref()),
            text: clip(text.as_ref()),
            details: bounded_details(details),
        });
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct DesignExplanation {
    pub element_id: u64,
    pub model_revision: Option<ModelRevision>,
    pub label: String,
    pub sections: Vec<ExplanationSection>,
    pub validation_sweep: Option<u64>,
    pub limits: Vec<String>,
}
fn clip(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_TEXT).collect();
    if text.chars().count() > MAX_TEXT {
        out.push_str("… [truncated]");
    }
    out
}
// Human display only. Structured control values retain their exact precision.
fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => clip(text),
        Value::Number(number) => {
            let value = number.as_f64().unwrap_or_default();
            if value != 0.0 && !(0.000001..1e12).contains(&value.abs()) {
                format!("{value:.6e}")
            } else {
                let text = format!("{value:.6}");
                let text = text.trim_end_matches('0').trim_end_matches('.');
                if text == "-0" {
                    "0".into()
                } else {
                    text.into()
                }
            }
        }
        _ => clip(&value.to_string()),
    }
}

fn bounded_details(value: Value) -> Value {
    let result = bounded(value, 0);
    if serde_json::to_vec(&result).map_or(true, |v| v.len() > 2048) {
        json!({"truncated":true})
    } else {
        result
    }
}
fn bounded(value: Value, depth: usize) -> Value {
    if depth >= 5 {
        return json!({"truncated":true});
    }
    match value {
        Value::String(s) => Value::String(clip(&s)),
        Value::Array(items) => {
            let omitted = items.len().saturating_sub(MAX_ROWS);
            let mut out: Vec<_> = items
                .into_iter()
                .take(MAX_ROWS)
                .map(|v| bounded(v, depth + 1))
                .collect();
            if omitted > 0 {
                out.push(json!({"omitted":omitted}));
            }
            Value::Array(out)
        }
        Value::Object(items) => {
            let omitted = items.len().saturating_sub(MAX_ROWS);
            let mut out: serde_json::Map<_, _> = items
                .into_iter()
                .take(MAX_ROWS)
                .map(|(k, v)| (clip(&k), bounded(v, depth + 1)))
                .collect();
            if omitted > 0 {
                out.insert("_omitted".into(), json!(omitted));
            }
            Value::Object(out)
        }
        other => other,
    }
}
fn encoded<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Rebuild on explicit inspection. UI calls this on selection/revision changes;
/// no work is performed per-frame for an unchanged selection.
pub fn explain_design(world: &World, element_id: u64) -> Result<DesignExplanation, String> {
    let mut query = world
        .try_query::<EntityRef>()
        .ok_or("No authored entities")?;
    let entity = query
        .iter(world)
        .find(|e| e.get::<ElementId>() == Some(&ElementId(element_id)))
        .ok_or("Element not found")?;
    let snapshot = world
        .get_resource::<CapabilityRegistry>()
        .and_then(|r| r.capture_snapshot(&entity, world));
    let label = snapshot
        .as_ref()
        .map(BoxedEntity::label)
        .unwrap_or_else(|| format!("Element {element_id}"));
    let mut source = ExplanationSection::new("Why it is here");
    let mut controls = ExplanationSection::new("What controls it");
    let mut dependencies = ExplanationSection::new("What it affects");
    let mut evidence = ExplanationSection::new("Evidence");
    let mut unresolved = ExplanationSection::new("Unresolved choices");
    let mut validation = ExplanationSection::new("Validation coverage");

    if let Some(p) = entity.get::<AuthoringProvenance>() {
        use crate::plugins::refinement::AuthoringMode;
        let origin = match &p.mode {
            AuthoringMode::Freeform => "Authored directly".into(),
            AuthoringMode::ViaRecipe(id) => format!(
                "Recipe: {}",
                world
                    .get_resource::<CapabilityRegistry>()
                    .and_then(|r| r.recipe_family_descriptor(
                        &crate::capability_registry::RecipeFamilyId(id.0.clone())
                    ))
                    .map(|r| r.label.as_str())
                    .unwrap_or(&id.0)
            ),
            AuthoringMode::Imported(source) => format!("Imported from {}", source.0),
            AuthoringMode::Refined(parent) => format!("Refined from element {parent}"),
        };
        source.add(
            "Recorded creation",
            format!(
                "{origin}{}",
                p.rationale
                    .as_ref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
            encoded(p),
        );
    } else {
        source.add(
            "Creation",
            "No direct authoring provenance is recorded.",
            json!({"direct_provenance_present":false}),
        );
    }
    if let Some(c) = entity.get::<ElementClassAssignment>() {
        source.add("Classification", &c.element_class.0, encoded(c));
    }
    if let Some(state) = entity.get::<RefinementStateComponent>() {
        source.add(
            "Resolved detail",
            format!("{:?}", state.state),
            encoded(state),
        );
    }
    if let Some(identity) = entity.get::<OccurrenceIdentity>() {
        if let Some(registry) = world.get_resource::<DefinitionRegistry>() {
            if let Some(definition) = registry.get(&identity.definition_id) {
                source.add("Reusable source", &definition.name, json!({"definition_id":identity.definition_id,"version":definition.definition_version,"occurrence_version":identity.definition_version,"next_tool":"definition.explain"}));
                // Resolve through the existing authority, never infer parameter values.
                if definition.interface.parameters.0.len() <= 128 {
                    match registry
                        .resolve_params_checked(&identity.definition_id, &identity.overrides)
                    {
                        Ok(resolved) => {
                            for parameter in &definition.interface.parameters.0 {
                                let value = resolved
                                    .get(&parameter.name)
                                    .map(|p| p.value.clone())
                                    .unwrap_or(Value::Null);
                                let editable = parameter.override_policy != OverridePolicy::Locked
                                    && parameter.metadata.mutability == ParameterMutability::Input;
                                use crate::plugins::{
                                    modeling::definition::ValueProvenance, units::ParameterUnit,
                                };
                                let value_source = resolved
                                    .get(&parameter.name)
                                    .map(|p| match p.provenance {
                                        ValueProvenance::DefinitionDefault => "definition default",
                                        ValueProvenance::OccurrenceOverride => {
                                            "occurrence override"
                                        }
                                    })
                                    .unwrap_or("unresolved");
                                let unit = parameter
                                    .metadata
                                    .unit
                                    .as_ref()
                                    .map(|u| match u {
                                        ParameterUnit::Typed { unit, .. } => {
                                            format!(" {}", unit.symbol())
                                        }
                                        ParameterUnit::UnknownLegacy { value } => {
                                            format!(" {value} (unrecognized unit)")
                                        }
                                    })
                                    .unwrap_or_default();
                                controls.add(parameter.name.replace('_', " "), format!("{}{unit} · {value_source}{}",display_value(&value),if editable {""} else {" · read only"}),
                                    json!({"parameter":parameter.name,"value":value,"value_source":value_source,"unit":parameter.metadata.unit,"min":parameter.metadata.min,"max":parameter.metadata.max,"geometry_affecting":parameter.geometry_affecting,"editable":editable,"override_policy":parameter.override_policy,"scale_behavior":parameter.metadata.scale_behavior,"authority":"definition_parameter","next_tool":"occurrence.resolve"}));
                            }
                        }
                        Err(error) => unresolved.add("Parameter resolution", error, Value::Null),
                    }
                } else {
                    unresolved.add("Parameter resolution", "Parameter schema exceeds this explanation's budget; inspect the definition.", json!({"next_tool":"definition.explain"}));
                }
            } else {
                unresolved.add(
                    "Reusable source",
                    "The referenced definition is missing.",
                    json!({"definition_id":identity.definition_id}),
                );
            }
        }
        if let Some(hosting) = &identity.hosting {
            let mut details = encoded(hosting);
            if let Some(object) = details.as_object_mut() {
                if let Some(id) = hosting.opening_element_id.or(hosting.host_element_id) {
                    object.insert("next_tool".into(), json!("get_entity_details"));
                    object.insert("element_id".into(), json!(id.0));
                }
            }
            controls.add(
                "Hosting",
                "Placement is controlled by the host contract.",
                details,
            );
        }
    } else if let Some(snapshot) = &snapshot {
        for field in snapshot.property_fields() {
            if controls.rows.len() == MAX_ROWS {
                controls.omitted += 1;
                continue;
            }
            let value = field
                .value
                .as_ref()
                .map(|v| v.to_json())
                .unwrap_or(Value::Null);
            controls.add(field.label, display_value(&value), json!({"property":field.name,"value":value,"editable":field.editable,"authority":"authored_property"}));
        }
    }
    if let Some(deps) = entity.get::<EntityDependencies>() {
        for edge in &deps.edges {
            dependencies.add(
                "Depends on",
                format!("Element {} · {}", edge.on.0, edge.role.as_str()),
                json!({"element_id":edge.on.0,"role":edge.role.as_str(),"direction":"input"}),
            );
        }
    }
    // Read authoritative components directly: the cached derived graph may not
    // have caught up with an immediately preceding MCP edit. One bounded-output
    // scan per inspection, never a per-frame presentation loop.
    for other in query.iter(world) {
        let Some(id) = other.get::<ElementId>() else {
            continue;
        };
        if let Some(deps) = other.get::<EntityDependencies>() {
            for edge in &deps.edges {
                if edge.on.0 == element_id {
                    if dependencies.rows.len() == MAX_ROWS {
                        dependencies.omitted += 1;
                        continue;
                    }
                    dependencies.add("Direct dependent",format!("Element {} · {}",id.0,edge.role.as_str()),json!({"element_id":id.0,"role":edge.role.as_str(),"direction":"dependent"}));
                }
            }
        }
        if let Some(group) = other.get::<GroupMembers>() {
            if group.member_ids.contains(&ElementId(element_id)) {
                source.add("Assembly context", &group.name, json!({"element_id":id.0,"relationship":"member_of","provenance":other.get::<AuthoringProvenance>().map(encoded),"scope":"context_only_not_inherited_claim_grounding"}));
            }
        }
    }
    if let Some(claims) = entity.get::<ClaimGrounding>() {
        let mut entries: Vec<_> = claims.claims.iter().collect();
        entries.sort_by_key(|(path, _)| &path.0);
        for (path, record) in entries {
            let resolution = match &record.grounding {
                Grounding::GeneratedByRecipe(id) => {
                    if world.get_resource::<CapabilityRegistry>().is_some_and(|r| {
                        r.recipe_family_descriptor(&crate::capability_registry::RecipeFamilyId(
                            id.clone(),
                        ))
                        .is_some()
                    }) {
                        "recipe_registered"
                    } else {
                        "recipe_missing"
                    }
                }
                Grounding::Refined(id) => {
                    if query
                        .iter(world)
                        .any(|e| e.get::<ElementId>() == Some(&ElementId(*id)))
                    {
                        "element_present"
                    } else {
                        "element_missing"
                    }
                }
                _ => "reference_not_resolved_by_this_projection",
            };
            evidence.add(&path.0, format!("{:?}",record.grounding),json!({"record":record,"resolution":resolution,"applicability":"not_established_by_presence"}));
        }
    }
    if evidence.rows.is_empty() {
        evidence.add(
            "Claim grounding",
            "No direct property grounding is recorded.",
            json!({"direct_grounding_present":false}),
        );
    }
    if let Some(identity) = entity.get::<OccurrenceIdentity>() {
        if world
            .get_resource::<DefinitionRegistry>()
            .is_some_and(|r| r.get(&identity.definition_id).is_some())
        {
            evidence.add("Reusable source context", "Inspect the Definition for its source evidence and limits; these do not establish this instance's claims.", json!({"definition_id":identity.definition_id,"next_tool":"definition.explain","scope":"context_only_not_inherited_claim_grounding"}));
        }
    }
    if let Some(intent) = entity.get::<SemanticIntent>() {
        for item in &intent.unresolved_decisions {
            unresolved.add(&item.question, &item.reason, encoded(item));
        }
        for item in &intent.source_refs {
            let found = world
                .get_resource::<CorpusPassageRegistry>()
                .and_then(|r| r.get(&PassageRef(item.reference.clone())));
            evidence.add(&item.reference, &item.claim, json!({"reference":item.reference,"grounding":item.grounding,"resolution":if found.is_some(){"passage_present"}else{"unresolved_reference"},"provenance":found.map(|p|encoded(&p.provenance)),"next_tool":"lookup_source_passage","applicability":"not_established_by_presence"}));
        }
    }
    if let Some(obligations) = entity.get::<ObligationSet>() {
        for obligation in &obligations.entries {
            if matches!(
                obligation.status,
                ObligationStatus::Unresolved | ObligationStatus::Deferred(_)
            ) {
                unresolved.add(
                    &obligation.role.0,
                    format!(
                        "Required by {:?} · {:?}",
                        obligation.required_by_state, obligation.status
                    ),
                    encoded(obligation),
                );
            }
        }
    }
    if unresolved.rows.is_empty() {
        unresolved.add("Recorded state", "No unresolved choices are recorded. This does not establish that all design requirements are resolved.", json!({"unresolved_records_present":false}));
    }
    let findings = world.get_resource::<Findings>();
    if let Some(findings) = findings {
        let mut relevant = findings.for_entity(element_id);
        relevant.sort_by_key(|f| &f.id.0);
        for finding in relevant {
            validation.add(finding.severity.as_str(),&finding.message,json!({"finding_id":finding.id,"constraint_id":finding.constraint_id,"rationale":finding.rationale,"backlink":finding.backlink,"next_tool":"explain_finding_v2"}));
        }
        let mut checked: Vec<_> = findings
            .cache
            .keys()
            .filter(|(_, id)| *id == element_id)
            .map(|(id, _)| id.0.clone())
            .collect();
        checked.sort();
        for id in &checked {
            validation.add("Last sweep included", id, json!({"constraint_id":id}));
        }
        if checked.is_empty() {
            validation.add(
                "Coverage",
                "No validator coverage recorded for this element.",
                Value::Null,
            );
        }
    } else {
        validation.add("Coverage", "Validation has not been observed.", Value::Null);
    }
    let mut result = DesignExplanation {element_id,model_revision:world.get_resource::<History>().map(History::revision_token),label:clip(&label),sections:vec![source,controls,dependencies,evidence,unresolved,validation],validation_sweep:findings.map(|f|f.sweep_generation),limits:vec![
        "Evidence presence does not establish applicability or correctness.".into(),
        "Dependencies are direct recorded edges, including inactive alternatives; this is not a proposed edit's complete impact plan.".into(),
        "Validation is the last recorded sweep, not a fresh validation run. Empty findings do not prove completeness.".into(),
        format!("At most {MAX_ROWS} rows per section; long values and nested details are truncated. Refresh after editing."),
    ]};
    let mut bytes = 0;
    for section in &mut result.sections {
        section.rows.retain(|row| {
            bytes += serde_json::to_vec(row).map_or(usize::MAX / 2, |v| v.len());
            if bytes <= 48 * 1024 {
                true
            } else {
                section.omitted += 1;
                false
            }
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_registry::{CorpusProvenance, LicenseTag},
        plugins::{
            modeling::{
                definition::*,
                generic_factory::PrimitiveFactory,
                primitives::{BoxPrimitive, ShapeRotation},
            },
            refinement::{AuthoringMode, SemanticSourceRef, UnresolvedDecisionRecord},
        },
    };
    fn world() -> World {
        let mut world = World::new();
        let mut registry = CapabilityRegistry::default();
        registry.register_factory(PrimitiveFactory::<BoxPrimitive>::new());
        world.insert_resource(registry);
        world.init_resource::<History>();
        world.spawn((
            ElementId(1),
            BoxPrimitive {
                centre: Vec3::ZERO,
                half_extents: Vec3::ONE,
            },
            ShapeRotation::default(),
        ));
        world
    }
    #[test]
    fn missing_provenance_is_not_freeform_and_context_is_not_grounding() {
        let mut world = world();
        world.spawn((
            ElementId(2),
            GroupMembers {
                name: "Curated assembly".into(),
                member_ids: vec![ElementId(1)],
                frame: Default::default(),
                linked_model: None,
            },
            AuthoringProvenance {
                mode: AuthoringMode::ViaRecipe(crate::plugins::refinement::RecipeId(
                    "fixture".into(),
                )),
                rationale: None,
            },
        ));
        world.spawn((
            ElementId(3),
            EntityDependencies::empty().with_edge(ElementId(1), "host"),
        ));
        let result = explain_design(&world, 1).unwrap();
        assert_eq!(
            result.sections[0].rows[0].details["direct_provenance_present"],
            false
        );
        assert_eq!(
            result.sections[0].rows[1].details["scope"],
            "context_only_not_inherited_claim_grounding"
        );
        assert_eq!(result.sections[2].rows[0].details["element_id"], 3);
        assert_eq!(
            result.sections[3].rows[0].details["direct_grounding_present"],
            false
        );
        assert_eq!(
            result.sections[4].rows[0].details["unresolved_records_present"],
            false
        );
        assert!(explain_design(&world, 999).is_err());
    }
    #[test]
    fn resolved_controls_use_definition_authority_and_override_policy() {
        let mut world = world();
        let definition: Definition = serde_json::from_value(json!({"id":"fixture","name":"Fixture","definition_kind":"Solid","definition_version":1,"interface":{"parameters":[{"name":"width","param_type":"Numeric","default_value":1.0,"override_policy":"Overridable"},{"name":"constant","param_type":"Numeric","default_value":0.1,"override_policy":"Locked"}]},"body":{},"domain_data":{}})).unwrap();
        let mut definitions = DefinitionRegistry::default();
        definitions.insert(definition);
        world.insert_resource(definitions);
        let mut identity = OccurrenceIdentity::new(DefinitionId("fixture".into()), 1);
        identity.overrides.set("width", json!(1.2f32 as f64));
        world.spawn((ElementId(4), identity));
        let result = explain_design(&world, 4).unwrap();
        assert_eq!(
            result.sections[1].rows[0].details["value"],
            json!(1.2f32 as f64)
        );
        assert!(result.sections[1].rows[0].text.starts_with("1.2 ·"));
        assert_eq!(result.sections[1].rows[0].details["editable"], true);
        assert_eq!(result.sections[1].rows[1].details["editable"], false);
    }
    #[test]
    fn evidence_resolution_is_separate_from_applicability_and_output_is_bounded() {
        let mut world = world();
        let mut passages = CorpusPassageRegistry::default();
        passages.register(
            PassageRef("fixture-source".into()),
            "Synthetic evidence",
            CorpusProvenance {
                source: "Synthetic fixture".into(),
                source_version: "1".into(),
                jurisdiction: None,
                ingested_at: 0,
                license: LicenseTag::Cc0,
                backlink: None,
                supersedes: vec![],
            },
        );
        world.insert_resource(passages);
        world.spawn((
            ElementId(8),
            SemanticIntent {
                source_refs: vec![
                    SemanticSourceRef {
                        reference: "fixture-source".into(),
                        claim: "Sample claim".into(),
                        grounding: "fixture".into(),
                    },
                    SemanticSourceRef {
                        reference: "missing".into(),
                        claim: "Unknown claim".into(),
                        grounding: "unverified".into(),
                    },
                ],
                unresolved_decisions: (0..100)
                    .map(|i| UnresolvedDecisionRecord {
                        id: i.to_string(),
                        question: "A".repeat(5000),
                        reason: "B".repeat(5000),
                        grounding: "unknown".into(),
                    })
                    .collect(),
                parameters: Value::Null,
            },
        ));
        let result = explain_design(&world, 8).unwrap();
        assert_eq!(
            result.sections[3].rows[1].details["resolution"],
            "passage_present"
        );
        assert_eq!(
            result.sections[3].rows[1].details["applicability"],
            "not_established_by_presence"
        );
        assert_eq!(
            result.sections[3].rows[2].details["resolution"],
            "unresolved_reference"
        );
        assert!(result.sections[4].omitted >= 76);
        assert!(serde_json::to_vec(&result).unwrap().len() < 64 * 1024);
        assert!(result.sections[4].rows[0].text.ends_with("[truncated]"));
    }
    #[test]
    #[ignore = "explicit explanation inspection budget"]
    fn explanation_inspection_budget() {
        let mut world = world();
        for id in 2..10002 {
            world.spawn((
                ElementId(id),
                EntityDependencies::empty().with_edge(ElementId(1), "fixture"),
            ));
        }
        let mut samples = vec![];
        for _ in 0..100 {
            let start = std::time::Instant::now();
            std::hint::black_box(explain_design(&world, 1).unwrap());
            samples.push(start.elapsed());
        }
        samples.sort();
        let p95 = samples[94];
        println!("explanation 10k direct dependencies p95={p95:?}");
        assert!(
            p95 < std::time::Duration::from_millis(20),
            "one-shot inspection budget; not a live-frame budget"
        );
    }
}
