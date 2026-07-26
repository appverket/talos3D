//! Design Concept Graph and Admissibility Kernel — domain-neutral substrate.
//!
//! Implements [ADR-064] (Talos Design Runtime), which resolves the open design
//! dependencies of the ratified `TALOS_DESIGN_RUNTIME_AGREEMENT`.
//!
//! # Why this exists
//!
//! Before this module, architecture registered five coarse element classes plus
//! a load-time alias, so `trim` covered bargeboard, fascia, soffit, corner
//! board, casing and batten alike. There was no name for the thing, therefore
//! no rule about the thing, and `bargeboard attached_to wall_cladding` reduced
//! to `occurrence attached_to occurrence` — accepted unconditionally.
//!
//! # The mechanism
//!
//! Hosts **publish** anchors; guests **resolve against** them. `roof_system`
//! publishes `rake_edge`; `wall_assembly` does not. So a bargeboard on a wall
//! is not merely forbidden — it has no referent. Correctness comes from
//! unsayability rather than from checking, and a whitelist closes the whole
//! space where a forbidden-host list would need one entry per discovered bug.
//!
//! # Layering
//!
//! | Module | Role |
//! |---|---|
//! | [`ids`] | opaque identifiers; core never interprets their contents |
//! | [`graph`] | canonical *authored* records, carrying evidence and scope |
//! | [`registry`] | compiled indexes; nothing here may be authored |
//! | [`plan`] | the semantic edit plan and the context trait |
//! | [`kernel`] | evaluation: `Admit` / `AdmitWithObligation` / `Refuse` |
//!
//! Provenance types are reused from [`crate::curation`]; this module adds no
//! second evidence or jurisdiction authority.
//!
//! [ADR-064]: https://github.com/appverket/talos3d-workspace/blob/main/decisions/ADR-064-Talos-Design-Runtime.md

pub mod components;
pub mod graph;
pub mod ids;
pub mod kernel;
pub mod lint;
pub mod plan;
pub mod registry;

#[cfg(test)]
mod enforcement_tests;
#[cfg(test)]
pub(crate) mod test_fixtures;

pub use components::{
    ActiveJurisdiction, ConceptAssignment, PublishedAnchor, PublishedAnchors, SemanticBinding,
    SemanticBindings, SemanticGraph, WorldSemanticContext,
};
pub use graph::{
    AdmissibilityProposition, AnchorCardinality, AnchorGeometry, AnchorKindDescriptor, Cardinality,
    Concept, ConceptStatus, LexicalEntry, PropositionObject, PublishedAnchorContract,
};
pub use ids::{AnchorKindId, AnchorRoleId, ConceptId, PredicateId, PropositionId};
pub use kernel::{evaluate, PendingObligation, Refusal, Verdict};
pub use lint::{lint_corpus, CorpusFinding, CorpusFindingKind};
pub use plan::{AnchorInstanceId, BindTarget, PlanIntent, SemanticContext, SemanticPlan};
pub use registry::SemanticRegistry;

#[cfg(test)]
mod tests {
    use super::test_fixtures::*;
    use super::*;
    use crate::plugins::identity::ElementId;
    use crate::plugins::refinement::RefinementState;
    use std::collections::HashMap;

    /// Minimal world model. Deliberately not Bevy: the kernel is pure, so its
    /// behaviour is provable without a `World`.
    #[derive(Default)]
    struct TestWorld {
        concepts: HashMap<u64, ConceptId>,
        anchors: HashMap<u64, Vec<AnchorInstanceId>>,
        state: RefinementState,
        bindings: HashMap<(u64, String), Vec<BindTarget>>,
    }

    impl TestWorld {
        fn with_concept(mut self, entity: u64, concept: &str) -> Self {
            self.concepts.insert(entity, ConceptId::new(concept));
            self
        }

        fn publishing(mut self, entity: u64, kind: &str, roles: &[&str]) -> Self {
            let instances = roles
                .iter()
                .map(|role| AnchorInstanceId::new(ElementId(entity), kind, *role))
                .collect::<Vec<_>>();
            self.anchors.entry(entity).or_default().extend(instances);
            self
        }

        fn at_state(mut self, state: RefinementState) -> Self {
            self.state = state;
            self
        }
    }

