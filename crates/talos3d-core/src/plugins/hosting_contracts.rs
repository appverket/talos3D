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
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
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

/// Generic kind of relationship a hosted occurrence has with its host.
///
/// Penetration is deliberately one case among several. Core uses this value to
/// route placement and resize plans; domain packages define the construction
/// meaning of each registered capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedInteractionKind {
    ThroughPenetration,
    PartialPenetration,
    Embedded,
    SurfaceMounted,
    EdgeHosted,
    SupportedOn,
    AdjacentAligned,
}

impl HostedInteractionKind {
    pub fn requires_host_feature(self) -> bool {
        matches!(
            self,
            Self::ThroughPenetration | Self::PartialPenetration | Self::Embedded
        )
    }

    pub fn is_penetrating(self) -> bool {
        matches!(self, Self::ThroughPenetration | Self::PartialPenetration)
    }
}

/// Axis in a host-local placement frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPlacementAxis {
    U,
    V,
    Through,
}

/// Cursor/authoring behavior for one placement axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementAxisBehavior {
    FollowCursor,
    FixedDefault,
    DerivedFromHost,
    DerivedFromHosted,
    UserInput,
}

/// Source of a default placement coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementDefaultSource {
    HostCapability,
    HostedDefinition,
    RegionalPrior,
    ProjectAssumption,
    UserInput,
    Derived,
}

/// A default value for one host-local placement axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementDefault {
    pub axis: HostPlacementAxis,
    pub value: Value,
    pub source: PlacementDefaultSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// Axis constraint used by host-constrained placement mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementAxisLock {
    pub axis: HostPlacementAxis,
    pub behavior: PlacementAxisBehavior,
}

/// How a hosted penetration binds to host feature/opening state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PenetrationBinding {
    /// Domain-owned role id for the generated or updated host feature.
    pub feature_role: String,
    /// Whether this binding expects an inline Definition `VoidDeclaration`.
    #[serde(default)]
    pub requires_void_declaration: bool,
    /// Parameter names used to compute clearance, rough opening, or fit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clearance_parameters: Vec<String>,
}

/// Semantic role of a parameter when a hosted occurrence is resized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartResizeRole {
    ProductExtentDriver,
    FixedWorld,
    Ratio,
    Semantic,
    OpeningDriver,
    AnchorPreserving,
    RepeatOrReflow,
    Derived,
}

/// The authored target a resize parameter patch belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartResizeTarget {
    HostedOccurrence,
    HostFeature,
    HostedRelation,
    DerivedOnly,
}

/// A parameter affected by a resize handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartResizeParameter {
    pub parameter: String,
    pub role: SmartResizeRole,
    pub target: SmartResizeTarget,
}

/// A semantically meaningful resize handle exposed by a Definition or adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartResizeHandle {
    pub handle_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axes: Vec<HostPlacementAxis>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<SmartResizeParameter>,
}

/// Capability offered by a host Definition or domain adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostCapabilityDeclaration {
    pub capability_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_kind: Option<HostingContractKindId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interaction_kinds: Vec<HostedInteractionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate_frame_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placement_locks: Vec<PlacementAxisLock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placement_defaults: Vec<PlacementDefault>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adaptation_obligations: Vec<String>,
}

/// Requirement declared by a hosted Definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostedRequirementDeclaration {
    pub requirement_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_capability_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_contract_kinds: Vec<HostingContractKindId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interaction_kinds: Vec<HostedInteractionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub penetration: Option<PenetrationBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resize_handles: Vec<SmartResizeHandle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_host_obligations: Vec<String>,
}

impl HostedRequirementDeclaration {
    pub fn supports_interaction(&self, kind: HostedInteractionKind) -> bool {
        self.interaction_kinds.contains(&kind)
    }

    pub fn has_host_feature_binding_for(&self, kind: HostedInteractionKind) -> bool {
        !kind.requires_host_feature() || self.penetration.is_some()
    }
}

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

