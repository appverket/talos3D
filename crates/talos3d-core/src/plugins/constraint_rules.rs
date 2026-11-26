//! Declarative constraint rules and the one generic validator that evaluates
//! them.
//!
//! A [`ConstraintRule`] is data: an element class, a parameter path, a numeric
//! requirement, the refinement state from which it applies, severity, message
//! and a corpus citation. [`ConstraintRule::into_descriptor`] lifts a rule into
//! an ordinary [`ConstraintDescriptor`] in the shared `CapabilityRegistry`, so
//! the validation sweep, `Findings`, `list_constraints` and promotion gating
//! treat it exactly like a compiled constraint. No rule-specific code exists.
//!
//! Rule semantics: the comparison states what must hold. A finding is emitted
//! when the subject carries a numeric value at `parameter_path` (a JSON pointer
//! into `SemanticIntent.parameters`, values stored in `unit`) and the
//! requirement does not hold. A missing or non-numeric value emits nothing: the
//! rule cannot be evaluated, which is not a violation.

use std::{collections::BTreeMap, sync::Arc};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability_registry::{
    Applicability, CapabilityRegistry, ConstraintDescriptor, ConstraintId, ConstraintRole,
    ElementClassId, Finding, FindingId, Severity,
};
use crate::plugins::{
    identity::ElementId,
    refinement::{PassageRef, RefinementState, SemanticIntent},
};

/// What must hold for the parameter value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "comparator", rename_all = "snake_case")]
pub enum Comparison {
    /// value < limit
    Lt { value: f64 },
    /// value <= limit
    Lte { value: f64 },
    /// value > limit
    Gt { value: f64 },
    /// value >= limit
    Gte { value: f64 },
    /// |value - target| <= tolerance
    Eq {
        value: f64,
        #[serde(default)]
        tolerance: f64,
    },
    /// min <= value <= max
    Range { min: f64, max: f64 },
}

impl Comparison {
    /// Whether `actual` satisfies the requirement.
    #[must_use]
    pub fn holds(&self, actual: f64) -> bool {
        match *self {
            Self::Lt { value } => actual < value,
            Self::Lte { value } => actual <= value,
            Self::Gt { value } => actual > value,
            Self::Gte { value } => actual >= value,
            Self::Eq { value, tolerance } => (actual - value).abs() <= tolerance,
            Self::Range { min, max } => (min..=max).contains(&actual),
        }
    }

    fn validate(&self) -> Result<(), String> {
        let ok = match *self {
            Self::Lt { value } | Self::Lte { value } | Self::Gt { value } | Self::Gte { value } => {
                value.is_finite()
            }
            Self::Eq { value, tolerance } => {
                value.is_finite() && tolerance.is_finite() && tolerance >= 0.0
            }
            Self::Range { min, max } => min.is_finite() && max.is_finite() && min <= max,
        };
        ok.then_some(())
            .ok_or_else(|| "Comparison values must be finite, tolerance >= 0, min <= max".into())
    }
}

/// A declarative validation rule delivered as corpus data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ConstraintRule {
    /// Stable id; becomes the constraint id and the asset id.
    pub id: String,
    pub label: String,
    /// Element class the rule applies to.
    pub element_class: String,
    /// JSON pointer into the subject's `SemanticIntent.parameters`, e.g. `/clear_height`.
    pub parameter_path: String,
    /// What must hold for the value; a finding is emitted when it does not.
    pub requirement: Comparison,
    /// Unit the parameter value and the comparison values are expressed in.
    pub unit: String,
    /// The rule applies at this refinement state and above.
    pub from_state: RefinementState,
    /// When set, only subjects whose `SemanticIntent.parameters.jurisdiction`
    /// equals this tag (case-insensitive) are checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    pub severity: Severity,
    /// Finding message shown to the user.
    pub message: String,
    /// Why the rule exists.
    pub rationale: String,
    /// Corpus passage that establishes the requirement.
    pub citation: PassageRef,
}

