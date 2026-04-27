//! BIM exchange identities (ADR-026 Phase 6b).
//!
//! `ExchangeIdentityMap` is a Bevy component that pairs an authored
//! entity with one or more **stable** exchange identifiers — one per
//! BIM exchange system the entity has ever been exported through.
//!
//! Per ADR-026 §2 the identifiers must survive:
//!
//! - parameter changes
//! - entity moves
//! - library upgrades
//! - re-imports
//! - file format changes
//!
//! The "never regenerate after first assignment" rule is enforced by
//! the [`ExchangeIdentityMap::assign_if_absent`] API: writes only
//! succeed when the system slot is empty. A separate
//! [`ExchangeIdentityMap::overwrite`] method exists for the rare
//! migration case (e.g. importing pre-existing GUIDs from a foreign
//! file) and is named loudly so review catches accidental use.
//!
//! Talos-internal identity (`ElementId`, `DefinitionId`, ECS
//! `Entity`) is explicitly **not** an exchange identifier. Exchange
//! identifiers are governed by the destination system's namespace
//! rules (the IFC GUID format, the Revit element id format, etc.).
//! Generating them is the export pack's responsibility; this module
//! only owns the storage and the assigned-once invariant.
//!
//! Like [`PropertySetMap`](super::property_sets::PropertySetMap),
//! this lives as a sibling component to `OccurrenceIdentity` /
//! `Definition` rather than a field inside them — keeping the
//! geometry pipeline agnostic of BIM identity state.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Identifier of a BIM exchange system. Open enum: capability packs
/// can declare additional systems via `Custom(name)` without core
/// changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeSystem {
    /// Industry Foundation Classes (the BIM exchange standard).
    /// `ExchangeId` for this system is the 22-character base64
    /// `IfcGloballyUniqueId`.
    Ifc,
    /// Autodesk Revit element identity.
    Revit,
    /// Autodesk DWG entity identity.
    Dwg,
    /// COBie (Construction-Operations Building Information Exchange).
    Cobie,
    /// Vendor- or workflow-specific identifier (e.g. a procurement
    /// system or facility-management database). The string is
    /// opaque to the kernel.
    Custom(String),
}

impl ExchangeSystem {
    /// Stable string discriminator for telemetry and UI labels.
    pub fn as_label(&self) -> &str {
        match self {
            Self::Ifc => "ifc",
            Self::Revit => "revit",
            Self::Dwg => "dwg",
            Self::Cobie => "cobie",
            Self::Custom(s) => s.as_str(),
        }
    }
}

/// Stable opaque identifier minted by an exchange system. The
/// internal format is the destination system's responsibility; the
/// kernel stores it as a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ExchangeId(pub String);

impl ExchangeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExchangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// The map
// ---------------------------------------------------------------------------

/// Why an `assign_if_absent` call did not write.
#[derive(Debug, Clone, PartialEq)]
pub enum ExchangeAssignmentRefused {
    /// The slot was already occupied; the existing value is included.
    AlreadyAssigned { existing: ExchangeId },
}

impl std::fmt::Display for ExchangeAssignmentRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyAssigned { existing } => write!(
                f,
                "exchange identifier already assigned: '{existing}'; refusing to regenerate"
            ),
        }
    }
}

impl std::error::Error for ExchangeAssignmentRefused {}

/// Per-entity map of exchange identifiers, one entry per system.
///
/// Lives as a sibling Bevy component to `OccurrenceIdentity` /
/// `Definition` (see ADR-026 §2 — *"lives on `OccurrenceIdentity`
/// and `Definition`, beside internal identity — not inside it"*).
#[derive(Component, Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeIdentityMap {
    pub entries: HashMap<ExchangeSystem, ExchangeId>,
}

impl ExchangeIdentityMap {
    /// Construct an empty map.
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Construct from a single (system, id) pair. Convenience for
    /// imports that already carry a foreign identifier.
    pub fn with(system: ExchangeSystem, id: ExchangeId) -> Self {
        let mut entries = HashMap::with_capacity(1);
        entries.insert(system, id);
        Self { entries }
    }

