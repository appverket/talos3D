//! View-intent records that can be declared before their target assembly exists.
//!
//! Core stores opaque per-view objects, resolves unambiguous labels and binds
//! records to semantic assemblies. Capabilities own the field vocabulary,
//! automatic binding policy and fidelity validators. Pending records are
//! session-scoped; bound records persist in the target assembly's parameters.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// One declared-but-unattached elevation description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingElevationIntent {
    pub intent_id: u64,
    /// Optional human name for the assembly this describes. When several
    /// intents are pending, this is what disambiguates them.
    pub label: Option<String>,
    /// The array of per-elevation intent objects, verbatim.
    pub elevations: serde_json::Value,
}

/// Session-scoped store of intents awaiting a assembly.
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

    /// The intent that unambiguously belongs to a assembly with this label.
    ///
    /// A label match wins outright. Failing that, a single pending intent is
    /// taken to be the one meant — one description and one assembly is the
    /// ordinary case. Several unbound intents with no matching label resolve to
    /// nothing on purpose: guessing there would attach a description to the
    /// wrong assembly and report confident nonsense about it.
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
    fn a_single_pending_intent_resolves_for_any_assembly() {
        let mut store = PendingElevationIntents::default();
        store.declare(None, elevations());
        assert!(store
            .resolve_for("Whatever the assembly is called")
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
    /// assembly would produce confident, wrong findings about it.
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
