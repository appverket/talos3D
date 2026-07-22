//! Capability-profile tool gating for the MCP model API.
//!
//! The full tool router advertises ~240 tool schemas (~140 KB) to every client
//! at initialize. Profiles gate that surface down to what a session actually
//! needs: a named profile selects a subset of tool categories, the server
//! advertises (and accepts calls for) only those tools, and a session can
//! switch profiles at runtime via `set_session_profile`, which emits an MCP
//! `tools/list_changed` notification.
//!
//! Invariants (enforced by tests in `model_api/tests.rs`):
//! - every router tool maps to exactly one [`ToolCategory`]; unclassified
//!   tools are reachable only through the `full` profile and fail the
//!   exhaustiveness test so they get classified,
//! - the MCP session contract (guidance, capability snapshot, guidance cards,
//!   curated-path discovery, agent-skill discovery, profile switching) is
//!   present in **every** profile,
//! - `full` exposes the entire router.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Named tool-gating profile a session can select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CapabilityProfile {
    /// Default. The standard authoring loop: inspection, entity/geometry
    /// editing, materials, recipe/curation/discovery flow, validation,
    /// refinement, and screenshot capture. Excludes UI automation, named
    /// views, clip planes, toolbars, drafting/2D, BIM-exchange, lighting
    /// look-dev, and bulk modeling extras.
    Authoring = 0,
    /// Read-only model/scene inspection plus validation checks and capture.
    Inspection = 1,
    /// Knowledge curation: corpus passages, recipe/assembly-pattern drafts,
    /// definition libraries and workspaces, material specs, rule packs,
    /// procedural sessions, agent-skill drafts.
    Curation = 2,
    /// UI automation: ux_* input simulation, named views, clip planes,
    /// toolbars, command invocation, plus inspection and capture.
    UxAutomation = 3,
    /// The entire tool surface, including tools not yet classified.
    Full = 4,
}

impl CapabilityProfile {
    pub const ALL: [CapabilityProfile; 5] = [
        CapabilityProfile::Authoring,
        CapabilityProfile::Inspection,
        CapabilityProfile::Curation,
        CapabilityProfile::UxAutomation,
        CapabilityProfile::Full,
    ];

    pub const DEFAULT: CapabilityProfile = CapabilityProfile::Authoring;

    pub fn name(self) -> &'static str {
        match self {
            CapabilityProfile::Authoring => "authoring",
            CapabilityProfile::Inspection => "inspection",
            CapabilityProfile::Curation => "curation",
            CapabilityProfile::UxAutomation => "ux-automation",
            CapabilityProfile::Full => "full",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            CapabilityProfile::Authoring => {
                "Standard authoring loop (default): inspection, entity/geometry editing, \
                 materials, recipes/curation/discovery, validation, refinement, screenshot."
            }
            CapabilityProfile::Inspection => {
                "Read-only inspection: model/scene/material/refinement reads, structured \
                 geometric checks, validation, camera and screenshot."
            }
            CapabilityProfile::Curation => {
                "Knowledge curation: corpus acquisition, recipe/assembly-pattern drafts, \
                 definition libraries/workspaces, material specs, rule packs, agent skills."
            }
            CapabilityProfile::UxAutomation => {
                "UI automation: pointer/keyboard simulation, named views, clip planes, \
                 toolbars, command invocation, plus inspection and capture."
            }
            CapabilityProfile::Full => "The entire MCP tool surface.",
        }
    }

    /// Parse a profile name. Accepts `ux_automation` as an alias for
    /// `ux-automation` so agents that snake_case the name still resolve.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "authoring" => Some(CapabilityProfile::Authoring),
            "inspection" => Some(CapabilityProfile::Inspection),
            "curation" => Some(CapabilityProfile::Curation),
            "ux-automation" | "ux_automation" => Some(CapabilityProfile::UxAutomation),
            "full" => Some(CapabilityProfile::Full),
            _ => None,
        }
    }

    pub(super) fn index(self) -> usize {
        self as usize
    }

    fn from_index(index: u8) -> CapabilityProfile {
        Self::ALL
            .into_iter()
            .find(|profile| *profile as u8 == index)
            .unwrap_or(Self::DEFAULT)
    }

    /// Whether tools of `category` are advertised and callable in this profile.
    /// `Full` is handled before category lookup (it includes everything, even
    /// [`ToolCategory::Unclassified`]).
    pub(super) fn includes(self, category: ToolCategory) -> bool {
        use ToolCategory::*;
        match self {
            CapabilityProfile::Full => true,
            CapabilityProfile::Authoring => matches!(
                category,
                SessionContract
                    | Inspection
                    | Validation
                    | Editing
                    | Commands
                    | Materials
                    | Refinement
                    | Discovery
                    | Definitions
                    | Parametric
                    | Capture
                    | ProjectIo
            ),
            CapabilityProfile::Inspection => matches!(
                category,
                SessionContract | Inspection | Validation | Capture
            ),
            CapabilityProfile::Curation => matches!(
                category,
                SessionContract | Inspection | Discovery | CurationExtended | Capture
            ),
            CapabilityProfile::UxAutomation => matches!(
                category,
                SessionContract | Inspection | Capture | Commands | Presentation | UxAutomation
            ),
        }
    }
}

