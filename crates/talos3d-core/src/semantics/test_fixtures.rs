//! Shared test fixtures.
//!
//! The roof-edge fixture uses architecture ids because it models the PP-TDR-1
//! scenario, but note that **core reads them as opaque strings** — nothing in
//! `semantics/` interprets `arch.*`. The equivalent naval fixture would differ
//! only in its string literals, which is the ADR-064 §13 domain-neutrality
//! claim made testable.

use crate::capability_registry::ElementClassId;

use super::graph::{
    AdmissibilityProposition, AnchorCardinality, AnchorGeometry, AnchorKindDescriptor, Cardinality,
    Concept, LexicalEntry, PropositionObject, PublishedAnchorContract,
};
use super::ids::{AnchorKindId, ConceptId, PredicateId, PropositionId};
use super::registry::SemanticRegistry;
use crate::plugins::refinement::RefinementState;

pub const ROOF_SYSTEM: &str = "arch.concept.roof_system";
pub const WALL_CLADDING: &str = "arch.concept.wall_cladding";
pub const BARGEBOARD: &str = "arch.concept.bargeboard";
pub const FASCIA: &str = "arch.concept.fascia";
pub const CORNER_BOARD: &str = "arch.concept.corner_board";
pub const RAKE_EDGE: &str = "arch.anchor.roof.rake_edge";
pub const EAVE_LINE: &str = "arch.anchor.roof.eave_line";
pub const WALL_CORNER_ARRIS: &str = "arch.anchor.wall.corner_arris";
pub const FACE_EXTERIOR: &str = "arch.anchor.wall.face_exterior";
pub const FOLLOWS: &str = "follows";

fn lexical(locale: &str, preferred: &str, aliases: &[&str]) -> LexicalEntry {
    LexicalEntry {
        locale: locale.to_string(),
        preferred_label: preferred.to_string(),
        aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
        deprecated_terms: Vec::new(),
        false_friends: Vec::new(),
        ambiguity_note: None,
    }
}

fn anchor_kind(
    id: &str,
    geometry: AnchorGeometry,
    cardinality: AnchorCardinality,
) -> AnchorKindDescriptor {
    AnchorKindDescriptor {
        id: AnchorKindId::new(id),
        label: id.to_string(),
        description: String::new(),
        geometry,
        cardinality,
    }
}

fn publishes(publisher: &str, kind: &str, roles: &[&str]) -> PublishedAnchorContract {
    PublishedAnchorContract {
        publisher: ConceptId::new(publisher),
        anchor_kind: AnchorKindId::new(kind),
        roles: roles.iter().map(|role| (*role).into()).collect(),
        resolver: format!("{publisher}::{kind}"),
        extent_rule: None,
    }
}

fn follows_anchor(
    id: &str,
    subject: &str,
    anchor: &str,
    reason: &str,
    hint: &str,
) -> AdmissibilityProposition {
    AdmissibilityProposition {
        id: PropositionId::new(id),
        subject: ConceptId::new(subject),
        predicate: PredicateId::new(FOLLOWS),
        object: PropositionObject::AnchorKind(AnchorKindId::new(anchor)),
        cardinality: Cardinality::ExactlyOne,
        identity_defining: true,
        required_by: Some(RefinementState::Schematic),
        regional_scope: Vec::new(),
        evidence: Vec::new(),
        refusal_reason: reason.to_string(),
        repair_hint: hint.to_string(),
    }
}

/// The PP-TDR-1 roof-edge slice: enough of the graph to exercise the whole
/// bargeboard chain, including the contrast concept the repair hint offers.
pub fn roof_edge_fixture() -> SemanticRegistry {
    let trim_class = ElementClassId("trim".to_string());

    let mut bargeboard = Concept::new(ConceptId::new(BARGEBOARD));
    bargeboard.lexical = vec![
        lexical("en-GB", "bargeboard", &["verge board", "gable board"]),
        lexical("sv-SE", "vindskiva", &[]),
        // en-US "rake board" is a regional *alias*, not a regional variant.
        lexical("en-US", "rake board", &["bargeboard"]),
    ];
    bargeboard.contrasts = vec![ConceptId::new(FASCIA), ConceptId::new(CORNER_BOARD)];
    bargeboard.system_membership = vec![ConceptId::new(ROOF_SYSTEM)];
    bargeboard.applicable_element_classes = vec![trim_class.clone()];

    let mut fascia = Concept::new(ConceptId::new(FASCIA));
    fascia.lexical = vec![lexical("en-GB", "fascia", &["fascia board"])];
    fascia.system_membership = vec![ConceptId::new(ROOF_SYSTEM)];
    fascia.applicable_element_classes = vec![trim_class.clone()];
    fascia.contrasts = vec![ConceptId::new(BARGEBOARD)];

    let mut corner_board = Concept::new(ConceptId::new(CORNER_BOARD));
    corner_board.lexical = vec![lexical("en-GB", "corner board", &[])];
    corner_board.system_membership = vec![ConceptId::new(WALL_CLADDING)];
    corner_board.applicable_element_classes = vec![trim_class];
    corner_board.contrasts = vec![ConceptId::new(BARGEBOARD)];

    let mut roof = Concept::new(ConceptId::new(ROOF_SYSTEM));
    roof.lexical = vec![lexical("en-GB", "roof system", &["roof"])];

    let mut cladding = Concept::new(ConceptId::new(WALL_CLADDING));
    cladding.lexical = vec![lexical("en-GB", "wall cladding", &["siding"])];

    SemanticRegistry::compile(
        [bargeboard, fascia, corner_board, roof, cladding],
        [
            anchor_kind(
                RAKE_EDGE,
                AnchorGeometry::Line,
                AnchorCardinality::Exactly(2),
            ),
            anchor_kind(
                EAVE_LINE,
                AnchorGeometry::Line,
                AnchorCardinality::Exactly(2),
            ),
            anchor_kind(
                WALL_CORNER_ARRIS,
                AnchorGeometry::Line,
                AnchorCardinality::ZeroOrMore,
            ),
            anchor_kind(
                FACE_EXTERIOR,
                AnchorGeometry::Surface,
                AnchorCardinality::ExactlyOne,
            ),
        ],
        [
            publishes(ROOF_SYSTEM, RAKE_EDGE, &["north_west", "north_east"]),
            publishes(ROOF_SYSTEM, EAVE_LINE, &["south", "north"]),
            // The wall publishes real anchors — just never a rake edge.
            publishes(WALL_CLADDING, FACE_EXTERIOR, &["outer"]),
            publishes(WALL_CLADDING, WALL_CORNER_ARRIS, &["ne", "nw"]),
        ],
        [
            follows_anchor(
                "arch.prop.bargeboard_follows_rake",
                BARGEBOARD,
                RAKE_EDGE,
                "A bargeboard closes the roof's own rake edge; a wall face is not a rake \
                 edge and a wall-mounted board misses the roof's rake overhang entirely.",
                "Resolve against the rake_edge anchor published by the roof that shelters \
                 this gable.",
            ),
            follows_anchor(
                "arch.prop.fascia_follows_eave",
                FASCIA,
                EAVE_LINE,
                "A fascia closes the eave, not the rake.",
                "Resolve against the eave_line anchor published by the roof.",
            ),
            follows_anchor(
                "arch.prop.corner_board_follows_arris",
                CORNER_BOARD,
                WALL_CORNER_ARRIS,
                "A corner board closes a wall corner.",
                "Resolve against the corner_arris anchor published by the wall cladding.",
            ),
        ],
    )
}
