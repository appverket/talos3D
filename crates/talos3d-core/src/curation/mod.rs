//! Curation substrate for curated knowledge assets.
//!
//! See ADR-040 (primary), ADR-041 (recipe-specific layer), and the two
//! cross-agent agreements:
//!
//! - `private/proposals/CURATION_SUBSTRATE_AGREEMENT.md`
//! - `private/proposals/DYNAMIC_RECIPE_LEARNING_AGREEMENT.md`
//!
//! This module owns the generic governance/provenance/publication/dependency/
//! pack contracts shared across curated kinds (recipes, material specs,
//! product entries, code rule packs, future vertical kinds). Domain-specific
//! bodies, validators, and authoring MCP surfaces live in capability crates.

pub mod compatibility;
pub mod dependencies;
pub mod identity;
pub mod meta;
pub mod pack;
pub mod policy;
pub mod provenance;
pub mod scope_trust;
pub mod source;

pub use compatibility::{CapabilityCompat, CompatibilityRef, SchemaVersion, VersionReq};
pub use dependencies::{DependencyRef, DependencyRole};
pub use identity::{
    AssetId, AssetKindId, AssetRevision, ContentHash, PackId, PackRevision, SourceId,
    SourceRevision,
};
pub use meta::CurationMeta;
pub use pack::{EntitlementHook, PackManifest, PackRef};
pub use policy::{JurisdictionPolicyHookId, LicenseMode, PublicationPolicy, ValidityFloor};
pub use provenance::{
    CatalogRef, Confidence, EvidenceRef, ExcerptRef, GroundingKind, JurisdictionTag, Lineage,
    Provenance,
};
pub use scope_trust::{Scope, Trust, ValidationStatus};
pub use source::{SourceLicense, SourceRegistryEntry, SourceStatus, SourceTier};
