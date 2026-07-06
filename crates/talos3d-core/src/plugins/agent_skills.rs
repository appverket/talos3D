//! Agent-operating-procedure knowledge exposed through the MCP surface.
//!
//! Agent skills are not recipes, validators, or Definition libraries. They are
//! bounded procedures that tell an MCP client how to acquire evidence, choose
//! Talos3D representations, and validate work before claiming completion.

use std::collections::BTreeMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AgentSkill {
    pub id: AgentSkillId,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub task_tags: Vec<String>,
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

    fn matches(&self, query: Option<&str>, tags: &[String]) -> bool {
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
    pub referenced_tool_ids: Vec<String>,
    pub required_tool_ids: Vec<String>,
    pub forbidden_tool_ids: Vec<String>,
    pub validation_tool_ids: Vec<String>,
    pub success_criteria: Vec<String>,
    pub stop_conditions: Vec<String>,
    pub trust_level: String,
    pub source_path: Option<String>,
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

    pub fn list(&self) -> Vec<&AgentSkill> {
        self.skills.values().collect()
    }

    pub fn search(&self, filter: AgentSkillSearch) -> Vec<&AgentSkill> {
        self.skills
            .values()
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
    fn draft_request_allocates_stable_id() {
        let skill = AgentSkillDraftRequest {
            id: None,
            title: "My Draft Skill".into(),
            summary: "Example".into(),
            task_tags: vec![],
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
