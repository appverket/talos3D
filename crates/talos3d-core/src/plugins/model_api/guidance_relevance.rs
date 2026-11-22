//! Task relevance for the onboarding must-read set.
//!
//! Selection is declarative. A guidance card or agent skill states where it
//! applies through its existing `task_tags` and the segments of its id. The
//! negotiated task text (plus any element classes the client named) is
//! tokenized, expanded one step through the domain-registered
//! [`DiscoveryAliases`], and matched against those declarations. Core holds no
//! domain vocabulary: every word that decides relevance comes from installed
//! data.
//!
//! The defaults are conservative:
//! - no task (or a task with no usable terms) selects everything;
//! - an item that declares no tags applies to every task;
//! - a tag naming the active guidance's own namespace (for example the domain
//!   prefix of `<domain>.component_structure`) applies to every task served by
//!   that composition.
//!
//! Items that are not selected are not dropped: callers list them as optional
//! resources with their retrieval tool.

use std::collections::{BTreeSet, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::plugins::authoring_guidance::{normalize_discovery_term, DiscoveryAliases};

/// What the session negotiated as its task. It is the only input to relevance
/// besides installed data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuidanceTaskContext {
    pub task: Option<String>,
    pub element_classes: Vec<String>,
}

/// How an item relates to the negotiated task. Ordered from least to most
/// relevant, so sorting descending puts the strongest matches first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Relevance {
    /// Declares task tags and none of them, nor its id, applies to the task.
    Unmatched,
    /// Declares no task tags, so it applies to every task.
    Untagged,
    /// Number of distinct declared terms that the task matched.
    Matched(usize),
}

impl Relevance {
    pub(crate) fn selected(self) -> bool {
        self != Relevance::Unmatched
    }
}

/// Why a resource that exists is not in the required set.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OptionalReason {
    /// Its declared tags do not apply to the negotiated task.
    NotSelectedForTask,
    /// Relevant, but past the bounded bootstrap budget; ranked lower than
    /// every required item of the same kind.
    BootstrapBudget,
}

/// Result of selecting a required set from candidates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    /// Required ids, most relevant first.
    pub required: Vec<String>,
    /// Ids left out of the required set, with the reason, sorted by id.
    pub optional: Vec<(String, OptionalReason)>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TaskRelevance {
    task_terms: HashSet<String>,
    universal_terms: HashSet<String>,
}

impl TaskRelevance {
    /// Returns `None` when the context carries no usable terms; callers then
    /// keep every candidate required.
    pub(crate) fn new<'a>(
        context: &GuidanceTaskContext,
        aliases: Option<&DiscoveryAliases>,
        universal_terms: impl IntoIterator<Item = &'a str>,
    ) -> Option<Self> {
        let tokens = context
            .task
            .as_deref()
            .unwrap_or_default()
            .split(|character: char| !character.is_alphanumeric())
            .map(normalize_discovery_term)
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        let mut terms = HashSet::new();
        // Adjacent-token n-grams match multi-word tags and ids such as
        // `surface placement` or `wall_assembly` without a phrase list.
        for width in 1..=3 {
            for window in tokens.windows(width) {
                terms.insert(window.concat());
            }
        }
        terms.extend(
            context
                .element_classes
                .iter()
                .map(|class| normalize_discovery_term(class)),
        );
        terms.retain(|term| term.len() >= 3);
        if terms.is_empty() {
            return None;
        }
        // One expansion step: a registered term and its aliases are one
        // equivalence group. It is not transitive, so two groups that share an
        // alias do not pull each other in.
        if let Some(aliases) = aliases {
            let expanded = aliases
                .entries()
                .map(|(term, synonyms)| {
                    std::iter::once(term)
                        .chain(synonyms.iter().map(String::as_str))
                        .map(str::to_string)
                        .collect::<BTreeSet<_>>()
                })
                .filter(|group| group.iter().any(|member| terms.contains(member)))
                .flatten()
                .collect::<Vec<_>>();
            terms.extend(expanded);
        }
        Some(Self {
            task_terms: terms,
            universal_terms: universal_terms
                .into_iter()
                .map(normalize_discovery_term)
                .filter(|term| !term.is_empty())
                .collect(),
        })
    }

    pub(crate) fn relevance(&self, id: &str, tags: &[String]) -> Relevance {
        if tags.is_empty() {
            return Relevance::Untagged;
        }
        let mut matched = tags
            .iter()
            .map(|tag| normalize_discovery_term(tag))
            .filter(|tag| self.task_terms.contains(tag) || self.universal_terms.contains(tag))
            .collect::<BTreeSet<_>>();
        matched.extend(
            id.split(['.', ':', '/'])
                .map(normalize_discovery_term)
                .filter(|segment| self.task_terms.contains(segment)),
        );
        match matched.len() {
            0 => Relevance::Unmatched,
            count => Relevance::Matched(count),
        }
    }
}

