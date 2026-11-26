//! Serialisable form of a generation prior.
//!
//! A compiled [`GenerationPriorDescriptor`] holds a closure and cannot travel
//! as data. This is the data equivalent: an ordered decision table over the
//! [`PriorContext`] fields every prior already receives. It is an interpreter,
//! not knowledge: which cases exist and what they return is corpus content.

use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability_registry::{
    CorpusProvenance, GenerationPriorDescriptor, PriorContext, PriorEvaluation, PriorId, PriorScope,
};

/// Upper bound on decision cases per prior.
pub const MAX_PRIOR_CASES: usize = 256;

/// Conditions over the prior context; every present condition must hold.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriorCondition {
    /// Case-insensitive jurisdiction code equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    /// Exact style-intent token equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_intent: Option<String>,
    /// Inclusive lower bound on `terrain_slope_pct`; absent slope fails the condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_slope_pct_min: Option<f64>,
    /// Inclusive upper bound on `terrain_slope_pct`; absent slope fails the condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_slope_pct_max: Option<f64>,
    /// Exact JSON equality on top-level keys of the caller context.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras_equal: BTreeMap<String, Value>,
}

impl PriorCondition {
    /// Whether every present condition holds for `ctx`.
    pub fn matches(&self, ctx: &PriorContext) -> bool {
        let text = |want: &Option<String>, got: &Option<String>, fold: bool| {
            want.as_ref().is_none_or(|w| {
                got.as_ref().is_some_and(|g| {
                    if fold {
                        g.eq_ignore_ascii_case(w)
                    } else {
                        g == w
                    }
                })
            })
        };
        let slope = |bound: Option<f64>, ok: fn(f64, f64) -> bool| {
            bound.is_none_or(|b| ctx.terrain_slope_pct.is_some_and(|s| ok(s, b)))
        };
        text(&self.jurisdiction, &ctx.jurisdiction, true)
            && text(&self.style_intent, &ctx.style_intent, false)
            && slope(self.terrain_slope_pct_min, |s, b| s >= b)
            && slope(self.terrain_slope_pct_max, |s, b| s <= b)
            && self
                .extras_equal
                .iter()
                .all(|(k, v)| ctx.extras.get(k) == Some(v))
    }
}

/// First matching case wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriorCase {
    pub when: PriorCondition,
    pub then: PriorEvaluation,
}

/// A generation prior as portable data.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationPriorSpec {
    pub id: PriorId,
    pub label: String,
    pub description: String,
    pub scope: PriorScope,
    pub source_provenance: CorpusProvenance,
    /// Ordered decision table; evaluated first-match-wins.
    #[serde(default)]
    pub cases: Vec<PriorCase>,
    /// Result when no case matches.
    pub otherwise: PriorEvaluation,
}

impl GenerationPriorSpec {
    /// Structural validation: identity, bounded table, finite weights in `[0, 1]`.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.0.trim().is_empty() {
            return Err("Prior identity is empty".into());
        }
        if self.cases.len() > MAX_PRIOR_CASES {
            return Err(format!("At most {MAX_PRIOR_CASES} prior cases"));
        }
        for (i, eval) in self
            .cases
            .iter()
            .map(|c| &c.then)
            .chain([&self.otherwise])
            .enumerate()
        {
            if !(0.0..=1.0).contains(&eval.weight) {
                return Err(format!(
                    "Prior weight at position {i} must be within [0, 1]"
                ));
            }
        }
        for case in &self.cases {
            let c = &case.when;
            if let (Some(lo), Some(hi)) = (c.terrain_slope_pct_min, c.terrain_slope_pct_max) {
                if lo > hi {
                    return Err("terrain_slope_pct_min exceeds terrain_slope_pct_max".into());
                }
            }
            if [c.terrain_slope_pct_min, c.terrain_slope_pct_max]
                .into_iter()
                .flatten()
                .any(|v| !v.is_finite())
            {
                return Err("Slope bounds must be finite".into());
            }
        }
        Ok(())
    }

    /// Build the runtime descriptor; the closure interprets the decision table.
    pub fn into_descriptor(self) -> GenerationPriorDescriptor {
        let Self {
            id,
            label,
            description,
            scope,
            source_provenance,
            cases,
            otherwise,
        } = self;
        GenerationPriorDescriptor {
            id,
            label,
            description,
            scope,
            source_provenance,
            prior_fn: Arc::new(move |ctx| {
                cases
                    .iter()
                    .find(|c| c.when.matches(ctx))
                    .map_or(&otherwise, |c| &c.then)
                    .clone()
            }),
        }
    }
}