    impl SemanticContext for TestWorld {
        fn concept_of(&self, entity: ElementId) -> Option<ConceptId> {
            self.concepts.get(&entity.0).cloned()
        }

        fn published_anchors(&self, entity: ElementId) -> Vec<AnchorInstanceId> {
            self.anchors.get(&entity.0).cloned().unwrap_or_default()
        }

        fn refinement_state(&self, _entity: ElementId) -> RefinementState {
            self.state
        }

        fn existing_bindings(
            &self,
            subject: ElementId,
            predicate: &PredicateId,
        ) -> Vec<BindTarget> {
            self.bindings
                .get(&(subject.0, predicate.0.clone()))
                .cloned()
                .unwrap_or_default()
        }
    }

    /// Roof #388 publishes two rake edges; wall cladding #412 publishes a face
    /// and two corner arrises; bargeboard #511 is the guest.
    fn world() -> TestWorld {
        TestWorld::default()
            .with_concept(388, ROOF_SYSTEM)
            .publishing(388, RAKE_EDGE, &["north_west", "north_east"])
            .publishing(388, EAVE_LINE, &["south", "north"])
            .with_concept(412, WALL_CLADDING)
            .publishing(412, FACE_EXTERIOR, &["outer"])
            .publishing(412, WALL_CORNER_ARRIS, &["ne", "nw"])
            .with_concept(511, BARGEBOARD)
    }

    fn bind(subject: u64, target: BindTarget) -> SemanticPlan {
        SemanticPlan::none().with(PlanIntent::Bind {
            subject: ElementId(subject),
            predicate: PredicateId::new(FOLLOWS),
            target,
        })
    }

    // ---- the headline case ----