/// Functional category a tool belongs to. Every tool maps to exactly one
/// category; profiles are unions of categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolCategory {
    /// The MCP session contract: instance identity, authoring guidance,
    /// capability snapshot, guidance cards, curated-path discovery,
    /// agent-skill discovery, and profile switching. In every profile.
    SessionContract,
    /// Read-only model/scene/semantic-registry reads used by the standard
    /// agent loop.
    Inspection,
    /// Structured geometric checks and validation runs/explanations.
    Validation,
    /// Entity/geometry creation and editing in the default loop.
    Editing,
    /// Generic registered-command surface (`list_commands`/`invoke_command`),
    /// the escape hatch capability plugins use for operations without a
    /// dedicated MCP tool.
    Commands,
    /// Material definition, assignment, and texture mapping.
    Materials,
    /// Progressive-refinement state, promotion gates, and obligations.
    Refinement,
    /// Recipe discovery/instantiation and the ADR-042 corpus-gap flow.
    Discovery,
    /// Definition/occurrence core lifecycle and hosted placement.
    Definitions,
    /// Parametric types (executable derivation paths).
    Parametric,
    /// Screenshot, camera, and framing — visual verification.
    Capture,
    /// Project save/load and file import.
    ProjectIo,
    /// Extended curation surface: drafts management, libraries, workspaces,
    /// passages, material specs, rule packs, procedural sessions, provenance.
    CurationExtended,
    /// Bulk/extra modeling: arrays, mirrors, align/distribute, booleans,
    /// groups, assemblies, layers editing, texture mapping, occurrence
    /// overrides, document properties, extended reads.
    ModelingExtended,
    /// Lighting and render-settings look-dev.
    Presentation,
    /// 2D drafting: dimensions, guides, sheets, drawing export.
    Drafting2d,
    /// BIM exchange/property/spatial/quantity surface.
    BimExtended,
    /// UI automation: ux_* simulation, named views, clip planes, toolbars.
    UxAutomation,
    /// Not yet classified: reachable only through `full`; the membership
    /// test fails on these so new tools get classified explicitly.
    Unclassified,
}

