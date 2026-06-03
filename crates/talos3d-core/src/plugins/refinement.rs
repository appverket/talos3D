//! Proof Point 70 — Refinement Substrate.
//!
//! This module provides the generic machinery for tracking the maturity of
//! authored entities: refinement state, obligation sets, authoring provenance,
//! and per-claim grounding. No domain-specific nouns live here — the
//! components are intended to be used by any domain capability (architecture,
//! naval, terrain, etc.) as established in ADR-037.
//!
//! Two `SemanticRelation` types are also registered here:
//! - `"refinement_of"` — child → parent; records that an entity was promoted
//!   from a coarser stub.
//! - `"refined_into"` — parent → child; inverse direction, built by the
//!   promote/demote commands to keep the original stub addressable.
//!
//! The `DeclaredStateRequiresResolvedObligations` validator is a plain
//! function wired into `run_validation`. A richer scheduling engine with Bevy
//! change detection and caching will land in PP74.

use std::collections::{BTreeSet, HashMap, VecDeque};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

// `Messages` is re-exported by bevy::prelude in Bevy 0.18.
// `Message` derive macro, `BeginCommandGroup`, etc. are imported via the
// crate-local module paths below.

// ---------------------------------------------------------------------------
// Newtype ID wrappers (placeholder — real types come in PP74/75/77)
// ---------------------------------------------------------------------------

/// Opaque identity for an obligation within an `ObligationSet`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ObligationId(pub String);

/// Semantic role of an obligation (e.g. `"primary_structure"`, `"envelope"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticRole(pub String);

impl SemanticRole {
    /// Returns `true` when this role designates a primary structural element.
    /// The starter validator uses this to escalate findings to `warning`.
    pub fn is_primary_structure(&self) -> bool {
        self.0 == "primary_structure"
    }
}

/// A slash-delimited path into a claim record
/// (e.g. `"riser_max_mm"`, `"envelope/exterior_finish"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ClaimPath(pub String);

/// References a rule in a rule pack.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RuleId(pub String);

/// References a row in a structured catalog.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CatalogRowId(pub String);

/// A tag on an LLM heuristic for traceability.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct HeuristicTag(pub String);

/// Identifies the agent that set a claim.  Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentId(pub String);

/// References a source document such as an imported file or a standard.
/// Stubbed as `String` for PP70.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SourceRef(pub String);

/// References a passage (section, article, paragraph) in a corpus document.
/// Stored as a slash-delimited path (e.g. `"BBR/9:2/table-1"`).
/// Stubbed as `String` for PP70; the corpus infrastructure lands in PP78.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PassageRef(pub String);

/// Identifies a parametric recipe family.  Stubbed as `String` for PP70.
/// Real recipe families arrive in PP71.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RecipeId(pub String);

// ---------------------------------------------------------------------------
// RefinementState
// ---------------------------------------------------------------------------

/// The declared maturity level of an authored entity.
///
/// Variants are ordered from least to most detailed; `PartialOrd` / `Ord`
/// implementations let the validator compare states numerically.
///
/// The level is a *claim about how resolved the model is*, and it is
/// **monotonic in resolved structure**: a higher level must contain strictly
/// more resolved content than a lower level of the same design. It follows
/// that two artifacts which are structurally identical cannot legitimately sit
/// at different levels — see [`REFINEMENT_LEVEL_SEMANTICS`] for the full,
/// domain-neutral contract and the rule for "identical X, one per refinement
/// level" requests.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum RefinementState {
    /// Coarse design intent — no detail obligations in force.
    #[default]
    Conceptual,
    /// Spatial massing defined; primary-structure obligations begin firing.
    Schematic,
    /// Buildable geometry — all obligations must be resolved.
    Constructible,
    /// Full assembly detail present.
    Detailed,
    /// Ready for fabrication or manufacturing output.
    FabricationReady,
}

impl RefinementState {
    /// Human-readable label used in MCP responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conceptual => "Conceptual",
            Self::Schematic => "Schematic",
            Self::Constructible => "Constructible",
            Self::Detailed => "Detailed",
            Self::FabricationReady => "FabricationReady",
        }
    }

    /// Parse from the string produced by `as_str`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Conceptual" => Some(Self::Conceptual),
            "Schematic" => Some(Self::Schematic),
            "Constructible" => Some(Self::Constructible),
            "Detailed" => Some(Self::Detailed),
            "FabricationReady" => Some(Self::FabricationReady),
            _ => None,
        }
    }
}

/// Domain-neutral, agent-facing definition of what the refinement levels mean.
///
/// This is platform knowledge: it holds for any domain (architecture, naval,
/// mechanical, …), independent of walls, hulls, or parts. Domain packs compose
/// it into their own `get_authoring_guidance` `prompt_text` ahead of any
/// domain-specific worked examples, so every authoring agent — internal
/// assistant or external MCP client — reads the same contract before it edits.
pub const REFINEMENT_LEVEL_SEMANTICS: &str = r#"# Refinement levels — what they mean (read this first)

A `RefinementState` is a **claim about how resolved a model is**, not a free
label you may attach at will. The levels are ordered

`Conceptual < Schematic < Constructible < Detailed < FabricationReady`

and the ordering is **monotonic in resolved structure**: each higher level
must contain *strictly more resolved content* than a lower level of the same
design. A "promotion" that adds no resolved structure is not a promotion — it
is only relabelling, and is invalid.

Each level is defined by *which decisions are resolved*, and therefore *which
downstream consumer can rely on the model*. A promotion resolves the next band
of decisions; it is not a label you grant ahead of the content. (Domain packs
specialise the *content* of each band, not the *intent*.)

- **Conceptual** — intent only. Coarse aggregate/massing elements; major
  parameters captured as data; major decisions may remain explicitly open.
  *Consumer:* feasibility, footprint/envelope fit. Little to validate yet.
- **Schematic** — intent + coordination. The same coarse roots persist; the
  top-level system/facet decisions are resolved and coordination data
  (spacing, declared layer/build-up stacks, performance targets, bearing or
  load strategy) is tightened — **declared as intent, not yet exploded into
  members**. *Consumer:* early estimate / performance model.
- **Constructible** — buildable. The coarse root persists as an identity
  anchor and is linked (`refined_into` / `refinement_of`) to an explicit
  realisation: reusable definitions, their occurrences, derived variants,
  justified singletons, declared build-ups realised as real sub-elements,
  hosted components occupying declared voids, and the structural/connective
  relations that make it actually buildable. **Every obligation in force at
  this level is resolved.** *Consumer:* pricing, permit, quantity takeoff.
- **Detailed** — resolved interfaces. The junctions *between* the Constructible
  parts are worked out (how members meet, bear, and frame around hosted
  voids), added where downstream use justifies it. *Consumer:* coordination /
  clash / permit set.
- **FabricationReady** — every part is an orderable, fabricable instance:
  catalog/BOM-level definiteness, connection hardware scheduled, material
  grades and cut/quantity data resolved. *Consumer:* the shop or manufacturer
  that builds it without an RFI.

## The identical-vs-level contradiction

Because a level is *defined by resolved content*, two models that are
structurally identical **cannot** legitimately sit at different refinement
levels. That is a contradiction, and it must be caught — never silently
resolved by making the models identical and varying only a label.

When a request implies identical artifacts at different levels — e.g.
"N identical houses, one per refinement level", "the same part modelled at
each level of detail" — interpret it as a **controlled comparison**:

- "identical" denotes identical *design intent / brief / controlled inputs*
  (size, program, site, placement) — the things held constant.
- the **refinement level is the independent variable that must actually be
  applied**, so resolved detail visibly increases with level.

Correct response (a decision rule, not a fixed action):

1. Recognise the contradiction-under-literal-reading as a trigger.
2. Resolve it by purpose: the comparison is only meaningful if the levels
   differ, so build the *same design realised at increasing levels of
   resolution*.
3. State that interpretation in one line so the user can redirect, then
   proceed.
4. Ask only if the intent is genuinely indeterminate (e.g. the request is
   equally consistent with "just demonstrate the label").

Trip-wire: if your plan would produce equal structure across levels, or a
promotion adds no structure, stop — you have misread the task. Two wrong
answers to avoid: (a) identical output across levels (makes the refinement
axis vacuous); (b) a higher-level label on lower-level content (an
unsubstantiated claim).
"#;

/// Platform-level anti-bluff authoring gate (ADR-042). Domain-neutral, composed
/// into every served `AuthoringGuidance.prompt_text` ahead of domain content so
/// that an agent — which receives ALL its guidance over MCP and never reads the
/// repo or source — is told, at authoring time, to use curated generators and
/// never substitute hand-rolled primitives for missing expertise. Bump the
/// owning guidance `version` when this text changes.
pub const ANTI_BLUFF_AUTHORING_GATE: &str = r#"# Before you author geometry — the anti-bluff gate (read this second)

Domain expertise (recipes, assembly patterns, parametric generators, priors) is
curated and consumed through tools — it is not something you reconstruct by hand.
Before you author or refine **any** element class at a target refinement state:

1. **Discover the curated path first.** Call `select_recipe(element_class,
   {target_state})`; also consult `list_recipe_families`, `parametric.list_types`,
   and `list_generation_priors`. If a curated path exists, **use it** (a recipe,
   an assembly pattern, or `definition` + `occurrence`).
2. **No curated path ⇒ a knowledge gap, not a licence to improvise.** An empty
   `select_recipe` result is a `CorpusGap`. Do **not** approximate the assembly
   from primitives. Instead: call `request_corpus_expansion`, then either
   acquire/curate a draft (`save_recipe_draft` / `save_assembly_pattern_draft`)
   and validate it, or — if you cannot ground it this session — **stop and tell
   the user in plain language**. Unknowns are reported, never bluffed.

**What counts as a bluff (prohibited):** ungrounded geometry authored *outside* a
recipe, assembly pattern, or definition/occurrence, for a class that has — or
should have — a curated path. Approximating a sloped, framed, or layered assembly
(roofs, trusses, stud walls, envelope build-ups) with axis-aligned boxes or
ad-hoc meshes is never a valid substitute. Raw primitives are legitimate only as
the committed body of a curated recipe/pattern, or as `Conceptual` massing.

**A registered type that emits no geometry is also a gap.** Some generators are
derivation-only (they compute quantities but do not place geometry). Verify that
the path you took actually produced geometry; if it did not, that is a gap to
record — not a reason to fall back to primitives.

**If interim stand-in geometry is truly unavoidable, never do it silently.** In
the same step you must: (a) `request_corpus_expansion` for the missing asset,
(b) record an `LLMHeuristic` claim grounding with rationale on the entity, and
(c) leave it blocked from promotion past `Conceptual`. There must never be
hand-rolled geometry without a recorded gap grounding it.

**Verify before you declare done.** Render the result (`take_screenshot`) and
actually inspect the geometry. A green entity count or a set refinement label is
not evidence the model is correct.
"#;

/// Platform-level composition contract. Domain-neutral, composed into every
/// served `AuthoringGuidance.prompt_text` after the refinement-level semantics
/// and anti-bluff gate. It tells an MCP-only agent *how* resolved structure is
/// actually added as a model refines — through the platform's reuse/derive,
/// hosting, and obligation mechanisms — so detail is composed, not improvised.
/// These mechanisms (definitions, occurrences, hosting/voids, obligations) are
/// platform capabilities shared across every domain; the domain pack supplies
/// only the concrete content. Bump the owning guidance `version` when this text
/// changes.
pub const COMPONENT_COMPOSITION_CONTRACT: &str = r#"# How resolved detail is added — composition, hosting, obligations (read this third)

Refinement is "more committed structure", not "more geometry". Three platform
mechanisms carry that structure; use them instead of ad-hoc geometry.

## Reuse vs. singletons — by level

Detail is added as a phase change from *singletons that describe intent* to
*families with instances*:

- At `Conceptual` / `Schematic`, prefer aggregate/singleton entities that carry
  intent; explicit repeated members are usually premature.
- At `Constructible` and above, any member that recurs by *role and topology*
  (two or more of the same kind) MUST be one reusable `Definition` placed as N
  `Occurrence`s (placement + a bounded override allowlist only), never inlined
  copies. Genuinely unique members stay singletons.
- When a new member is a *modification* of an existing family, derive it
  (`base_definition_id`) rather than authoring topology from scratch.
