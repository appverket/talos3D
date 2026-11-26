//! Agent-operating-procedure knowledge exposed through the MCP surface.
//!
//! Agent skills are not recipes, validators, or Definition libraries. They are
//! bounded procedures that tell an MCP client how to acquire evidence, choose
//! Talos3D representations, and validate work before claiming completion.

use std::collections::BTreeMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Host-owned connection. Credentials never appear in tool arguments or results.
#[derive(Clone, Default, Resource)]
pub struct AgentCorpusConnection {
    pub base_url: String,
    pub bearer_token: Option<String>,
    pub trusted_public_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentSkillId(pub String);

impl AgentSkillId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentSkillId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AgentSkillTrustLevel {
    Shipped,
    ProjectDraft,
    #[default]
    SessionDraft,
}

impl AgentSkillTrustLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shipped => "shipped",
            Self::ProjectDraft => "project_draft",
            Self::SessionDraft => "session_draft",
        }
    }
}

/// Task tag marking compiled domain knowledge exposed only so the curator can
/// migrate it to the shared corpus. Such skills are hidden from ordinary agent
/// sessions; the curator lists them by searching for this tag.
pub const MIGRATION_CANDIDATE_TAG: &str = "migration_candidate";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentSkill {
    pub id: AgentSkillId,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub task_tags: Vec<String>,
    /// Whether every session connected to an app that installs this skill must
    /// read it before authoring. Keep this false for ordinary task-ranked
    /// skills; use it for safety-critical substrate contracts that the live
    /// authoring guidance names as mandatory.
    #[serde(default)]
    pub must_read_at_bootstrap: bool,
    #[serde(default)]
    pub referenced_tool_ids: Vec<String>,
    #[serde(default)]
    pub required_tool_ids: Vec<String>,
    #[serde(default)]
    pub forbidden_tool_ids: Vec<String>,
    #[serde(default)]
    pub validation_tool_ids: Vec<String>,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    #[serde(default)]
    pub stop_conditions: Vec<String>,
    #[serde(default)]
    pub screenshot_requirements: Vec<String>,
    #[serde(default)]
    pub common_failure_modes: Vec<String>,
    #[serde(default)]
    pub regression_prompt_ids: Vec<String>,
    #[serde(default)]
    pub next_skill_ids: Vec<AgentSkillId>,
    pub body_markdown: String,
    #[serde(default)]
    pub trust_level: AgentSkillTrustLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

impl AgentSkill {
    pub fn summary_info(&self) -> AgentSkillSummary {
        AgentSkillSummary {
            id: self.id.0.clone(),
            title: self.title.clone(),
            summary: self.summary.clone(),
            task_tags: self.task_tags.clone(),
            must_read_at_bootstrap: self.must_read_at_bootstrap,
            referenced_tool_ids: self.referenced_tool_ids.clone(),
            required_tool_ids: self.required_tool_ids.clone(),
            forbidden_tool_ids: self.forbidden_tool_ids.clone(),
            validation_tool_ids: self.validation_tool_ids.clone(),
            success_criteria: self.success_criteria.clone(),
            stop_conditions: self.stop_conditions.clone(),
            trust_level: self.trust_level.as_str().to_string(),
            source_path: self.source_path.clone(),
        }
    }

    /// Whether this is a compiled migration candidate (see [`MIGRATION_CANDIDATE_TAG`]).
    pub fn is_migration_candidate(&self) -> bool {
        self.task_tags.iter().any(|t| t == MIGRATION_CANDIDATE_TAG)
    }

    pub(crate) fn matches(&self, query: Option<&str>, tags: &[String]) -> bool {
        let query_matches = query
            .map(|query| {
                let query = query.to_ascii_lowercase();
                self.id.0.to_ascii_lowercase().contains(&query)
                    || self.title.to_ascii_lowercase().contains(&query)
                    || self.summary.to_ascii_lowercase().contains(&query)
                    || self
                        .task_tags
                        .iter()
                        .any(|tag| tag.to_ascii_lowercase().contains(&query))
            })
            .unwrap_or(true);
        let tags_match = tags.iter().all(|wanted| {
            self.task_tags
                .iter()
                .any(|tag| tag.eq_ignore_ascii_case(wanted))
        });
        query_matches && tags_match
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentSkillSummary {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub task_tags: Vec<String>,
    pub must_read_at_bootstrap: bool,
    pub referenced_tool_ids: Vec<String>,
    pub required_tool_ids: Vec<String>,
    pub forbidden_tool_ids: Vec<String>,
    pub validation_tool_ids: Vec<String>,
    pub success_criteria: Vec<String>,
    pub stop_conditions: Vec<String>,
    pub trust_level: String,
    pub source_path: Option<String>,
}

/// Tools a reference record needs to retrieve its cited passages.
const PASSAGE_RETRIEVAL_TOOLS: [&str; 2] = ["search_corpus_knowledge", "lookup_source_passage"];

impl AgentSkillSummary {
    /// A reference-passage pointer, not an operating procedure: it names no
    /// tools to act with or validate by, and only needs passage retrieval.
    /// Its passages are served through `search_corpus_knowledge`, so listing
    /// and recommending it as an agent skill only adds discovery noise.
    /// Structural classification; no domain vocabulary. A bootstrap-mandatory
    /// record is never classified away.
    pub fn is_reference_pointer(&self) -> bool {
        !self.must_read_at_bootstrap
            && self.referenced_tool_ids.is_empty()
            && self.validation_tool_ids.is_empty()
            && self.success_criteria.is_empty()
            && !self.required_tool_ids.is_empty()
            && self
                .required_tool_ids
                .iter()
                .all(|tool| PASSAGE_RETRIEVAL_TOOLS.contains(&tool.as_str()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentSkillSearch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentSkillDraftRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub task_tags: Vec<String>,
    #[serde(default)]
    pub must_read_at_bootstrap: bool,
    #[serde(default)]
    pub referenced_tool_ids: Vec<String>,
    #[serde(default)]
    pub required_tool_ids: Vec<String>,
    #[serde(default)]
    pub forbidden_tool_ids: Vec<String>,
    #[serde(default)]
    pub validation_tool_ids: Vec<String>,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    #[serde(default)]
    pub stop_conditions: Vec<String>,
    #[serde(default)]
    pub screenshot_requirements: Vec<String>,
    #[serde(default)]
    pub common_failure_modes: Vec<String>,
    #[serde(default)]
    pub regression_prompt_ids: Vec<String>,
    #[serde(default)]
    pub next_skill_ids: Vec<String>,
    pub body_markdown: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

impl AgentSkillDraftRequest {
    pub fn into_skill(self) -> AgentSkill {
        let id = self.id.unwrap_or_else(|| stable_skill_id(&self.title));
        AgentSkill {
            id: AgentSkillId(id),
            title: self.title,
            summary: self.summary,
            task_tags: self.task_tags,
            must_read_at_bootstrap: self.must_read_at_bootstrap,
            referenced_tool_ids: self.referenced_tool_ids,
            required_tool_ids: self.required_tool_ids,
            forbidden_tool_ids: self.forbidden_tool_ids,
            validation_tool_ids: self.validation_tool_ids,
            success_criteria: self.success_criteria,
            stop_conditions: self.stop_conditions,
            screenshot_requirements: self.screenshot_requirements,
            common_failure_modes: self.common_failure_modes,
            regression_prompt_ids: self.regression_prompt_ids,
            next_skill_ids: self.next_skill_ids.into_iter().map(AgentSkillId).collect(),
            body_markdown: self.body_markdown,
            trust_level: AgentSkillTrustLevel::SessionDraft,
            source_path: self.source_path,
        }
    }
}

#[derive(Debug, Clone, Default, Resource)]
pub struct AgentSkillRegistry {
    skills: BTreeMap<AgentSkillId, AgentSkill>,
}

impl AgentSkillRegistry {
    pub fn insert(&mut self, skill: AgentSkill) {
        self.skills.insert(skill.id.clone(), skill);
    }

    pub fn get(&self, id: &AgentSkillId) -> Option<&AgentSkill> {
        self.skills.get(id)
    }

    pub fn private_skills(&self) -> Vec<AgentSkill> {
        self.skills
            .values()
            .filter(|s| s.trust_level != AgentSkillTrustLevel::Shipped)
            .cloned()
            .collect()
    }
    pub fn clear_private(&mut self) {
        self.skills
            .retain(|_, s| s.trust_level == AgentSkillTrustLevel::Shipped);
    }
    pub fn insert_private(&mut self, mut skill: AgentSkill) -> Result<(), String> {
        if self
            .get(&skill.id)
            .is_some_and(|s| s.trust_level == AgentSkillTrustLevel::Shipped)
            || skill.trust_level == AgentSkillTrustLevel::Shipped
        {
            return Err(
                "Private skills cannot acquire shipped trust or replace a shipped skill".into(),
            );
        }
        skill.must_read_at_bootstrap = false;
        skill.trust_level = AgentSkillTrustLevel::ProjectDraft;
        self.insert(skill);
        Ok(())
    }
    /// Skills an ordinary agent session receives: every skill except compiled
    /// migration candidates, which only the curator enumerates.
    pub fn list(&self) -> Vec<&AgentSkill> {
        self.skills
            .values()
            .filter(|skill| !skill.is_migration_candidate())
            .collect()
    }

    /// Search skills. Migration candidates match only when the filter names
    /// [`MIGRATION_CANDIDATE_TAG`], which is how the curator lists them.
    pub fn search(&self, filter: AgentSkillSearch) -> Vec<&AgentSkill> {
        let wants_candidates = filter.tags.iter().any(|t| t == MIGRATION_CANDIDATE_TAG);
        self.skills
            .values()
            .filter(|skill| wants_candidates || !skill.is_migration_candidate())
            .filter(|skill| skill.matches(filter.query.as_deref(), &filter.tags))
            .collect()
    }
}

pub trait AgentSkillAppExt {
    fn register_agent_skill(&mut self, skill: AgentSkill) -> &mut Self;
}

impl AgentSkillAppExt for App {
    fn register_agent_skill(&mut self, skill: AgentSkill) -> &mut Self {
        if !self.world().contains_resource::<AgentSkillRegistry>() {
            self.init_resource::<AgentSkillRegistry>();
        }
        self.world_mut()
            .resource_mut::<AgentSkillRegistry>()
            .insert(skill);
        self
    }
}

fn stable_skill_id(title: &str) -> String {
    let normalized = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '.'
            }
        })
        .collect::<String>();
    let slug = normalized
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".");
    format!("agent_skill.draft.{slug}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_search_matches_query_and_tags() {
        let mut registry = AgentSkillRegistry::default();
        registry.insert(AgentSkill {
            id: AgentSkillId("architecture.skill.window_authoring".into()),
            title: "Window Authoring".into(),
            summary: "Hosted window workflow".into(),
            task_tags: vec!["window".into(), "hosted_component".into()],
            must_read_at_bootstrap: false,
            referenced_tool_ids: vec!["definition.instantiate_hosted".into()],
            required_tool_ids: Vec::new(),
            forbidden_tool_ids: Vec::new(),
            validation_tool_ids: Vec::new(),
            success_criteria: Vec::new(),
            stop_conditions: Vec::new(),
            screenshot_requirements: Vec::new(),
            common_failure_modes: Vec::new(),
            regression_prompt_ids: Vec::new(),
            next_skill_ids: vec![],
            body_markdown: "Use Definitions.".into(),
            trust_level: AgentSkillTrustLevel::Shipped,
            source_path: None,
        });

        let found = registry.search(AgentSkillSearch {
            query: Some("window".into()),
            tags: vec!["hosted_component".into()],
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id.0, "architecture.skill.window_authoring");
    }

    #[test]
    fn migration_candidates_are_hidden_unless_the_curator_asks_for_them() {
        let mut registry = AgentSkillRegistry::default();
        for (id, tags) in [
            ("ordinary", vec!["window".to_string()]),
            (
                "candidate",
                vec!["window".to_string(), MIGRATION_CANDIDATE_TAG.to_string()],
            ),
        ] {
            registry.insert(AgentSkill {
                id: AgentSkillId(id.into()),
                title: id.into(),
                summary: "s".into(),
                task_tags: tags,
                must_read_at_bootstrap: false,
                referenced_tool_ids: vec![],
                required_tool_ids: vec![],
                forbidden_tool_ids: vec![],
                validation_tool_ids: vec![],
                success_criteria: vec![],
                stop_conditions: vec![],
                screenshot_requirements: vec![],
                common_failure_modes: vec![],
                regression_prompt_ids: vec![],
                next_skill_ids: vec![],
                body_markdown: "b".into(),
                trust_level: AgentSkillTrustLevel::Shipped,
                source_path: None,
            });
        }
        let ids = |v: Vec<&AgentSkill>| v.into_iter().map(|s| s.id.0.clone()).collect::<Vec<_>>();
        assert_eq!(ids(registry.list()), ["ordinary"]);
        assert_eq!(
            ids(registry.search(AgentSkillSearch {
                query: None,
                tags: vec!["window".into()]
            })),
            ["ordinary"]
        );
        assert_eq!(
            ids(registry.search(AgentSkillSearch {
                query: None,
                tags: vec![MIGRATION_CANDIDATE_TAG.into()]
            })),
            ["candidate"]
        );
    }

    #[test]
    fn draft_request_allocates_stable_id() {
        let skill = AgentSkillDraftRequest {
            id: None,
            title: "My Draft Skill".into(),
            summary: "Example".into(),
            task_tags: vec![],
            must_read_at_bootstrap: false,
            referenced_tool_ids: vec![],
            required_tool_ids: vec![],
            forbidden_tool_ids: vec![],
            validation_tool_ids: vec![],
            success_criteria: vec![],
            stop_conditions: vec![],
            screenshot_requirements: vec![],
            common_failure_modes: vec![],
            regression_prompt_ids: vec![],
            next_skill_ids: vec![],
            body_markdown: "Body".into(),
            source_path: None,
        }
        .into_skill();
        assert_eq!(skill.id.0, "agent_skill.draft.my.draft.skill");
    }
}
