//! Portable unit of Design Concept Graph delivery.
//!
//! A pack is a set of canonical authored records installed and withdrawn
//! together under one identity. Records are never delivered one by one because
//! their compiled indexes (publishers, consumers, subject/predicate dispatch)
//! span records: a proposition without the concept and anchor kind it names is
//! a dangling reference. The pack itself carries no domain content; it is the
//! envelope for whatever the curator authors.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::graph::{
    AdmissibilityProposition, AnchorKindDescriptor, Concept, PublishedAnchorContract,
};

/// Upper bound on records per pack, keeping a pack well inside the asset size limit.
pub const MAX_PACK_RECORDS: usize = 2000;

/// Authored Design Concept Graph records delivered under one identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ConceptGraphPack {
    /// Pack identity; must equal the knowledge asset id.
    pub id: String,
    #[serde(default)]
    pub concepts: Vec<Concept>,
    #[serde(default)]
    pub anchor_kinds: Vec<AnchorKindDescriptor>,
    #[serde(default)]
    pub publications: Vec<PublishedAnchorContract>,
    #[serde(default)]
    pub propositions: Vec<AdmissibilityProposition>,
}

impl ConceptGraphPack {
    /// Total number of records.
    pub fn record_count(&self) -> usize {
        self.concepts.len()
            + self.anchor_kinds.len()
            + self.publications.len()
            + self.propositions.len()
    }

    /// Structural checks: bounded, nonempty, and no record identity repeated
    /// within the pack (a later duplicate would silently shadow an earlier one).
    pub fn validate(&self) -> Result<(), String> {
        let count = self.record_count();
        if count == 0 || count > MAX_PACK_RECORDS {
            return Err(format!(
                "A concept graph pack needs between 1 and {MAX_PACK_RECORDS} records, got {count}"
            ));
        }
        fn unique<'a, T: Ord + 'a>(
            what: &str,
            keys: impl Iterator<Item = T>,
        ) -> Result<(), String> {
            let mut seen = BTreeSet::new();
            keys.into_iter()
                .all(|k| seen.insert(k))
                .then_some(())
                .ok_or_else(|| format!("Duplicate {what} identity within the pack"))
        }
        unique("concept", self.concepts.iter().map(|c| &c.id))?;
        unique("anchor kind", self.anchor_kinds.iter().map(|k| &k.id))?;
        unique(
            "publication",
            self.publications
                .iter()
                .map(|p| (&p.publisher, &p.anchor_kind)),
        )?;
        unique("proposition", self.propositions.iter().map(|p| &p.id))
    }
}
