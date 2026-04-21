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

pub mod api;
pub mod authoring_script;
pub mod compat_shim;
pub mod compatibility;
pub mod dependencies;
pub mod entitlement;
pub mod identity;
pub mod meta;
pub mod nomination;
pub mod pack;
pub mod plugin;
pub mod policy;
pub mod provenance;
pub mod publication;
pub mod recipes;
pub mod registry;
pub mod replay;
pub mod scope_trust;
pub mod source;

pub use authoring_script::{
    ArgExpr, AuthoringScript, AuthoringScriptStructuralError, McpToolId, MutationScope, OutputPath,
    Postcondition, Predicate, Step, StepId, AUTHORING_SCRIPT_SCHEMA_VERSION,
};
pub use plugin::CurationPlugin;
pub use replay::{
    replay, InvocationError, InvocationReport, PostconditionOracle, PostconditionResult,
    PostconditionVerdict, ResolvedPostcondition, ToolCall, ToolDispatchError, ToolDispatcher,
};
pub use publication::{
    evidence_resolution_report, PublicationFinding, PublicationFindingSeverity,
};
pub use recipes::{
    mirror_recipe_descriptors_to_artifacts, recipe_artifact_from_descriptor, NativeFnId,
    RecipeArtifact, RecipeArtifactRegistry, RecipeBody, ScenarioTest, RECIPE_ARTIFACT_KIND,
};

pub use compat_shim::corpus_provenance_to_registry_entry;
pub use compatibility::{CapabilityCompat, CompatibilityRef, SchemaVersion, VersionReq};
pub use dependencies::{DependencyRef, DependencyRole};
pub use identity::{
    AssetId, AssetKindId, AssetRevision, ContentHash, PackId, PackRevision, SourceId,
    SourceRevision,
};
pub use meta::CurationMeta;
pub use nomination::{Nomination, NominationError, NominationId, NominationKind, NominationQueue};
pub use entitlement::{
    Actor, AllowAllEntitlements, AlwaysDenyEntitlements, Entitlement, EntitlementError,
    EntitlementResolver,
};
pub use pack::{
    check_pack_compatibility, detect_cycles, load_pack, load_pack_open, resolve_pack_deps,
    CompatFinding, CompatFindingSeverity, DepResolverCtx, EntitlementHook, PackError, PackManifest,
    PackRef, PackRegistry, ResolvedPack,
};
pub use policy::{
    HookRegistry, JurisdictionPolicyHook, JurisdictionPolicyHookId, LicenseMode,
    PublicationPolicy, ValidityFloor,
};
pub use provenance::{
    CatalogRef, Confidence, EvidenceRef, ExcerptRef, GroundingKind, JurisdictionTag, Lineage,
    Provenance,
};
pub use registry::{ensure_canonical_seed, SourceFilter, SourceRegistry};
pub use scope_trust::{Scope, Trust, ValidationStatus};
pub use source::{SourceLicense, SourceRegistryEntry, SourceStatus, SourceTier};