    /// Read the identifier for `system`, if assigned.
    pub fn get(&self, system: &ExchangeSystem) -> Option<&ExchangeId> {
        self.entries.get(system)
    }

    /// True if `system` has an identifier assigned.
    pub fn contains(&self, system: &ExchangeSystem) -> bool {
        self.entries.contains_key(system)
    }

    /// Assign `id` to `system` only if no identifier was previously
    /// assigned. This is the **default** write API — preserves the
    /// "never regenerate" rule from ADR-026 §2.
    pub fn assign_if_absent(
        &mut self,
        system: ExchangeSystem,
        id: ExchangeId,
    ) -> Result<(), ExchangeAssignmentRefused> {
        if let Some(existing) = self.entries.get(&system) {
            return Err(ExchangeAssignmentRefused::AlreadyAssigned {
                existing: existing.clone(),
            });
        }
        self.entries.insert(system, id);
        Ok(())
    }

    /// Force-overwrite the identifier for `system`. Returns the
    /// prior value if any.
    ///
    /// **Use sparingly** — this bypasses the "never regenerate"
    /// rule. The legitimate use case is a migration that adopts a
    /// pre-existing identifier from an imported file (e.g. an IFC
    /// re-import where the foreign GUID should now be authoritative).
    /// Routine export must use [`assign_if_absent`].
    pub fn overwrite(&mut self, system: ExchangeSystem, id: ExchangeId) -> Option<ExchangeId> {
        self.entries.insert(system, id)
    }

    /// Iterate `(system, id)` pairs. Order is not guaranteed.
    pub fn iter(&self) -> impl Iterator<Item = (&ExchangeSystem, &ExchangeId)> {
        self.entries.iter()
    }

    /// Number of assigned identifiers.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no identifiers are assigned.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin: present for symmetry with `PropertySetsPlugin`. The
/// `ExchangeIdentityMap` component does not require any resources or
/// systems in core — it is data attached to entities. The plugin
/// exists so applications can declare intent (`add_plugins(ExchangeIdentityPlugin)`)
/// even though it has no setup work today; this avoids forcing a
/// later breaking change if the kernel adds a registry or system.
pub struct ExchangeIdentityPlugin;

impl Plugin for ExchangeIdentityPlugin {
    fn build(&self, _app: &mut App) {
        // Intentionally empty. See struct docs.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ifc_guid() -> ExchangeId {
        ExchangeId::new("0Lh3Y2nzz3wuRfV4z4xRGn")
    }

    #[test]
    fn empty_map_has_no_entries() {
        let m = ExchangeIdentityMap::empty();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert!(m.get(&ExchangeSystem::Ifc).is_none());
    }

