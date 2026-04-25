//! `AssumptionLog` — core explanation state per ADR-042 §11.
//!
//! `AssumptionLog` records the design decisions an agent took on a
//! top-level intent (building, storey, system) so the user-facing brief
//! can be regenerated deterministically. It is **distinct from
//! provenance**:
//!
//! - Provenance answers: *where did this entity, claim, or curated
//!   asset come from?* (lineage, evidence, sources)
//! - AssumptionLog answers: *what design decision did the agent make,
//!   why, and can it still be changed?*
//!
//! The brief restatement is a deterministic projection of the log
//! plus open obligations and findings. Per ADR-042 the brief is **not**
//! regenerated from prompt history; it is a projection of structured
//! state.
//!
//! This module owns the data shape and the projection. Integration
//! with `ObligationSet` and findings is left to the surrounding
//! refinement/curation plumbing.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::curation::AssetId;
use crate::plugins::identity::ElementId;
use crate::plugins::refinement::{AgentId, ClaimPath};

/// Anchor describing what kind of top-level intent an `AssumptionLog`
/// is attached to. Keeps the data shape inspectable without taking on
/// architecture-specific names like "building" or "system" as enum
/// variants in core.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "anchor", rename_all = "snake_case")]
pub enum AssumptionAnchor {
    /// Whole-project intent. The element id of a project root entity
    /// when present, otherwise the log is associated with the
    /// `DocumentState` resource.
    Project { root_element: Option<ElementId> },
    /// A discipline-specific top-level intent — `kind` is a free
    /// string the capability crate chooses (e.g. `"building"`,
    /// `"storey"`, `"system"`, `"hull"`, `"powertrain"`).
    Domain {
        kind: String,
        element_id: ElementId,
    },
    /// Anchored at a single element regardless of role. Useful when an
    /// assumption applies only to one entity.
    Element { element_id: ElementId },
}

impl AssumptionAnchor {
    pub fn project_root(root: ElementId) -> Self {
        Self::Project {
            root_element: Some(root),
        }
    }

    pub fn project_default() -> Self {
        Self::Project { root_element: None }
    }

    pub fn domain(kind: impl Into<String>, element_id: ElementId) -> Self {
        Self::Domain {
            kind: kind.into(),
            element_id,
        }
    }
}

/// Where a chosen value came from. Used by the projection to phrase
/// the assumption in plain language ("the agent assumed", "you
/// chose", "the BBR rule prescribes", etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AssumptionDefaultSource {
    /// User explicitly provided this value (or locked it).
    User { agent: AgentId },
    /// A registered generation prior chose this value.
    PriorRef { prior_id: String },
    /// The recipe's own default applied because no prior or rule
    /// argued otherwise.
    RecipeDefault {
        recipe_id: AssetId,
        parameter: String,
    },
    /// A vocabulary disambiguation flow resolved a concept this way.
    ConceptResolution { concept_ref: String },
    /// Pure LLM heuristic with rationale; should never remain in this
    /// state on a promotion-critical path past the constructible
    /// boundary unless explicitly waived.
    LLMHeuristic { rationale: String },
}

/// One structured assumption. Free-form `kind` lets capability crates
/// label assumption families (`"truss_variant"`, `"foundation_choice"`,
/// `"hull_form"`) without hard-coding them in core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssumptionEntry {
    pub kind: String,
    /// Optional vocabulary concept this assumption resolves
    /// (`"roof.truss.attic"`, `"foundation.slab_on_grade"`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_ref: Option<String>,
    /// Optional element this assumption applies to. When `None`, the
    /// assumption applies to the anchor itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<ElementId>,
    pub chosen_value: Value,
    /// Other values the agent considered, in plain JSON form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives_considered: Vec<Value>,
    pub default_source: AssumptionDefaultSource,
    /// Optional pointer back into `ClaimGrounding` for more detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_ref: Option<ClaimPath>,
    /// `true` if this choice can still be revisited at the current
    /// refinement state.
    pub reversible: bool,
    /// `true` if the user locked this choice — projection language
    /// shifts from "the agent assumed" to "you chose".
    pub user_locked: bool,
    /// Plain-language brief line. Kept as part of the entry (not
    /// regenerated) so the projection is stable across model edits.
    pub plain_language_summary: String,
}