- At `FabricationReady`, every fabricable part should belong to a `Definition`
  that carries one shop/BOM identity, with its `Occurrence`s as the placed
  instances. This is what makes quantities and fabrication output tractable:
  you cost or fabricate the `Definition` once and place it many times.

## Hosting contracts — composition instead of ad-hoc cuts

When one component is embedded in another (a component set into an opening or
void in a host), model it as a *hosting contract*, never a free-floating overlap
or an unmanaged boolean subtraction:

- The host `Definition` declares where and how it accepts hosted components (the
  void it offers and the bounds it accepts).
- The hosted component declares the void it needs; placement is *validated*
  against the host contract (`occurrence.validate_host_fit`,
  `definition.validate_host_contract`) and occupies a declared void
  (`bim_void.declare_for_definition`, `bim_void.plan_placement`) cut in the host.
- Place hosted components with `definition.instantiate_hosted` / `occurrence.place`
  rather than subtracting primitive geometry by hand.
- The host↔hosted relation is first-class and *survives refinement*: at higher
  levels the declared void deterministically drives the surrounding edge/framing
  detail.

## Obligations — the machine-checkable level contract

Each element class declares, per level, the obligations that must be resolved
before an entity can legitimately claim that level:

- `list_element_classes` returns the per-state obligation ladder for each class
  (what must be resolved, by which level, for which role).
- `get_obligations(element_id)` returns the live status on a concrete entity.
- A promotion is valid only once every obligation in force at the target level
  is resolved — each one `SatisfiedBy` a real sub-element, or explicitly
  `Deferred` / `Waived` with a reason.
- Before promoting, call `preview_promotion` to see what is still missing, and
  `run_validation` afterwards to confirm nothing in force is left `Unresolved`.

This contract is machine-enforced, not advisory. `promote_refinement` will
**refuse** to advance an entity to a level while any in-force obligation is
still `Unresolved`, returning an error that lists the blocking obligation ids.
That is not a failure — it is the platform handing you the punch list. The loop
to clear it:

1. Attempt the promotion. If it is blocked, the entity now carries the
   materialised obligation set (a blocked promote still installs it), so
   `get_obligations(element_id)` shows every id and its status.
2. For each blocked obligation, call `resolve_obligation`:
   - `{ element_id, obligation_id, resolution: { satisfied_by: { element_id: <child> } } }`
     when a real sub-element fulfils it (build/host that sub-element first);
   - `{ ..., resolution: { deferred: { reason: "<why>" } } }` to intentionally
     defer it with a recorded rationale;
   - `{ ..., resolution: { waived: { rationale: "<why>" } } }` when it is
     explicitly out of scope.
   Each call is undoable and returns the updated obligation set.
3. Re-attempt the promotion. Recorded resolutions are preserved across the
   re-promote (they are not clobbered back to `Unresolved`), so once every
   in-force obligation is `SatisfiedBy` / `Deferred` / `Waived` the gate passes.

Important: **building the sub-element does not satisfy the obligation by
itself.** Instantiating a recipe, placing an occurrence, or spawning a child
that *could* fulfil an obligation does not auto-link it — the obligation stays
`Unresolved` until you explicitly record the link with `resolve_obligation`
`satisfied_by` that child's `element_id`. (A recipe only auto-satisfies the
specific obligations it declares satisfaction-links for; do not assume it covers
the class ladder.) So the normal order is: build the sub-element → read its
`element_id` → `resolve_obligation { satisfied_by }` → re-promote.

Prefer `SatisfiedBy` a genuine sub-element over `Deferred`/`Waived`: deferring or
waiving records an honest gap, but it does not add the resolved content the level
claims. Do not reach for `Waived` just to silence the gate. And never try to set
a level whose obligations you have not resolved — that is the same
unsubstantiated-claim error as relabelling without adding structure.
"#;

/// Platform-level orientation for the Semantic Procedural Session (ADR-051).
/// Domain-neutral, composed into every served `AuthoringGuidance.prompt_text`
/// after the composition contract so an MCP-only agent learns *when* to reach
/// for `procedural_session.*` instead of streaming individual Model-API
/// mutations. The five tools themselves are domain-neutral capabilities of
/// `talos3d-core`; the orientation lives here next to the other domain-neutral
/// preambles. Bump the owning guidance `version` when this text changes.
pub const PROCEDURAL_SESSION_ORIENTATION: &str = r#"# Composing multi-step authoring — the procedural session (read this fourth)

Most edits are a single Model-API call (`definition.create`, `occurrence.place`,
`set_property`, `instantiate_recipe`, …). When the work is a **sequence** of
calls that share parameters, depend on each other's outputs, or must validate
as a whole before any of it commits, do not stream those calls into the live
model. Open a **Semantic Procedural Session** (ADR-051) and assemble the
sequence there first.

**Reach for `procedural_session.*` when any of these are true:**

1. **Repeated or datum-derived placement.** Rows of openings, fixture grids,
   stud spacing, truss stations — anything where step N's position depends on
   step N-1's output. The session captures bindings between steps so you do
   not recompute coordinates by hand.
2. **Multi-step authoring that must validate as a whole.** A wall plus its
   openings plus their hosted hardware should commit together or not at all.
   Use `procedural_session.eval` in `dry_run` to project obligations and
   findings before any command queues.
3. **You are about to author a reusable recipe — or fill a `CorpusGap`.**
   Build the sequence in a session, validate it, `procedural_session.export`
   it, then **`install_recipe_from_session_export`** it. Export alone only
   freezes the script; install is what registers it as a durable, executable
   recipe that `instantiate_recipe` can replay and that future sessions
   rediscover. This is the mechanism by which the corpus *learns new
   construction knowledge as data and improves over time without any code
   change* — when `select_recipe`/`discover_curated_paths` return nothing for
   an element class, authoring + installing a session recipe is how you close
   that gap permanently rather than hand-rolling one-off geometry. Do not
   hand-write recipe JSON.
4. **You want a safe scratchpad against a declared scope.** A session
   declares its refinement target, stage transition, MutationScope, and
   allowed-tool set up front; the platform type-checks each step against the
   registered capability/command descriptors and enforces the scope.

**Do not** use a session for a single isolated mutation, for read-only queries
(`get_*`, `list_*`, `take_screenshot`), or as a way to bypass the anti-bluff
gate above — a session that hand-rolls primitives for an element class with a
curated path is still a bluff.

**The tools, in order of use:**

- `procedural_session.create` — open a session against a declared spec; get
  `session_id` and the session-scoped guidance overlay.
- `procedural_session.eval` — append/preview one step. Modes: `bind_only`
  (type-check + append), `dry_run` (project commands/obligations/findings;
  do not append), `dry_run_and_bind`. Iterate here, cheaply.
- `procedural_session.snapshot` — inspect the accumulated `AuthoringScript`,
  live bindings, outstanding obligations, and accrued findings.
- `procedural_session.commit` — flush the script through the command queue
  (ADR-002 / ADR-011). Policies: `require_clean`, `accept_with_waivers`,
  `accept_partial`.
- `procedural_session.export` — freeze the script as a curated artifact
  (`recipe.authoring_script.v1`); the session stays re-exportable until close.
  Export does **not** by itself make the recipe callable.
- `install_recipe_from_session_export` — register the exported artifact as a
  durable, **executable** recipe. After install, `instantiate_recipe` and
  `promote_refinement` can replay it by `family_id`, and `list_persisted_recipes`
  / `discover_curated_paths` / `select_recipe` surface it to every future
  session. Use `scope: "Project"` (default) to persist it under
  `~/.talos3d/knowledge/recipes/` so it survives restarts. **This install step
  is what turns a one-off authoring episode into reusable corpus knowledge — do
  not stop at export.**

**Closing a `CorpusGap` as data.** When the curated path for an element class is
empty, the durable fix is: learn the construction (cite the source; persist it
with `acquire_corpus_passage` so the grounding survives), build it once in a
session, `export`, then `install_recipe_from_session_export`. The next agent that
asks for that element class finds an executable recipe instead of the same gap.
That is how the corpus improves over time without a code change. (For a single
immediate parametric component you may instead author an inline
`parametric.create` `representation` — but that geometry is ephemeral and born
`Conceptual`; it does not enter the reusable corpus, so prefer the install path
whenever the knowledge should persist.)

**Verify before declaring done.** A clean commit report is not evidence the
geometry is right — render the result (`take_screenshot`) and look.
"#;

// ---------------------------------------------------------------------------
// RefinementStateComponent
// ---------------------------------------------------------------------------

/// ECS component that holds the declared refinement level for an entity.
#[derive(Component, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RefinementStateComponent {
    pub state: RefinementState,
}

// ---------------------------------------------------------------------------
// ObligationSet
// ---------------------------------------------------------------------------

/// The status of a single obligation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum ObligationStatus {
    /// Not yet resolved.
    Unresolved,
    /// Satisfied by the entity with the given `ElementId` (stored as `u64`
    /// to keep this module free of direct `ElementId` imports).
    SatisfiedBy(u64),
    /// Intentionally deferred with a human-readable reason.
    Deferred(String),
    /// Waived with a human-readable rationale.
    Waived(String),
}

/// A single obligation entry in an `ObligationSet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct Obligation {
    /// Unique identifier within the set.
    pub id: ObligationId,
    /// Semantic role this obligation covers.
    pub role: SemanticRole,
    /// The lowest `RefinementState` at which this obligation must be satisfied.
    pub required_by_state: RefinementState,
    /// Current status.
    pub status: ObligationStatus,
}

/// ECS component holding the declared obligation list for an entity.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ObligationSet {
    pub entries: Vec<Obligation>,
}

// ---------------------------------------------------------------------------
// AuthoringProvenance
// ---------------------------------------------------------------------------

/// Records how an entity was created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum AuthoringMode {
    /// Entity was generated by a registered recipe family.
    ViaRecipe(RecipeId),
    /// Entity was authored by hand without a recipe.
    Freeform,
    /// Entity was imported from an external file.
    Imported(SourceRef),
    /// Entity was promoted from a coarser entity (given as `u64` element-id).
    Refined(u64),
}

/// ECS component that records *why* an entity was placed.
///
/// Orthogonal to [`ClaimGrounding`]: provenance answers "why does this entity
/// exist?"; grounding answers "why does this property have this value?".
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthoringProvenance {
    pub mode: AuthoringMode,
    pub rationale: Option<String>,
}

impl Default for AuthoringProvenance {
    fn default() -> Self {
        Self {
            mode: AuthoringMode::Freeform,
            rationale: None,
        }
    }
}

// ---------------------------------------------------------------------------
// SemanticIntent
// ---------------------------------------------------------------------------

/// Generic non-geometric intent attached to an authored entity.
///
/// Domain crates define the vocabulary and parameter schema via
/// `ElementClassDescriptor`; this component stores the instance-level values
/// and unresolved choices without committing the entity to a constructible
/// recipe or domain-specific component.
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticIntent {
    pub parameters: serde_json::Value,
    pub unresolved_decisions: Vec<UnresolvedDecisionRecord>,
    pub source_refs: Vec<SemanticSourceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct UnresolvedDecisionRecord {
    pub id: String,
    pub question: String,
    pub reason: String,
    pub grounding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SemanticSourceRef {
    pub reference: String,
    pub claim: String,
    pub grounding: String,
}

// ---------------------------------------------------------------------------
// ClaimGrounding
// ---------------------------------------------------------------------------

/// Describes the provenance of a single typed property value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum Grounding {
    /// Grounded by an explicit rule in a rule pack.
    ExplicitRule(RuleId),
    /// Grounded by a row in a structured catalog.
    CatalogRow(CatalogRowId),
    /// Copied verbatim from an imported file.
    Imported(SourceRef),
    /// Inherited from a parent entity during refinement (element-id as `u64`).
    Refined(u64),
    /// Generated locally by a recipe's `generate` function (PP74 / F6).
    ///
    /// Use this when the claim is computed by the recipe itself (e.g. a slab
    /// computing its own `top_datum_mm` from `floor_datum_mm`). Stored as the
    /// recipe family id string to avoid a circular dependency between
    /// `refinement` and `capability_registry`.
    ///
    /// The `RecipeFamilyId` newtype in `capability_registry` converts to/from
    /// this via `RecipeFamilyId(String)`.
    GeneratedByRecipe(String),
    /// Derived from LLM implicit knowledge; requires rationale + tag.
    LLMHeuristic {
        rationale: String,
        heuristic_tag: HeuristicTag,
    },
}

/// A single grounded claim record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ClaimRecord {
    pub grounding: Grounding,
    /// Wall-clock timestamp (Unix seconds) at which the claim was set.
    pub set_at: i64,
    /// Agent that set the claim, if known.
    pub set_by: Option<AgentId>,
}