/// Split candidates into a required set (relevance-ordered, at most `limit`)
/// and an optional remainder. With no relevance (no task) every candidate is
/// untagged-equivalent: the required set is the first `limit` ids in their
/// given order, which keeps the pre-selection behaviour for that case.
pub(crate) fn select_required<'a>(
    relevance: Option<&TaskRelevance>,
    candidates: impl IntoIterator<Item = (&'a str, &'a [String])>,
    limit: usize,
) -> Selection {
    let mut ranked = Vec::new();
    let mut optional = Vec::new();
    for (index, (id, tags)) in candidates.into_iter().enumerate() {
        let score = relevance.map_or(Relevance::Untagged, |relevance| {
            relevance.relevance(id, tags)
        });
        if score.selected() {
            ranked.push((score, index, id.to_string()));
        } else {
            optional.push((id.to_string(), OptionalReason::NotSelectedForTask));
        }
    }
    // Stable on the given order within one relevance class.
    ranked.sort_by(|(left, left_index, _), (right, right_index, _)| {
        right.cmp(left).then(left_index.cmp(right_index))
    });
    let overflow = ranked.split_off(limit.min(ranked.len()));
    optional.extend(
        overflow
            .into_iter()
            .map(|(_, _, id)| (id, OptionalReason::BootstrapBudget)),
    );
    optional.sort();
    Selection {
        required: ranked.into_iter().map(|(_, _, id)| id).collect(),
        optional,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn aliases() -> DiscoveryAliases {
        let mut aliases = DiscoveryAliases::default();
        aliases.register("alpha_system", &["alpha", "apex"]);
        aliases.register("beta_assembly", &["beta", "shared"]);
        aliases.register("gamma", &["gamma", "shared"]);
        aliases
    }

    fn for_task(task: &str) -> TaskRelevance {
        TaskRelevance::new(
            &GuidanceTaskContext {
                task: Some(task.into()),
                element_classes: Vec::new(),
            },
            Some(&aliases()),
            ["domainx"],
        )
        .expect("task has terms")
    }

    #[test]
    fn no_task_or_termless_task_has_no_relevance() {
        assert!(
            TaskRelevance::new(&GuidanceTaskContext::default(), Some(&aliases()), []).is_none()
        );
        let termless = GuidanceTaskContext {
            task: Some("a b !!".into()),
            element_classes: Vec::new(),
        };
        assert!(TaskRelevance::new(&termless, None, []).is_none());
    }

    #[test]
    fn alias_expansion_selects_class_tagged_items_one_step_only() {
        let relevance = for_task("make an apex thing");
        assert_eq!(
            relevance.relevance("x", &tags(&["alpha_system"])),
            Relevance::Matched(1)
        );
        assert_eq!(
            relevance.relevance("x", &tags(&["alpha"])),
            Relevance::Matched(1)
        );
        assert_eq!(
            relevance.relevance("x", &tags(&["beta"])),
            Relevance::Unmatched
        );
        // `shared` belongs to two groups; naming it pulls in both groups but
        // neither group pulls in the other's other members transitively.
        let shared = for_task("shared");
        assert!(shared.relevance("x", &tags(&["beta_assembly"])).selected());
        assert!(shared.relevance("x", &tags(&["gamma"])).selected());
        assert_eq!(
            shared.relevance("x", &tags(&["alpha"])),
            Relevance::Unmatched
        );
    }

    #[test]
    fn multi_word_tags_ids_and_named_classes_match() {
        let relevance = for_task("place on a surface placement grid");
        assert!(relevance
            .relevance("x", &tags(&["surface placement"]))
            .selected());
        let by_id = relevance.relevance("pkg.grid", &tags(&["unrelated"]));
        assert_eq!(by_id, Relevance::Matched(1));
        let classes = TaskRelevance::new(
            &GuidanceTaskContext {
                task: None,
                element_classes: vec!["beta_assembly".into()],
            },
            Some(&aliases()),
            [],
        )
        .expect("named classes are terms");
        assert!(classes.relevance("x", &tags(&["beta"])).selected());
    }

    #[test]
    fn untagged_and_namespace_tagged_items_stay_selected() {
        let relevance = for_task("make an apex thing");
        assert_eq!(relevance.relevance("x", &[]), Relevance::Untagged);
        assert_eq!(
            relevance.relevance("x", &tags(&["DomainX", "curation"])),
            Relevance::Matched(1)
        );
    }

    #[test]
    fn selection_orders_by_relevance_and_lists_the_rest_as_optional() {
        let relevance = for_task("apex alpha beta");
        let a = tags(&["unrelated"]);
        let b = tags(&["alpha"]);
        let c: Vec<String> = Vec::new();
        let d = tags(&["alpha", "beta"]);
        let e = tags(&["beta"]);
        let candidates = [
            ("a", a.as_slice()),
            ("b", b.as_slice()),
            ("c", c.as_slice()),
            ("d", d.as_slice()),
            ("e", e.as_slice()),
        ];
        let selection = select_required(Some(&relevance), candidates, 3);
        assert_eq!(selection.required, vec!["d", "b", "e"]);
        assert_eq!(
            selection.optional,
            vec![
                ("a".to_string(), OptionalReason::NotSelectedForTask),
                ("c".to_string(), OptionalReason::BootstrapBudget),
            ]
        );
        // Without a task every candidate is required, in the given order, up
        // to the budget; the remainder is listed, never dropped.
        let unscoped = select_required(None, candidates, 4);
        assert_eq!(unscoped.required, vec!["a", "b", "c", "d"]);
        assert_eq!(
            unscoped.optional,
            vec![("e".to_string(), OptionalReason::BootstrapBudget)]
        );
    }
}