impl AssumptionEntry {
    pub fn new(
        kind: impl Into<String>,
        chosen_value: Value,
        default_source: AssumptionDefaultSource,
        plain_language_summary: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            concept_ref: None,
            subject_ref: None,
            chosen_value,
            alternatives_considered: Vec::new(),
            default_source,
            grounding_ref: None,
            reversible: true,
            user_locked: false,
            plain_language_summary: plain_language_summary.into(),
        }
    }

    pub fn with_concept(mut self, concept: impl Into<String>) -> Self {
        self.concept_ref = Some(concept.into());
        self
    }

    pub fn with_subject(mut self, element_id: ElementId) -> Self {
        self.subject_ref = Some(element_id);
        self
    }

    pub fn with_alternatives(mut self, alts: Vec<Value>) -> Self {
        self.alternatives_considered = alts;
        self
    }

    pub fn with_grounding_ref(mut self, claim_path: ClaimPath) -> Self {
        self.grounding_ref = Some(claim_path);
        self
    }

    pub fn user_locked(mut self) -> Self {
        self.user_locked = true;
        self
    }

    pub fn irreversible(mut self) -> Self {
        self.reversible = false;
        self
    }
}

/// Bevy component holding the structured assumption log for a single
/// anchored intent.
///
/// Per ADR-042 the log is anchored at building/storey/system level (or
/// at the project root). The component is attached to the entity that
/// represents that intent.
#[derive(Component, Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AssumptionLog {
    pub anchor: Option<AssumptionAnchor>,
    pub entries: Vec<AssumptionEntry>,
}

impl AssumptionLog {
    pub fn new(anchor: AssumptionAnchor) -> Self {
        Self {
            anchor: Some(anchor),
            entries: Vec::new(),
        }
    }

    pub fn record(&mut self, entry: AssumptionEntry) {
        self.entries.push(entry);
    }

    pub fn lock(&mut self, kind: &str) -> bool {
        let mut any = false;
        for entry in self.entries.iter_mut() {
            if entry.kind == kind {
                entry.user_locked = true;
                any = true;
            }
        }
        any
    }

    pub fn entries_by_kind<'a>(
        &'a self,
        kind: &'a str,
    ) -> impl Iterator<Item = &'a AssumptionEntry> + 'a {
        self.entries.iter().filter(move |e| e.kind == kind)
    }

    /// Projection: deterministic plain-language brief lines, in the
    /// order the assumptions were recorded. Each entry produces one
    /// line. The phrasing reflects `default_source` and `user_locked`:
    ///
    /// - `User`: `"You chose ..."`
    /// - `LLMHeuristic` not user-locked: `"Assumed (heuristic) ..."`
    /// - `PriorRef` not user-locked: `"Defaulted to ... per prior ..."`
    /// - `RecipeDefault`: `"Recipe default: ..."`
    /// - `ConceptResolution`: `"Resolved ... to ..."`
    ///
    /// The trailing `(locked)` marker is appended when `user_locked`,
    /// independent of the source — it tells the user the assumption
    /// will not be revisited automatically.
    pub fn project_brief(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| project_entry(e))
            .collect()
    }

    /// Projection grouped by `kind`, useful for findings panels that
    /// surface every truss-variant assumption together, every
    /// foundation-choice assumption together, etc.
    pub fn project_brief_by_kind(&self) -> Vec<(String, Vec<String>)> {
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for entry in &self.entries {
            let line = project_entry(entry);
            if let Some(existing) = groups.iter_mut().find(|(k, _)| k == &entry.kind) {
                existing.1.push(line);
            } else {
                groups.push((entry.kind.clone(), vec![line]));
            }
        }
        groups
    }

    /// Number of entries that still depend on `LLMHeuristic` and are
    /// not user-locked. The agent-freedom rule (ADR-042 §12) asks that
    /// no promotion-critical claim remain `LLMHeuristic` past the
    /// constructible boundary; this counter is the projection-side
    /// signal of remaining heuristic assumptions.
    pub fn unresolved_heuristic_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| {
                matches!(e.default_source, AssumptionDefaultSource::LLMHeuristic { .. })
                    && !e.user_locked
            })
            .count()
    }
}

