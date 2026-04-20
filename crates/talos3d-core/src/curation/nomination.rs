//! `NominationQueue` — pending source-registry changes proposed by
//! agents or users, awaiting human approval.
//!
//! Per ADR-040 the flow is agent-nominates / user-approves:
//!
//! 1. An agent encounters a source it wants to cite that isn't
//!    registered yet, or finds a cited source has been superseded.
//! 2. It pushes a `Nomination` via `NominationQueue::push`.
//! 3. A user (or an auto-approve policy layered on top) calls
//!    `approve` or `reject`.
//! 4. Approval mutates the `SourceRegistry`; rejection discards.
//!
//! This module owns only the queue and the approval primitives. The
//! actual MCP tools that expose the flow land in PP80 slice 6.

use std::collections::VecDeque;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::identity::{SourceId, SourceRevision};
use super::registry::SourceRegistry;
use super::source::SourceRegistryEntry;

/// Stable id of a single nomination. Auto-assigned by the queue on
/// push.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct NominationId(pub String);

/// What the nomination is asking the registry to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NominationKind {
    /// Add a new source entry to the registry.
    AddSource { entry: SourceRegistryEntry },
    /// Mark an existing source entry superseded.
    SunsetSource {
        source_id: SourceId,
        revision: SourceRevision,
        replacement: Option<SourceId>,
        reason: String,
    },
}

/// A single pending nomination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Nomination {
    pub id: NominationId,
    pub kind: NominationKind,
    /// Free-form attribution (e.g. `"agent:codex"`, `"user:hjon"`).
    pub proposed_by: String,
    /// Unix seconds.
    pub proposed_at: i64,
    /// Free-form justification; shown when the user reviews the
    /// nomination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// The reason an approval or rejection failed (if any).
#[derive(Debug, Clone, PartialEq)]
pub enum NominationError {
    NotFound(NominationId),
    TargetNotInRegistry {
        source_id: SourceId,
        revision: SourceRevision,
    },
}

impl std::fmt::Display for NominationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "no nomination with id {}", id.0),
            Self::TargetNotInRegistry { source_id, revision } => write!(
                f,
                "sunset target ({}, {}) not found in SourceRegistry",
                source_id.0, revision.0,
            ),
        }
    }
}

impl std::error::Error for NominationError {}

/// Bevy resource holding pending nominations. Persists with the project
/// document at `Project` scope (nominations for Canonical seed changes,
/// jurisdictional-pack changes, or org-level promotions are handled by
/// the operator flow, not this queue).
#[derive(Resource, Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct NominationQueue {
    pending: VecDeque<Nomination>,
    next_serial: u64,
}

impl NominationQueue {
    /// Push a new nomination; returns the assigned id.
    pub fn push(
        &mut self,
        kind: NominationKind,
        proposed_by: impl Into<String>,
        proposed_at: i64,
        justification: Option<String>,
    ) -> NominationId {
        let id = NominationId(format!("nom-{}", self.next_serial));
        self.next_serial += 1;
        self.pending.push_back(Nomination {
            id: id.clone(),
            kind,
            proposed_by: proposed_by.into(),
            proposed_at,
            justification,
        });
        id
    }

    pub fn list(&self) -> &VecDeque<Nomination> {
        &self.pending
    }

    pub fn get(&self, id: &NominationId) -> Option<&Nomination> {
        self.pending.iter().find(|n| &n.id == id)
    }

    /// Apply a pending nomination against the registry, then remove it
    /// from the queue. Returns the applied nomination on success.
    pub fn approve(
        &mut self,
        id: &NominationId,
        registry: &mut SourceRegistry,
    ) -> Result<Nomination, NominationError> {
        let position = self
            .pending
            .iter()
            .position(|n| &n.id == id)
            .ok_or_else(|| NominationError::NotFound(id.clone()))?;
        // Validate before mutating.
        if let NominationKind::SunsetSource {
            source_id,
            revision,
            ..
        } = &self.pending[position].kind
        {
            if registry.get(source_id, revision).is_none() {
                return Err(NominationError::TargetNotInRegistry {
                    source_id: source_id.clone(),
                    revision: revision.clone(),
                });
            }
        }
        let nomination = self
            .pending
            .remove(position)
            .expect("position came from iter above");
        match &nomination.kind {
            NominationKind::AddSource { entry } => {
                registry.insert(entry.clone());
            }
            NominationKind::SunsetSource {
                source_id,
                revision,
                replacement,
                reason: _,
            } => {
                registry.supersede(source_id, revision, replacement.clone());
            }
        }
        Ok(nomination)
    }

