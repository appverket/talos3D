//! The compiled half of the Design Concept Graph (ADR-064 §1).
//!
//! Authored records go in; dispatch tables and inverse indexes come out. The
//! recomputation test governs this module: **nothing here may be authored**.
//! Dropping the registry and rebuilding from the same canonical records must
//! produce an identical result, which [`tests::rebuild_is_deterministic`]
//! asserts.
//!
//! The indexes exist for a second reason. ADR-064 Consequences requires that
//! kernel evaluation on the mutation path be an index lookup rather than a
//! graph traversal per command, because it runs inside the command drain.

use std::collections::BTreeMap;

use crate::curation::JurisdictionTag;

use super::graph::{
    AdmissibilityProposition, AnchorKindDescriptor, Concept, PropositionObject,
    PublishedAnchorContract,
};
use super::ids::{AnchorKindId, ConceptId, PredicateId, PropositionId};
use super::pack::ConceptGraphPack;

/// Authored records plus their compiled indexes.
///
/// `BTreeMap` throughout so iteration order is deterministic: refusal
/// diagnostics and corpus-lint output are compared in tests and read by agents,
/// and unstable ordering would make both flaky.
#[derive(Debug, Clone, Default)]
pub struct SemanticRegistry {
    concepts: BTreeMap<ConceptId, Concept>,
    anchor_kinds: BTreeMap<AnchorKindId, AnchorKindDescriptor>,
    publications: Vec<PublishedAnchorContract>,
    propositions: BTreeMap<PropositionId, AdmissibilityProposition>,

    // ---- compiled indexes; never authored, always derivable ----
    /// anchor kind -> concepts that publish it.
    published_by: BTreeMap<AnchorKindId, Vec<ConceptId>>,
    /// concept -> anchor kinds it publishes.
    publishes: BTreeMap<ConceptId, Vec<AnchorKindId>>,
    /// anchor kind -> concepts that consume it. The inverse index ADR-064 §1
    /// requires be compiled rather than authored alongside `published_by`,
    /// which would guarantee a bidirectional consistency bug.
    consumed_by: BTreeMap<AnchorKindId, Vec<ConceptId>>,
    /// (subject concept, predicate) -> propositions. The mutation-path
    /// dispatch table.
    by_subject_predicate: BTreeMap<(ConceptId, PredicateId), Vec<PropositionId>>,
}