    #[test]
    fn with_creates_single_entry_map() {
        let m = ExchangeIdentityMap::with(ExchangeSystem::Ifc, ifc_guid());
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&ExchangeSystem::Ifc), Some(&ifc_guid()));
    }

    #[test]
    fn assign_if_absent_succeeds_on_empty_slot() {
        let mut m = ExchangeIdentityMap::empty();
        assert!(m
            .assign_if_absent(ExchangeSystem::Ifc, ifc_guid())
            .is_ok());
        assert_eq!(m.get(&ExchangeSystem::Ifc), Some(&ifc_guid()));
    }

    #[test]
    fn assign_if_absent_refuses_on_occupied_slot() {
        let mut m = ExchangeIdentityMap::empty();
        m.assign_if_absent(ExchangeSystem::Ifc, ifc_guid())
            .unwrap();
        let again = m.assign_if_absent(ExchangeSystem::Ifc, ExchangeId::new("different"));
        match again {
            Err(ExchangeAssignmentRefused::AlreadyAssigned { existing }) => {
                assert_eq!(existing, ifc_guid());
            }
            Ok(()) => panic!("must refuse to overwrite"),
        }
        // Original identifier is unchanged.
        assert_eq!(m.get(&ExchangeSystem::Ifc), Some(&ifc_guid()));
    }

    #[test]
    fn overwrite_replaces_existing_and_returns_prior() {
        let mut m = ExchangeIdentityMap::with(ExchangeSystem::Ifc, ifc_guid());
        let prior = m.overwrite(
            ExchangeSystem::Ifc,
            ExchangeId::new("import-derived-guid"),
        );
        assert_eq!(prior, Some(ifc_guid()));
        assert_eq!(
            m.get(&ExchangeSystem::Ifc),
            Some(&ExchangeId::new("import-derived-guid"))
        );
    }

    #[test]
    fn multiple_systems_coexist() {
        let mut m = ExchangeIdentityMap::empty();
        m.assign_if_absent(ExchangeSystem::Ifc, ifc_guid())
            .unwrap();
        m.assign_if_absent(ExchangeSystem::Revit, ExchangeId::new("123456"))
            .unwrap();
        m.assign_if_absent(
            ExchangeSystem::Custom("vendor.qto".into()),
            ExchangeId::new("Q-2026-04-001"),
        )
        .unwrap();
        assert_eq!(m.len(), 3);
        assert!(m.contains(&ExchangeSystem::Ifc));
        assert!(m.contains(&ExchangeSystem::Revit));
        assert!(m.contains(&ExchangeSystem::Custom("vendor.qto".into())));
    }

    #[test]
    fn custom_system_with_different_name_is_distinct() {
        let mut m = ExchangeIdentityMap::empty();
        m.assign_if_absent(
            ExchangeSystem::Custom("vendor.a".into()),
            ExchangeId::new("a"),
        )
        .unwrap();
        // Different custom name → independent slot.
        assert!(m
            .assign_if_absent(
                ExchangeSystem::Custom("vendor.b".into()),
                ExchangeId::new("b"),
            )
            .is_ok());
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn map_round_trips_through_json() {
        let mut m = ExchangeIdentityMap::empty();
        m.assign_if_absent(ExchangeSystem::Ifc, ifc_guid()).unwrap();
        m.assign_if_absent(ExchangeSystem::Revit, ExchangeId::new("rev-1"))
            .unwrap();
        let json = serde_json::to_string(&m).unwrap();
        let parsed: ExchangeIdentityMap = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn exchange_system_as_label_is_stable() {
        assert_eq!(ExchangeSystem::Ifc.as_label(), "ifc");
        assert_eq!(ExchangeSystem::Revit.as_label(), "revit");
        assert_eq!(ExchangeSystem::Dwg.as_label(), "dwg");
        assert_eq!(ExchangeSystem::Cobie.as_label(), "cobie");
        assert_eq!(
            ExchangeSystem::Custom("anything".into()).as_label(),
            "anything"
        );
    }

    #[test]
    fn iter_yields_all_assigned_pairs() {
        let mut m = ExchangeIdentityMap::empty();
        m.assign_if_absent(ExchangeSystem::Ifc, ifc_guid()).unwrap();
        m.assign_if_absent(ExchangeSystem::Revit, ExchangeId::new("rev-1"))
            .unwrap();
        let collected: Vec<(&ExchangeSystem, &ExchangeId)> = m.iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn refusal_error_carries_existing_id_for_diagnostics() {
        let mut m = ExchangeIdentityMap::with(ExchangeSystem::Ifc, ifc_guid());
        let err = m
            .assign_if_absent(ExchangeSystem::Ifc, ExchangeId::new("new"))
            .unwrap_err();
        let display = format!("{err}");
        assert!(display.contains("0Lh3Y2nzz3wuRfV4z4xRGn"));
        assert!(display.contains("regenerate"));
    }

    #[test]
    fn plugin_can_be_added_without_panic() {
        let mut app = App::new();
        app.add_plugins(ExchangeIdentityPlugin);
        // Nothing to assert beyond "no panic"; the plugin is
        // intentionally a no-op today.
        app.update();
    }
}