fn project_entry(entry: &AssumptionEntry) -> String {
    let lead = match &entry.default_source {
        AssumptionDefaultSource::User { .. } => "You chose",
        AssumptionDefaultSource::PriorRef { .. } => "Defaulted",
        AssumptionDefaultSource::RecipeDefault { .. } => "Recipe default",
        AssumptionDefaultSource::ConceptResolution { .. } => "Resolved concept",
        AssumptionDefaultSource::LLMHeuristic { .. } => "Assumed (heuristic)",
    };
    let body = if entry.plain_language_summary.is_empty() {
        format!(
            "{}: {} = {}",
            lead, entry.kind, entry.chosen_value
        )
    } else {
        format!("{}: {}", lead, entry.plain_language_summary)
    };
    if entry.user_locked {
        format!("{} (locked)", body)
    } else if !entry.reversible {
        format!("{} (irreversible at this state)", body)
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_entry(kind: &str, chosen: Value, summary: &str) -> AssumptionEntry {
        AssumptionEntry::new(
            kind,
            chosen,
            AssumptionDefaultSource::User {
                agent: AgentId("user".into()),
            },
            summary,
        )
    }

    fn heuristic_entry(kind: &str, chosen: Value, summary: &str) -> AssumptionEntry {
        AssumptionEntry::new(
            kind,
            chosen,
            AssumptionDefaultSource::LLMHeuristic {
                rationale: "no prior available".into(),
            },
            summary,
        )
    }

    #[test]
    fn empty_log_projects_to_empty_brief() {
        let log = AssumptionLog::default();
        assert!(log.project_brief().is_empty());
        assert_eq!(log.unresolved_heuristic_count(), 0);
    }

    #[test]
    fn user_choice_renders_as_you_chose() {
        let mut log = AssumptionLog::new(AssumptionAnchor::project_default());
        log.record(user_entry(
            "truss_variant",
            Value::String("storage".into()),
            "attic truss configured for storage use (no living space)",
        ));
        let brief = log.project_brief();
        assert_eq!(brief.len(), 1);
        assert!(brief[0].starts_with("You chose:"));
        assert!(brief[0].contains("storage"));
    }

    #[test]
    fn heuristic_assumption_is_marked_and_counted() {
        let mut log = AssumptionLog::new(AssumptionAnchor::project_default());
        log.record(heuristic_entry(
            "truss_variant",
            Value::String("storage".into()),
            "attic truss assumed to be storage variant",
        ));
        let brief = log.project_brief();
        assert_eq!(brief.len(), 1);
        assert!(brief[0].starts_with("Assumed (heuristic):"));
        assert_eq!(log.unresolved_heuristic_count(), 1);
    }

    #[test]
    fn user_lock_clears_heuristic_count() {
        let mut log = AssumptionLog::new(AssumptionAnchor::project_default());
        log.record(heuristic_entry(
            "truss_variant",
            Value::String("storage".into()),
            "attic truss assumed to be storage variant",
        ));
        assert_eq!(log.unresolved_heuristic_count(), 1);
        log.lock("truss_variant");
        assert_eq!(log.unresolved_heuristic_count(), 0);
        let brief = log.project_brief();
        assert!(brief[0].contains("(locked)"));
    }

    #[test]
    fn irreversible_marker_appears_in_brief() {
        let mut log = AssumptionLog::new(AssumptionAnchor::project_default());
        let entry = AssumptionEntry::new(
            "foundation_choice",
            Value::String("slab_on_grade".into()),
            AssumptionDefaultSource::PriorRef {
                prior_id: "se.boverket.foundation.slab_on_grade.v1".into(),
            },
            "slab-on-grade foundation per Swedish small-house defaults",
        )
        .irreversible();
        log.record(entry);
        let brief = log.project_brief();
        assert_eq!(brief.len(), 1);
        assert!(brief[0].contains("(irreversible at this state)"));
    }

    #[test]
    fn entries_by_kind_filters() {
        let mut log = AssumptionLog::new(AssumptionAnchor::project_default());
        log.record(user_entry("truss_variant", Value::String("a".into()), "a"));
        log.record(user_entry("truss_variant", Value::String("b".into()), "b"));
        log.record(user_entry(
            "foundation_choice",
            Value::String("slab".into()),
            "slab",
        ));
        let trusses: Vec<&AssumptionEntry> = log.entries_by_kind("truss_variant").collect();
        assert_eq!(trusses.len(), 2);
        let foundations: Vec<&AssumptionEntry> = log.entries_by_kind("foundation_choice").collect();
        assert_eq!(foundations.len(), 1);
    }

    #[test]
    fn project_brief_by_kind_groups_in_record_order() {
        let mut log = AssumptionLog::new(AssumptionAnchor::project_default());
        log.record(user_entry("truss_variant", Value::String("a".into()), "first"));
        log.record(user_entry(
            "foundation_choice",
            Value::String("slab".into()),
            "slab",
        ));
        log.record(user_entry("truss_variant", Value::String("b".into()), "second"));
        let groups = log.project_brief_by_kind();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "truss_variant");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, "foundation_choice");
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn anchor_round_trips_through_json() {
        for anchor in [
            AssumptionAnchor::project_default(),
            AssumptionAnchor::project_root(ElementId(42)),
            AssumptionAnchor::domain("building", ElementId(7)),
            AssumptionAnchor::Element {
                element_id: ElementId(99),
            },
        ] {
            let json = serde_json::to_string(&anchor).unwrap();
            let parsed: AssumptionAnchor = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, anchor);
        }
    }

    #[test]
    fn entry_round_trips_through_json() {
        let entry = AssumptionEntry::new(
            "truss_variant",
            Value::String("storage".into()),
            AssumptionDefaultSource::ConceptResolution {
                concept_ref: "roof.truss.attic".into(),
            },
            "attic truss resolved to storage variant by default",
        )
        .with_concept("roof.truss.attic")
        .with_subject(ElementId(101))
        .with_alternatives(vec![Value::String("room".into())])
        .with_grounding_ref(ClaimPath("truss/variant".into()))
        .user_locked();
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: AssumptionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn log_round_trips_through_json() {
        let mut log = AssumptionLog::new(AssumptionAnchor::domain("building", ElementId(1)));
        log.record(user_entry("truss_variant", Value::String("storage".into()), "storage"));
        log.record(heuristic_entry(
            "ridge_height_mm",
            Value::Number(7800.into()),
            "ridge height assumed at 7.8m to keep eave at 2.4m",
        ));
        let json = serde_json::to_string(&log).unwrap();
        let parsed: AssumptionLog = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, log);
    }

    #[test]
    fn projection_uses_summary_when_present_and_falls_back_otherwise() {
        let mut log = AssumptionLog::default();
        let with_summary = user_entry("a", Value::Bool(true), "with summary");
        let mut without_summary = user_entry("b", Value::Bool(false), "");
        without_summary.plain_language_summary.clear();
        log.record(with_summary);
        log.record(without_summary);
        let brief = log.project_brief();
        assert!(brief[0].contains("with summary"));
        assert!(brief[1].contains("b ="));
        assert!(brief[1].contains("false"));
    }
}
