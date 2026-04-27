//! BIM void / fill structure (ADR-026 Phase 6f).
//!
//! ADR-026 §6 prescribes a typed three-way relationship for elements
//! that cut their host (windows, doors, penetrations):
//!
//! 1. **Host** — the wall, slab, or roof being cut.
//! 2. **Opening Occurrence** — a first-class authored entity
//!    representing the cut volume itself, with its own dimensions,
//!    BIM identity, and lifecycle.
//! 3. **Filling Occurrence** — the window or door that fills the
//!    opening.
//!
//! The cut is **not** a single `hosted_on` semantic relation,
//! because the opening element has its own authored identity,
//! geometry, BIM exchange identity, and lifecycle state. ADR-026 §6
//! requires the three-way structure.
//!
//! This module lands the additive substrate:
//!
//! - `VoidDeclaration` — declared on a `Definition` (e.g. a window
//!   definition) describing the void it cuts when placed.
//! - `VoidShape` — rectangular by parameter reference, or
//!   delegated to a profile node.
//! - `VoidPlacement` — the local-frame transform of the void
//!   relative to the filling element's slot.
//! - `VoidDeclarationRegistry` — Resource keyed by `DefinitionId`
//!   for the type-level void declaration.
//! - `OpeningContext` — Bevy component on the **opening
//!   Occurrence** entity. Records the host and filling element
//!   ids. Lives as a sibling to `OccurrenceIdentity` — the
//!   geometry pipeline does not depend on it.
//! - `VoidLink` — Bevy component on the **filling Occurrence**
//!   entity, pointing to its opening.
//! - `place_void_atomically` — helper that creates the opening
//!   and wires both back-links in a single function call. The
//!   ADR-026 §Consequences atomic-command requirement is the
//!   responsibility of the surrounding command pipeline; this
//!   helper is the data primitive.
//!
//! `Interface::void_declaration` (the field on the legacy
//! `Interface` struct in `definition.rs`) is **not** modified by
//! this slice — that change requires touching every existing
//! Definition construction site. Definitions register their void
//! declarations via the registry instead, keyed by
//! `DefinitionId`. A future non-additive slice can move the field
//! inline once all consumers are updated.

use std::collections::HashMap;

use bevy::math::DVec3;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plugins::identity::ElementId;
use crate::plugins::modeling::definition::DefinitionId;

// ---------------------------------------------------------------------------
// Shape declaration
// ---------------------------------------------------------------------------

/// Shape of a void cut. Parameter-driven for the common rectangular
/// case (window / door openings); delegated to a node id for the
/// arbitrary-shape case (custom profile cuts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VoidShape {
    /// Rectangular void sized by two parameters on the filling
    /// definition's parameter schema. The schema parameter names
    /// are stored as strings; the host's evaluator resolves them
    /// at evaluation time.
    Rectangular {
        width_param: String,
        height_param: String,
    },
    /// Profile node identifier. The named node on the filling
    /// definition's evaluator graph supplies the cut profile (an
    /// arbitrary 2D outline). The string is opaque to this module.
    Profile { node_id: String },
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// Position of the void within the filling element's local frame.
/// Storage-only — the host evaluator interprets this when
/// performing the boolean subtraction.
///
/// Kept minimal: the ADR mentions `SlotTransform` from ADR-025; the
/// actual `SlotTransform` type lives in the slot/template
/// substrate. To stay additive, we use a self-contained
/// translation + axis-aligned orientation here. A follow-up may
/// upgrade this field to the full `SlotTransform` once that type
/// stabilises.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VoidPlacement {
    /// Translation from the filling element's origin to the void
    /// centre, in metres.
    pub translation: [f64; 3],
    /// Rotation about the world-up axis, in radians. Most window
    /// / door cuts only need axis-aligned placement; the broader
    /// orientation is captured by the filling element's transform.
    pub yaw_radians: f64,
}

impl VoidPlacement {
    pub fn at_origin() -> Self {
        Self {
            translation: [0.0, 0.0, 0.0],
            yaw_radians: 0.0,
        }
    }