impl ConstraintRule {
    /// Pure structural checks.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("id", &self.id),
            ("label", &self.label),
            ("element_class", &self.element_class),
            ("unit", &self.unit),
            ("message", &self.message),
            ("citation", &self.citation.0),
        ] {
            if value.trim().is_empty() {
                return Err(format!("Constraint rule {name} must be non-empty"));
            }
        }
        if !self.parameter_path.starts_with('/') {
            return Err("parameter_path must be a JSON pointer starting with '/'".into());
        }
        self.requirement.validate()
    }

    fn applies_to_jurisdiction(&self, intent: &SemanticIntent) -> bool {
        self.jurisdiction.as_deref().is_none_or(|tag| {
            intent
                .parameters
                .get("jurisdiction")
                .and_then(Value::as_str)
                .is_some_and(|j| j.eq_ignore_ascii_case(tag))
        })
    }

    /// The generic evaluator: findings for one subject.
    fn evaluate(&self, subject: Entity, world: &World) -> Vec<Finding> {
        let (Some(eid), Some(intent)) = (
            world.get::<ElementId>(subject),
            world.get::<SemanticIntent>(subject),
        ) else {
            return Vec::new();
        };
        let Some(actual) = intent
            .parameters
            .pointer(&self.parameter_path)
            .and_then(Value::as_f64)
        else {
            return Vec::new();
        };
        if self.requirement.holds(actual) || !self.applies_to_jurisdiction(intent) {
            return Vec::new();
        }
        vec![Finding {
            id: FindingId(format!("{}:{}", self.id, eid.0)),
            constraint_id: ConstraintId(self.id.clone()),
            subject: eid.0,
            severity: self.severity,
            message: format!(
                "{} (found {actual} {} at {})",
                self.message, self.unit, self.parameter_path
            ),
            rationale: self.rationale.clone(),
            backlink: Some(self.citation.clone()),
            emitted_at: crate::time::SystemTime::now()
                .duration_since(crate::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            role: ConstraintRole::Validation,
        }]
    }

    /// The ordinary constraint descriptor evaluating this rule.
    #[must_use]
    pub fn into_descriptor(self) -> ConstraintDescriptor {
        let applicability = Applicability {
            element_classes: vec![ElementClassId(self.element_class.clone())],
            required_state: Some(self.from_state),
        };
        ConstraintDescriptor {
            id: ConstraintId(self.id.clone()),
            label: self.label.clone(),
            description: self.message.clone(),
            applicability,
            default_severity: self.severity,
            rationale: self.rationale.clone(),
            source_backlink: Some(self.citation.clone()),
            role: ConstraintRole::Validation,
            validator: Arc::new({
                let rule = self;
                move |entity, world| rule.evaluate(entity, world)
            }),
        }
    }
}

/// Rules installed through the declarative form (compiled or corpus-delivered),
/// kept so they can be exported back as data.
#[derive(Resource, Debug, Clone, Default)]
pub struct DeclarativeConstraintRules(pub BTreeMap<String, ConstraintRule>);

/// Register or replace a declarative rule in the shared constraint registry.
pub fn install_rule(world: &mut World, rule: ConstraintRule) {
    world.init_resource::<CapabilityRegistry>();
    world.init_resource::<DeclarativeConstraintRules>();
    world
        .resource_mut::<DeclarativeConstraintRules>()
        .0
        .insert(rule.id.clone(), rule.clone());
    world
        .resource_mut::<CapabilityRegistry>()
        .upsert_constraint(rule.into_descriptor());
}

/// Remove a declarative rule; nothing compiled is restored.
pub fn remove_rule(world: &mut World, id: &str) {
    if let Some(mut rules) = world.get_resource_mut::<DeclarativeConstraintRules>() {
        rules.0.remove(id);
    }
    if let Some(mut registry) = world.get_resource_mut::<CapabilityRegistry>() {
        registry.remove_constraint(&ConstraintId(id.to_owned()));
    }
}

/// `App` extension for compiled plugins that declare a constraint as data.
pub trait DeclarativeConstraintAppExt {
    /// Declare `rule` through the same form the corpus delivers.
    fn register_declarative_constraint(&mut self, rule: ConstraintRule) -> &mut Self;
}

impl DeclarativeConstraintAppExt for App {
    fn register_declarative_constraint(&mut self, rule: ConstraintRule) -> &mut Self {
        install_rule(self.world_mut(), rule);
        self
    }
}