/// ECS component holding per-claim grounding for an entity.
///
/// Keys are [`ClaimPath`] values (e.g. `"riser_max_mm"`,
/// `"envelope/exterior_finish"`).
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClaimGrounding {
    pub claims: HashMap<ClaimPath, ClaimRecord>,
}

// ---------------------------------------------------------------------------
// Validation findings
// ---------------------------------------------------------------------------

/// Severity level for a validator finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum FindingSeverity {
    Advice,
    Warning,
    Error,
}

impl FindingSeverity {
    /// Human-readable label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advice => "advice",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// A single finding emitted by a validator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ValidationFinding {
    /// Unique finding identifier (format: `"{validator_id}:{entity_id}:{obligation_id}"`).
    pub finding_id: String,
    /// Element-id of the entity this finding applies to.
    pub entity_element_id: u64,
    /// Validator that produced this finding.
    pub validator: String,
    pub severity: FindingSeverity,
    /// Human-readable message.
    pub message: String,
    /// Rationale for why this rule exists.
    pub rationale: String,
    /// The obligation id that triggered this finding, if applicable.
    pub obligation_id: Option<ObligationId>,
}

// ---------------------------------------------------------------------------
// Validator: DeclaredStateRequiresResolvedObligations
// ---------------------------------------------------------------------------

/// The PP70 starter completeness validator.
///
/// Rules (from ADR-038 §11 and the PP70 spec):
///
/// * `Conceptual` — never emits any finding.
/// * `Schematic` — for each `Unresolved` obligation whose
///   `required_by_state ≤ Schematic`:
///   * `SemanticRole::PrimaryStructure` → `warning`
///   * otherwise → `advice`
/// * `Constructible | Detailed | FabricationReady` — any `Unresolved`
///   obligation whose `required_by_state ≤ entity.state` → `error`.
///   (`SatisfiedBy | Deferred | Waived` all pass.)
pub fn validate_declared_state_obligations(
    entity_element_id: u64,
    state: RefinementState,
    obligations: &ObligationSet,
) -> Vec<ValidationFinding> {
    if state == RefinementState::Conceptual {
        return Vec::new();
    }

    let mut findings = Vec::new();

    for obligation in &obligations.entries {
        // Only obligations whose trigger threshold has been reached.
        if obligation.required_by_state > state {
            continue;
        }

        if !matches!(obligation.status, ObligationStatus::Unresolved) {
            continue;
        }

        let (severity, rationale) = match state {
            RefinementState::Conceptual => unreachable!("handled above"),
            RefinementState::Schematic => {
                if obligation.role.is_primary_structure() {
                    (
                        FindingSeverity::Warning,
                        "Primary-structure obligations must be resolved by the Schematic state \
                         to ensure structural intent is captured before further detail work begins.",
                    )
                } else {
                    (
                        FindingSeverity::Advice,
                        "This obligation is expected by the Schematic state. Consider resolving \
                         it before advancing further.",
                    )
                }
            }
            RefinementState::Constructible
            | RefinementState::Detailed
            | RefinementState::FabricationReady => (
                FindingSeverity::Error,
                "All obligations must be resolved at or above the Constructible state. \
                 Use SatisfiedBy, Deferred(reason), or Waived(rationale) to close this obligation.",
            ),
        };

        findings.push(ValidationFinding {
            finding_id: format!(
                "declared_state_obligations:{}:{}",
                entity_element_id, obligation.id.0
            ),
            entity_element_id,
            validator: "DeclaredStateRequiresResolvedObligations".to_string(),
            severity,
            message: format!(
                "Obligation '{}' (role: '{}', required by: {}) is Unresolved",
                obligation.id.0,
                obligation.role.0,
                obligation.required_by_state.as_str()
            ),
            rationale: rationale.to_string(),
            obligation_id: Some(obligation.id.clone()),
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Promote / Demote operations (world-mutating, called from model_api handlers)
// ---------------------------------------------------------------------------

use crate::plugins::{
    commands::{ApplyEntityChangesCommand, BeginCommandGroup, EndCommandGroup},
    identity::ElementId,
    modeling::assembly::{RelationSnapshot, SemanticAssembly, SemanticRelation},
};

// ---------------------------------------------------------------------------
// Assembly member-composition obligations (ADR-042 anti-bluff gate)
// ---------------------------------------------------------------------------

/// The outcome of evaluating one [`AssemblyMemberObligationTemplate`] against
/// an assembly's live membership at a promotion target state.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedAssemblyObligation {
    pub id: ObligationId,
    pub role: SemanticRole,
    pub member_role: String,
    pub required_by_state: RefinementState,
    pub status: ObligationStatus,
    /// Human-readable explanation. Empty when satisfied.
    pub detail: String,
}

impl EvaluatedAssemblyObligation {
    pub fn is_unresolved(&self) -> bool {
        matches!(self.status, ObligationStatus::Unresolved)
    }
}

/// Read each member's current refinement state in a single query pass.
///
/// Works on `&World` (read-only) so it can be shared by both the preview and
/// commit paths. Members with no `RefinementStateComponent` default to
/// `Conceptual`; members whose `ElementId` is not present in the world are
/// omitted (treated as missing).
fn member_refinement_states(
    world: &World,
    members: &[crate::plugins::modeling::assembly::AssemblyMemberRef],
) -> HashMap<u64, RefinementState> {
    let wanted: BTreeSet<u64> = members.iter().map(|m| m.target.0).collect();
    let mut states = HashMap::new();
    if wanted.is_empty() {
        return states;
    }
    let Some(mut q) = world.try_query::<(&ElementId, Option<&RefinementStateComponent>)>() else {
        return states;
    };
    for (id, state) in q.iter(world) {
        if wanted.contains(&id.0) {
            states.insert(id.0, state.map(|c| c.state).unwrap_or_default());
        }
    }
    states
}

/// Evaluate an assembly type's member-composition obligations against the
/// assembly's live membership for a given promotion `target_state`.
///
/// An obligation is *in force* when `required_by_state <= target_state`, so
/// promoting straight from `Conceptual` to `Detailed` still pulls in every
/// obligation that any skipped intermediate level would have required. An
/// in-force obligation is `SatisfiedBy` the first qualifying member when at
/// least `min_count` members match its `member_role` (and, when
/// `member_tracks_target_state`, have themselves reached `target_state`);
/// otherwise it is `Unresolved` with a `detail` explaining the shortfall.
///
/// Obligations whose `required_by_state` exceeds `target_state` are skipped
/// entirely (not returned), mirroring the single-element obligation ladder.
pub fn evaluate_assembly_member_obligations(
    world: &World,
    assembly: &SemanticAssembly,
    member_obligations: &[crate::capability_registry::AssemblyMemberObligationTemplate],
    target_state: RefinementState,
) -> Vec<EvaluatedAssemblyObligation> {
    let member_states = member_refinement_states(world, &assembly.members);

    let mut results = Vec::new();
    for template in member_obligations {
        if template.required_by_state > target_state {
            continue;
        }

        let required_member_state = template
            .member_tracks_target_state
            .then_some(target_state);

        // Members whose role matches this obligation.
        let role_matches: Vec<u64> = assembly
            .members
            .iter()
            .filter(|m| m.role == template.member_role)
            .map(|m| m.target.0)
            .collect();

        let present_count = role_matches.len();

        // Of those, the ones that also meet the required resolved state.
        let satisfying: Vec<u64> = role_matches
            .iter()
            .copied()
            .filter(|eid| {
                let state = member_states.get(eid).copied();
                match (state, required_member_state) {
                    // Member not present in the world at all.
                    (None, _) => false,
                    // Presence is enough.
                    (Some(_), None) => true,
                    // Member must itself have reached the target state.
                    (Some(s), Some(req)) => s >= req,
                }
            })
            .collect();

        let (status, detail) = if satisfying.len() >= template.min_count {
            (ObligationStatus::SatisfiedBy(satisfying[0]), String::new())
        } else if let Some(req) = required_member_state {
            (
                ObligationStatus::Unresolved,
                format!(
                    "needs {} member(s) in role '{}' resolved to at least {}; found {} present, {} at that state",
                    template.min_count,
                    template.member_role,
                    req.as_str(),
                    present_count,
                    satisfying.len(),
                ),
            )
        } else {
            (
                ObligationStatus::Unresolved,
                format!(
                    "needs {} member(s) in role '{}'; found {}",
                    template.min_count, template.member_role, present_count,
                ),
            )
        };

        results.push(EvaluatedAssemblyObligation {
            id: template.id.clone(),
            role: template.role.clone(),
            member_role: template.member_role.clone(),
            required_by_state: template.required_by_state,
            status,
            detail,
        });
    }

    results
}

/// Resolve an assembly type's member obligations from the registry, evaluate
/// them, and return any that are `Unresolved`. Returns an empty vector when the
/// entity is not a `SemanticAssembly`, the assembly type declares no member
/// obligations, or all in-force obligations are satisfied.
///
/// This is the shared core behind both the preview (advisory findings) and the
/// commit gate (hard rejection).
pub fn unmet_assembly_member_obligations(
    world: &World,
    entity: Entity,
    target_state: RefinementState,
) -> Vec<EvaluatedAssemblyObligation> {
    let Some(assembly) = world.get::<SemanticAssembly>(entity).cloned() else {
        return Vec::new();
    };
    let templates = {
        let Some(registry) =
            world.get_resource::<crate::capability_registry::CapabilityRegistry>()
        else {
            return Vec::new();
        };
        match registry.assembly_type_descriptor(&assembly.assembly_type) {
            Some(desc) if !desc.member_obligations.is_empty() => desc.member_obligations.clone(),
            _ => return Vec::new(),
        }
    };

    evaluate_assembly_member_obligations(world, &assembly, &templates, target_state)
        .into_iter()
        .filter(EvaluatedAssemblyObligation::is_unresolved)
        .collect()
}

/// A promote-refinement command payload.
///
/// In PP70, `recipe_id` and `overrides` are accepted but not acted upon
/// (no recipe families exist yet). The command updates `RefinementStateComponent`
/// and records authoring provenance.
#[derive(Debug, Clone)]
pub struct PromoteRefinementRequest {
    pub entity_element_id: u64,
    pub target_state: RefinementState,
    pub recipe_id: Option<RecipeId>,
    pub overrides: HashMap<ClaimPath, serde_json::Value>,
}

/// A demote-refinement command payload.
#[derive(Debug, Clone)]
pub struct DemoteRefinementRequest {
    pub entity_element_id: u64,
    pub target_state: RefinementState,
}

/// Explicit target scope for a refinement promotion preview or commit.
///
/// The subtree is rooted at a stable coarse handle and contains the root plus
/// active descendants reachable through `refined_into` relations. Parked branch
/// exclusion lands in PP-PREF-2; until then every reachable child is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RefinementSubtree {
    pub root_element_id: u64,
    pub active_element_ids: Vec<u64>,
}

impl RefinementSubtree {
    pub fn singleton(root_element_id: u64) -> Self {
        Self {
            root_element_id,
            active_element_ids: vec![root_element_id],
        }
    }

    pub fn contains(&self, element_id: u64) -> bool {
        self.active_element_ids.contains(&element_id)
    }
}

/// Lifecycle state for a generated refinement branch.
///
/// Parked branches remain in the model for inspection/reactivation, but are
/// excluded from active refinement traversals, validation, schedules, and
/// dependency graph reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum RefinementBranchStatus {
    #[default]
    Active,
    Parked,
}

impl RefinementBranchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Parked => "parked",
        }
    }
}

