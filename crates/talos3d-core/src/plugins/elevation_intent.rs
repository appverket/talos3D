//! Elevation intent that can be declared before the building exists.
//!
//! The depiction-first gate is only worth having if it genuinely comes first.
//! An intent stored *on* the building assembly cannot: the assembly has to exist
//! to be written to, so the author must build something before describing it —
//! exactly backwards, and a cold agent hit that catch-22 immediately.
//!
//! The fix is to stop treating the description as a property of the thing it
//! describes. An intent is a record in its own right, declarable as the very
//! first call of a session, and *bound* to a building whenever one exists. That
//! ordering is what makes it a gate rather than an annotation.
//!
//! Two states, and the second is the useful one:
//!
//! * **Pending** — declared, not yet attached to anything. Lives here.
//! * **Bound** — written into the assembly's own parameters, so it persists with
//!   the project and travels with the building it describes.
//!
//! Binding can be explicit, or implicit when there is exactly one candidate.
//! Implicit binding is what keeps the common case (one building, one
//! description) free of ceremony without making the ambiguous case guess: with
//! several unbound intents and no label to match on, the validator says so
//! rather than picking one.
//!
//! An intent that is never bound is itself a finding. Declaring a building and
//! then building something else is precisely the failure this gate exists to
//! catch, and silence there would make the whole mechanism optional.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// One declared-but-unattached elevation description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingElevationIntent {
    pub intent_id: u64,
    /// Optional human name for the building this describes. When several
    /// intents are pending, this is what disambiguates them.
    pub label: Option<String>,
    /// The array of per-elevation intent objects, verbatim.
    pub elevations: serde_json::Value,
}

/// Session-scoped store of intents awaiting a building.
///
/// Deliberately not persisted: an unbound intent is a statement about work in
/// progress, not part of the document. The moment it binds it moves into the
/// assembly's parameters and persists from there.
#[derive(Resource, Debug, Default)]
pub struct PendingElevationIntents {
    next_id: u64,
    intents: Vec<PendingElevationIntent>,
}

impl PendingElevationIntents {
    pub fn declare(&mut self, label: Option<String>, elevations: serde_json::Value) -> u64 {
        self.next_id += 1;
        let intent_id = self.next_id;
        // Re-declaring for the same label replaces rather than accumulates, so
        // correcting a description does not leave a stale one to match against.
        if let Some(label) = &label {
            self.intents
                .retain(|intent| intent.label.as_deref() != Some(label.as_str()));
        }
        self.intents.push(PendingElevationIntent {
            intent_id,
            label,
            elevations,
        });
        intent_id
    }

    pub fn take(&mut self, intent_id: u64) -> Option<PendingElevationIntent> {
        let index = self
            .intents
            .iter()
            .position(|intent| intent.intent_id == intent_id)?;
        Some(self.intents.remove(index))
    }

    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }

    pub fn len(&self) -> usize {
        self.intents.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PendingElevationIntent> {
        self.intents.iter()
    }

    /// The intent that unambiguously belongs to a building with this label.
    ///
    /// A label match wins outright. Failing that, a single pending intent is
    /// taken to be the one meant — one description and one building is the
    /// ordinary case. Several unbound intents with no matching label resolve to
    /// nothing on purpose: guessing there would attach a description to the
    /// wrong building and report confident nonsense about it.
    pub fn resolve_for(&self, assembly_label: &str) -> Option<&PendingElevationIntent> {
        if let Some(matched) = self.intents.iter().find(|intent| {
            intent
                .label
                .as_deref()
                .is_some_and(|label| label.eq_ignore_ascii_case(assembly_label.trim()))
        }) {
            return Some(matched);
        }
        match self.intents.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elevations() -> serde_json::Value {
        serde_json::json!([{ "elevation": "south", "apex_count": 1 }])
    }

    #[test]
    fn a_single_pending_intent_resolves_for_any_building() {
        let mut store = PendingElevationIntents::default();
        store.declare(None, elevations());
        assert!(store
            .resolve_for("Whatever the building is called")
            .is_some());
    }

    #[test]
    fn a_matching_label_wins_over_position() {
        let mut store = PendingElevationIntents::default();
        store.declare(
            Some("Barn".into()),
            serde_json::json!([{ "elevation": "north" }]),
        );
        let cottage = store.declare(Some("Cottage".into()), elevations());
        let resolved = store.resolve_for("cottage").expect("label match");
        assert_eq!(resolved.intent_id, cottage);
    }

    /// Ambiguity must resolve to nothing. Attaching a description to the wrong
    /// building would produce confident, wrong findings about it.
    #[test]
    fn several_unlabelled_intents_resolve_to_nothing() {
        let mut store = PendingElevationIntents::default();
        store.declare(None, elevations());
        store.declare(None, elevations());
        assert!(store.resolve_for("Cottage").is_none());
    }

    #[test]
    fn redeclaring_a_label_replaces_rather_than_accumulates() {
        let mut store = PendingElevationIntents::default();
        store.declare(
            Some("Cottage".into()),
            serde_json::json!([{ "elevation": "north" }]),
        );
        let second = store.declare(Some("Cottage".into()), elevations());
        assert_eq!(store.len(), 1);
        assert_eq!(store.resolve_for("Cottage").unwrap().intent_id, second);
    }

    #[test]
    fn taking_an_intent_removes_it() {
        let mut store = PendingElevationIntents::default();
        let id = store.declare(None, elevations());
        assert!(store.take(id).is_some());
        assert!(store.is_empty());
        assert!(store.take(id).is_none());
    }
}