    /// Discard a pending nomination without mutating the registry.
    pub fn reject(
        &mut self,
        id: &NominationId,
        _reason: Option<String>,
    ) -> Result<Nomination, NominationError> {
        let position = self
            .pending
            .iter()
            .position(|n| &n.id == id)
            .ok_or_else(|| NominationError::NotFound(id.clone()))?;
        Ok(self
            .pending
            .remove(position)
            .expect("position came from iter above"))
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drain the queue (used by persistence code that needs to save
    /// contents to disk and reload into a fresh queue).
    pub fn drain(&mut self) -> impl Iterator<Item = Nomination> + '_ {
        self.pending.drain(..)
    }

    pub fn restore(&mut self, nominations: Vec<Nomination>) {
        let max_serial = nominations
            .iter()
            .filter_map(|n| n.id.0.strip_prefix("nom-"))
            .filter_map(|s| s.parse::<u64>().ok())
            .max();
        self.pending = nominations.into();
        if let Some(m) = max_serial {
            self.next_serial = m + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::source::{SourceLicense, SourceTier};

    fn sample_entry(id: &str, rev: &str) -> SourceRegistryEntry {
        SourceRegistryEntry::new(
            SourceId::new(id),
            SourceRevision::new(rev),
            format!("{id} {rev}"),
            "Test",
            SourceTier::Project,
            SourceLicense::PublicDomain,
        )
    }

    #[test]
    fn push_assigns_sequential_ids() {
        let mut q = NominationQueue::default();
        let a = q.push(
            NominationKind::AddSource {
                entry: sample_entry("a", "v1"),
            },
            "agent:test",
            0,
            None,
        );
        let b = q.push(
            NominationKind::AddSource {
                entry: sample_entry("b", "v1"),
            },
            "agent:test",
            0,
            None,
        );
        assert_eq!(a.0, "nom-0");
        assert_eq!(b.0, "nom-1");
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn approve_add_source_inserts_into_registry_and_removes_nomination() {
        let mut q = NominationQueue::default();
        let mut reg = SourceRegistry::default();
        let id = q.push(
            NominationKind::AddSource {
                entry: sample_entry("boverket.bbr.8", "2011:6"),
            },
            "agent:codex",
            1_700_000_000,
            Some("cited while authoring stair rules".into()),
        );
        q.approve(&id, &mut reg).unwrap();
        assert!(q.is_empty());
        assert!(reg
            .get(
                &SourceId::new("boverket.bbr.8"),
                &SourceRevision::new("2011:6")
            )
            .is_some());
    }

    #[test]
    fn approve_sunset_updates_registry_status() {
        let mut q = NominationQueue::default();
        let mut reg = SourceRegistry::default();
        reg.insert(sample_entry("bbr.8", "2011:6"));
        let id = q.push(
            NominationKind::SunsetSource {
                source_id: SourceId::new("bbr.8"),
                revision: SourceRevision::new("2011:6"),
                replacement: Some(SourceId::new("bbr.8.v2025")),
                reason: "superseded by 2025:1".into(),
            },
            "agent:codex",
            0,
            None,
        );
        q.approve(&id, &mut reg).unwrap();
        let e = reg
            .get(&SourceId::new("bbr.8"), &SourceRevision::new("2011:6"))
            .unwrap();
        assert!(matches!(e.status, crate::curation::SourceStatus::Superseded { .. }));
    }

    #[test]
    fn approve_sunset_of_missing_target_errors_and_leaves_nomination_in_queue() {
        let mut q = NominationQueue::default();
        let mut reg = SourceRegistry::default();
        let id = q.push(
            NominationKind::SunsetSource {
                source_id: SourceId::new("unknown"),
                revision: SourceRevision::new("?"),
                replacement: None,
                reason: "test".into(),
            },
            "agent:test",
            0,
            None,
        );
        let err = q.approve(&id, &mut reg).unwrap_err();
        assert!(matches!(err, NominationError::TargetNotInRegistry { .. }));
        assert_eq!(q.len(), 1, "failed approval must not drop the nomination");
    }

    #[test]
    fn reject_drops_without_mutating_registry() {
        let mut q = NominationQueue::default();
        let mut reg = SourceRegistry::default();
        let id = q.push(
            NominationKind::AddSource {
                entry: sample_entry("a", "v1"),
            },
            "agent:test",
            0,
            None,
        );
        q.reject(&id, Some("out of scope".into())).unwrap();
        assert!(q.is_empty());
        assert!(reg.is_empty());
    }

    #[test]
    fn restore_preserves_next_serial_so_new_pushes_dont_collide() {
        let mut q = NominationQueue::default();
        q.restore(vec![
            Nomination {
                id: NominationId("nom-5".into()),
                kind: NominationKind::AddSource {
                    entry: sample_entry("a", "v1"),
                },
                proposed_by: "agent:test".into(),
                proposed_at: 0,
                justification: None,
            },
            Nomination {
                id: NominationId("nom-7".into()),
                kind: NominationKind::AddSource {
                    entry: sample_entry("b", "v1"),
                },
                proposed_by: "agent:test".into(),
                proposed_at: 0,
                justification: None,
            },
        ]);
        let next = q.push(
            NominationKind::AddSource {
                entry: sample_entry("c", "v1"),
            },
            "agent:test",
            0,
            None,
        );
        assert_eq!(next.0, "nom-8");
    }

    #[test]
    fn nomination_round_trips_through_json() {
        let n = Nomination {
            id: NominationId("nom-3".into()),
            kind: NominationKind::AddSource {
                entry: sample_entry("iso.80000-1", "2022"),
            },
            proposed_by: "agent:codex".into(),
            proposed_at: 1_700_000_000,
            justification: Some("needed for dimensional consistency".into()),
        };
        let json = serde_json::to_string(&n).unwrap();
        let parsed: Nomination = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, n);
    }

    #[test]
    fn queue_round_trips_through_json() {
        let mut q = NominationQueue::default();
        q.push(
            NominationKind::AddSource {
                entry: sample_entry("a", "v1"),
            },
            "agent:test",
            0,
            None,
        );
        q.push(
            NominationKind::SunsetSource {
                source_id: SourceId::new("b"),
                revision: SourceRevision::new("v1"),
                replacement: None,
                reason: "test".into(),
            },
            "agent:test",
            0,
            None,
        );
        let json = serde_json::to_string(&q).unwrap();
        let parsed: NominationQueue = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, q);
    }
}