/// Metadata attached to both `refinement_of` and `refined_into` relation
/// entities so demotion can park generated detail without deleting authored
/// overrides.
#[derive(Component, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinementBranch {
    pub root_element_id: u64,
    pub parent_element_id: u64,
    pub child_element_id: u64,
    pub target_state: RefinementState,
    pub recipe_id: Option<RecipeId>,
    pub status: RefinementBranchStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct RefinementBranchInfo {
    pub root_element_id: u64,
    pub parent_element_id: u64,
    pub child_element_id: u64,
    pub target_state: RefinementState,
    pub recipe_id: Option<RecipeId>,
    pub status: RefinementBranchStatus,
}

/// Apply a promotion to the world, queuing history commands for undo/redo.
///
/// Returns the new state on success.
///
/// If the entity has an `ElementClassAssignment` component, the effective
/// merged `RefinementContract` (class-minimum + recipe specialisations) is
/// computed from the registered descriptors and installed as an `ObligationSet`.
/// If a `recipe_id` is provided, the recipe's `generate` function is invoked
/// to spawn child entities and link them via `create_refinement_relation_pair`.
pub fn apply_promote_refinement(
    world: &mut World,
    request: PromoteRefinementRequest,
) -> Result<RefinementState, String> {
    use crate::capability_registry::{
        effective_obligations, effective_promotion_critical_paths, ElementClassAssignment,
        GenerateInput, RecipeFamilyId,
    };

    let eid = ElementId(request.entity_element_id);

    // Locate the entity.
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == eid)
            .map(|(entity, _)| entity)
            .ok_or_else(|| format!("Entity {} not found", request.entity_element_id))?
    };

    // Validate state direction.
    let current_state = world
        .get::<RefinementStateComponent>(entity)
        .map(|c| c.state)
        .unwrap_or_default();

    if request.target_state <= current_state {
        return Err(format!(
            "Cannot promote from {} to {} — target must be higher",
            current_state.as_str(),
            request.target_state.as_str()
        ));
    }

    let before_state = current_state;
    let target_state = request.target_state;

    // Anti-bluff gate (ADR-042): if this entity is a SemanticAssembly whose
    // type declares member-composition obligations, the promotion target must
    // be backed by the resolved sub-structure that state demands. Level-skipping
    // does not skip obligations — every obligation with required_by_state <=
    // target_state is in force. Reject the commit if any remain unresolved.
    {
        let unmet = unmet_assembly_member_obligations(world, entity, target_state);
        if !unmet.is_empty() {
            let summary = unmet
                .iter()
                .map(|o| format!("{} ({})", o.id.0, o.detail))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "Cannot promote assembly to {} — {} unmet member obligation(s): {}",
                target_state.as_str(),
                unmet.len(),
                summary
            ));
        }
    }

    // Read class/recipe assignment from the entity (if any).
    let class_assignment: Option<ElementClassAssignment> =
        world.get::<ElementClassAssignment>(entity).cloned();

    // Determine the effective recipe id: request overrides the component.
    let effective_recipe_id: Option<RecipeFamilyId> = request
        .recipe_id
        .as_ref()
        .map(|r| RecipeFamilyId(r.0.clone()))
        .or_else(|| {
            class_assignment
                .as_ref()
                .and_then(|a| a.active_recipe.clone())
        });

    let requested_recipe_id = effective_recipe_id
        .as_ref()
        .map(|recipe_id| RecipeId(recipe_id.0.clone()));

    if reactivate_parked_refinement_branch(world, eid, target_state, requested_recipe_id.as_ref())?
    {
        set_refinement_state(world, entity, target_state);
        send_refinement_change_command(
            world,
            "Promote Refinement",
            RefinementStateSnapshot {
                element_id: eid,
                state: before_state,
            },
            RefinementStateSnapshot {
                element_id: eid,
                state: target_state,
            },
        );
        return Ok(target_state);
    }

    // Lookup descriptors from the registry (requires CapabilityRegistry resource).
    let (obligation_templates, _critical_paths) = {
        if let Some(ref assignment) = class_assignment {
            let registry = world.get_resource::<crate::capability_registry::CapabilityRegistry>();
            if let Some(registry) = registry {
                let class_desc = registry.element_class_descriptor(&assignment.element_class);
                let recipe_desc = effective_recipe_id
                    .as_ref()
                    .and_then(|rid| registry.recipe_family_descriptor(rid));

                let templates = class_desc.map_or_else(Vec::new, |cd| {
                    effective_obligations(cd, recipe_desc, target_state)
                });
                let paths = class_desc.map_or_else(Vec::new, |cd| {
                    effective_promotion_critical_paths(cd, recipe_desc, target_state)
                });
                (templates, paths)
            } else {
                (Vec::new(), Vec::new())
            }
        } else {
            (Vec::new(), Vec::new())
        }
    };

    // Build the merged ObligationSet from templates.
    //
    // Change 3 — merge-not-clobber: on re-promote, preserve any agent-recorded
    // non-Unresolved status for obligations whose id already existed in the
    // entity's current ObligationSet. New templates not previously present
    // start Unresolved as before.
    let prior_statuses: HashMap<ObligationId, ObligationStatus> = {
        world
            .get::<ObligationSet>(entity)
            .map(|existing| {
                existing
                    .entries
                    .iter()
                    .filter(|o| !matches!(o.status, ObligationStatus::Unresolved))
                    .map(|o| (o.id.clone(), o.status.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };

    let initial_obligations: Vec<crate::plugins::refinement::Obligation> = obligation_templates
        .into_iter()
        .map(|t| {
            let status = prior_statuses
                .get(&t.id)
                .cloned()
                .unwrap_or(ObligationStatus::Unresolved);
            crate::plugins::refinement::Obligation {
                id: t.id,
                role: t.role,
                required_by_state: t.required_by_state,
                status,
            }
        })
        .collect();

    // Invoke the recipe's `generate` function if we have one.
    // Returns (obligation_id, child_element_id) satisfaction links and grounding updates.
    let (satisfaction_links, grounding_updates): (
        Vec<(ObligationId, u64)>,
        HashMap<ClaimPath, crate::plugins::refinement::ClaimRecord>,
    ) = if let Some(ref recipe_id) = effective_recipe_id {
        // Build parameter map from overrides (request.overrides).
        let parameters: std::collections::HashMap<String, serde_json::Value> = request
            .overrides
            .iter()
            .map(|(k, v)| (k.0.clone(), v.clone()))
            .collect();

        let input = GenerateInput {
            element_id: request.entity_element_id,
            target_state,
            parameters,
        };

        // Clone the GenerateFn Arc to avoid holding the registry borrow.
        let generate_fn = {
            let registry = world.get_resource::<crate::capability_registry::CapabilityRegistry>();
            registry
                .and_then(|r| r.recipe_family_descriptor(recipe_id))
                .map(|d| d.generate.clone())
        };

        if let Some(generate_fn) = generate_fn {
            match generate_fn(input, world) {
                Ok(output) => (output.satisfaction_links, output.grounding_updates),
                Err(e) => return Err(format!("Recipe generate failed: {e}")),
            }
        } else {
            (Vec::new(), HashMap::new())
        }
    } else {
        (Vec::new(), HashMap::new())
    };
    annotate_new_refinement_branches(world, eid, target_state, requested_recipe_id.clone());

    // Re-locate entity after possible world mutations from generate.
    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == eid)
            .map(|(entity, _)| entity)
            .ok_or_else(|| {
                format!(
                    "Entity {} not found after generate",
                    request.entity_element_id
                )
            })?
    };

    // Apply satisfaction links to the initial obligation set.
    let mut obligations = initial_obligations;
    for (obligation_id, child_eid) in &satisfaction_links {
        if let Some(ob) = obligations.iter_mut().find(|o| &o.id == obligation_id) {
            ob.status = ObligationStatus::SatisfiedBy(*child_eid);
        }
    }

    // -----------------------------------------------------------------------
    // Cross-entity bears_on obligation resolution (PP72).
    //
    // For any obligation with id "bears_on" that is still Unresolved, search
    // for a SemanticRelation with relation_type == "bears_on" whose source is
    // this entity. If found, check whether the target is at Constructible or
    // higher AND has a top_datum_mm ClaimGrounding entry. If so, mark the
    // obligation SatisfiedBy the target entity and add a claim grounding entry
    // on this entity for "bears_on".
    //
    // This logic is intentionally generic — it applies to any class with a
    // bears_on obligation, not just walls.
    // TODO(PP76): replace the target-readiness check with a GenerationPriorDescriptor query.
    {
        // Collect bears_on SemanticRelation targets for this entity.
        let bears_on_targets: Vec<ElementId> = {
            let mut q = world.try_query::<(EntityRef,)>().unwrap();
            q.iter(world)
                .filter_map(|(entity_ref,)| {
                    let rel = entity_ref.get::<SemanticRelation>()?;
                    if rel.relation_type == "bears_on" && rel.source == eid {
                        Some(rel.target)
                    } else {
                        None
                    }
                })
                .collect()
        };

        for target_eid in bears_on_targets {
            // Check target readiness: must be Constructible+ and have top_datum_mm claim.
            let target_ready = {
                let mut q = world.try_query::<(EntityRef,)>().unwrap();
                q.iter(world).any(|(entity_ref,)| {
                    if entity_ref.get::<ElementId>().copied() != Some(target_eid) {
                        return false;
                    }
                    let state = entity_ref
                        .get::<RefinementStateComponent>()
                        .map(|c| c.state)
                        .unwrap_or_default();
                    let has_claim = entity_ref.get::<ClaimGrounding>().is_some_and(|cg| {
                        cg.claims.contains_key(&ClaimPath("top_datum_mm".into()))
                    });
                    state >= RefinementState::Constructible && has_claim
                })
            };

            if target_ready {
                // Satisfy the bears_on obligation with the target entity id.
                if let Some(ob) = obligations.iter_mut().find(|o| o.id.0 == "bears_on") {
                    if ob.status == ObligationStatus::Unresolved {
                        ob.status = ObligationStatus::SatisfiedBy(target_eid.0);
                    }
                }
            }
        }
    }

    // Change 4 — per-entity class-obligation promotion gate.
    //
    // This gate is a SEPARATE block from the assembly-member-obligation gate
    // above (~line 1063). It evaluates the entity's own (per-entity-class)
    // obligations *after* all satisfaction links and bears_on resolution have
    // been applied, so agent-recorded resolutions (SatisfiedBy, Deferred,
    // Waived) from a prior resolve_obligation call are visible.
    //
    // Only in-force obligations (required_by_state <= target_state) that are
    // still Unresolved block the promotion. Demotion does not go through this
    // path, so it is never gated.
    //
    // CRITICAL ORDERING: the ObligationSet is materialised on the entity
    // *before* the gate returns. Obligations are only ever materialised at the
    // exact state they are keyed at (`effective_obligations` reads
    // `class_min_obligations[target_state]`), so on a first/direct promote they
    // are all in-force and Unresolved and the gate would fire. If we returned
    // before installing, the entity would carry no ObligationSet and
    // `resolve_obligation` (which requires one) could never run — a dead-end.
    // Installing first means a blocked promote leaves the obligations queryable
    // (`get_obligations`) and resolvable (`resolve_obligation`); the agent then
    // re-promotes and merge-not-clobber preserves the recorded resolutions so
    // the gate passes. The refinement state itself is only advanced *after* the
    // gate (`set_refinement_state` below), so a blocked promote does not move
    // the entity up a level.
    // Structural load-path obligations must be `SatisfiedBy` a real bearing
    // element — they may NOT be closed with `Deferred`/`Waived`. This enforces
    // real-world erection order: a roof cannot promote until the walls it
    // `bears_on` exist, and a foundation until what it `bears_on_terrain`
    // bears on exists. All other obligations still accept Deferred/Waived.
    fn is_load_path_obligation(id: &str) -> bool {
        matches!(id, "bears_on" | "bears_on_terrain")
    }
    let unresolved_ids: Vec<String> = obligations
        .iter()
        .filter(|o| o.required_by_state <= target_state)
        .filter(|o| match &o.status {
            ObligationStatus::Unresolved => true,
            ObligationStatus::Deferred(_) | ObligationStatus::Waived(_) => {
                is_load_path_obligation(o.id.0.as_str())
            }
            ObligationStatus::SatisfiedBy(_) => false,
        })
        .map(|o| o.id.0.clone())
        .collect();

    // Install ObligationSet on the entity (replace any existing one) BEFORE the
    // gate so blocked promotions still expose resolvable obligations.
    if !obligations.is_empty() {
        world.entity_mut(entity).insert(ObligationSet {
            entries: obligations,
        });
    }

    if !unresolved_ids.is_empty() {
        return Err(format!(
            "Cannot promote {} to {} — {} unsatisfied obligation(s): {}; \
             resolve each with resolve_obligation. Structural load-path \
             obligations (bears_on, bears_on_terrain) must be SatisfiedBy a real \
             bearing element (not Deferred/Waived) so components are built in \
             erection order; other obligations also accept Deferred/Waived with a reason",
            request.entity_element_id,
            target_state.as_str(),
            unresolved_ids.len(),
            unresolved_ids.join(", ")
        ));
    }

    // Apply grounding updates from the generate output.
    // Updates are merged into any existing ClaimGrounding component (or a new one).
    if !grounding_updates.is_empty() {
        if let Some(mut cg) = world.get_mut::<ClaimGrounding>(entity) {
            for (path, record) in grounding_updates {
                cg.claims.insert(path, record);
            }
        } else {
            world.entity_mut(entity).insert(ClaimGrounding {
                claims: grounding_updates,
            });
        }
    }

    // Update the ElementClassAssignment to record the active recipe.
    if let Some(mut assignment) = world.get_mut::<ElementClassAssignment>(entity) {
        if effective_recipe_id.is_some() {
            assignment.active_recipe = effective_recipe_id.clone();
        }
    }

    // Update AuthoringProvenance.
    if let Some(mut prov) = world.get_mut::<AuthoringProvenance>(entity) {
        if matches!(prov.mode, AuthoringMode::Freeform) {
            prov.mode = AuthoringMode::Refined(request.entity_element_id);
        }
    }

    set_refinement_state(world, entity, target_state);

    // Queue a history entry so the promotion is undoable.
    let before_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: before_state,
    };
    let after_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: target_state,
    };

    send_refinement_change_command(world, "Promote Refinement", before_snapshot, after_snapshot);

    Ok(target_state)
}