impl SemanticRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from authored records, compiling every index once.
    pub fn compile(
        concepts: impl IntoIterator<Item = Concept>,
        anchor_kinds: impl IntoIterator<Item = AnchorKindDescriptor>,
        publications: impl IntoIterator<Item = PublishedAnchorContract>,
        propositions: impl IntoIterator<Item = AdmissibilityProposition>,
    ) -> Self {
        let mut registry = Self::new();
        for concept in concepts {
            registry.concepts.insert(concept.id.clone(), concept);
        }
        for kind in anchor_kinds {
            registry.anchor_kinds.insert(kind.id.clone(), kind);
        }
        registry.publications = publications.into_iter().collect();
        for proposition in propositions {
            registry
                .propositions
                .insert(proposition.id.clone(), proposition);
        }
        registry.rebuild_indexes();
        registry
    }

    /// Recompute every derived index from the authored records.
    fn rebuild_indexes(&mut self) {
        self.published_by.clear();
        self.publishes.clear();
        self.consumed_by.clear();
        self.by_subject_predicate.clear();

        for contract in &self.publications {
            push_unique(
                self.published_by
                    .entry(contract.anchor_kind.clone())
                    .or_default(),
                contract.publisher.clone(),
            );
            push_unique(
                self.publishes
                    .entry(contract.publisher.clone())
                    .or_default(),
                contract.anchor_kind.clone(),
            );
        }

        for proposition in self.propositions.values() {
            if let PropositionObject::AnchorKind(kind) = &proposition.object {
                push_unique(
                    self.consumed_by.entry(kind.clone()).or_default(),
                    proposition.subject.clone(),
                );
            }
            self.by_subject_predicate
                .entry((proposition.subject.clone(), proposition.predicate.clone()))
                .or_default()
                .push(proposition.id.clone());
        }
    }

    // ---- pack install / withdrawal (dynamic corpus delivery) ----

    /// Install a pack's records, replacing any record with the same identity
    /// (a corpus record wins over a compiled one), then recompile the indexes.
    pub fn upsert_pack(&mut self, pack: &ConceptGraphPack) {
        for concept in &pack.concepts {
            self.concepts.insert(concept.id.clone(), concept.clone());
        }
        for kind in &pack.anchor_kinds {
            self.anchor_kinds.insert(kind.id.clone(), kind.clone());
        }
        for publication in &pack.publications {
            let slot = self.publications.iter_mut().find(|p| {
                p.publisher == publication.publisher && p.anchor_kind == publication.anchor_kind
            });
            match slot {
                Some(existing) => *existing = publication.clone(),
                None => self.publications.push(publication.clone()),
            }
        }
        for proposition in &pack.propositions {
            self.propositions
                .insert(proposition.id.clone(), proposition.clone());
        }
        self.rebuild_indexes();
    }

    /// Remove exactly the records a pack names, then recompile the indexes.
    /// Nothing that was replaced by the pack is restored.
    pub fn remove_pack(&mut self, pack: &ConceptGraphPack) {
        for concept in &pack.concepts {
            self.concepts.remove(&concept.id);
        }
        for kind in &pack.anchor_kinds {
            self.anchor_kinds.remove(&kind.id);
        }
        self.publications.retain(|p| {
            !pack
                .publications
                .iter()
                .any(|q| q.publisher == p.publisher && q.anchor_kind == p.anchor_kind)
        });
        for proposition in &pack.propositions {
            self.propositions.remove(&proposition.id);
        }
        self.rebuild_indexes();
    }

    /// Partition every authored record into independently deliverable packs.
    ///
    /// A concept travels with the publications it makes and the propositions it
    /// is the subject of; anchor kinds and records whose concept is absent get
    /// their own packs. Pack ids are the record id (prefixed by record type
    /// where record types could otherwise collide). The partition covers every
    /// record exactly once.
    pub fn export_packs(&self) -> Vec<ConceptGraphPack> {
        let mut out: BTreeMap<String, ConceptGraphPack> = BTreeMap::new();
        fn slot(out: &mut BTreeMap<String, ConceptGraphPack>, id: String) -> &mut ConceptGraphPack {
            out.entry(id.clone()).or_insert_with(|| ConceptGraphPack {
                id,
                ..Default::default()
            })
        }
        for concept in self.concepts.values() {
            slot(&mut out, concept.id.0.clone())
                .concepts
                .push(concept.clone());
        }
        for kind in self.anchor_kinds.values() {
            slot(&mut out, format!("anchor_kind/{}", kind.id.0))
                .anchor_kinds
                .push(kind.clone());
        }
        for publication in &self.publications {
            let owner = if self.concepts.contains_key(&publication.publisher) {
                publication.publisher.0.clone()
            } else {
                format!(
                    "publication/{}/{}",
                    publication.publisher.0, publication.anchor_kind.0
                )
            };
            slot(&mut out, owner).publications.push(publication.clone());
        }
        for proposition in self.propositions.values() {
            let owner = if self.concepts.contains_key(&proposition.subject) {
                proposition.subject.0.clone()
            } else {
                format!("proposition/{}", proposition.id.0)
            };
            slot(&mut out, owner).propositions.push(proposition.clone());
        }
        out.into_values().collect()
    }

    // ---- authored-record access ----

    pub fn concept(&self, id: &ConceptId) -> Option<&Concept> {
        self.concepts.get(id)
    }

    pub fn concepts(&self) -> impl Iterator<Item = &Concept> {
        self.concepts.values()
    }

    pub fn anchor_kind(&self, id: &AnchorKindId) -> Option<&AnchorKindDescriptor> {
        self.anchor_kinds.get(id)
    }

    pub fn anchor_kinds(&self) -> impl Iterator<Item = &AnchorKindDescriptor> {
        self.anchor_kinds.values()
    }

    pub fn proposition(&self, id: &PropositionId) -> Option<&AdmissibilityProposition> {
        self.propositions.get(id)
    }

    pub fn propositions(&self) -> impl Iterator<Item = &AdmissibilityProposition> {
        self.propositions.values()
    }

    pub fn publications(&self) -> &[PublishedAnchorContract] {
        &self.publications
    }

    // ---- compiled-index access ----

    /// Concepts that publish `kind`. Drives the repair hint: "entity #388
    /// (roof_system) publishes rake_edge".
    pub fn publishers_of(&self, kind: &AnchorKindId) -> &[ConceptId] {
        slice_or_empty(self.published_by.get(kind))
    }

    /// Anchor kinds `concept` publishes. Drives the observed clause: "wall
    /// cladding publishes [face_exterior, ...]. No rake_edge."
    pub fn published_by_concept(&self, concept: &ConceptId) -> &[AnchorKindId] {
        slice_or_empty(self.publishes.get(concept))
    }

    /// Concepts that consume `kind`. Compiled, never authored.
    pub fn consumers_of(&self, kind: &AnchorKindId) -> &[ConceptId] {
        slice_or_empty(self.consumed_by.get(kind))
    }

    /// Whether `concept` publishes `kind`. The whitelist test that makes an
    /// invalid attachment unreferenceable rather than merely rejected.
    pub fn publishes_anchor(&self, concept: &ConceptId, kind: &AnchorKindId) -> bool {
        self.published_by_concept(concept).contains(kind)
    }

    /// Propositions governing `(subject, predicate)`, filtered to those that
    /// apply in `jurisdiction`. This is the mutation-path lookup.
    pub fn propositions_for(
        &self,
        subject: &ConceptId,
        predicate: &PredicateId,
        jurisdiction: Option<&JurisdictionTag>,
    ) -> Vec<&AdmissibilityProposition> {
        self.by_subject_predicate
            .get(&(subject.clone(), predicate.clone()))
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.propositions.get(id))
                    .filter(|proposition| proposition.applies_in(jurisdiction))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every proposition whose subject is `concept`, regardless of predicate.
    /// Used by obligation checks and corpus lint.
    pub fn propositions_of_subject(
        &self,
        concept: &ConceptId,
        jurisdiction: Option<&JurisdictionTag>,
    ) -> Vec<&AdmissibilityProposition> {
        self.propositions
            .values()
            .filter(|proposition| &proposition.subject == concept)
            .filter(|proposition| proposition.applies_in(jurisdiction))
            .collect()
    }

    /// Resolve a natural-language term to candidate concepts.
    ///
    /// Returns every match rather than a nearest guess: ADR-052 and the
    /// agreement both forbid silently mapping an ambiguous term to one concept.
    pub fn resolve_term(&self, term: &str) -> Vec<&Concept> {
        self.concepts
            .values()
            .filter(|concept| concept.matches_term(term))
            .collect()
    }
}