/// Explicit name → category table. Prefix fallbacks in [`tool_category`]
/// classify future tools in obviously-namespaced families; everything else
/// must be listed here or it is `Unclassified` (full-only + failing test).
pub(super) const TOOL_CATEGORIES: &[(&str, ToolCategory)] = &[
    // --- Session contract (every profile) ---
    ("get_instance_info", ToolCategory::SessionContract),
    ("negotiate_agent_session", ToolCategory::SessionContract),
    ("set_session_profile", ToolCategory::SessionContract),
    ("get_authoring_guidance", ToolCategory::SessionContract),
    ("get_capability_snapshot", ToolCategory::SessionContract),
    ("list_guidance_cards", ToolCategory::SessionContract),
    ("get_guidance_card", ToolCategory::SessionContract),
    ("discover_curated_paths", ToolCategory::SessionContract),
    ("list_agent_skills", ToolCategory::SessionContract),
    ("find_agent_skills", ToolCategory::SessionContract),
    ("get_agent_skill", ToolCategory::SessionContract),
    // --- Inspection (core reads) ---
    ("list_entities", ToolCategory::Inspection),
    ("get_entity", ToolCategory::Inspection),
    ("get_entity_details", ToolCategory::Inspection),
    ("get_entities_details", ToolCategory::Inspection),
    ("model_summary", ToolCategory::Inspection),
    ("outline_tree", ToolCategory::Inspection),
    ("get_assembly", ToolCategory::Inspection),
    ("list_assemblies", ToolCategory::Inspection),
    ("list_assembly_members", ToolCategory::Inspection),
    ("query_relations", ToolCategory::Inspection),
    ("entity_dependencies", ToolCategory::Inspection),
    ("get_selection", ToolCategory::Inspection),
    ("get_subobject_selection", ToolCategory::Inspection),
    ("list_subobjects", ToolCategory::Inspection),
    ("get_editing_context", ToolCategory::Inspection),
    ("list_layers", ToolCategory::Inspection),
    ("list_constraints", ToolCategory::Inspection),
    ("list_element_classes", ToolCategory::Inspection),
    ("list_vocabulary", ToolCategory::Inspection),
    (
        "preview_semantic_assembly_from_selection",
        ToolCategory::Inspection,
    ),
    ("elevation_at", ToolCategory::Inspection),
    // --- Validation ---
    ("get_world_aabb", ToolCategory::Validation),
    ("check_overlaps", ToolCategory::Validation),
    ("check_floating", ToolCategory::Validation),
    ("check_clearance", ToolCategory::Validation),
    ("run_validation", ToolCategory::Validation),
    ("run_validation_v2", ToolCategory::Validation),
    ("explain_finding", ToolCategory::Validation),
    ("explain_finding_v2", ToolCategory::Validation),
    // --- Editing ---
    ("create_entity", ToolCategory::Editing),
    ("create_box", ToolCategory::Editing),
    ("create_assembly", ToolCategory::Editing),
    (
        "create_semantic_assembly_from_selection",
        ToolCategory::Editing,
    ),
    ("delete_entities", ToolCategory::Editing),
    ("transform", ToolCategory::Editing),
    ("set_property", ToolCategory::Editing),
    ("set_entity_property", ToolCategory::Editing),
    ("set_selection", ToolCategory::Editing),
    ("set_subobject_selection", ToolCategory::Editing),
    ("expand_subobject_selection", ToolCategory::Editing),
    ("apply_subobject_edit", ToolCategory::Editing),
    ("split_box_face", ToolCategory::Editing),
    ("prepare_site_surface", ToolCategory::Editing),
    // --- Commands (generic capability-command surface) ---
    ("list_commands", ToolCategory::Commands),
    ("invoke_command", ToolCategory::Commands),
    // --- Materials ---
    ("create_material", ToolCategory::Materials),
    ("update_material", ToolCategory::Materials),
    ("delete_material", ToolCategory::Materials),
    ("apply_material", ToolCategory::Materials),
    ("assign_material", ToolCategory::Materials),
    ("set_material_assignment", ToolCategory::Materials),
    ("remove_material_assignment", ToolCategory::Materials),
    ("get_material", ToolCategory::Materials),
    ("list_materials", ToolCategory::Materials),
    ("get_material_assignment", ToolCategory::Materials),
    // --- Refinement ---
    ("promote_refinement", ToolCategory::Refinement),
    ("demote_refinement", ToolCategory::Refinement),
    ("preview_promotion", ToolCategory::Refinement),
    ("get_refinement_state", ToolCategory::Refinement),
    ("inspect_refinement_branches", ToolCategory::Refinement),
    ("discard_refinement_branch", ToolCategory::Refinement),
    ("get_obligations", ToolCategory::Refinement),
    ("resolve_obligation", ToolCategory::Refinement),
    // --- Discovery (recipes + ADR-042 gap flow) ---
    ("list_recipe_families", ToolCategory::Discovery),
    ("select_recipe", ToolCategory::Discovery),
    ("instantiate_recipe", ToolCategory::Discovery),
    ("list_generation_priors", ToolCategory::Discovery),
    ("list_corpus_gaps", ToolCategory::Discovery),
    ("request_corpus_expansion", ToolCategory::Discovery),
    ("acquire_corpus_passage", ToolCategory::Discovery),
    ("lookup_source_passage", ToolCategory::Discovery),
    ("catalog_query", ToolCategory::Discovery),
    ("list_catalog_providers", ToolCategory::Discovery),
    ("save_recipe_draft", ToolCategory::Discovery),
    ("set_recipe_draft_status", ToolCategory::Discovery),
    ("save_assembly_pattern_draft", ToolCategory::Discovery),
    ("materialize_learned_asset", ToolCategory::Discovery),
    ("save_agent_skill_draft", ToolCategory::Discovery),
    // --- Definitions / occurrences / hosted placement ---
    ("definition.create", ToolCategory::Definitions),
    ("definition.explain", ToolCategory::Definitions),
    ("definition.get", ToolCategory::Definitions),
    ("definition.instantiate", ToolCategory::Definitions),
    ("definition.instantiate_hosted", ToolCategory::Definitions),
    ("definition.list", ToolCategory::Definitions),
    ("definition.update", ToolCategory::Definitions),
    ("definition.validate", ToolCategory::Definitions),
    (
        "definition.validate_host_contract",
        ToolCategory::Definitions,
    ),
    ("occurrence.place", ToolCategory::Definitions),
    ("occurrence.resolve", ToolCategory::Definitions),
    ("occurrence.explain", ToolCategory::Definitions),
    ("occurrence.validate_host_fit", ToolCategory::Definitions),
    ("bim_void.declare_for_definition", ToolCategory::Definitions),
    ("bim_void.plan_placement", ToolCategory::Definitions),
    // --- Capture ---
    ("take_screenshot", ToolCategory::Capture),
    ("frame_entities", ToolCategory::Capture),
    ("frame_model", ToolCategory::Capture),
    ("get_camera", ToolCategory::Capture),
    ("set_camera", ToolCategory::Capture),
    // --- Project I/O ---
    ("save_project", ToolCategory::ProjectIo),
    ("save_model", ToolCategory::ProjectIo),
    ("load_project", ToolCategory::ProjectIo),
    ("import_file", ToolCategory::ProjectIo),
    ("list_importers", ToolCategory::ProjectIo),
    // --- Curation extended ---
    ("get_recipe_draft", ToolCategory::CurationExtended),
    ("list_recipe_drafts", ToolCategory::CurationExtended),
    ("list_persisted_recipes", ToolCategory::CurationExtended),
    (
        "install_recipe_from_session_export",
        ToolCategory::CurationExtended,
    ),
    ("get_assembly_pattern_draft", ToolCategory::CurationExtended),
    (
        "list_assembly_pattern_drafts",
        ToolCategory::CurationExtended,
    ),
    (
        "set_assembly_pattern_draft_status",
        ToolCategory::CurationExtended,
    ),
    ("draft_rule_pack", ToolCategory::CurationExtended),
    ("check_rule_pack_backlinks", ToolCategory::CurationExtended),
    ("create_material_spec", ToolCategory::CurationExtended),
    ("update_material_spec", ToolCategory::CurationExtended),
    ("delete_material_spec", ToolCategory::CurationExtended),
    ("get_material_spec", ToolCategory::CurationExtended),
    ("list_material_specs", ToolCategory::CurationExtended),
    ("save_material_spec", ToolCategory::CurationExtended),
    ("publish_material_spec", ToolCategory::CurationExtended),
    ("get_authoring_provenance", ToolCategory::CurationExtended),
    ("get_claim_grounding", ToolCategory::CurationExtended),
    ("definition.compile", ToolCategory::CurationExtended),
    // --- Modeling extended ---
    ("align_execute", ToolCategory::ModelingExtended),
    ("align_preview", ToolCategory::ModelingExtended),
    ("distribute_execute", ToolCategory::ModelingExtended),
    ("distribute_preview", ToolCategory::ModelingExtended),
    ("boolean_difference", ToolCategory::Editing),
    ("boolean_intersection", ToolCategory::Editing),
    ("boolean_union", ToolCategory::Editing),
    ("enter_group", ToolCategory::ModelingExtended),
    ("exit_group", ToolCategory::ModelingExtended),
    ("list_group_members", ToolCategory::ModelingExtended),
    ("assign_layer", ToolCategory::ModelingExtended),
    ("create_layer", ToolCategory::ModelingExtended),
    ("delete_layer", ToolCategory::ModelingExtended),
    ("rename_layer", ToolCategory::ModelingExtended),
    ("set_layer_visibility", ToolCategory::ModelingExtended),
    ("set_layer_locked", ToolCategory::ModelingExtended),
    ("get_texture_mapping", ToolCategory::ModelingExtended),
    ("update_texture_mapping", ToolCategory::ModelingExtended),
    ("reset_texture_mapping", ToolCategory::ModelingExtended),
    (
        "occurrence.set_material_override",
        ToolCategory::ModelingExtended,
    ),
    (
        "occurrence.clear_material_override",
        ToolCategory::ModelingExtended,
    ),
    (
        "occurrence.update_overrides",
        ToolCategory::ModelingExtended,
    ),
    ("occurrence.make_unique", ToolCategory::ModelingExtended),
    (
        "semantic_shadow.accept_candidate",
        ToolCategory::ModelingExtended,
    ),
    ("get_document_properties", ToolCategory::ModelingExtended),
    ("set_document_properties", ToolCategory::ModelingExtended),
    ("dependency_graph", ToolCategory::ModelingExtended),
    ("list_handles", ToolCategory::ModelingExtended),
    ("cut_fill_analysis", ToolCategory::ModelingExtended),
    // --- Presentation (lighting / render look-dev) ---
    ("create_light", ToolCategory::Presentation),
    ("delete_light", ToolCategory::Presentation),
    ("update_light", ToolCategory::Presentation),
    ("list_lights", ToolCategory::Presentation),
    // Ambient illumination is part of capture verification: it lets an
    // authoring agent prove dark/shaded finishes without granting authored
    // light creation, mutation, or deletion.
    ("get_lighting_scene", ToolCategory::Presentation),
    ("set_ambient_light", ToolCategory::Capture),
    ("restore_default_light_rig", ToolCategory::Presentation),
    // Exposure/tonemapping are part of capture verification: authoring agents
    // must be able to make requested finishes legible without receiving the
    // broader light-authoring surface.
    ("get_render_settings", ToolCategory::Presentation),
    ("get_perf_stats", ToolCategory::Presentation),
    ("set_render_settings", ToolCategory::Capture),
    // --- Drafting / 2D ---
    ("place_dimension_between_handles", ToolCategory::Drafting2d),
    ("place_dimension_line", ToolCategory::Drafting2d),
    ("place_guide_line", ToolCategory::Drafting2d),
    ("place_sheet_dimension", ToolCategory::Drafting2d),
    ("export_drafting_sheet", ToolCategory::Drafting2d),
    ("export_drawing", ToolCategory::Drafting2d),
    ("export.fidelity.describe", ToolCategory::Drafting2d),
    // --- UI automation ---
    ("list_toolbars", ToolCategory::UxAutomation),
    ("set_toolbar_layout", ToolCategory::UxAutomation),
];