/// Apply a demotion to the world, queuing history commands for undo/redo.
///
/// Parks active refinement branches instead of deleting generated detail.
pub fn apply_demote_refinement(
    world: &mut World,
    request: DemoteRefinementRequest,
) -> Result<RefinementState, String> {
    let eid = ElementId(request.entity_element_id);

    let entity = {
        let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
        q.iter(world)
            .find(|(_, id)| **id == eid)
            .map(|(entity, _)| entity)
            .ok_or_else(|| format!("Entity {} not found", request.entity_element_id))?
    };

    let current_state = world
        .get::<RefinementStateComponent>(entity)
        .map(|c| c.state)
        .unwrap_or_default();

    if request.target_state >= current_state {
        return Err(format!(
            "Cannot demote from {} to {} — target must be lower",
            current_state.as_str(),
            request.target_state.as_str()
        ));
    }

    let target_state = request.target_state;

    let children_to_park = query_refined_into(world, eid);

    set_refinement_state(world, entity, target_state);

    for child in children_to_park {
        set_refinement_branch_status(world, eid, child, RefinementBranchStatus::Parked);
    }

    // Queue undo/redo history.
    let before_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: current_state,
    };
    let after_snapshot = RefinementStateSnapshot {
        element_id: eid,
        state: target_state,
    };
    send_refinement_change_command(world, "Demote Refinement", before_snapshot, after_snapshot);

    Ok(target_state)
}

fn set_refinement_state(world: &mut World, entity: Entity, state: RefinementState) {
    if let Some(mut comp) = world.get_mut::<RefinementStateComponent>(entity) {
        comp.state = state;
    } else {
        world
            .entity_mut(entity)
            .insert(RefinementStateComponent { state });
    }
}

// ---------------------------------------------------------------------------
// Internal: lightweight undo/redo snapshot for refinement state changes
// ---------------------------------------------------------------------------

/// A minimal snapshot that records just the refinement state of an entity.
/// Used as the before/after pair in `ApplyEntityChangesCommand`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RefinementStateSnapshot {
    element_id: ElementId,
    state: RefinementState,
}

use crate::authored_entity::{
    AuthoredEntity, BoxedEntity, EntityBounds, HandleInfo, PropertyFieldDef,
};
use std::any::Any;

impl AuthoredEntity for RefinementStateSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "refinement_state_snapshot"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("refinement:{}", self.element_id.0)
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        Vec::new()
    }

    fn set_property_json(
        &self,
        _name: &str,
        _value: &serde_json::Value,
    ) -> Result<BoxedEntity, String> {
        Err("RefinementStateSnapshot is read-only".to_string())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        // Find the entity and update its RefinementStateComponent.
        let entity = {
            let mut q = world.try_query::<(Entity, &ElementId)>().unwrap();
            q.iter(world)
                .find(|(_, id)| **id == self.element_id)
                .map(|(entity, _)| entity)
        };
        if let Some(entity) = entity {
            if let Some(mut comp) = world.get_mut::<RefinementStateComponent>(entity) {
                comp.state = self.state;
            } else {
                world
                    .entity_mut(entity)
                    .insert(RefinementStateComponent { state: self.state });
            }
        }
    }

    fn remove_from(&self, _world: &mut World) {
        // Removing a refinement snapshot means "undo" the state component
        // — handled by apply_to with the before snapshot; nothing to despawn.
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn box_clone(&self) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }
}

impl From<RefinementStateSnapshot> for BoxedEntity {
    fn from(s: RefinementStateSnapshot) -> Self {
        BoxedEntity(Box::new(s))
    }
}

// ---------------------------------------------------------------------------
// ObligationSetSnapshot — lightweight undo/redo for resolve_obligation
// ---------------------------------------------------------------------------

/// A snapshot of an entity's `ObligationSet` at a single point in time.
/// Used as the before/after pair in `ApplyEntityChangesCommand` for
/// `resolve_obligation` so the change is undoable through the history pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObligationSetSnapshot {
    pub element_id: ElementId,
    /// Serialised snapshot of the `ObligationSet::entries`. Stored as JSON
    /// so undo/redo can re-insert the exact set without coupling to the full
    /// type from inside `AuthoredEntity::apply_to`.
    pub entries_json: serde_json::Value,
}

impl ObligationSetSnapshot {
    /// Capture the current `ObligationSet` of `entity`.  When the entity
    /// has no `ObligationSet`, the snapshot records an empty entries array
    /// so undo correctly removes any obligations that were added.
    pub fn capture(world: &World, entity: Entity, element_id: u64) -> Self {
        let entries_json = world
            .get::<ObligationSet>(entity)
            .and_then(|set| serde_json::to_value(&set.entries).ok())
            .unwrap_or(serde_json::Value::Array(Vec::new()));

        Self {
            element_id: ElementId(element_id),
            entries_json,
        }
    }
}

impl AuthoredEntity for ObligationSetSnapshot {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        "obligation_set_snapshot"
    }

    fn element_id(&self) -> ElementId {
        self.element_id
    }

    fn label(&self) -> String {
        format!("obligation_set:{}", self.element_id.0)
    }

    fn center(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn bounds(&self) -> Option<EntityBounds> {
        None
    }

    fn translate_by(&self, _delta: Vec3) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn rotate_by(&self, _rotation: Quat) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn scale_by(&self, _factor: Vec3, _center: Vec3) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn property_fields(&self) -> Vec<PropertyFieldDef> {
        Vec::new()
    }

    fn set_property_json(
        &self,
        _name: &str,
        _value: &serde_json::Value,
    ) -> Result<BoxedEntity, String> {
        Err("ObligationSetSnapshot is read-only".to_string())
    }

    fn handles(&self) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    fn apply_to(&self, world: &mut World) {
        let Some(entity) = find_entity_by_element_id_readonly(world, self.element_id) else {
            return;
        };
        if let Ok(entries) =
            serde_json::from_value::<Vec<Obligation>>(self.entries_json.clone())
        {
            if entries.is_empty() {
                world.entity_mut(entity).remove::<ObligationSet>();
            } else {
                world
                    .entity_mut(entity)
                    .insert(ObligationSet { entries });
            }
        }
    }

    fn remove_from(&self, _world: &mut World) {
        // Nothing to remove; undo is handled by apply_to with the before snapshot.
    }

    fn draw_preview(&self, _gizmos: &mut Gizmos, _color: Color) {}

    fn box_clone(&self) -> BoxedEntity {
        BoxedEntity(Box::new(self.clone()))
    }

    fn eq_snapshot(&self, other: &dyn AuthoredEntity) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }
}

impl From<ObligationSetSnapshot> for BoxedEntity {
    fn from(s: ObligationSetSnapshot) -> Self {
        BoxedEntity(Box::new(s))
    }
}

// ---------------------------------------------------------------------------
// Helper: send Messages resources for command pipeline
// ---------------------------------------------------------------------------

fn send_refinement_change_command(
    world: &mut World,
    label: &'static str,
    before: RefinementStateSnapshot,
    after: RefinementStateSnapshot,
) {
    // Use ApplyEntityChangesCommand to get free undo/redo via the existing pipeline.
    world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .write(BeginCommandGroup { label });
    world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .write(ApplyEntityChangesCommand {
            label,
            before: vec![before.into()],
            after: vec![after.into()],
        });
    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);
}

// ---------------------------------------------------------------------------
// Registration helper (called from ModelingPlugin or a RefinementPlugin)
// ---------------------------------------------------------------------------

use crate::capability_registry::{CapabilityRegistryAppExt, RelationTypeDescriptor};

/// Register the `refinement_of` and `refined_into` relation types.
///
/// Call this from an `App::add_systems(Startup, ...)` or from a plugin's
/// `build` method.
pub fn register_refinement_relations(app: &mut App) {
    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "refinement_of".to_string(),
        label: "Refinement Of".to_string(),
        description: "Directed child → parent link that preserves the coarse stub's \
                      identity when an entity is promoted to a more detailed form. \
                      Cardinality: each child has exactly one parent."
            .to_string(),
        valid_source_types: Vec::new(), // any entity type
        valid_target_types: Vec::new(), // any entity type
        parameter_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "promoted_from_state": { "type": "string" }
            }
        }),
        participates_in_dependency_graph: false,
        external_classification: None,
        host_contract_kind: None,
    });

    app.register_relation_type(RelationTypeDescriptor {
        relation_type: "refined_into".to_string(),
        label: "Refined Into".to_string(),
        description: "Directed parent → child link, inverse of `refinement_of`. \
                      A parent entity may have multiple `refined_into` children \
                      if it was promoted in stages."
            .to_string(),
        valid_source_types: Vec::new(),
        valid_target_types: Vec::new(),
        parameter_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "target_state": { "type": "string" }
            }
        }),
        participates_in_dependency_graph: false,
        external_classification: None,
        host_contract_kind: None,
    });
}

// ---------------------------------------------------------------------------
// Refinement relation helpers (used by handlers and tests)
// ---------------------------------------------------------------------------

use crate::plugins::identity::ElementIdAllocator;

/// Create a `refinement_of(child → parent)` and a matching
/// `refined_into(parent → child)` relation pair, returning both element-ids.
///
/// Caller is responsible for queuing the spawns via the command pipeline.
pub fn create_refinement_relation_pair(
    world: &mut World,
    parent_eid: ElementId,
    child_eid: ElementId,
    promoted_from_state: RefinementState,
    target_state: RefinementState,
) -> (ElementId, ElementId) {
    let fwd_id = world.resource::<ElementIdAllocator>().next_id();
    let inv_id = world.resource::<ElementIdAllocator>().next_id();

    let fwd = RelationSnapshot {
        element_id: fwd_id,
        relation: SemanticRelation {
            source: child_eid,
            target: parent_eid,
            relation_type: "refinement_of".to_string(),
            parameters: serde_json::json!({
                "promoted_from_state": promoted_from_state.as_str()
            }),
        },
    };
    let inv = RelationSnapshot {
        element_id: inv_id,
        relation: SemanticRelation {
            source: parent_eid,
            target: child_eid,
            relation_type: "refined_into".to_string(),
            parameters: serde_json::json!({
                "target_state": target_state.as_str()
            }),
        },
    };

    fwd.apply_to(world);
    inv.apply_to(world);
    tag_refinement_relation_pair(
        world,
        fwd_id,
        inv_id,
        parent_eid,
        child_eid,
        target_state,
        None,
    );

    (fwd_id, inv_id)
}