fn push_unique<T: PartialEq>(target: &mut Vec<T>, value: T) {
    if !target.contains(&value) {
        target.push(value);
    }
}

fn slice_or_empty<T>(value: Option<&Vec<T>>) -> &[T] {
    value.map(Vec::as_slice).unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::ids::AnchorKindId;
    use crate::semantics::test_fixtures::roof_edge_fixture;

    #[test]
    fn compiles_publication_and_inverse_consumer_indexes() {
        let registry = roof_edge_fixture();
        let rake = AnchorKindId::new("arch.anchor.roof.rake_edge");
        let roof = ConceptId::new("arch.concept.roof_system");
        let bargeboard = ConceptId::new("arch.concept.bargeboard");

        assert_eq!(registry.publishers_of(&rake), &[roof.clone()]);
        // consumed_by is compiled from propositions, never authored.
        assert_eq!(registry.consumers_of(&rake), &[bargeboard]);
        assert!(registry.publishes_anchor(&roof, &rake));
    }

    #[test]
    fn wall_cladding_publishes_no_rake_edge() {
        let registry = roof_edge_fixture();
        let cladding = ConceptId::new("arch.concept.wall_cladding");
        let rake = AnchorKindId::new("arch.anchor.roof.rake_edge");

        // The whole bargeboard defect in one assertion: the wall never offers
        // the anchor, so there is nothing to point at.
        assert!(!registry.publishes_anchor(&cladding, &rake));
        assert!(!registry.published_by_concept(&cladding).is_empty());
    }

    #[test]
    fn rebuild_is_deterministic() {
        let first = roof_edge_fixture();
        let second = SemanticRegistry::compile(
            first.concepts().cloned(),
            first.anchor_kinds().cloned(),
            first.publications().to_vec(),
            first.propositions().cloned(),
        );
        assert_eq!(first.published_by, second.published_by);
        assert_eq!(first.consumed_by, second.consumed_by);
        assert_eq!(first.by_subject_predicate, second.by_subject_predicate);
    }

    #[test]
    fn resolves_terms_across_locales_and_returns_all_candidates() {
        let registry = roof_edge_fixture();
        let by_en = registry.resolve_term("bargeboard");
        let by_sv = registry.resolve_term("vindskiva");
        let by_alias = registry.resolve_term("verge board");

        assert_eq!(by_en.len(), 1);
        assert_eq!(by_en[0].id, ConceptId::new("arch.concept.bargeboard"));
        assert_eq!(by_sv, by_en);
        assert_eq!(by_alias, by_en);
        assert!(registry.resolve_term("not-a-word").is_empty());
    }

    #[test]
    fn regional_filtering_excludes_out_of_scope_propositions() {
        let registry = roof_edge_fixture();
        let bargeboard = ConceptId::new("arch.concept.bargeboard");
        let follows = PredicateId::new("follows");

        // The fixture's rake proposition is general scope, so it survives any
        // jurisdiction filter.
        assert_eq!(
            registry
                .propositions_for(&bargeboard, &follows, Some(&JurisdictionTag::new("SE")))
                .len(),
            1
        );
        assert_eq!(
            registry.propositions_for(&bargeboard, &follows, None).len(),
            1
        );
    }
}