    #[test]
    fn bargeboard_on_the_roof_rake_is_admitted() {
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world(),
            &bind(
                511,
                BindTarget::Anchor(AnchorInstanceId::new(
                    ElementId(388),
                    RAKE_EDGE,
                    "north_west",
                )),
            ),
        );
        assert_eq!(verdict, Verdict::Admit);
    }

    #[test]
    fn bargeboard_on_wall_cladding_refuses_with_an_actionable_diagnostic() {
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world(),
            &bind(511, BindTarget::Entity(ElementId(412))),
        );

        let refusals = verdict.refusals();
        assert_eq!(refusals.len(), 1, "expected exactly one refusal");
        let refusal = &refusals[0];

        assert_eq!(
            refusal.violated,
            Some(PropositionId::new("arch.prop.bargeboard_follows_rake"))
        );
        // Names the violated proposition...
        assert!(refusal.reason.contains(RAKE_EDGE));
        // ...reports what was actually observed, including that the wall does
        // publish anchors, just never a rake edge...
        assert!(refusal.observed.contains("#412"));
        assert!(refusal.observed.contains(FACE_EXTERIOR));
        assert!(!refusal.observed.contains(RAKE_EDGE));
        // ...and points at the concept that would satisfy it.
        assert!(refusal.repair.contains(ROOF_SYSTEM));
        // Reidentification: the caller may have meant a corner board.
        assert!(refusal.contrasts.contains(&ConceptId::new(CORNER_BOARD)));
    }

    #[test]
    fn bargeboard_on_a_wall_anchor_refuses_even_though_the_anchor_is_real() {
        // The wall genuinely publishes corner_arris. It is still not a rake.
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world(),
            &bind(
                511,
                BindTarget::Anchor(AnchorInstanceId::new(
                    ElementId(412),
                    WALL_CORNER_ARRIS,
                    "ne",
                )),
            ),
        );
        assert!(!verdict.is_admitted());
    }

    #[test]
    fn a_fabricated_anchor_instance_does_not_pass() {
        // Right kind, right host concept, but the host does not actually
        // publish that role.
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world(),
            &bind(
                511,
                BindTarget::Anchor(AnchorInstanceId::new(ElementId(388), RAKE_EDGE, "invented")),
            ),
        );
        assert!(!verdict.is_admitted());
    }

    // ---- missing versus contradictory ----

    #[test]
    fn conceptual_massing_defers_a_missing_binding_as_an_obligation() {
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world().at_state(RefinementState::Conceptual),
            &SemanticPlan::none().with(PlanIntent::AssignConcept {
                entity: ElementId(511),
                concept: ConceptId::new(BARGEBOARD),
            }),
        );
        assert!(verdict.is_admitted(), "absence must not block early design");
        assert_eq!(verdict.obligations().len(), 1);
        assert_eq!(verdict.obligations()[0].due_by, RefinementState::Schematic);
    }

    #[test]
    fn an_asserted_contradiction_refuses_at_conceptual_too() {
        // The distinction Codex identified: refinement defers missing detail,
        // it never authorises an asserted falsehood.
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world().at_state(RefinementState::Conceptual),
            &bind(511, BindTarget::Entity(ElementId(412))),
        );
        assert!(!verdict.is_admitted());
    }

    // ---- the freeform boundary ----

    #[test]
    fn unclassified_geometry_makes_no_claim_and_is_not_governed() {
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world(),
            // #999 carries no concept.
            &bind(999, BindTarget::Entity(ElementId(412))),
        );
        assert_eq!(verdict, Verdict::Admit);
    }

    #[test]
    fn claiming_the_concept_arms_the_kernel_within_one_plan() {
        // Assign then bind, in a single command: the assignment must be visible
        // to the binding or "make semantic" would bypass enforcement.
        let plan = SemanticPlan::none()
            .with(PlanIntent::AssignConcept {
                entity: ElementId(999),
                concept: ConceptId::new(BARGEBOARD),
            })
            .with(PlanIntent::Bind {
                subject: ElementId(999),
                predicate: PredicateId::new(FOLLOWS),
                target: BindTarget::Entity(ElementId(412)),
            });
        let verdict = evaluate(&roof_edge_fixture(), &world(), &plan);
        assert!(!verdict.is_admitted(), "assignment must arm the kernel");
    }

    #[test]
    fn removing_a_concept_is_an_explicit_downgrade_with_a_loss_manifest() {
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world(),
            &SemanticPlan::none().with(PlanIntent::RemoveConcept {
                entity: ElementId(388),
                concept: ConceptId::new(ROOF_SYSTEM),
            }),
        );
        let obligations = verdict.obligations();
        assert_eq!(obligations.len(), 1);
        // The manifest must name what is lost, or removal becomes a quiet
        // escape from a refusal.
        assert!(obligations[0].summary.contains(RAKE_EDGE));
        assert!(obligations[0].summary.contains("invalidated"));
    }

    // ---- generalisation: one mechanism, many concepts ----

    #[test]
    fn the_same_mechanism_routes_fascia_to_the_eave_and_refuses_the_rake() {
        let registry = roof_edge_fixture();
        let world = world().with_concept(600, FASCIA);

        let on_eave = evaluate(
            &registry,
            &world,
            &bind(
                600,
                BindTarget::Anchor(AnchorInstanceId::new(ElementId(388), EAVE_LINE, "south")),
            ),
        );
        let on_rake = evaluate(
            &registry,
            &world,
            &bind(
                600,
                BindTarget::Anchor(AnchorInstanceId::new(
                    ElementId(388),
                    RAKE_EDGE,
                    "north_west",
                )),
            ),
        );

        assert_eq!(on_eave, Verdict::Admit, "fascia belongs on the eave");
        assert!(!on_rake.is_admitted(), "fascia is not a bargeboard");
    }

    #[test]
    fn a_corner_board_correctly_resolves_against_the_wall_system() {
        // Same mechanism, opposite system membership — no extra code.
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world().with_concept(700, CORNER_BOARD),
            &bind(
                700,
                BindTarget::Anchor(AnchorInstanceId::new(
                    ElementId(412),
                    WALL_CORNER_ARRIS,
                    "ne",
                )),
            ),
        );
        assert_eq!(verdict, Verdict::Admit);
    }

    #[test]
    fn an_empty_plan_admits_without_consulting_the_registry() {
        assert_eq!(
            evaluate(&roof_edge_fixture(), &world(), &SemanticPlan::none()),
            Verdict::Admit
        );
    }

    #[test]
    fn an_unknown_concept_refuses_rather_than_being_invented() {
        let verdict = evaluate(
            &roof_edge_fixture(),
            &world(),
            &SemanticPlan::none().with(PlanIntent::AssignConcept {
                entity: ElementId(1),
                concept: ConceptId::new("arch.concept.not_registered"),
            }),
        );
        assert!(!verdict.is_admitted());
        assert!(verdict.refusals()[0].reason.contains("Unknown concept"));
    }
}