fn tag_refinement_relation_pair(
    world: &mut World,
    fwd_id: ElementId,
    inv_id: ElementId,
    parent_eid: ElementId,
    child_eid: ElementId,
    target_state: RefinementState,
    recipe_id: Option<RecipeId>,
) {
    let root_element_id = root_refinement_handle(world, parent_eid).0;
    let branch = RefinementBranch {
        root_element_id,
        parent_element_id: parent_eid.0,
        child_element_id: child_eid.0,
        target_state,
        recipe_id,
        status: RefinementBranchStatus::Active,
    };

    for relation_id in [fwd_id, inv_id] {
        if let Some(entity) = find_entity_by_element_id_readonly(world, relation_id) {
            world.entity_mut(entity).insert(branch.clone());
        }
    }
}

fn find_entity_by_element_id_readonly(world: &World, element_id: ElementId) -> Option<Entity> {
    let mut q = world.try_query::<(Entity, &ElementId)>()?;
    q.iter(world)
        .find_map(|(entity, id)| (*id == element_id).then_some(entity))
}

fn root_refinement_handle(world: &World, element_id: ElementId) -> ElementId {
    let mut current = element_id;
    let mut seen = BTreeSet::new();
    while seen.insert(current.0) {
        let Some(parent) = query_refinement_of(world, current) else {
            return current;
        };
        current = parent;
    }
    element_id
}

fn refinement_branch_status(entity_ref: &EntityRef) -> RefinementBranchStatus {
    entity_ref
        .get::<RefinementBranch>()
        .map(|branch| branch.status)
        .unwrap_or_default()
}

fn relation_matches_refinement_pair(
    relation: &SemanticRelation,
    parent_eid: ElementId,
    child_eid: ElementId,
) -> bool {
    (relation.relation_type == "refined_into"
        && relation.source == parent_eid
        && relation.target == child_eid)
        || (relation.relation_type == "refinement_of"
            && relation.source == child_eid
            && relation.target == parent_eid)
}

fn set_refinement_branch_status(
    world: &mut World,
    parent_eid: ElementId,
    child_eid: ElementId,
    status: RefinementBranchStatus,
) {
    let relation_entities: Vec<Entity> = {
        let mut q = world.try_query::<(Entity, EntityRef)>().unwrap();
        q.iter(world)
            .filter_map(|(entity, entity_ref)| {
                let rel = entity_ref.get::<SemanticRelation>()?;
                relation_matches_refinement_pair(rel, parent_eid, child_eid).then_some(entity)
            })
            .collect()
    };

    for entity in relation_entities {
        if let Some(mut branch) = world.get_mut::<RefinementBranch>(entity) {
            branch.status = status;
        }
    }
    set_refinement_subtree_visibility(world, child_eid, status);
}

