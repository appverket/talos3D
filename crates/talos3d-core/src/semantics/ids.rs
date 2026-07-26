//! Stable identifiers for the Design Concept Graph.
//!
//! Every id is an opaque interned string. Core never interprets the contents:
//! `arch.concept.bargeboard` and `naval.concept.rubbing_strake` are equally
//! opaque here, which is what keeps this module domain-neutral per ADR-064 §13.

use serde::{Deserialize, Serialize};

/// Declares an opaque string newtype id with the derives every DCG id needs.
///
/// These ids are structurally identical and differ only in what they may be
/// confused with — which is precisely why they are distinct types rather than
/// bare `String`s. The macro keeps the shared shape in one place.
macro_rules! semantic_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

semantic_id!(
    /// Stable identity of a domain concept, e.g. `arch.concept.bargeboard`.
    ///
    /// A concept is *meaning*. It is a separate axis from
    /// [`ElementClassId`](crate::capability_registry::ElementClassId), which is
    /// *representation*. Neither is generated from the other (ADR-064 §5).
    ConceptId
);

semantic_id!(
    /// Stable identity of an anchor kind, e.g. `arch.anchor.roof.rake_edge`.
    ///
    /// Anchor kinds are public API: retiring one is a breaking change that
    /// requires migration (ADR-064 Consequences).
    AnchorKindId
);

semantic_id!(
    /// A registered relation predicate, e.g. `follows`, `supported_by`.
    ///
    /// Predicate identity stays owned by `RelationTypeDescriptor`; an
    /// admissibility proposition only *narrows* a predicate for a concept
    /// (ADR-064 §2).
    PredicateId
);

semantic_id!(
    /// Stable identity of one admissibility proposition.
    PropositionId
);

semantic_id!(
    /// Role-derived discriminator distinguishing sibling anchor instances
    /// published by the same host, e.g. `north_west`.
    ///
    /// Load-bearing: anchor-instance identity is the publishing element plus
    /// this role, never the current coordinates, so regeneration re-resolves
    /// rather than detaching dependents (ADR-064 §1, §5).
    AnchorRoleId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_types_with_shared_shape() {
        let concept = ConceptId::new("arch.concept.bargeboard");
        let anchor = AnchorKindId::from("arch.anchor.roof.rake_edge");
        assert_eq!(concept.as_str(), "arch.concept.bargeboard");
        assert_eq!(anchor.to_string(), "arch.anchor.roof.rake_edge");
    }

    #[test]
    fn ids_roundtrip_through_serde() {
        let id = PropositionId::new("arch.prop.bargeboard_follows_rake");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"arch.prop.bargeboard_follows_rake\"");
        assert_eq!(serde_json::from_str::<PropositionId>(&json).unwrap(), id);
    }
}