#[cfg(test)]
mod hosted_occurrence_contract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hosted_occurrence_contract_distinguishes_penetrating_and_surface_hosting() {
        assert!(HostedInteractionKind::ThroughPenetration.requires_host_feature());
        assert!(HostedInteractionKind::PartialPenetration.is_penetrating());
        assert!(!HostedInteractionKind::SurfaceMounted.requires_host_feature());
        assert!(!HostedInteractionKind::SupportedOn.is_penetrating());
    }

    #[test]
    fn hosted_occurrence_contract_requirement_round_trips_with_resize_roles() {
        let requirement = HostedRequirementDeclaration {
            requirement_id: "window.wall_through_opening".into(),
            accepted_capability_ids: vec!["wall.planar_opening".into()],
            accepted_contract_kinds: vec![HostingContractKindId(
                "architecture::wall_opening".into(),
            )],
            interaction_kinds: vec![HostedInteractionKind::ThroughPenetration],
            penetration: Some(PenetrationBinding {
                feature_role: "rough_opening".into(),
                requires_void_declaration: true,
                clearance_parameters: vec!["shim_gap_m".into()],
            }),
            resize_handles: vec![SmartResizeHandle {
                handle_id: "right_edge".into(),
                axes: vec![HostPlacementAxis::U],
                parameters: vec![
                    SmartResizeParameter {
                        parameter: "overall_width".into(),
                        role: SmartResizeRole::ProductExtentDriver,
                        target: SmartResizeTarget::HostedOccurrence,
                    },
                    SmartResizeParameter {
                        parameter: "opening_width_m".into(),
                        role: SmartResizeRole::OpeningDriver,
                        target: SmartResizeTarget::HostFeature,
                    },
                    SmartResizeParameter {
                        parameter: "frame_profile_width".into(),
                        role: SmartResizeRole::FixedWorld,
                        target: SmartResizeTarget::HostedOccurrence,
                    },
                ],
            }],
            required_host_obligations: vec!["architecture.framing.header".into()],
        };

        let value = serde_json::to_value(&requirement).unwrap();
        assert_eq!(value["interaction_kinds"][0], json!("through_penetration"));
        assert_eq!(
            value["resize_handles"][0]["parameters"][1]["role"],
            json!("opening_driver")
        );

        let parsed: HostedRequirementDeclaration = serde_json::from_value(value).unwrap();
        assert!(parsed.supports_interaction(HostedInteractionKind::ThroughPenetration));
        assert!(parsed.has_host_feature_binding_for(HostedInteractionKind::ThroughPenetration));
    }

    #[test]
    fn hosted_occurrence_contract_host_capability_round_trips_with_axis_locks() {
        let capability = HostCapabilityDeclaration {
            capability_id: "wall.planar_opening".into(),
            contract_kind: Some(HostingContractKindId("architecture::wall_opening".into())),
            interaction_kinds: vec![HostedInteractionKind::ThroughPenetration],
            coordinate_frame_id: Some("wall.local_uvn".into()),
            placement_locks: vec![
                PlacementAxisLock {
                    axis: HostPlacementAxis::U,
                    behavior: PlacementAxisBehavior::FollowCursor,
                },
                PlacementAxisLock {
                    axis: HostPlacementAxis::V,
                    behavior: PlacementAxisBehavior::FixedDefault,
                },
            ],
            placement_defaults: vec![PlacementDefault {
                axis: HostPlacementAxis::V,
                value: json!(0.9),
                source: PlacementDefaultSource::RegionalPrior,
                rationale: Some("default sill height".into()),
            }],
            adaptation_obligations: vec!["architecture.wall.local_opening_regeneration".into()],
        };

        let value = serde_json::to_value(&capability).unwrap();
        assert_eq!(
            value["placement_locks"][0]["behavior"],
            json!("follow_cursor")
        );
        let parsed: HostCapabilityDeclaration = serde_json::from_value(value).unwrap();
        assert_eq!(
            parsed.placement_defaults[0].source,
            PlacementDefaultSource::RegionalPrior
        );
    }
}