    pub fn at(translation: DVec3) -> Self {
        Self {
            translation: [translation.x, translation.y, translation.z],
            yaw_radians: 0.0,
        }
    }

    pub fn translation_dvec3(&self) -> DVec3 {
        DVec3::new(self.translation[0], self.translation[1], self.translation[2])
    }
}

impl Default for VoidPlacement {
    fn default() -> Self {
        Self::at_origin()
    }
}

// ---------------------------------------------------------------------------
// VoidDeclaration
// ---------------------------------------------------------------------------

/// Type-level declaration that placing this Definition cuts a void
/// in its host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoidDeclaration {
    pub shape: VoidShape,
    #[serde(default)]
    pub placement: VoidPlacement,
    /// Free-form tag that export packs map to format-specific
    /// concepts (e.g. IFC pack maps `"Opening"` to
    /// `IfcOpeningElement`).
    pub exchange_role: String,
}

impl VoidDeclaration {
    pub fn rectangular(
        width_param: impl Into<String>,
        height_param: impl Into<String>,
    ) -> Self {
        Self {
            shape: VoidShape::Rectangular {
                width_param: width_param.into(),
                height_param: height_param.into(),
            },
            placement: VoidPlacement::at_origin(),
            exchange_role: "Opening".to_string(),
        }
    }

    pub fn with_placement(mut self, placement: VoidPlacement) -> Self {
        self.placement = placement;
        self
    }

    pub fn with_exchange_role(mut self, role: impl Into<String>) -> Self {
        self.exchange_role = role.into();
        self
    }
}

/// Bevy resource: per-`DefinitionId` void declarations registered by
/// capability crates. A Definition without a registered declaration
/// does not cut a void when placed.
#[derive(Resource, Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoidDeclarationRegistry {
    pub by_definition: HashMap<DefinitionId, VoidDeclaration>,
}

impl VoidDeclarationRegistry {
    pub fn register(
        &mut self,
        definition_id: DefinitionId,
        declaration: VoidDeclaration,
    ) -> Option<VoidDeclaration> {
        self.by_definition.insert(definition_id, declaration)
    }

    pub fn get(&self, definition_id: &DefinitionId) -> Option<&VoidDeclaration> {
        self.by_definition.get(definition_id)
    }
}

// ---------------------------------------------------------------------------
// Three-way back-links
// ---------------------------------------------------------------------------

/// Bevy component placed on the **opening Occurrence** entity.
/// Records the host (the wall being cut) and the filling
/// (the window/door).
///
/// Per ADR-026 §6 the opening is a first-class authored Occurrence
/// in the occurrence graph with its own ElementId, OccurrenceIdentity,
/// and BIM exchange identity. This component carries the back-links
/// that make the three-way relationship navigable.
#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpeningContext {
    /// Element id of the host being cut.
    pub host: ElementId,
    /// Element id of the filling element that "owns" this opening.
    /// May be set later if the filling is placed after the opening
    /// (rare but allowed); start with `None`.
    pub filling: Option<ElementId>,
}

impl OpeningContext {
    pub fn new(host: ElementId) -> Self {
        Self {
            host,
            filling: None,
        }
    }

    pub fn with_filling(mut self, filling: ElementId) -> Self {
        self.filling = Some(filling);
        self
    }
}

/// Bevy component placed on the **filling Occurrence** entity
/// (the window/door). Points to the opening it created.
#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VoidLink {
    /// Element id of the opening Occurrence this filling owns.
    pub opening: ElementId,
}

// ---------------------------------------------------------------------------
// Atomic placement helper
// ---------------------------------------------------------------------------

/// Errors that can be raised by the void-placement helper.
#[derive(Debug, Clone, PartialEq)]
pub enum VoidPlacementError {
    /// The filling Definition has no `VoidDeclaration` registered.
    NoVoidDeclaration { definition_id: DefinitionId },
    /// The filling element id and host element id are equal.
    HostIsFilling,
}

