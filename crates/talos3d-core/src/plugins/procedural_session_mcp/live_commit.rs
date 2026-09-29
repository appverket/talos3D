//! Synchronous live transaction adapter for procedural sessions. The script is
//! still the durable IR; accepted EditorCommands are the only undo authority.
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use super::*;
use crate::curation::procedural_session::{
    commit_with_validation, FindingSeverity, ProceduralSession, SessionFinding, SessionObligation,
    StageTransition,
};
use crate::curation::{
    MutationScope, PostconditionOracle, PostconditionVerdict, ResolvedPostcondition,
};
use crate::plugins::{
    history::{HistoryTransaction, SemanticEnforcement},
    identity::ElementId,
    modeling::assembly::SemanticRelation,
    refinement::{ClaimGrounding, ObligationSet, ObligationStatus},
    validation::{validation_sweep_system, DiscoveryFindingsBudget, Findings},
};

struct SharedDispatcher<'a, 'w, 'e> {
    inner: &'a RefCell<CommandQueueDispatcher<'w, 'e>>,
    original_ids: &'a BTreeSet<u64>,
}

impl ToolDispatcher for SharedDispatcher<'_, '_, '_> {
    fn dispatch(&mut self, call: &ToolCall<'_>) -> Result<Value, ToolDispatchError> {
        // ProjectRoot permits new content, not editing pre-existing objects.
        if call.tool.as_str() == "set_property"
            && call
                .args
                .get("element_id")
                .and_then(Value::as_u64)
                .is_some_and(|id| self.original_ids.contains(&id))
        {
            return Err(ToolDispatchError::new("mutation_out_of_scope", "ProjectRoot can edit only entities created in this session; use an explicit native edit for existing content"));
        }
        let mut inner = self.inner.borrow_mut();
        let before = inner
            .world
            .get_resource::<SemanticEnforcement>()
            .map_or(0, |e| e.refusals.len());
        let result = inner.dispatch(call);
        if let Some(enforcement) = inner.world.get_resource::<SemanticEnforcement>() {
            if enforcement.refusals.len() > before {
                return Err(ToolDispatchError::new(
                    "semantic_refusal",
                    enforcement.refusals[before].summary(),
                ));
            }
        }
        result
    }
}

struct LiveOracle<'a, 'w, 'e> {
    inner: &'a RefCell<CommandQueueDispatcher<'w, 'e>>,
    target: Option<u64>,
}

impl PostconditionOracle for LiveOracle<'_, '_, '_> {
    fn check(
        &self,
        condition: &ResolvedPostcondition,
        outputs: &BTreeMap<StepId, Map<String, Value>>,
        _params: &Map<String, Value>,
    ) -> PostconditionVerdict {
        let bridge = self.inner.borrow();
        let world = &*bridge.world;
        let output_ids: BTreeSet<u64> = outputs
            .values()
            .filter_map(|out| out.get("element_id").and_then(Value::as_u64))
            .collect();
        let target = self
            .target
            .or_else(|| (output_ids.len() == 1).then(|| *output_ids.first().unwrap()));
        let passed = match condition {
            ResolvedPostcondition::Relation {
                relation_kind,
                from,
                to,
            } => {
                match (
                    from.as_u64(),
                    to.as_u64(),
                    world.try_query::<&SemanticRelation>(),
                ) {
                    (Some(from), Some(to), Some(mut query)) => query.iter(world).any(|r| {
                        r.source.0 == from && r.target.0 == to && &r.relation_type == relation_kind
                    }),
                    _ => false,
                }
            }
            ResolvedPostcondition::Claim { path, grounding } => {
                if let (Some(target), Some(mut query)) =
                    (target, world.try_query::<(&ElementId, &ClaimGrounding)>())
                {
                    query.iter(world).any(|(id, claims)| {
                        id.0 == target
                            && claims
                                .claims
                                .get(path)
                                .is_some_and(|c| &c.grounding == grounding)
                    })
                } else {
                    false
                }
            }
            ResolvedPostcondition::ObligationSatisfied {
                obligation_id,
                by_step,
            } => {
                let by = outputs
                    .get(by_step)
                    .and_then(|out| out.get("element_id"))
                    .and_then(Value::as_u64);
                if let (Some(obligation), Some(by), Some(mut query)) = (
                    obligation_id.as_str(),
                    by,
                    world.try_query::<(&ElementId, &ObligationSet)>(),
                ) {
                    let matching: Vec<_> = query
                        .iter(world)
                        .filter(|(id, _)| {
                            self.target
                                .map_or_else(|| output_ids.contains(&id.0), |target| id.0 == target)
                        })
                        .flat_map(|(_, set)| set.entries.iter())
                        .filter(|entry| entry.id.0 == obligation)
                        .collect();
                    matching.len() == 1 && matching[0].status == ObligationStatus::SatisfiedBy(by)
                } else {
                    false
                }
            }
        };
        if passed {
            PostconditionVerdict::Pass
        } else {
            PostconditionVerdict::Fail { reason:
            format!("Live model does not establish {condition:?}; claim/obligation checks need an unambiguous target and actual grounding/satisfaction") }
        }
    }
}