/// Category for a tool name: prefix rules for namespaced families first (so
/// future tools in those families classify automatically), then the explicit
/// table, otherwise [`ToolCategory::Unclassified`].
pub(super) fn tool_category(name: &str) -> ToolCategory {
    if let Some((_, category)) = TOOL_CATEGORIES.iter().find(|(tool, _)| *tool == name) {
        return *category;
    }
    for (prefix, category) in [
        ("ux_", ToolCategory::UxAutomation),
        ("view_", ToolCategory::UxAutomation),
        ("clip_plane_", ToolCategory::UxAutomation),
        ("bim_exchange_identity.", ToolCategory::BimExtended),
        ("bim_material.", ToolCategory::BimExtended),
        ("bim_property_set.", ToolCategory::BimExtended),
        ("bim_spatial.", ToolCategory::BimExtended),
        ("quantity.", ToolCategory::BimExtended),
        ("definition.draft.", ToolCategory::CurationExtended),
        ("definition.library.", ToolCategory::CurationExtended),
        ("procedural_session.", ToolCategory::CurationExtended),
        ("parametric.", ToolCategory::Parametric),
        ("array_", ToolCategory::ModelingExtended),
        ("mirror_", ToolCategory::ModelingExtended),
        ("representation.", ToolCategory::ModelingExtended),
    ] {
        if name.starts_with(prefix) {
            return category;
        }
    }
    ToolCategory::Unclassified
}