fn reactivate_parked_refinement_branch(
    world: &mut World,
    parent_eid: ElementId,
    target_state: RefinementState,
    recipe_id: Option<&RecipeId>,
) -> Result<bool, String> {
    let parked_child = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world).find_map(|(entity_ref,)| {
            let rel = entity_ref.get::<SemanticRelation>()?;
            if rel.relation_type != "refined_into" || rel.source != parent_eid {
                return None;
            }
            let branch = entity_ref.get::<RefinementBranch>()?;
            if branch.status != RefinementBranchStatus::Parked {
                return None;
            }
            if branch.target_state != target_state || branch.recipe_id.as_ref() != recipe_id {
                return None;
            }
            Some(rel.target)
        })
    };

    if let Some(child) = parked_child {
        set_refinement_branch_status(world, parent_eid, child, RefinementBranchStatus::Active);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn annotate_new_refinement_branches(
    world: &mut World,
    parent_eid: ElementId,
    target_state: RefinementState,
    recipe_id: Option<RecipeId>,
) {
    let relation_entities: Vec<Entity> = {
        let mut q = world.try_query::<(Entity, EntityRef)>().unwrap();
        q.iter(world)
            .filter_map(|(entity, entity_ref)| {
                let rel = entity_ref.get::<SemanticRelation>()?;
                let branch = entity_ref.get::<RefinementBranch>()?;
                (branch.status == RefinementBranchStatus::Active
                    && branch.parent_element_id == parent_eid.0
                    && branch.target_state == target_state
                    && branch.recipe_id.is_none()
                    && (rel.relation_type == "refined_into"
                        || rel.relation_type == "refinement_of"))
                    .then_some(entity)
            })
            .collect()
    };

    for entity in relation_entities {
        if let Some(mut branch) = world.get_mut::<RefinementBranch>(entity) {
            branch.recipe_id = recipe_id.clone();
        }
    }
}

/// Query all active `refinement_of` child element-ids for a given parent.
pub fn query_refined_into(world: &World, parent_eid: ElementId) -> Vec<ElementId> {
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    q.iter(world)
        .filter_map(|(entity_ref,)| {
            let rel = entity_ref.get::<SemanticRelation>()?;
            if rel.relation_type == "refined_into"
                && rel.source == parent_eid
                && refinement_branch_status(&entity_ref) == RefinementBranchStatus::Active
            {
                Some(rel.target)
            } else {
                None
            }
        })
        .collect()
}

/// Query parked child element-ids for a given parent.
pub fn query_parked_refined_into(world: &World, parent_eid: ElementId) -> Vec<ElementId> {
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    q.iter(world)
        .filter_map(|(entity_ref,)| {
            let rel = entity_ref.get::<SemanticRelation>()?;
            if rel.relation_type == "refined_into"
                && rel.source == parent_eid
                && refinement_branch_status(&entity_ref) == RefinementBranchStatus::Parked
            {
                Some(rel.target)
            } else {
                None
            }
        })
        .collect()
}

/// Query the parent element-id for a given child via `refinement_of`.
pub fn query_refinement_of(world: &World, child_eid: ElementId) -> Option<ElementId> {
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    q.iter(world).find_map(|(entity_ref,)| {
        let rel = entity_ref.get::<SemanticRelation>()?;
        if rel.relation_type == "refinement_of" && rel.source == child_eid {
            Some(rel.target)
        } else {
            None
        }
    })
}

pub fn is_parked_refinement_entity(world: &World, element_id: ElementId) -> bool {
    let mut current = element_id;
    let mut seen = BTreeSet::new();
    while seen.insert(current.0) {
        let relation = {
            let mut q = world.try_query::<(EntityRef,)>().unwrap();
            q.iter(world).find_map(|(entity_ref,)| {
                let rel = entity_ref.get::<SemanticRelation>()?;
                if rel.relation_type == "refinement_of" && rel.source == current {
                    Some((rel.target, refinement_branch_status(&entity_ref)))
                } else {
                    None
                }
            })
        };
        let Some((parent, status)) = relation else {
            return false;
        };
        if status == RefinementBranchStatus::Parked {
            return true;
        }
        current = parent;
    }
    false
}

pub fn is_active_refinement_entity(world: &World, element_id: ElementId) -> bool {
    !is_parked_refinement_entity(world, element_id)
}

pub fn list_refinement_branches(world: &World, parent_eid: ElementId) -> Vec<RefinementBranchInfo> {
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    let mut branches: Vec<RefinementBranchInfo> = q
        .iter(world)
        .filter_map(|(entity_ref,)| {
            let rel = entity_ref.get::<SemanticRelation>()?;
            if rel.relation_type != "refined_into" || rel.source != parent_eid {
                return None;
            }
            let branch = entity_ref.get::<RefinementBranch>()?;
            Some(RefinementBranchInfo {
                root_element_id: branch.root_element_id,
                parent_element_id: branch.parent_element_id,
                child_element_id: branch.child_element_id,
                target_state: branch.target_state,
                recipe_id: branch.recipe_id.clone(),
                status: branch.status,
            })
        })
        .collect();
    branches.sort_by_key(|branch| (branch.status.as_str().to_string(), branch.child_element_id));
    branches
}

pub fn discard_refinement_branch(
    world: &mut World,
    parent_eid: ElementId,
    child_eid: ElementId,
) -> Result<Vec<u64>, String> {
    let parked = query_parked_refined_into(world, parent_eid).contains(&child_eid);
    if !parked {
        return Err(format!(
            "Refinement branch {} -> {} is not parked",
            parent_eid.0, child_eid.0
        ));
    }

    let mut ids_to_despawn: BTreeSet<u64> = BTreeSet::new();
    let mut queue = VecDeque::from([child_eid]);
    while let Some(parent) = queue.pop_front() {
        if !ids_to_despawn.insert(parent.0) {
            continue;
        }
        for child in query_all_refined_into(world, parent) {
            queue.push_back(child);
        }
    }

    let relation_ids: Vec<u64> = {
        let mut q = world.try_query::<(EntityRef,)>().unwrap();
        q.iter(world)
            .filter_map(|(entity_ref,)| {
                let id = entity_ref.get::<ElementId>()?;
                let rel = entity_ref.get::<SemanticRelation>()?;
                let source_in_branch = ids_to_despawn.contains(&rel.source.0);
                let target_in_branch = ids_to_despawn.contains(&rel.target.0);
                let boundary_pair = relation_matches_refinement_pair(rel, parent_eid, child_eid);
                (source_in_branch || target_in_branch || boundary_pair).then_some(id.0)
            })
            .collect()
    };
    ids_to_despawn.extend(relation_ids);

    let discarded: Vec<u64> = ids_to_despawn.iter().copied().collect();
    for id in &discarded {
        if let Some(entity) = find_entity_by_element_id_readonly(world, ElementId(*id)) {
            let _ = world.despawn(entity);
        }
    }

    Ok(discarded)
}

fn query_all_refined_into(world: &World, parent_eid: ElementId) -> Vec<ElementId> {
    let mut q = world.try_query::<(EntityRef,)>().unwrap();
    q.iter(world)
        .filter_map(|(entity_ref,)| {
            let rel = entity_ref.get::<SemanticRelation>()?;
            if rel.relation_type == "refined_into" && rel.source == parent_eid {
                Some(rel.target)
            } else {
                None
            }
        })
        .collect()
}

fn set_refinement_subtree_visibility(
    world: &mut World,
    root_eid: ElementId,
    status: RefinementBranchStatus,
) {
    let visibility = match status {
        RefinementBranchStatus::Active => Visibility::Visible,
        RefinementBranchStatus::Parked => Visibility::Hidden,
    };
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::from([root_eid]);
    while let Some(eid) = queue.pop_front() {
        if !visited.insert(eid.0) {
            continue;
        }
        if let Some(entity) = find_entity_by_element_id_readonly(world, eid) {
            world.entity_mut(entity).insert(visibility);
        }
        for child in query_all_refined_into(world, eid) {
            queue.push_back(child);
        }
    }
}

/// Resolve the active refinement subtree rooted at `root_eid`.
///
/// The result is deterministic: ids are de-duplicated and sorted, while the root
/// is always present. This is the generic scope that promotion previews expose
/// for user or agent editing before commit.
pub fn resolve_refinement_subtree(
    world: &World,
    root_eid: ElementId,
) -> Result<RefinementSubtree, String> {
    let root_exists = {
        let mut q = world.try_query::<(&ElementId,)>().unwrap();
        q.iter(world).any(|(id,)| *id == root_eid)
    };
    if !root_exists {
        return Err(format!("Entity {} not found", root_eid.0));
    }

    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    visited.insert(root_eid.0);
    queue.push_back(root_eid);

    while let Some(parent) = queue.pop_front() {
        for child in query_refined_into(world, parent) {
            if visited.insert(child.0) {
                queue.push_back(child);
            }
        }
    }

    Ok(RefinementSubtree {
        root_element_id: root_eid.0,
        active_element_ids: visited.into_iter().collect(),
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- RefinementState ordering & serialization ---

    #[test]
    fn refinement_state_ordering_is_correct() {
        assert!(RefinementState::Conceptual < RefinementState::Schematic);
        assert!(RefinementState::Schematic < RefinementState::Constructible);
        assert!(RefinementState::Constructible < RefinementState::Detailed);
        assert!(RefinementState::Detailed < RefinementState::FabricationReady);
    }

    #[test]
    fn refinement_state_default_is_conceptual() {
        assert_eq!(RefinementState::default(), RefinementState::Conceptual);
    }

    #[test]
    fn refinement_level_semantics_states_invariant_and_contradiction() {
        let s = REFINEMENT_LEVEL_SEMANTICS;
        // Every level is described.
        for level in [
            "Conceptual",
            "Schematic",
            "Constructible",
            "Detailed",
            "FabricationReady",
        ] {
            assert!(s.contains(level), "semantics must describe {level}");
        }
        // The two load-bearing rules are present.
        assert!(
            s.contains("monotonic"),
            "semantics must state the monotonic-resolution invariant"
        );
        assert!(
            s.to_lowercase().contains("identical"),
            "semantics must address the identical-vs-level contradiction"
        );
    }

    #[test]
    fn composition_contract_states_reuse_hosting_and_obligations() {
        let s = COMPONENT_COMPOSITION_CONTRACT;
        // The three platform mechanisms by which detail is added.
        assert!(
            s.contains("Occurrence"),
            "must describe reuse via Occurrences"
        );
        assert!(
            s.contains("base_definition_id"),
            "must describe derivation of family variants"
        );
        assert!(
            s.to_lowercase().contains("hosting"),
            "must describe the hosting contract for embedded components"
        );
        // It must point the MCP-only agent at the machine-checkable contract.
        for tool in [
            "list_element_classes",
            "get_obligations",
            "preview_promotion",
            "run_validation",
            "validate_host_fit",
            // The obligation gate is machine-enforced, so the contract must
            // teach the escape hatch tool and its three resolution variants.
            "resolve_obligation",
            "satisfied_by",
            "deferred",
            "waived",
        ] {
            assert!(
                s.contains(tool),
                "composition contract must reference {tool}"
            );
        }
        // It must make clear the gate is enforced, not advisory.
        assert!(
            s.contains("machine-enforced"),
            "contract must state the obligation gate is machine-enforced"
        );
    }

    #[test]
    fn procedural_session_orientation_names_all_five_tools_and_when_to_use() {
        let s = PROCEDURAL_SESSION_ORIENTATION;
        // All five tools are named, in their canonical dotted form, so an
        // MCP-only agent can grep the served prompt for them.
        for tool in [
            "procedural_session.create",
            "procedural_session.eval",
            "procedural_session.snapshot",
            "procedural_session.commit",
            "procedural_session.export",
        ] {
            assert!(s.contains(tool), "orientation must name {tool}");
        }
        // The "when to use" framing must be present — the whole point of the
        // block is that descriptions alone leave agents guessing.
        assert!(
            s.to_lowercase().contains("when") && s.contains("Reach for"),
            "orientation must state when to reach for procedural_session.*"
        );
        // The eval modes must be discoverable from the orientation alone, so
        // an agent does not have to re-read each tool description to plan a
        // dry-run loop.
        for mode in ["bind_only", "dry_run", "dry_run_and_bind"] {
            assert!(
                s.contains(mode),
                "orientation must surface eval mode {mode}"
            );
        }
        // The durable-reuse loop must not dead-end at export: the orientation
        // has to name the install step that registers an executable recipe,
        // and the corpus-gap path that uses it, so an agent learns new
        // construction as persistent data rather than hand-rolling geometry.
        assert!(
            s.contains("install_recipe_from_session_export"),
            "orientation must name the install step that makes a recipe executable"
        );
        assert!(
            s.contains("CorpusGap") && s.contains("acquire_corpus_passage"),
            "orientation must connect the install path to closing a corpus gap as data"
        );
    }

    #[test]
    fn refinement_state_round_trips_through_json() {
        for state in [
            RefinementState::Conceptual,
            RefinementState::Schematic,
            RefinementState::Constructible,
            RefinementState::Detailed,
            RefinementState::FabricationReady,
        ] {
            let json = serde_json::to_value(state).expect("serialize");
            let back: RefinementState = serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, state);
        }
    }

    #[test]
    fn refinement_state_from_str_round_trip() {
        for state in [
            RefinementState::Conceptual,
            RefinementState::Schematic,
            RefinementState::Constructible,
            RefinementState::Detailed,
            RefinementState::FabricationReady,
        ] {
            assert_eq!(RefinementState::from_str(state.as_str()), Some(state));
        }
        assert_eq!(RefinementState::from_str("Unknown"), None);
    }

    // --- ObligationSet round-trip through JSON ---

    #[test]
    fn obligation_set_round_trips_through_json() {
        let set = ObligationSet {
            entries: vec![
                Obligation {
                    id: ObligationId("structural_layer".to_string()),
                    role: SemanticRole("primary_structure".to_string()),
                    required_by_state: RefinementState::Schematic,
                    status: ObligationStatus::Unresolved,
                },
                Obligation {
                    id: ObligationId("exterior_cladding".to_string()),
                    role: SemanticRole("envelope".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::SatisfiedBy(42),
                },
                Obligation {
                    id: ObligationId("insulation".to_string()),
                    role: SemanticRole("thermal".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::Deferred("awaiting thermal spec".to_string()),
                },
            ],
        };
        let json = serde_json::to_value(&set).expect("serialize");
        let back: ObligationSet = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, set);
    }

    // --- Validator: Conceptual → no findings ---

    #[test]
    fn validator_conceptual_emits_no_findings() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("structure".to_string()),
                role: SemanticRole("primary_structure".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Conceptual, &obligations);
        assert!(findings.is_empty(), "Conceptual must never emit findings");
    }

    // --- Validator: Schematic severity rules ---

    #[test]
    fn validator_schematic_primary_structure_is_warning() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("structure".to_string()),
                role: SemanticRole("primary_structure".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Schematic, &obligations);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, FindingSeverity::Warning);
    }

    #[test]
    fn validator_schematic_non_primary_is_advice() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("envelope".to_string()),
                role: SemanticRole("envelope".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Schematic, &obligations);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, FindingSeverity::Advice);
    }

    // --- Validator: Constructible → error for unresolved ---

    #[test]
    fn validator_constructible_unresolved_is_error() {
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("structure".to_string()),
                role: SemanticRole("primary_structure".to_string()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Constructible, &obligations);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, FindingSeverity::Error);
    }

    #[test]
    fn validator_satisfied_deferred_waived_pass_at_constructible() {
        let obligations = ObligationSet {
            entries: vec![
                Obligation {
                    id: ObligationId("a".to_string()),
                    role: SemanticRole("primary_structure".to_string()),
                    required_by_state: RefinementState::Schematic,
                    status: ObligationStatus::SatisfiedBy(99),
                },
                Obligation {
                    id: ObligationId("b".to_string()),
                    role: SemanticRole("envelope".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::Deferred("spec pending".to_string()),
                },
                Obligation {
                    id: ObligationId("c".to_string()),
                    role: SemanticRole("thermal".to_string()),
                    required_by_state: RefinementState::Constructible,
                    status: ObligationStatus::Waived("out of scope for phase".to_string()),
                },
            ],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Constructible, &obligations);
        assert!(
            findings.is_empty(),
            "Satisfied/Deferred/Waived must not generate findings"
        );
    }

    // --- Validator: obligation not yet required at current state ---

    #[test]
    fn validator_obligation_not_yet_required_is_not_fired() {
        // Obligation required at Constructible, but entity is only Schematic.
        let obligations = ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("detail_drawings".to_string()),
                role: SemanticRole("documentation".to_string()),
                required_by_state: RefinementState::Constructible,
                status: ObligationStatus::Unresolved,
            }],
        };
        let findings =
            validate_declared_state_obligations(1, RefinementState::Schematic, &obligations);
        assert!(findings.is_empty());
    }

    #[test]
    fn refinement_subtree_resolves_root_and_active_descendants_deterministically() {
        let mut world = World::new();
        world.spawn((ElementId(1),));
        world.spawn((ElementId(2),));
        world.spawn((ElementId(3),));
        world.spawn((
            ElementId(20),
            SemanticRelation {
                source: ElementId(1),
                target: ElementId(2),
                relation_type: "unrelated".to_string(),
                parameters: serde_json::Value::Null,
            },
        ));
        world.spawn((
            ElementId(21),
            SemanticRelation {
                source: ElementId(1),
                target: ElementId(3),
                relation_type: "refined_into".to_string(),
                parameters: serde_json::Value::Null,
            },
        ));
        world.spawn((
            ElementId(22),
            SemanticRelation {
                source: ElementId(3),
                target: ElementId(2),
                relation_type: "refined_into".to_string(),
                parameters: serde_json::Value::Null,
            },
        ));

        let subtree = resolve_refinement_subtree(&world, ElementId(1)).unwrap();

        assert_eq!(subtree.root_element_id, 1);
        assert_eq!(subtree.active_element_ids, vec![1, 2, 3]);
        assert!(subtree.contains(2));
    }

    fn refinement_command_world() -> World {
        let mut world = World::new();
        world.init_resource::<ElementIdAllocator>();
        world.resource_mut::<ElementIdAllocator>().set_next(10_000);
        world.init_resource::<Messages<BeginCommandGroup>>();
        world.init_resource::<Messages<ApplyEntityChangesCommand>>();
        world.init_resource::<Messages<EndCommandGroup>>();
        world
    }

    #[test]
    fn demotion_parks_refinement_branch_without_deleting_child() {
        let mut world = refinement_command_world();
        world.spawn((
            ElementId(1),
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
        ));
        world.spawn((ElementId(2),));
        create_refinement_relation_pair(
            &mut world,
            ElementId(1),
            ElementId(2),
            RefinementState::Conceptual,
            RefinementState::Constructible,
        );

        apply_demote_refinement(
            &mut world,
            DemoteRefinementRequest {
                entity_element_id: 1,
                target_state: RefinementState::Conceptual,
            },
        )
        .unwrap();

        assert!(query_refined_into(&world, ElementId(1)).is_empty());
        assert_eq!(
            query_parked_refined_into(&world, ElementId(1)),
            vec![ElementId(2)]
        );
        assert!(is_parked_refinement_entity(&world, ElementId(2)));
        let child_entity = find_entity_by_element_id_readonly(&world, ElementId(2)).unwrap();
        assert_eq!(
            world.get::<Visibility>(child_entity).copied(),
            Some(Visibility::Hidden)
        );
    }

    #[test]
    fn re_promotion_reactivates_parked_branch() {
        let mut world = refinement_command_world();
        let parent = world
            .spawn((
                ElementId(1),
                RefinementStateComponent {
                    state: RefinementState::Constructible,
                },
            ))
            .id();
        world.spawn((ElementId(2),));
        create_refinement_relation_pair(
            &mut world,
            ElementId(1),
            ElementId(2),
            RefinementState::Conceptual,
            RefinementState::Constructible,
        );
        apply_demote_refinement(
            &mut world,
            DemoteRefinementRequest {
                entity_element_id: 1,
                target_state: RefinementState::Conceptual,
            },
        )
        .unwrap();

        let new_state = apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 1,
                target_state: RefinementState::Constructible,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .unwrap();

        assert_eq!(new_state, RefinementState::Constructible);
        assert_eq!(query_refined_into(&world, ElementId(1)), vec![ElementId(2)]);
        assert!(query_parked_refined_into(&world, ElementId(1)).is_empty());
        assert!(!is_parked_refinement_entity(&world, ElementId(2)));
        let child_entity = find_entity_by_element_id_readonly(&world, ElementId(2)).unwrap();
        assert_eq!(
            world.get::<Visibility>(child_entity).copied(),
            Some(Visibility::Visible)
        );
        assert_eq!(
            world.get::<RefinementStateComponent>(parent).unwrap().state,
            RefinementState::Constructible
        );
    }

    #[test]
    fn discard_refinement_branch_removes_parked_child_subtree() {
        let mut world = refinement_command_world();
        world.spawn((
            ElementId(1),
            RefinementStateComponent {
                state: RefinementState::Constructible,
            },
        ));
        world.spawn((ElementId(2),));
        world.spawn((ElementId(3),));
        create_refinement_relation_pair(
            &mut world,
            ElementId(1),
            ElementId(2),
            RefinementState::Conceptual,
            RefinementState::Constructible,
        );
        create_refinement_relation_pair(
            &mut world,
            ElementId(2),
            ElementId(3),
            RefinementState::Constructible,
            RefinementState::Detailed,
        );
        apply_demote_refinement(
            &mut world,
            DemoteRefinementRequest {
                entity_element_id: 1,
                target_state: RefinementState::Conceptual,
            },
        )
        .unwrap();

        let discarded = discard_refinement_branch(&mut world, ElementId(1), ElementId(2)).unwrap();

        assert!(discarded.contains(&2));
        assert!(discarded.contains(&3));
        assert!(find_entity_by_element_id_readonly(&world, ElementId(1)).is_some());
        assert!(find_entity_by_element_id_readonly(&world, ElementId(2)).is_none());
        assert!(find_entity_by_element_id_readonly(&world, ElementId(3)).is_none());
        assert!(query_refined_into(&world, ElementId(1)).is_empty());
        assert!(query_parked_refined_into(&world, ElementId(1)).is_empty());
    }

    // --- Assembly member-composition obligation evaluator (ADR-042) ---

    fn member_template(
        id: &str,
        member_role: &str,
        required_by: RefinementState,
        tracks: bool,
    ) -> crate::capability_registry::AssemblyMemberObligationTemplate {
        crate::capability_registry::AssemblyMemberObligationTemplate {
            id: ObligationId(id.into()),
            role: SemanticRole("primary_structure".into()),
            member_role: member_role.into(),
            min_count: 1,
            required_by_state: required_by,
            member_tracks_target_state: tracks,
        }
    }

    fn spawn_member(world: &mut World, eid: u64, state: RefinementState) {
        world.spawn((ElementId(eid), RefinementStateComponent { state }));
    }

    fn house_with(members: Vec<(&str, u64)>) -> SemanticAssembly {
        SemanticAssembly {
            assembly_type: "house".into(),
            label: "h".into(),
            members: members
                .into_iter()
                .map(
                    |(role, target)| crate::plugins::modeling::assembly::AssemblyMemberRef {
                        target: ElementId(target),
                        role: role.into(),
                    },
                )
                .collect(),
            parameters: serde_json::Value::Null,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn assembly_obligation_skipped_above_target_state() {
        let world = World::new();
        let templates = vec![member_template(
            "needs_roof",
            "roof_element",
            RefinementState::Detailed,
            true,
        )];
        // Target below the obligation's required_by_state — not in force.
        let evaluated = evaluate_assembly_member_obligations(
            &world,
            &house_with(Vec::new()),
            &templates,
            RefinementState::Schematic,
        );
        assert!(evaluated.is_empty());
    }

    #[test]
    fn assembly_obligation_unresolved_when_member_absent() {
        let world = World::new();
        let templates = vec![member_template(
            "needs_wall",
            "exterior_wall",
            RefinementState::Schematic,
            true,
        )];
        let evaluated = evaluate_assembly_member_obligations(
            &world,
            &house_with(Vec::new()),
            &templates,
            RefinementState::Detailed,
        );
        assert_eq!(evaluated.len(), 1);
        assert!(evaluated[0].is_unresolved());
    }

    #[test]
    fn assembly_obligation_unresolved_when_member_underresolved() {
        let mut world = World::new();
        spawn_member(&mut world, 11, RefinementState::Conceptual);
        let templates = vec![member_template(
            "needs_wall",
            "exterior_wall",
            RefinementState::Schematic,
            true,
        )];
        let evaluated = evaluate_assembly_member_obligations(
            &world,
            &house_with(vec![("exterior_wall", 11)]),
            &templates,
            RefinementState::Detailed,
        );
        assert!(evaluated[0].is_unresolved());
        assert!(evaluated[0].detail.contains("Detailed"));
    }

    #[test]
    fn assembly_obligation_satisfied_when_member_tracks_target() {
        let mut world = World::new();
        spawn_member(&mut world, 11, RefinementState::Detailed);
        let templates = vec![member_template(
            "needs_wall",
            "exterior_wall",
            RefinementState::Schematic,
            true,
        )];
        let evaluated = evaluate_assembly_member_obligations(
            &world,
            &house_with(vec![("exterior_wall", 11)]),
            &templates,
            RefinementState::Detailed,
        );
        assert_eq!(evaluated[0].status, ObligationStatus::SatisfiedBy(11));
    }

    #[test]
    fn assembly_obligation_presence_only_ignores_member_state() {
        let mut world = World::new();
        spawn_member(&mut world, 11, RefinementState::Conceptual);
        // tracks = false: mere presence in the role satisfies it.
        let templates = vec![member_template(
            "needs_wall",
            "exterior_wall",
            RefinementState::Schematic,
            false,
        )];
        let evaluated = evaluate_assembly_member_obligations(
            &world,
            &house_with(vec![("exterior_wall", 11)]),
            &templates,
            RefinementState::Detailed,
        );
        assert_eq!(evaluated[0].status, ObligationStatus::SatisfiedBy(11));
    }

    // --- Change 3 & 4: re-promote merges obligations + gate ---

    /// Builds a world with the command-pipeline resources needed by promote/demote.
    fn obligation_gate_world() -> World {
        let mut world = refinement_command_world();
        // Register CapabilityRegistry with one element class that has one
        // domain-neutral obligation required by Schematic. No recipe families.
        use crate::capability_registry::{
            CapabilityRegistry, ElementClassDescriptor, ElementClassId, ObligationTemplate,
        };
        let mut registry = CapabilityRegistry::default();
        let mut class_min_obligations = HashMap::new();
        class_min_obligations.insert(
            RefinementState::Schematic,
            vec![ObligationTemplate {
                id: ObligationId("load_path".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Schematic,
            }],
        );
        registry.register_element_class(ElementClassDescriptor {
            id: ElementClassId("synthetic_element".into()),
            label: "Synthetic Element".into(),
            description: "Domain-neutral test class".into(),
            semantic_roles: Vec::new(),
            class_min_obligations,
            class_min_promotion_critical_paths: HashMap::new(),
            parameter_schema: serde_json::Value::Null,
        });
        world.insert_resource(registry);
        world
    }

    /// Spawn an entity with an ElementClassAssignment pointing at the
    /// `synthetic_element` class, starting at Conceptual.
    fn spawn_synthetic_entity(world: &mut World, eid: u64) -> Entity {
        use crate::capability_registry::{ElementClassAssignment, ElementClassId};
        world
            .spawn((
                ElementId(eid),
                RefinementStateComponent {
                    state: RefinementState::Conceptual,
                },
                ElementClassAssignment {
                    element_class: ElementClassId("synthetic_element".into()),
                    active_recipe: None,
                },
            ))
            .id()
    }

    #[test]
    fn promote_blocked_when_obligation_unresolved() {
        let mut world = obligation_gate_world();
        spawn_synthetic_entity(&mut world, 100);

        // Attempting to promote to Schematic must fail because `load_path`
        // is required_by_state = Schematic and is still Unresolved.
        let err = apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 100,
                target_state: RefinementState::Schematic,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .unwrap_err();
        assert!(
            err.contains("unsatisfied obligation"),
            "expected gate error, got: {err}"
        );
        assert!(err.contains("load_path"), "error should name the obligation: {err}");
    }

    #[test]
    fn promote_allowed_after_obligation_deferred() {
        let mut world = obligation_gate_world();
        let entity = spawn_synthetic_entity(&mut world, 101);

        // Manually install an ObligationSet with `load_path` marked Deferred.
        world.entity_mut(entity).insert(ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("load_path".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Deferred("pending structural engineer review".into()),
            }],
        });

        let state = apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 101,
                target_state: RefinementState::Schematic,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .expect("promotion should succeed with Deferred obligation");
        assert_eq!(state, RefinementState::Schematic);
    }

    #[test]
    fn promote_allowed_after_obligation_waived() {
        let mut world = obligation_gate_world();
        let entity = spawn_synthetic_entity(&mut world, 102);

        world.entity_mut(entity).insert(ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("load_path".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::Waived("out of scope for this phase".into()),
            }],
        });

        let state = apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 102,
                target_state: RefinementState::Schematic,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .expect("promotion should succeed with Waived obligation");
        assert_eq!(state, RefinementState::Schematic);
    }

    #[test]
    fn promote_allowed_after_obligation_satisfied_by() {
        let mut world = obligation_gate_world();
        let entity = spawn_synthetic_entity(&mut world, 103);
        // A child entity that "satisfies" the load_path obligation.
        world.spawn((ElementId(999),));

        world.entity_mut(entity).insert(ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("load_path".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::SatisfiedBy(999),
            }],
        });

        let state = apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 103,
                target_state: RefinementState::Schematic,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .expect("promotion should succeed with SatisfiedBy obligation");
        assert_eq!(state, RefinementState::Schematic);
    }

    #[test]
    fn re_promote_preserves_prior_satisfied_by_status() {
        // Change 3: re-promoting should NOT clobber a prior SatisfiedBy status.
        let mut world = obligation_gate_world();
        let entity = spawn_synthetic_entity(&mut world, 104);

        // First promotion: manually pre-seed SatisfiedBy so it passes the gate.
        world.entity_mut(entity).insert(ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("load_path".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::SatisfiedBy(888),
            }],
        });

        apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 104,
                target_state: RefinementState::Schematic,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .expect("first promotion should succeed");

        // Demote back to Conceptual.
        apply_demote_refinement(
            &mut world,
            DemoteRefinementRequest {
                entity_element_id: 104,
                target_state: RefinementState::Conceptual,
            },
        )
        .expect("demote should succeed");

        // Re-seed the obligation before the second promote so the gate passes.
        // (The ObligationSet may have been cleared by demotion — check that
        // re-promote re-installs with the *merged* status from the component.)
        world.entity_mut(entity).insert(ObligationSet {
            entries: vec![Obligation {
                id: ObligationId("load_path".into()),
                role: SemanticRole("primary_structure".into()),
                required_by_state: RefinementState::Schematic,
                status: ObligationStatus::SatisfiedBy(888),
            }],
        });

        // Re-promote: the SatisfiedBy(888) status must survive the re-materialize.
        apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 104,
                target_state: RefinementState::Schematic,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .expect("re-promotion should succeed");

        // Verify the SatisfiedBy survived the merge.
        let obligation_set = world.get::<ObligationSet>(entity).expect("ObligationSet present");
        let ob = obligation_set
            .entries
            .iter()
            .find(|o| o.id.0 == "load_path")
            .expect("load_path obligation present");
        assert_eq!(
            ob.status,
            ObligationStatus::SatisfiedBy(888),
            "SatisfiedBy status must survive re-promote (Change 3 merge)"
        );
    }

    #[test]
    fn obligation_not_in_force_does_not_block_lower_promote() {
        // A class where full_spec is required by Constructible (not Schematic).
        // Promoting to Schematic must NOT be blocked by an unresolved Constructible-level obligation.
        use crate::capability_registry::{
            CapabilityRegistry, ElementClassDescriptor, ElementClassAssignment, ElementClassId,
            ObligationTemplate,
        };
        let mut world = refinement_command_world();
        let mut registry = CapabilityRegistry::default();
        let mut class_min_obligations = HashMap::new();
        class_min_obligations.insert(
            RefinementState::Constructible,
            vec![ObligationTemplate {
                id: ObligationId("full_spec".into()),
                role: SemanticRole("documentation".into()),
                required_by_state: RefinementState::Constructible,
            }],
        );
        registry.register_element_class(ElementClassDescriptor {
            id: ElementClassId("future_class".into()),
            label: "Future Class".into(),
            description: "Domain-neutral test class with late obligation".into(),
            semantic_roles: Vec::new(),
            class_min_obligations,
            class_min_promotion_critical_paths: HashMap::new(),
            parameter_schema: serde_json::Value::Null,
        });
        world.insert_resource(registry);

        world.spawn((
            ElementId(200),
            RefinementStateComponent {
                state: RefinementState::Conceptual,
            },
            crate::capability_registry::ElementClassAssignment {
                element_class: ElementClassId("future_class".into()),
                active_recipe: None,
            },
        ));

        // Promoting to Schematic: the Constructible-level obligation is NOT in force.
        let state = apply_promote_refinement(
            &mut world,
            PromoteRefinementRequest {
                entity_element_id: 200,
                target_state: RefinementState::Schematic,
                recipe_id: None,
                overrides: HashMap::new(),
            },
        )
        .expect("Schematic promotion must not be blocked by Constructible obligation");
        assert_eq!(state, RefinementState::Schematic);
    }
}