fn live_validation(world: &mut World) -> (Vec<SessionFinding>, Vec<SessionObligation>) {
    // Presentation budgets must not hide findings from an acceptance policy.
    world.init_resource::<DiscoveryFindingsBudget>();
    let previous_limit = world.resource::<DiscoveryFindingsBudget>().max_per_sweep;
    world
        .resource_mut::<DiscoveryFindingsBudget>()
        .max_per_sweep = u32::MAX;
    validation_sweep_system(world);
    world
        .resource_mut::<DiscoveryFindingsBudget>()
        .max_per_sweep = previous_limit;
    let mut findings = world
        .get_resource::<Findings>()
        .map(|all| {
            all.all()
                .map(|f| SessionFinding {
                    id: f.id.0.clone(),
                    severity: match f.severity {
                        crate::capability_registry::Severity::Error => FindingSeverity::Error,
                        crate::capability_registry::Severity::Warning => FindingSeverity::Warning,
                        _ => FindingSeverity::Info,
                    },
                    description: format!(
                        "element {} / {}: {}",
                        f.subject, f.constraint_id.0, f.message
                    ),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut obligations = Vec::new();
    if let Some(mut query) = world.try_query::<(&ElementId, &ObligationSet)>() {
        for (id, set) in query.iter(world) {
            for entry in &set.entries {
                if matches!(
                    entry.status,
                    ObligationStatus::Unresolved | ObligationStatus::Deferred(_)
                ) {
                    obligations.push(SessionObligation {
                        id: format!("{}:{}", id.0, entry.id.0),
                        kind: "live_model_obligation".into(),
                        description: format!(
                            "{:?}; required by {:?}; status {:?}",
                            entry.role, entry.required_by_state, entry.status
                        ),
                    });
                }
            }
        }
    }
    if let Some(enforcement) = world.get_resource::<SemanticEnforcement>() {
        for (index, obligation) in enforcement.obligations.iter().enumerate() {
            obligations.push(SessionObligation {
                id: format!("semantic:{index}"),
                kind: "admissibility_obligation".into(),
                description: format!("{obligation:?}"),
            });
        }
    }
    findings.sort_by(|a, b| a.id.cmp(&b.id));
    obligations.sort_by(|a, b| a.id.cmp(&b.id));
    (findings, obligations)
}

pub(super) fn commit_live(
    world: &mut World,
    session: &mut ProceduralSession,
    registry: &SessionToolRegistry,
    config: &ProceduralSessionConfig,
    options: CommitOptions,
    executor: &mut dyn SessionStepExecutor,
) -> Result<CommitReport, SessionError> {
    let unsupported = |reason: String| SessionError::UnsupportedTransaction { reason };
    executor.prepare_transaction(world).map_err(unsupported)?;
    if !matches!(
        session.spec.mutation_scope,
        MutationScope::ProjectRoot | MutationScope::None
    ) || !matches!(
        session.spec.stage_transition,
        StageTransition::NewConceptual | StageTransition::PureQuery
    ) {
        return Err(unsupported("Only new Conceptual document content or pure queries have an audited transaction contract; refinement and organization-library scopes require their native plan".into()));
    }
    for instruction in &session.script.steps {
        let step = instruction.as_call().ok_or_else(|| {
            unsupported("Nested session instructions do not have a transaction contract".into())
        })?;
        if !executor.supports_atomic_step(&step.tool) {
            return Err(unsupported(format!(
                "Tool '{}' has no audited rollback contract",
                step.tool.0
            )));
        }
        if matches!(session.spec.stage_transition, StageTransition::PureQuery)
            && registry.get(&step.tool).is_some_and(|d| d.mutates)
        {
            return Err(unsupported(
                "A pure-query session cannot contain mutations".into(),
            ));
        }
    }
    let original_ids = world
        .try_query::<&ElementId>()
        .map(|mut q| q.iter(world).map(|id| id.0).collect())
        .unwrap_or_default();
    let old_findings = world.get_resource::<Findings>().cloned();
    let old_budget = world.get_resource::<DiscoveryFindingsBudget>().cloned();
    let old_enforcement = world.get_resource::<SemanticEnforcement>().cloned();
    let transaction = HistoryTransaction::begin(world).map_err(unsupported)?;
    let mut result = {
        let shared = RefCell::new(CommandQueueDispatcher {
            world,
            session_id: session.id.clone(),
            step_order: session
                .script
                .steps
                .iter()
                .map(|s| s.id().clone())
                .collect(),
            executor: Some(executor),
        });
        let mut dispatcher = SharedDispatcher {
            inner: &shared,
            original_ids: &original_ids,
        };
        let oracle = LiveOracle {
            inner: &shared,
            target: session
                .spec
                .refinement_target
                .as_ref()
                .and_then(Value::as_u64),
        };
        commit_with_validation(
            session,
            registry,
            config,
            options,
            &mut dispatcher,
            &oracle,
            || {
                let mut bridge = shared.borrow_mut();
                let (findings, obligations) = live_validation(bridge.world);
                Ok(Some((findings, obligations)))
            },
        )
    };
    if let Ok(report) = &mut result {
        let mut ids = world
            .get_resource::<crate::capability_registry::CapabilityRegistry>()
            .map(|registry| {
                registry
                    .constraint_descriptors()
                    .iter()
                    .map(|d| d.id.0.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ids.sort();
        report.validation_evidence =
            crate::curation::procedural_session::CommitValidationEvidence::LiveModel {
                registered_constraint_ids: ids,
                scope: "whole_document".into(),
                geometry_reviewed: false,
            };
        if let Some((_, receipt)) = &mut session.committed {
            *receipt = report.clone();
        }
    }
    transaction.finish(world, result.is_ok());
    if result.is_err() {
        // Validation caches are derived. Restore their previous snapshot after
        // compensating commands, without retaining findings for rolled-back ids.
        if let Some(value) = old_findings {
            world.insert_resource(value);
        } else {
            world.remove_resource::<Findings>();
        }
        if let Some(value) = old_budget {
            world.insert_resource(value);
        } else {
            world.remove_resource::<DiscoveryFindingsBudget>();
        }
        if let Some(value) = old_enforcement {
            world.insert_resource(value);
        } else {
            world.remove_resource::<SemanticEnforcement>();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::refinement::{
        ClaimPath, ClaimRecord, Grounding, Obligation, ObligationId, RefinementState, SemanticRole,
    };
    use serde_json::json;

    #[test]
    fn live_oracle_requires_actual_target_grounding_relation_and_satisfaction() {
        let mut world = World::new();
        let grounding = Grounding::GeneratedByRecipe("fixture".into());
        world.spawn((
            ElementId(42),
            ClaimGrounding {
                claims: [(
                    ClaimPath("span".into()),
                    ClaimRecord {
                        grounding: grounding.clone(),
                        set_at: 0,
                        set_by: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
            ObligationSet {
                entries: vec![Obligation {
                    id: ObligationId("member".into()),
                    role: SemanticRole("member".into()),
                    required_by_state: RefinementState::Conceptual,
                    status: ObligationStatus::SatisfiedBy(43),
                }],
            },
        ));
        world.spawn((
            ElementId(90),
            SemanticRelation {
                source: ElementId(43),
                target: ElementId(42),
                relation_type: "hosted_on".into(),
                parameters: Value::Null,
            },
        ));
        let shared = RefCell::new(CommandQueueDispatcher {
            world: &mut world,
            session_id: SessionId("oracle".into()),
            step_order: Default::default(),
            executor: None,
        });
        let oracle = LiveOracle {
            inner: &shared,
            target: Some(42),
        };
        let outputs = [(
            StepId::new("member"),
            [("element_id".into(), json!(43))].into_iter().collect(),
        )]
        .into_iter()
        .collect();
        for condition in [
            ResolvedPostcondition::Claim {
                path: ClaimPath("span".into()),
                grounding: grounding.clone(),
            },
            ResolvedPostcondition::Relation {
                relation_kind: "hosted_on".into(),
                from: json!(43),
                to: json!(42),
            },
            ResolvedPostcondition::ObligationSatisfied {
                obligation_id: json!("member"),
                by_step: StepId::new("member"),
            },
        ] {
            assert_eq!(
                oracle.check(&condition, &outputs, &Map::new()),
                PostconditionVerdict::Pass
            );
        }
        for condition in [
            ResolvedPostcondition::Claim {
                path: ClaimPath("missing".into()),
                grounding,
            },
            ResolvedPostcondition::Relation {
                relation_kind: "hosted_on".into(),
                from: json!(42),
                to: json!(43),
            },
            ResolvedPostcondition::ObligationSatisfied {
                obligation_id: json!("member"),
                by_step: StepId::new("absent"),
            },
        ] {
            assert!(matches!(
                oracle.check(&condition, &outputs, &Map::new()),
                PostconditionVerdict::Fail { .. }
            ));
        }
    }
}