/// Whether `tool_name` is advertised and callable under `profile`.
pub(super) fn profile_allows(profile: CapabilityProfile, tool_name: &str) -> bool {
    profile == CapabilityProfile::Full || profile.includes(tool_category(tool_name))
}

/// Profiles (other than `full`) that include `tool_name`, for gate errors.
pub(super) fn profiles_containing(tool_name: &str) -> Vec<&'static str> {
    CapabilityProfile::ALL
        .into_iter()
        .filter(|profile| *profile != CapabilityProfile::Full)
        .filter(|profile| profile_allows(*profile, tool_name))
        .map(CapabilityProfile::name)
        .collect()
}

/// Shared, atomically-switchable active profile for one MCP session scope.
///
/// stdio gets one per connection; each HTTP profile endpoint shares one across
/// its (stateless-mode) requests, so `set_session_profile` persists for that
/// endpoint. Talos3D is a single-user local app, so endpoint scope is an
/// acceptable session approximation.
#[derive(Debug, Clone)]
pub(super) struct SessionProfileState(Arc<AtomicU8>);

impl SessionProfileState {
    pub(super) fn new(profile: CapabilityProfile) -> Self {
        Self(Arc::new(AtomicU8::new(profile as u8)))
    }

    pub(super) fn get(&self) -> CapabilityProfile {
        CapabilityProfile::from_index(self.0.load(Ordering::Relaxed))
    }

    /// Set the active profile; returns `true` when it actually changed.
    pub(super) fn set(&self, profile: CapabilityProfile) -> bool {
        self.0.swap(profile as u8, Ordering::Relaxed) != profile as u8
    }
}

/// Default profile for new sessions: `TALOS3D_MCP_PROFILE` when set and valid,
/// otherwise [`CapabilityProfile::DEFAULT`]. Invalid values warn and fall back
/// rather than failing startup.
pub(super) fn default_profile_from_env() -> CapabilityProfile {
    match std::env::var("TALOS3D_MCP_PROFILE") {
        Ok(value) if !value.trim().is_empty() => CapabilityProfile::from_name(&value)
            .unwrap_or_else(|| {
                eprintln!(
                    "invalid TALOS3D_MCP_PROFILE value {value:?}; using '{}' \
                     (known profiles: {})",
                    CapabilityProfile::DEFAULT.name(),
                    CapabilityProfile::ALL
                        .map(CapabilityProfile::name)
                        .join(", ")
                );
                CapabilityProfile::DEFAULT
            }),
        _ => CapabilityProfile::DEFAULT,
    }
}
