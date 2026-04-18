//! Architectural constraint validators for PP74.
//!
//! Three validators are registered by `ArchitecturalPlugin`:
//!
//! 1. `SupportPathIntegrity` — every `wall_assembly` at `Constructible` must
//!    have a continuous `bears_on` chain to a `foundation_system` at `Constructible`
//!    or higher. The same check applies to `roof_system` reaching `wall_assembly`.
//!
//! 2. `HostOpeningGeometry` — every opening entity must be hosted on a
//!    `wall_assembly` at `Constructible` or higher. Aperture-vs-host-size check
//!    is a no-op when opening entities are not yet fully recipe-backed.
//!    TODO: activate end-to-end when openings migrate to PP70-style recipes.
//!
//! 3. `AssemblyCompleteness` — generalises the PP70 `DeclaredStateRequiresResolvedObligations`
//!    validator. Since the core engine already registers the completeness constraint
//!    centrally, this module is intentionally thin — it delegates to the same
//!    logic rather than duplicating it. Domain-specific escalation rules can be
//!    added here in PP77+.

pub mod assembly_completeness;
pub mod host_opening;
pub mod support_path;
