pub mod architectural_plugin;
pub mod components;
pub mod create_commands;
pub mod hosted_layout;
pub mod mesh_generation;
pub mod rules;
pub mod snapshots;
pub mod tools;

// Per ADR-037, architectural *semantic content* (element classes, recipe
// families, domain validators) lives in `talos3d-architecture-core` in the
// talos3d-architecture repo. This crate keeps only the legacy wall/opening
// factories and geometry primitives. Applications wanting the semantic
// substrate register both `ArchitecturalPlugin` (this crate) and
// `ArchitectureCorePlugin` (architecture-core crate).
pub use architectural_plugin::ArchitecturalPlugin;