impl std::fmt::Display for VoidPlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoVoidDeclaration { definition_id } => write!(
                f,
                "definition '{}' has no VoidDeclaration registered",
                definition_id.0
            ),
            Self::HostIsFilling => f.write_str("filling element cannot be its own host"),
        }
    }
}

impl std::error::Error for VoidPlacementError {}

/// Plan for an atomic void placement.
///
/// The caller holds whatever entity / element handles it has and
/// applies this plan in a single command. Splitting the plan from
/// its application keeps the helper testable without a full Bevy
/// world setup, and lets the command system enforce
/// ADR-026 §Consequences's atomic-undo requirement.
#[derive(Debug, Clone, PartialEq)]
pub struct VoidPlacementOutcome {
    /// Newly minted `ElementId` for the opening Occurrence.
    pub opening_element: ElementId,
    /// `VoidLink` to attach to the filling entity.
    pub filling_link: VoidLink,
    /// `OpeningContext` to attach to the freshly-spawned opening
    /// entity.
    pub opening_context: OpeningContext,
}

/// Plan a void placement: validate inputs, allocate a fresh
/// `ElementId` for the opening, and produce the components that
/// should be attached.
///
/// The caller is responsible for the actual world mutation:
/// spawning the opening Occurrence entity, inserting the
/// `OpeningContext` on it, inserting the `VoidLink` on the filling,
/// triggering the host's evaluator to subtract the void.
///
/// Per ADR-026 §6 #4: "the host element's Evaluator is responsible
/// for subtracting the void from its solid" — that is plumbed by
/// the host's recipe, not by this helper.
pub fn plan_void_placement(
    registry: &VoidDeclarationRegistry,
    filling_definition: &DefinitionId,
    host: ElementId,
    filling: ElementId,
    next_opening_id: ElementId,
) -> Result<VoidPlacementOutcome, VoidPlacementError> {
    if registry.get(filling_definition).is_none() {
        return Err(VoidPlacementError::NoVoidDeclaration {
            definition_id: filling_definition.clone(),
        });
    }
    if host == filling {
        return Err(VoidPlacementError::HostIsFilling);
    }
    Ok(VoidPlacementOutcome {
        opening_element: next_opening_id,
        filling_link: VoidLink {
            opening: next_opening_id,
        },
        opening_context: OpeningContext {
            host,
            filling: Some(filling),
        },
    })
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Bevy plugin: installs `VoidDeclarationRegistry`. Components
/// (`OpeningContext`, `VoidLink`) are entity data — no resources
/// or systems are needed today.
pub struct VoidDeclarationPlugin;

impl Plugin for VoidDeclarationPlugin {
    fn build(&self, app: &mut App) {
        if !app
            .world()
            .contains_resource::<VoidDeclarationRegistry>()
        {
            app.init_resource::<VoidDeclarationRegistry>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_def() -> DefinitionId {
        DefinitionId("window.double_european_v1".into())
    }

    fn door_def() -> DefinitionId {
        DefinitionId("door.entrance_v1".into())
    }

    fn rectangular_window() -> VoidDeclaration {
        VoidDeclaration::rectangular("opening_width_m", "opening_height_m")
    }

    #[test]
    fn rectangular_constructor_uses_opening_role() {
        let v = rectangular_window();
        assert_eq!(v.exchange_role, "Opening");
    }

    #[test]
    fn rectangular_placement_default_is_at_origin() {
        let v = rectangular_window();
        assert_eq!(v.placement, VoidPlacement::at_origin());
    }

    #[test]
    fn placement_at_round_trips_translation() {
        let p = VoidPlacement::at(DVec3::new(0.4, 0.0, -0.2));
        assert_eq!(p.translation, [0.4, 0.0, -0.2]);
        assert_eq!(p.translation_dvec3(), DVec3::new(0.4, 0.0, -0.2));
    }

    #[test]
    fn declaration_with_placement_and_role_builders() {
        let v = rectangular_window()
            .with_placement(VoidPlacement::at(DVec3::new(0.0, 1.0, 0.0)))
            .with_exchange_role("CustomOpening");
        assert_eq!(v.exchange_role, "CustomOpening");
        assert_eq!(v.placement.translation_dvec3().y, 1.0);
    }

    #[test]
    fn registry_register_and_get() {
        let mut reg = VoidDeclarationRegistry::default();
        let prior = reg.register(window_def(), rectangular_window());
        assert!(prior.is_none());
        let fetched = reg.get(&window_def()).unwrap();
        assert_eq!(fetched.exchange_role, "Opening");
    }

    #[test]
    fn registry_register_returns_prior_on_overwrite() {
        let mut reg = VoidDeclarationRegistry::default();
        reg.register(window_def(), rectangular_window());
        let new = VoidDeclaration::rectangular("w", "h").with_exchange_role("Custom");
        let prior = reg.register(window_def(), new);
        assert!(prior.is_some());
        assert_eq!(prior.unwrap().exchange_role, "Opening");
    }

    #[test]
    fn opening_context_records_host_and_optional_filling() {
        let ctx = OpeningContext::new(ElementId(10));
        assert_eq!(ctx.host, ElementId(10));
        assert!(ctx.filling.is_none());

        let ctx = ctx.with_filling(ElementId(11));
        assert_eq!(ctx.filling, Some(ElementId(11)));
    }

    #[test]
    fn plan_void_placement_succeeds_for_registered_definition() {
        let mut reg = VoidDeclarationRegistry::default();
        reg.register(window_def(), rectangular_window());
        let outcome =
            plan_void_placement(&reg, &window_def(), ElementId(1), ElementId(2), ElementId(3))
                .unwrap();
        assert_eq!(outcome.opening_element, ElementId(3));
        assert_eq!(outcome.filling_link.opening, ElementId(3));
        assert_eq!(outcome.opening_context.host, ElementId(1));
        assert_eq!(outcome.opening_context.filling, Some(ElementId(2)));
    }

    #[test]
    fn plan_void_placement_rejects_unregistered_definition() {
        let reg = VoidDeclarationRegistry::default();
        let err = plan_void_placement(
            &reg,
            &door_def(),
            ElementId(1),
            ElementId(2),
            ElementId(3),
        )
        .unwrap_err();
        match err {
            VoidPlacementError::NoVoidDeclaration { definition_id } => {
                assert_eq!(definition_id, door_def());
            }
            _ => panic!("expected NoVoidDeclaration"),
        }
    }

    #[test]
    fn plan_void_placement_rejects_self_hosting() {
        let mut reg = VoidDeclarationRegistry::default();
        reg.register(window_def(), rectangular_window());
        let err = plan_void_placement(
            &reg,
            &window_def(),
            ElementId(7),
            ElementId(7),
            ElementId(8),
        )
        .unwrap_err();
        assert_eq!(err, VoidPlacementError::HostIsFilling);
    }

    #[test]
    fn declaration_round_trips_through_json() {
        let v = rectangular_window()
            .with_placement(VoidPlacement::at(DVec3::new(0.5, 1.2, 0.0)))
            .with_exchange_role("Opening");
        let json = serde_json::to_string(&v).unwrap();
        let parsed: VoidDeclaration = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, v);
    }

    #[test]
    fn void_shape_profile_variant_round_trips() {
        let v = VoidDeclaration {
            shape: VoidShape::Profile {
                node_id: "vault_arch_profile".into(),
            },
            placement: VoidPlacement::at_origin(),
            exchange_role: "Opening".into(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let parsed: VoidDeclaration = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, v);
    }

    #[test]
    fn opening_context_round_trips_through_json() {
        let ctx = OpeningContext::new(ElementId(42)).with_filling(ElementId(99));
        let json = serde_json::to_string(&ctx).unwrap();
        let parsed: OpeningContext = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ctx);
    }

    #[test]
    fn plugin_installs_registry() {
        let mut app = App::new();
        app.add_plugins(VoidDeclarationPlugin);
        assert!(app.world().contains_resource::<VoidDeclarationRegistry>());
    }
}
