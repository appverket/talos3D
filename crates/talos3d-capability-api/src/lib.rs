pub mod prelude {
    pub use crate::capabilities::{
        AssemblyTypeDescriptor, CapabilityActivation, CapabilityDescriptor, CapabilityDistribution,
        CapabilityMaturity, CapabilityRegistryAppExt, DefaultsContributor, DefaultsRegistryAppExt,
        GeneratedFaceRef, RelationTypeDescriptor, RequireWorkbench, TerrainProvider,
        TerrainProviderRegistryAppExt, WorkbenchDescriptor, CAPABILITY_API_VERSION,
    };
    pub use crate::commands::{
        activate_tool_command, CommandCategory, CommandDescriptor, CommandRegistryAppExt,
        CommandResult, IconRegistry,
    };
    pub use crate::document_properties::DocumentProperties;
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
        CommandResult, IconRegistry,
    };
}

pub mod document_properties {
    pub use talos3d_core::plugins::document_properties::DocumentProperties;
}

pub mod icons {
    pub use talos3d_core::plugins::icons::{render_icon, ICON_SIZE};
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
