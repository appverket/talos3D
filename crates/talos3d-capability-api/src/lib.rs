pub mod prelude {
    pub use crate::capabilities::{
        AssemblyTypeDescriptor, CapabilityActivation, CapabilityDescriptor, CapabilityDistribution,
        CapabilityMaturity, CapabilityRegistryAppExt, DefaultsContributor, DefaultsRegistryAppExt,
        GeneratedFaceRef, RelationTypeDescriptor, RequireWorkbench, TerrainProvider,
        TerrainProviderRegistryAppExt, WorkbenchDescriptor, CAPABILITY_API_VERSION,
    };
    pub use crate::commands::{
        activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
        CommandResult, IconRegistry, ViewportContextCommandRegistry,
    };
    pub use crate::definitions::{
        BindingSide, ConstraintSeverity, OverridePolicy, ParamType, ParameterBinding, ParameterDef,
        ParameterMetadata, ParameterMutability, ParameterSchema,
    };
    pub use crate::document_properties::DocumentProperties;
    pub use crate::hosting::{
        HostAffectedRegion, HostedGeometryPolicies, HostedGeometryPolicy, HostingCheckId,
        HostingCheckStatus, HostingContractDescriptor, HostingContractInfo, HostingContractKindId,
        HostingValidationCheck, HostingValidationRequest, HostingValidationResult,
        HostingValidationStatus, HostingValidatorFn, HostingValidatorId, MeasuredValue,
    };
    pub use crate::identity::ElementId;
    pub use crate::toolbar::{
        ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection,
    };
    pub use crate::tools::ActiveTool;
}

pub mod capabilities {
    pub use talos3d_core::capability_registry::{
        AssemblyTypeDescriptor, CapabilityActivation, CapabilityDescriptor, CapabilityDistribution,
        CapabilityMaturity, CapabilityRegistry, CapabilityRegistryAppExt, DefaultsContributor,
        DefaultsRegistryAppExt, FaceHitCandidate, FaceId, GeneratedFaceRef, HitCandidate,
        ModelSummaryAccumulator, RelationTypeDescriptor, RequireWorkbench, SnapPoint,
        TerrainProvider, TerrainProviderRegistryAppExt, WorkbenchDescriptor,
        CAPABILITY_API_VERSION,
    };
}

pub mod commands {
    pub use talos3d_core::plugins::command_registry::{
        activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
        CommandResult, IconRegistry, ViewportContextCommandRegistry,
    };
}

/// Transient native planning contracts. AuthoringScript remains the durable IR.
pub mod edit_plans {
    pub use talos3d_core::plugins::authored_edit_plan::{
        modifiers::{EditModifierRequest, EditPlanDraft, EditPlanModifiers, OrderedEditModifier},
        queue_captured_plan, AuthoredEditPlan, AuthoredEditPlanRegistry, InteractionId,
        PlanContext, PlanError, PlanId,
    };
    pub use talos3d_core::plugins::history::ModelRevision;
}

pub mod definitions {
    pub use talos3d_core::plugins::modeling::definition::{
        BindingSide, ConstraintSeverity, OverridePolicy, ParamType, ParameterBinding, ParameterDef,
        ParameterMetadata, ParameterMutability, ParameterSchema,
    };
}

pub mod document_properties {
    pub use talos3d_core::plugins::document_properties::DocumentProperties;
}

pub mod hosting {
    pub use talos3d_core::plugins::hosting_contracts::{
        HostAffectedRegion, HostedGeometryPolicies, HostedGeometryPolicy, HostingCheckId,
        HostingCheckStatus, HostingContractDescriptor, HostingContractInfo, HostingContractKindId,
        HostingValidationCheck, HostingValidationRequest, HostingValidationResult,
        HostingValidationStatus, HostingValidatorFn, HostingValidatorId, MeasuredValue,
    };
}

pub mod identity {
    pub use talos3d_core::plugins::identity::ElementId;
}

pub mod icons {
    pub use talos3d_core::plugins::icons::{render_icon, ICON_SIZE};
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use crate::prelude::*;

    #[test]
    fn hosting_contract_types_are_available_through_capability_api() {
        let descriptor = HostingContractDescriptor {
            kind: HostingContractKindId("example::hosted_contract".to_string()),
            label: "Hosted Contract".to_string(),
            description: "Example domain-owned hosting contract".to_string(),
            valid_host_types: vec!["example.host".to_string()],
            valid_hosted_types: vec!["example.hosted".to_string()],
            parameter_schema: ParameterSchema(vec![ParameterDef {
                name: "host_axis".to_string(),
                param_type: ParamType::AxisRef,
                default_value: json!("host.normal"),
                override_policy: OverridePolicy::Locked,
                geometry_affecting: true,
                metadata: ParameterMetadata::default(),
            }]),
            validator_id: HostingValidatorId("example.validator".to_string()),
            default_severity: ConstraintSeverity::Error,
            validator: Arc::new(|request, _world| HostingValidationResult {
                contract_kind: request.contract_kind,
                host_element_id: request.host_element_id,
                hosted_element_id: request.hosted_element_id,
                status: HostingValidationStatus::Passed,
                checks: vec![HostingValidationCheck {
                    id: HostingCheckId("example.alignment".to_string()),
                    label: "Alignment".to_string(),
                    severity: ConstraintSeverity::Error,
                    status: HostingCheckStatus::Passed,
                    message: "hosted object is aligned with host".to_string(),
                    measured_value: None,
                    expected_value: None,
                    affected_region: Some(HostAffectedRegion("host_normal".to_string())),
                }],
            }),
        };

        let info = HostingContractInfo::from(&descriptor);
        assert_eq!(info.kind.0, "example::hosted_contract");
        assert_eq!(info.parameter_schema.0[0].param_type, ParamType::AxisRef);
    }
}

pub mod toolbar {
    pub use talos3d_core::plugins::toolbar::{
        ToolbarDescriptor, ToolbarDock, ToolbarRegistryAppExt, ToolbarSection,
    };
}

pub mod tools {
    pub use talos3d_core::plugins::tools::ActiveTool;
}

pub mod modeling {
    pub use talos3d_core::plugins::modeling::ModelingWorkbench;
}

/// Data-driven family schemas shared by specialized editions.
pub mod component_families {
    pub use talos3d_core::plugins::component_family::{
        apply_component_family_spec_to_definition, component_family_spec_from_definition,
        derive_component_family_parameters, instantiate_component_family_definition,
        parse_component_family_asset, parse_component_family_spec, ComponentFamily,
        ComponentFamilyAssetFile, ComponentFamilyParameter, ComponentFamilyRegistry,
        ComponentFamilyValueType, ParametricDriverBinding,
    };
}

/// Capability-owned guidance and discovery vocabulary.
pub mod guidance {
    pub use talos3d_core::plugins::authoring_guidance::{
        DiscoveryAliases, GuidanceCardInfo, GuidanceCardRegistry,
    };
}

/// Capability policies used by shared refinement and surface placement.
pub mod placement {
    pub use talos3d_core::plugins::materials::BuiltinMaterialDefaults;
    pub use talos3d_core::plugins::refinement::{SettingOutPolicies, SettingOutPolicy};
    pub use talos3d_core::plugins::surface_placement::SurfacePlacementVocabulary;
}
