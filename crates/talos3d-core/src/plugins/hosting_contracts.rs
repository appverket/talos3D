//! Domain-neutral hosted Definition contract substrate.
//!
//! PP-DHOST-1 starts with shared data shapes and validator dispatch. Domain
//! crates register concrete contracts, such as architecture's wall opening.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugins::identity::ElementId;
use crate::plugins::modeling::definition::{ConstraintSeverity, ParameterSchema};

/// Stable identifier for a hosting contract kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostingContractKindId(pub String);

/// Stable identifier for a registered hosting validator.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostingValidatorId(pub String);

/// Stable identifier for a single check emitted by a hosting validator.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostingCheckId(pub String);

/// Host region affected by a validation check.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostAffectedRegion(pub String);

/// Overall validation outcome for a hosting contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostingValidationStatus {
    Passed,
    Warning,
    Blocked,
}

/// Outcome for one hosting-contract check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostingCheckStatus {
    Passed,
    Warning,
    Failed,
    NotApplicable,
}

/// Numeric or structured measured/expected value with a display unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasuredValue {
    pub value: Value,
    pub unit: String,
}

/// One structured result emitted by a hosting validator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostingValidationCheck {
    pub id: HostingCheckId,
    pub label: String,
    pub severity: ConstraintSeverity,
    pub status: HostingCheckStatus,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_value: Option<MeasuredValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_value: Option<MeasuredValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_region: Option<HostAffectedRegion>,
}

/// Complete validation result for a hosted occurrence against a host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostingValidationResult {
    pub contract_kind: HostingContractKindId,
    pub host_element_id: ElementId,
    pub hosted_element_id: ElementId,
    pub status: HostingValidationStatus,
    pub checks: Vec<HostingValidationCheck>,
}

/// Request passed to a registered hosting validator.
#[derive(Debug, Clone)]
pub struct HostingValidationRequest {
    pub contract_kind: HostingContractKindId,
    pub host_element_id: ElementId,
    pub hosted_element_id: ElementId,
    pub contract_parameters: Value,
}

/// Domain-owned validator function.
pub type HostingValidatorFn = std::sync::Arc<
    dyn Fn(HostingValidationRequest, &World) -> HostingValidationResult + Send + Sync,
>;

/// Capability-registered hosting contract descriptor.
pub struct HostingContractDescriptor {
    pub kind: HostingContractKindId,
    pub label: String,
    pub description: String,
    pub valid_host_types: Vec<String>,
    pub valid_hosted_types: Vec<String>,
    pub parameter_schema: ParameterSchema,
    pub validator_id: HostingValidatorId,
    pub default_severity: ConstraintSeverity,
    pub validator: HostingValidatorFn,
}

impl std::fmt::Debug for HostingContractDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostingContractDescriptor")
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("validator_id", &self.validator_id)
            .field("default_severity", &self.default_severity)
            .finish_non_exhaustive()
    }
}

impl Clone for HostingContractDescriptor {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            valid_host_types: self.valid_host_types.clone(),
            valid_hosted_types: self.valid_hosted_types.clone(),
            parameter_schema: self.parameter_schema.clone(),
            validator_id: self.validator_id.clone(),
            default_severity: self.default_severity.clone(),
            validator: self.validator.clone(),
        }
    }
}

/// Serializable descriptor summary for MCP discovery and diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostingContractInfo {
    pub kind: HostingContractKindId,
    pub label: String,
    pub description: String,
    pub valid_host_types: Vec<String>,
    pub valid_hosted_types: Vec<String>,
    pub parameter_schema: ParameterSchema,
    pub validator_id: HostingValidatorId,
    pub default_severity: ConstraintSeverity,
}

impl From<&HostingContractDescriptor> for HostingContractInfo {
    fn from(descriptor: &HostingContractDescriptor) -> Self {
        Self {
            kind: descriptor.kind.clone(),
            label: descriptor.label.clone(),
            description: descriptor.description.clone(),
            valid_host_types: descriptor.valid_host_types.clone(),
            valid_hosted_types: descriptor.valid_hosted_types.clone(),
            parameter_schema: descriptor.parameter_schema.clone(),
            validator_id: descriptor.validator_id.clone(),
            default_severity: descriptor.default_severity.clone(),
        }
    }
}
