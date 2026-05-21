//! Relational & parametric substrate (ADR-007 realization).
//!
//! Generic core mechanism for parametric authored components: one typed
//! dependency graph + dirty scheduler ([`graph`]), with later proof points
//! adding the scalar expression layer, Definition interior, incremental
//! `AuthoringScript` propagation, the bounded lock solver, and parallel
//! evaluation on top of this root.
//!
//! Domain content (component types, derivations, evaluation functions) lives in
//! capability crates per ADR-037; this module names no discipline nouns.

pub mod component;
pub mod graph;
pub mod incremental;
pub mod param_expr;
pub mod parallel;
pub mod service;
pub mod transform;

pub use component::{
    derived_part_id, diff_parts, ComponentParams, DriverEditError, DriverPolicy, OccurrenceDrivers,
    ParamRole, PartDiff,
};
pub use graph::{CycleError, DependencyGraph, NodeId};
