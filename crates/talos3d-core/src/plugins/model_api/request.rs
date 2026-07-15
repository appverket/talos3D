use super::*;

#[cfg(feature = "model-api")]
#[derive(Resource)]
pub(super) struct ModelApiReceiver(pub(super) Mutex<mpsc::Receiver<ModelApiRequest>>);

#[cfg(feature = "model-api")]
pub(super) enum ModelApiRequest {
    GetInstanceInfo(oneshot::Sender<InstanceInfo>),
    ListEntities(oneshot::Sender<Vec<EntityEntry>>),
    GetEntity {
        element_id: u64,
        response: oneshot::Sender<Option<serde_json::Value>>,
    },
    GetEntityDetails {
        element_id: u64,
        response: oneshot::Sender<Option<EntityDetails>>,
    },
    GetEntitiesDetails {
        element_ids: Vec<u64>,
        response: oneshot::Sender<EntitiesDetailsResponse>,
    },
    ModelSummary(oneshot::Sender<ModelSummary>),
    OutlineTree(oneshot::Sender<Value>),
    ListImporters(oneshot::Sender<Vec<ImporterDescriptor>>),
    CreateEntity {
        json: Value,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ImportFile {
        path: String,
        format_hint: Option<String>,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    AcceptSemanticShadowCandidate {
        request: AcceptSemanticShadowCandidateRequest,
        response: oneshot::Sender<ApiResult<EntityDetails>>,
    },
    ListHandles {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<HandleInfo>>>,
    },
    BimPropertySetGet {
        element_id: u64,
        set_name: String,
        property_name: String,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimPropertySetSet {
        element_id: u64,
        definition_id: String,
        set_name: String,
        property_name: String,
        value: Value,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimExchangeIdentityAssign {
        element_id: u64,
        system: String,
        exchange_id: String,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimExchangeIdentityGet {
        element_id: u64,
        system: String,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimExchangeIdentityList {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimVoidDeclareForDefinition {
        definition_id: String,
        declaration: Value,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimVoidPlanPlacement {
        filling_definition: String,
        host_element_id: u64,
        filling_element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimSpatialAssign {
        child_element_id: u64,
        container_element_id: u64,
        container_kind: String,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimSpatialListKindRegistry {
        response: oneshot::Sender<ApiResult<Value>>,
    },
    GetDocumentProperties(oneshot::Sender<serde_json::Value>),
    SetDocumentProperties {
        partial: serde_json::Value,
        response: oneshot::Sender<ApiResult<serde_json::Value>>,
    },
    ListToolbars(oneshot::Sender<Vec<ToolbarDetails>>),
    SetToolbarLayout {
        updates: Vec<ToolbarLayoutUpdate>,
        response: oneshot::Sender<ApiResult<Vec<ToolbarDetails>>>,
    },
    ListCommands(oneshot::Sender<Value>),
    InvokeCommand {
        command_id: String,
        parameters: Value,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    PrepareSiteSurface {
        request: PrepareSiteSurfaceRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::command_registry::CommandResult>>,
    },
    TerrainCutFillAnalysis {
        request: TerrainCutFillAnalysisRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::command_registry::CommandResult>>,
    },
    TerrainElevationAt {
        request: TerrainElevationAtRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    GetEditingContext(oneshot::Sender<EditingContextInfo>),
    EnterGroup {
        element_id: u64,
        response: oneshot::Sender<ApiResult<EditingContextInfo>>,
    },
    ExitGroup(oneshot::Sender<ApiResult<EditingContextInfo>>),
    ListGroupMembers {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<GroupMemberEntry>>>,
    },
    // --- Layer Management ---
    ListLayers(oneshot::Sender<Vec<LayerInfo>>),
    SetLayerVisibility {
        name: String,
        visible: bool,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    SetLayerLocked {
        name: String,
        locked: bool,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    AssignLayer {
        element_id: u64,
        layer_name: String,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    CreateLayer {
        name: String,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    RenameLayer {
        old_name: String,
        new_name: String,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    DeleteLayer {
        name: String,
        response: oneshot::Sender<ApiResult<Vec<LayerInfo>>>,
    },
    // --- Dependency Graph (read-only) ---
    DependencyGraph(oneshot::Sender<Value>),
    EntityDependencies {
        element_id: u64,
        response: oneshot::Sender<Value>,
    },
    // --- Materials ---
    ListMaterials(oneshot::Sender<Vec<MaterialInfo>>),
    GetMaterial {
        id: String,
        response: oneshot::Sender<ApiResult<MaterialInfo>>,
    },
    CreateMaterial {
        request: CreateMaterialRequest,
        response: oneshot::Sender<ApiResult<MaterialInfo>>,
    },
    UpdateMaterial {
        id: String,
        request: CreateMaterialRequest,
        response: oneshot::Sender<ApiResult<MaterialInfo>>,
    },
    DeleteMaterial {
        id: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    ApplyMaterial {
        request: ApplyMaterialRequest,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    AssignMaterial {
        request: AssignMaterialRequest,
        response: oneshot::Sender<ApiResult<AssignMaterialResponse>>,
    },
    RemoveMaterial {
        element_ids: Vec<u64>,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    GetMaterialAssignment {
        element_id: u64,
        response: oneshot::Sender<ApiResult<EntityMaterialAssignmentInfo>>,
    },
    SetMaterialAssignment {
        request: SetMaterialAssignmentRequest,
        response: oneshot::Sender<ApiResult<Vec<EntityMaterialAssignmentInfo>>>,
    },
    GetTextureMapping {
        request: GetTextureMappingRequest,
        response: oneshot::Sender<ApiResult<TextureMappingInfo>>,
    },
    UpdateTextureMapping {
        request: UpdateTextureMappingRequest,
        response: oneshot::Sender<ApiResult<TextureMappingInfo>>,
    },
    ResetTextureMapping {
        request: ResetTextureMappingRequest,
        response: oneshot::Sender<ApiResult<TextureMappingInfo>>,
    },
    BimMaterialAssignLayered {
        request: BimMaterialAssignLayeredRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimMaterialAssignConstituents {
        request: BimMaterialAssignConstituentsRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    BimMaterialGetEffective {
        request: BimMaterialGetEffectiveRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    QuantitySet {
        request: QuantitySetRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    QuantityGet {
        request: QuantityGetRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    QuantityListProvenance {
        request: QuantityListProvenanceRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    QuantityCheckInvariants {
        request: QuantityCheckInvariantsRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    ListMaterialSpecs {
        filter: ListMaterialSpecsFilter,
        response: oneshot::Sender<ApiResult<Vec<MaterialSpecInfo>>>,
    },
    GetMaterialSpec {
        asset_id: String,
        response: oneshot::Sender<ApiResult<MaterialSpecInfo>>,
    },
    CreateMaterialSpec {
        request: DraftMaterialSpecRequest,
        response: oneshot::Sender<ApiResult<MaterialSpecInfo>>,
    },
    UpdateMaterialSpec {
        asset_id: String,
        body: MaterialSpecBody,
        rationale: Option<String>,
        response: oneshot::Sender<ApiResult<MaterialSpecInfo>>,
    },
    SaveMaterialSpec {
        asset_id: String,
        scope: String,
        response: oneshot::Sender<ApiResult<MaterialSpecInfo>>,
    },
    PublishMaterialSpec {
        asset_id: String,
        response: oneshot::Sender<ApiResult<MaterialSpecInfo>>,
    },
    DeleteMaterialSpec {
        asset_id: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    GetLightingScene(oneshot::Sender<LightingSceneInfo>),
    ListLights(oneshot::Sender<Vec<SceneLightInfo>>),
    CreateLight {
        request: CreateLightRequest,
        response: oneshot::Sender<ApiResult<SceneLightInfo>>,
    },
    UpdateLight {
        request: UpdateLightRequest,
        response: oneshot::Sender<ApiResult<SceneLightInfo>>,
    },
    DeleteLight {
        element_id: u64,
        response: oneshot::Sender<ApiResult<usize>>,
    },
    SetAmbientLight {
        request: AmbientLightUpdateRequest,
        response: oneshot::Sender<ApiResult<AmbientLightInfo>>,
    },
    RestoreDefaultLightRig {
        response: oneshot::Sender<ApiResult<Vec<SceneLightInfo>>>,
    },
    GetRenderSettings(oneshot::Sender<RenderSettingsInfo>),
    GetPerfStats(oneshot::Sender<PerfStatsInfo>),
    SetRenderSettings {
        request: RenderSettingsUpdateRequest,
        response: oneshot::Sender<ApiResult<RenderSettingsInfo>>,
    },
    GetCamera(oneshot::Sender<CameraStateInfo>),
    SetCamera {
        params: CameraParams,
        response: oneshot::Sender<ApiResult<CameraStateInfo>>,
    },
    // --- Selection ---
    GetSelection(oneshot::Sender<Vec<u64>>),
    SetSelection {
        element_ids: Vec<u64>,
        response: oneshot::Sender<ApiResult<Vec<u64>>>,
    },
    ListSubobjects {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<SelectableSubobjectInfo>>>,
    },
    GetSubobjectSelection {
        response: oneshot::Sender<Vec<SelectableSubobjectRef>>,
    },
    SetSubobjectSelection {
        refs: Vec<SelectableSubobjectRef>,
        response: oneshot::Sender<ApiResult<Vec<SelectableSubobjectRef>>>,
    },
    ExpandSubobjectSelection {
        reference: SelectableSubobjectRef,
        mode: String,
        response: oneshot::Sender<ApiResult<Vec<SelectableSubobjectRef>>>,
    },
    ApplySubobjectEdit {
        reference: SelectableSubobjectRef,
        operation: String,
        parameters: Value,
        response: oneshot::Sender<ApiResult<SubobjectEditResult>>,
    },
    // --- Live UX harness ---
    UxObserve {
        response: oneshot::Sender<ApiResult<crate::plugins::ux_harness::UxHarnessSnapshot>>,
    },
    UxMovePointer {
        request: crate::plugins::ux_harness::UxPointerMoveRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::ux_harness::UxInputResult>>,
    },
    UxClick {
        request: crate::plugins::ux_harness::UxClickRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::ux_harness::UxInputResult>>,
    },
    UxDrag {
        request: crate::plugins::ux_harness::UxDragRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::ux_harness::UxInputResult>>,
    },
    UxPressKey {
        request: crate::plugins::ux_harness::UxPressKeyRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::ux_harness::UxInputResult>>,
    },
    AlignPreview {
        request: AlignRequest,
        response: oneshot::Sender<ApiResult<Vec<SpatialPreviewEntry>>>,
    },
    AlignExecute {
        request: AlignRequest,
        response: oneshot::Sender<ApiResult<Vec<SpatialPreviewEntry>>>,
    },
    DistributePreview {
        request: DistributeRequest,
        response: oneshot::Sender<ApiResult<Vec<SpatialPreviewEntry>>>,
    },
    DistributeExecute {
        request: DistributeRequest,
        response: oneshot::Sender<ApiResult<Vec<SpatialPreviewEntry>>>,
    },
    // --- Face Subdivision ---
    SplitBoxFace {
        element_id: u64,
        face_id: u32,
        split_position: f32,
        response: oneshot::Sender<ApiResult<SplitResult>>,
    },
    // --- Screenshot ---
    TakeScreenshot {
        path: String,
        include_ui: bool,
        response: oneshot::Sender<ApiResult<String>>,
    },
    ExportDrawing {
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    ExportDraftingSheet {
        path: String,
        scale_denominator: Option<f32>,
        response: oneshot::Sender<ApiResult<String>>,
    },
    PlaceSheetDimension {
        request: PlaceSheetDimensionRequest,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    PlaceDimensionBetweenHandles {
        request: PlaceDimensionBetweenHandlesRequest,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    SaveProject {
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    FrameModel {
        response: oneshot::Sender<ApiResult<BoundingBox>>,
    },
    FrameEntities {
        element_ids: Vec<u64>,
        response: oneshot::Sender<ApiResult<BoundingBox>>,
    },
    LoadProject {
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    // --- Semantic Assembly / Relation ---
    ListVocabulary(oneshot::Sender<VocabularyInfo>),
    CreateAssembly {
        request: CreateAssemblyRequest,
        response: oneshot::Sender<ApiResult<CreateAssemblyResult>>,
    },
    PreviewSemanticAssemblyFromSelection {
        request: SemanticAssemblyFromSelectionPreviewRequest,
        response: oneshot::Sender<ApiResult<SemanticAssemblyFromSelectionPreview>>,
    },
    CreateSemanticAssemblyFromSelection {
        request: CreateSemanticAssemblyFromSelectionRequest,
        response: oneshot::Sender<ApiResult<CreateSemanticAssemblyFromSelectionResult>>,
    },
    GetAssembly {
        element_id: u64,
        response: oneshot::Sender<ApiResult<AssemblyDetails>>,
    },
    ListAssemblies(oneshot::Sender<Vec<AssemblyEntry>>),
    QueryRelations {
        source: Option<u64>,
        target: Option<u64>,
        relation_type: Option<String>,
        response: oneshot::Sender<Vec<RelationEntry>>,
    },
    ListAssemblyMembers {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<AssemblyMemberEntry>>>,
    },
    // --- Definition / Occurrence ---
    ListDefinitions {
        /// When `true`, `InternalPart` definitions are included in the result.
        include_internal: bool,
        response: oneshot::Sender<Vec<DefinitionEntry>>,
    },
    GetDefinition {
        definition_id: String,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    CreateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    UpdateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    RepresentationDeclare {
        request: RepresentationDeclareRequest,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    RepresentationSetLod {
        request: RepresentationSetLodRequest,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    RepresentationSetUpdatePolicy {
        request: RepresentationSetUpdatePolicyRequest,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    ListDefinitionDrafts(oneshot::Sender<Vec<DefinitionDraftEntry>>),
    GetDefinitionDraft {
        draft_id: String,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    OpenDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    CreateDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    DeriveDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    PatchDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionDraftEntry>>,
    },
    PublishDefinitionDraft {
        draft_id: String,
        response: oneshot::Sender<ApiResult<DefinitionEntry>>,
    },
    ValidateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionValidationResult>>,
    },
    CompileDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionCompileResult>>,
    },
    ExplainDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionExplainResult>>,
    },
    ListDefinitionLibraries(oneshot::Sender<Vec<DefinitionLibraryEntry>>),
    GetDefinitionLibrary {
        library_id: String,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    CreateDefinitionLibrary {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    AddDefinitionToLibrary {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    ImportDefinitionLibrary {
        path: String,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    ExportDefinitionLibrary {
        library_id: String,
        path: String,
        response: oneshot::Sender<ApiResult<String>>,
    },
    ListWorkspaceDefinitionLibraries {
        request: Value,
        response: oneshot::Sender<ApiResult<Vec<DefinitionLibraryEntry>>>,
    },
    CreateWorkspaceDefinitionLibrary {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    ImportWorkspaceDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    UpdateWorkspaceDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    DeleteWorkspaceDefinitionDraft {
        request: Value,
        response: oneshot::Sender<ApiResult<DefinitionLibraryEntry>>,
    },
    InstantiateDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<InstantiateDefinitionResult>>,
    },
    InstantiateHostedDefinition {
        request: Value,
        response: oneshot::Sender<ApiResult<InstantiateDefinitionResult>>,
    },
    PlaceOccurrence {
        request: Value,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    UpdateOccurrenceOverrides {
        element_id: u64,
        overrides: Value,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    SetOccurrenceMaterialOverride {
        request: SetOccurrenceMaterialOverrideRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    ClearOccurrenceMaterialOverride {
        request: ClearOccurrenceMaterialOverrideRequest,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    MakeOccurrenceUnique {
        request: OccurrenceMakeUniqueRequest,
        response: oneshot::Sender<ApiResult<MakeOccurrenceUniqueResult>>,
    },
    ExplainOccurrence {
        element_id: u64,
        response: oneshot::Sender<ApiResult<OccurrenceExplainResult>>,
    },
    ResolveOccurrence {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    // --- Array ---
    ArrayCreateLinear {
        source_id: u64,
        count: u32,
        spacing: [f32; 3],
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ArrayCreatePolar {
        source_id: u64,
        count: u32,
        axis: [f32; 3],
        total_angle_degrees: f32,
        center: [f32; 3],
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ArrayUpdate {
        element_id: u64,
        count: Option<u32>,
        spacing: Option<[f32; 3]>,
        axis: Option<[f32; 3]>,
        total_angle_degrees: Option<f32>,
        center: Option<[f32; 3]>,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    ArrayDissolve {
        element_id: u64,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ArrayGet {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    // --- Mirror ---
    MirrorCreate {
        source_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    MirrorUpdate {
        element_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    MirrorDissolve {
        element_id: u64,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    MirrorGet {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Value>>,
    },
    // --- Named Views ---
    ViewList(oneshot::Sender<Vec<NamedViewInfo>>),
    ViewSave {
        name: String,
        description: Option<String>,
        camera_params: Option<CameraParams>,
        response: oneshot::Sender<ApiResult<NamedViewInfo>>,
    },
    ViewRestore {
        name: String,
        response: oneshot::Sender<ApiResult<NamedViewInfo>>,
    },
    ViewUpdate {
        name: String,
        new_name: Option<String>,
        description: Option<String>,
        camera_params: Option<CameraParams>,
        response: oneshot::Sender<ApiResult<NamedViewInfo>>,
    },
    ViewDelete {
        name: String,
        response: oneshot::Sender<ApiResult<()>>,
    },
    // --- Clipping Planes ---
    ClipPlaneCreate {
        name: String,
        origin: [f32; 3],
        normal: [f32; 3],
        active: bool,
        response: oneshot::Sender<ApiResult<u64>>,
    },
    ClipPlaneUpdate {
        element_id: u64,
        name: Option<String>,
        origin: Option<[f32; 3]>,
        normal: Option<[f32; 3]>,
        active: Option<bool>,
        response: oneshot::Sender<ApiResult<ClipPlaneInfo>>,
    },
    ClipPlaneList(oneshot::Sender<Vec<ClipPlaneInfo>>),
    ClipPlaneToggle {
        element_id: u64,
        active: bool,
        response: oneshot::Sender<ApiResult<ClipPlaneInfo>>,
    },
    // --- Refinement (PP70) ---
    GetRefinementState {
        element_id: u64,
        response: oneshot::Sender<ApiResult<RefinementStateInfo>>,
    },
    GetObligations {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<ObligationInfo>>>,
    },
    ResolveObligation {
        request: ResolveObligationRequest,
        response: oneshot::Sender<ApiResult<ResolveObligationResult>>,
    },
    GetAuthoringProvenance {
        element_id: u64,
        response: oneshot::Sender<ApiResult<AuthoringProvenanceInfo>>,
    },
    GetClaimGrounding {
        element_id: u64,
        path: Option<String>,
        response: oneshot::Sender<ApiResult<Vec<ClaimGroundingEntry>>>,
    },
    PromoteRefinement {
        element_id: u64,
        target_state: String,
        recipe_id: Option<String>,
        overrides: serde_json::Value,
        response: oneshot::Sender<ApiResult<PromoteRefinementResult>>,
    },
    DemoteRefinement {
        element_id: u64,
        target_state: String,
        response: oneshot::Sender<ApiResult<DemoteRefinementResult>>,
    },
    InspectRefinementBranches {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<RefinementBranchApiInfo>>>,
    },
    DiscardRefinementBranch {
        parent_element_id: u64,
        child_element_id: u64,
        response: oneshot::Sender<ApiResult<DiscardRefinementBranchResult>>,
    },
    RunValidation {
        element_id: u64,
        response: oneshot::Sender<ApiResult<Vec<ValidationFindingInfo>>>,
    },
    OccurrenceValidateHostFit {
        request: ValidateHostFitRequest,
        response: oneshot::Sender<ApiResult<HostingValidationResult>>,
    },
    DefinitionValidateHostContract {
        request: ValidateDefinitionHostContractRequest,
        response: oneshot::Sender<ApiResult<HostingValidationResult>>,
    },
    ExplainFinding {
        finding_id: String,
        response: oneshot::Sender<ApiResult<serde_json::Value>>,
    },
    // --- Descriptor discovery (PP71) ---
    ListElementClasses(oneshot::Sender<Vec<ElementClassInfo>>),
    GetCapabilitySnapshot {
        expanded: bool,
        response: oneshot::Sender<CapabilitySnapshotInfo>,
    },
    ListRecipeFamilies {
        element_class: Option<String>,
        include_session_drafts: bool,
        response: oneshot::Sender<Vec<RecipeFamilyInfo>>,
    },
    SelectRecipe {
        element_class: String,
        context: serde_json::Value,
        response: oneshot::Sender<ApiResult<Vec<RecipeRankingInfo>>>,
    },
    DiscoverCuratedPaths {
        request: CuratedPathDiscoveryRequest,
        response: oneshot::Sender<ApiResult<CuratedPathDiscoveryInfo>>,
    },
    InstantiateRecipe {
        request: Box<InstantiateRecipeRequest>,
        response: oneshot::Sender<ApiResult<InstantiateRecipeResult>>,
    },
    // --- Constraint engine (PP74) ---
    ListConstraints {
        scope: Option<String>,
        response: oneshot::Sender<Vec<ConstraintInfo>>,
    },
    RunValidationV2 {
        element_id: Option<u64>,
        response: oneshot::Sender<Vec<ValidationFindingInfo>>,
    },
    ExplainFindingV2 {
        finding_id: String,
        response: oneshot::Sender<ApiResult<serde_json::Value>>,
    },
    PreviewPromotion {
        element_id: u64,
        target_state: String,
        recipe_id: Option<String>,
        overrides: serde_json::Value,
        response: oneshot::Sender<ApiResult<PreviewPromotionResult>>,
    },
    // --- PP75: Catalog providers ---
    ListCatalogProviders(oneshot::Sender<Vec<CatalogProviderInfo>>),
    CatalogQuery {
        provider_id: String,
        filter: serde_json::Value,
        response: oneshot::Sender<ApiResult<Vec<CatalogRowInfo>>>,
    },
    // --- PP76: Generation priors ---
    ListGenerationPriors {
        /// Optional JSON scope-filter object; absent means "all priors".
        scope_filter: Option<serde_json::Value>,
        response: oneshot::Sender<Vec<GenerationPriorInfo>>,
    },
    // --- PP78: Corpus operations ---
    ListCorpusGaps(oneshot::Sender<Vec<CorpusGapInfo>>),
    RequestCorpusExpansion {
        element_class: Option<String>,
        jurisdiction: Option<String>,
        kind: String,
        rationale: String,
        response: oneshot::Sender<ApiResult<CorpusGapInfo>>,
    },
    LookupSourcePassage {
        passage_ref: String,
        response: oneshot::Sender<ApiResult<PassageLookupInfo>>,
    },
    DraftRulePack {
        chunk_id: String,
        element_class: String,
        response: oneshot::Sender<ApiResult<DraftRulePackInfo>>,
    },
    CheckRulePackBacklinks(oneshot::Sender<BacklinkCheckReportInfo>),
    // --- PP92: Session recipe drafts ---
    ListRecipeDrafts {
        target_class: Option<String>,
        status: Option<String>,
        response: oneshot::Sender<ApiResult<Vec<RecipeDraftInfo>>>,
    },
    GetRecipeDraft {
        recipe_draft_id: String,
        response: oneshot::Sender<ApiResult<RecipeDraftInfo>>,
    },
    SaveRecipeDraft {
        request: SaveRecipeDraftRequest,
        response: oneshot::Sender<ApiResult<RecipeDraftInfo>>,
    },
    SetRecipeDraftStatus {
        recipe_draft_id: String,
        status: String,
        response: oneshot::Sender<ApiResult<RecipeDraftInfo>>,
    },
    ListAssemblyPatternDrafts {
        target_type: Option<String>,
        status: Option<String>,
        response: oneshot::Sender<ApiResult<Vec<AssemblyPatternDraftInfo>>>,
    },
    GetAssemblyPatternDraft {
        assembly_pattern_draft_id: String,
        response: oneshot::Sender<ApiResult<AssemblyPatternDraftInfo>>,
    },
    SaveAssemblyPatternDraft {
        request: SaveAssemblyPatternDraftRequest,
        response: oneshot::Sender<ApiResult<AssemblyPatternDraftInfo>>,
    },
    SetAssemblyPatternDraftStatus {
        assembly_pattern_draft_id: String,
        status: String,
        response: oneshot::Sender<ApiResult<AssemblyPatternDraftInfo>>,
    },
    MaterializeLearnedAsset {
        request: MaterializeLearnedAssetRequest,
        response: oneshot::Sender<ApiResult<MaterializeLearnedAssetResult>>,
    },
    // --- Authoring guidance (Slice A of COMPONENT_STRUCTURE) ---
    GetAuthoringGuidance(oneshot::Sender<AuthoringGuidance>),
    ListGuidanceCards {
        task: Option<String>,
        response: oneshot::Sender<Vec<GuidanceCardInfo>>,
    },
    GetGuidanceCard {
        card_id: String,
        response: oneshot::Sender<ApiResult<GuidanceCardInfo>>,
    },
    ListAgentSkills {
        filter: crate::plugins::agent_skills::AgentSkillSearch,
        response: oneshot::Sender<Vec<crate::plugins::agent_skills::AgentSkillSummary>>,
    },
    GetAgentSkill {
        skill_id: String,
        response: oneshot::Sender<ApiResult<crate::plugins::agent_skills::AgentSkill>>,
    },
    SaveAgentSkillDraft {
        request: crate::plugins::agent_skills::AgentSkillDraftRequest,
        response: oneshot::Sender<ApiResult<crate::plugins::agent_skills::AgentSkill>>,
    },
    // --- Semantic Procedural Session (ADR-051, PP-SPS-3) ---
    ProceduralSessionCreate {
        request: crate::plugins::procedural_session_mcp::SessionCreateRequest,
        response: oneshot::Sender<crate::plugins::procedural_session_mcp::SessionCreateResponse>,
    },
    ProceduralSessionEval {
        request: crate::plugins::procedural_session_mcp::SessionEvalRequest,
        response:
            oneshot::Sender<Result<crate::curation::EvalReport, crate::curation::SessionError>>,
    },
    ProceduralSessionSnapshot {
        request: crate::plugins::procedural_session_mcp::SessionSnapshotRequest,
        response: oneshot::Sender<
            Result<crate::curation::SessionSnapshot, crate::curation::SessionError>,
        >,
    },
    ProceduralSessionCommit {
        request: crate::plugins::procedural_session_mcp::SessionCommitRequest,
        response:
            oneshot::Sender<Result<crate::curation::CommitReport, crate::curation::SessionError>>,
    },
    ProceduralSessionExport {
        request: crate::plugins::procedural_session_mcp::SessionExportRequest,
        response:
            oneshot::Sender<Result<crate::curation::ExportHandle, crate::curation::SessionError>>,
    },
    // --- Parametric components (RELATIONAL_PARAMETRIC_SUBSTRATE, PP-RPS-7 UX) ---
    ParametricListTypes {
        response: oneshot::Sender<Vec<crate::plugins::parametric_mcp::ParametricTypeInfo>>,
    },
    ParametricCreate {
        request: crate::plugins::parametric_mcp::CreateParametricRequest,
        response: oneshot::Sender<
            Result<crate::plugins::parametric_mcp::CreateParametricResponse, String>,
        >,
    },
    ParametricInspect {
        request: crate::plugins::parametric_mcp::InspectParametricRequest,
        response: oneshot::Sender<Result<crate::relational::registry::ParametricSnapshot, String>>,
    },
    ParametricSetDriver {
        request: crate::plugins::parametric_mcp::SetParametricDriverRequest,
        response:
            oneshot::Sender<Result<crate::plugins::parametric_mcp::SetDriverResponse, String>>,
    },
    ParametricTransform {
        request: crate::plugins::parametric_mcp::ParametricTransformRequest,
        response: oneshot::Sender<Result<crate::relational::transform::TransformOutcome, String>>,
    },
    ParametricExplain {
        request: crate::plugins::parametric_mcp::ExplainParametricRequest,
        response: oneshot::Sender<
            Result<crate::plugins::parametric_mcp::ExplainParametricResponse, String>,
        >,
    },
    // --- Knowledge persistence (Change-2 / Change-3 / Change-7) ---
    InstallRecipeFromSessionExport {
        request: InstallRecipeFromSessionExportRequest,
        response: oneshot::Sender<Result<InstallRecipeResult, String>>,
    },
    ListPersistedRecipes {
        response: oneshot::Sender<Vec<PersistedRecipeInfo>>,
    },
    AcquireCorpusPassage {
        request: AcquireCorpusPassageRequest,
        response: oneshot::Sender<Result<AcquireCorpusPassageResult, String>>,
    },
    // --- Geometric validators (Item C) ---
    GetWorldAabb {
        request: GetWorldAabbRequest,
        response: oneshot::Sender<ApiResult<GetWorldAabbResult>>,
    },
    CheckOverlaps {
        request: CheckOverlapsRequest,
        response: oneshot::Sender<ApiResult<CheckOverlapsResult>>,
    },
    CheckFloating {
        request: CheckFloatingRequest,
        response: oneshot::Sender<ApiResult<CheckFloatingResult>>,
    },
    CheckClearance {
        request: CheckClearanceRequest,
        response: oneshot::Sender<ApiResult<CheckClearanceResult>>,
    },
}

// -----------------------------------------------------------------------
// resolve_obligation DTOs
// -----------------------------------------------------------------------

/// How an obligation is resolved by the agent.
///
/// Exactly one variant should be set.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ObligationResolution {
    /// Obligation is satisfied by an existing child element (identified by
    /// `element_id`).
    SatisfiedBy { element_id: u64 },
    /// Obligation is intentionally deferred with a human-readable reason.
    Deferred { reason: String },
    /// Obligation is waived (explicitly out of scope) with a rationale.
    Waived { rationale: String },
}

/// Request payload for `resolve_obligation`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ResolveObligationRequest {
    /// Element-id of the entity whose obligation is being resolved.
    pub element_id: u64,
    /// Id of the obligation within the entity's `ObligationSet`.
    pub obligation_id: String,
    /// How the obligation is resolved.
    pub resolution: ObligationResolution,
}

/// Result returned by `resolve_obligation`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ResolveObligationResult {
    pub element_id: u64,
    pub obligation_id: String,
    /// Updated status string after the resolution (e.g. `"SatisfiedBy:42"`).
    pub new_status: String,
    /// Full updated obligation set for the entity.
    pub obligations: Vec<ObligationInfo>,
}

// -----------------------------------------------------------------------
// Change-2: install_recipe_from_session_export DTOs
// -----------------------------------------------------------------------

/// Install an exported `AuthoringScript` from a procedural session as a
/// durable, executable [`RecipeArtifact`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct InstallRecipeFromSessionExportRequest {
    /// Session id that holds the exported script.
    pub session_id: String,
    /// Export handle asset-id returned by `procedural_session.export`.
    pub export_handle: String,
    /// Stable family id for the new recipe (e.g. `"my_stair_flight"`).
    pub family_id: String,
    /// Human-readable label.
    pub label: String,
    /// Short description shown to agents in discovery.
    pub description: String,
    /// Supported refinement levels (e.g. `["Schematic", "Constructible"]`).
    #[serde(default)]
    pub supported_refinement_levels: Vec<String>,
    /// Element class this recipe targets (e.g. `"stair_flight"`).
    /// Optional — if omitted, stored as empty string.
    #[serde(default)]
    pub target_class: Option<String>,
    /// Scope: `"Session"` (in-memory only) or `"Project"` (persisted to disk).
    /// Defaults to `"Project"`.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Result returned by `install_recipe_from_session_export`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct InstallRecipeResult {
    /// The family id the recipe was registered under.
    pub family_id: String,
    /// `"Session"` or `"User"`.
    pub scope: String,
    /// Disk path written, if `scope == "User"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_path: Option<String>,
    /// Refinement levels the recipe claims to support.
    pub supported_refinement_levels: Vec<String>,
}

/// Summary of a persisted recipe returned by `list_persisted_recipes`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct PersistedRecipeInfo {
    pub family_id: String,
    pub asset_id: String,
    pub label: String,
    pub description: String,
    pub body_kind: String,
    pub supported_refinement_levels: Vec<String>,
}

// -----------------------------------------------------------------------
// Change-7: acquire_corpus_passage DTOs
// -----------------------------------------------------------------------

/// Store a plain-text passage extracted from an external source into the
/// `CorpusPassageRegistry` and optionally persist it to disk.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AcquireCorpusPassageRequest {
    /// Stable reference id for the passage (e.g. `"IRC_2021_R311_7_8_1"`).
    pub passage_ref: String,
    /// Citation / source description.
    pub citation: String,
    /// Full plain-text of the passage.
    pub text: String,
    /// URL pointing to the source document, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// ISO 3166-1 alpha-2 country or regional jurisdiction code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    /// Free-text classification tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    /// License: `"cc0"`, `"public_record"`, `"boverket_public"`,
    /// `"icc_cite_only"`, `"standards_body_citation_only"`.
    /// Defaults to `"public_record"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Whether to persist the passage to disk (`true`, default) or only
    /// register it for this session (`false`).
    #[serde(default = "default_persist")]
    pub persist: bool,
}

fn default_persist() -> bool {
    true
}

/// Result returned by `acquire_corpus_passage`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct AcquireCorpusPassageResult {
    pub passage_ref: String,
    pub stored: bool,
    pub registry_size: usize,
    /// Disk path written, if persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted_path: Option<String>,
}

#[cfg(feature = "model-api")]
pub(super) fn poll_model_api_requests(world: &mut World) {
    loop {
        let Some(request) = next_model_api_request(world) else {
            break;
        };
        handle_model_api_request(world, request);
    }
}

#[cfg(feature = "model-api")]
fn next_model_api_request(world: &World) -> Option<ModelApiRequest> {
    let receiver = world.get_resource::<ModelApiReceiver>()?;
    let guard = receiver.0.lock().ok()?;
    match guard.try_recv() {
        Ok(request) => Some(request),
        Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => None,
    }
}

#[cfg(feature = "model-api")]
pub(super) type ApiResult<T> = Result<T, String>;

#[cfg(feature = "model-api")]
pub(super) fn handle_model_api_request(world: &mut World, request: ModelApiRequest) {
    match request {
        ModelApiRequest::GetInstanceInfo(response) => {
            let _ = response.send(handle_get_instance_info(world));
        }
        ModelApiRequest::ListEntities(response) => {
            let _ = response.send(list_entities(world));
        }
        ModelApiRequest::GetEntity {
            element_id,
            response,
        } => {
            let _ = response.send(get_entity_snapshot(world, ElementId(element_id)));
        }
        ModelApiRequest::GetEntityDetails {
            element_id,
            response,
        } => {
            let _ = response.send(get_entity_details(world, ElementId(element_id)));
        }
        ModelApiRequest::GetEntitiesDetails {
            element_ids,
            response,
        } => {
            let _ = response.send(get_entities_details(world, element_ids));
        }
        ModelApiRequest::ModelSummary(response) => {
            let _ = response.send(model_summary(world));
        }
        ModelApiRequest::OutlineTree(response) => {
            let _ = response.send(crate::plugins::outliner::outline_tree_json(world));
        }
        ModelApiRequest::ListImporters(response) => {
            let importers = world.resource::<ImportRegistry>().list_importers();
            let _ = response.send(importers);
        }
        ModelApiRequest::CreateEntity { json, response } => {
            let _ = response.send(handle_create_entity(world, json));
        }
        ModelApiRequest::ImportFile {
            path,
            format_hint,
            response,
        } => {
            let _ = response.send(handle_import_file(world, &path, format_hint.as_deref()));
        }
        ModelApiRequest::AcceptSemanticShadowCandidate { request, response } => {
            let _ = response.send(handle_accept_semantic_shadow_candidate(world, request));
        }
        ModelApiRequest::ListHandles {
            element_id,
            response,
        } => {
            let _ = response.send(handle_list_handles(world, element_id));
        }
        ModelApiRequest::BimPropertySetGet {
            element_id,
            set_name,
            property_name,
            response,
        } => {
            let _ = response.send(handle_bim_property_set_get(
                world,
                element_id,
                &set_name,
                &property_name,
            ));
        }
        ModelApiRequest::BimPropertySetSet {
            element_id,
            definition_id,
            set_name,
            property_name,
            value,
            response,
        } => {
            let _ = response.send(handle_bim_property_set_set(
                world,
                element_id,
                &definition_id,
                &set_name,
                &property_name,
                value,
            ));
        }
        ModelApiRequest::BimExchangeIdentityAssign {
            element_id,
            system,
            exchange_id,
            response,
        } => {
            let _ = response.send(handle_bim_exchange_identity_assign(
                world,
                element_id,
                &system,
                &exchange_id,
            ));
        }
        ModelApiRequest::BimExchangeIdentityGet {
            element_id,
            system,
            response,
        } => {
            let _ = response.send(handle_bim_exchange_identity_get(world, element_id, &system));
        }
        ModelApiRequest::BimExchangeIdentityList {
            element_id,
            response,
        } => {
            let _ = response.send(handle_bim_exchange_identity_list(world, element_id));
        }
        ModelApiRequest::BimVoidDeclareForDefinition {
            definition_id,
            declaration,
            response,
        } => {
            let _ = response.send(handle_bim_void_declare_for_definition(
                world,
                &definition_id,
                declaration,
            ));
        }
        ModelApiRequest::BimVoidPlanPlacement {
            filling_definition,
            host_element_id,
            filling_element_id,
            response,
        } => {
            let _ = response.send(handle_bim_void_plan_placement(
                world,
                &filling_definition,
                host_element_id,
                filling_element_id,
            ));
        }
        ModelApiRequest::BimSpatialAssign {
            child_element_id,
            container_element_id,
            container_kind,
            response,
        } => {
            let _ = response.send(handle_bim_spatial_assign(
                world,
                child_element_id,
                container_element_id,
                &container_kind,
            ));
        }
        ModelApiRequest::BimSpatialListKindRegistry { response } => {
            let _ = response.send(handle_bim_spatial_list_kind_registry(world));
        }
        ModelApiRequest::GetDocumentProperties(response) => {
            let props = world.resource::<DocumentProperties>();
            let json = serde_json::to_value(props.clone()).unwrap_or_default();
            let _ = response.send(json);
        }
        ModelApiRequest::SetDocumentProperties { partial, response } => {
            let _ = response.send(handle_set_document_properties(world, partial));
        }
        ModelApiRequest::ListToolbars(response) => {
            let _ = response.send(list_toolbars(world));
        }
        ModelApiRequest::SetToolbarLayout { updates, response } => {
            let _ = response.send(handle_set_toolbar_layout(world, updates));
        }
        ModelApiRequest::ListCommands(response) => {
            let schema = world
                .resource::<crate::plugins::command_registry::CommandRegistry>()
                .export_schema();
            let _ = response.send(schema);
        }
        ModelApiRequest::InvokeCommand {
            command_id,
            parameters,
            response,
        } => {
            let _ = response.send(handle_invoke_command(world, &command_id, parameters));
        }
        ModelApiRequest::PrepareSiteSurface { request, response } => {
            let _ = response.send(handle_prepare_site_surface(world, request));
        }
        ModelApiRequest::TerrainCutFillAnalysis { request, response } => {
            let _ = response.send(handle_terrain_cut_fill_analysis(world, request));
        }
        ModelApiRequest::TerrainElevationAt { request, response } => {
            let _ = response.send(handle_terrain_elevation_at(world, request));
        }
        ModelApiRequest::GetEditingContext(response) => {
            let _ = response.send(get_editing_context(world));
        }
        ModelApiRequest::EnterGroup {
            element_id,
            response,
        } => {
            let _ = response.send(handle_enter_group(world, element_id));
        }
        ModelApiRequest::ExitGroup(response) => {
            let _ = response.send(handle_exit_group(world));
        }
        ModelApiRequest::ListGroupMembers {
            element_id,
            response,
        } => {
            let _ = response.send(handle_list_group_members(world, element_id));
        }
        // --- Layer Management ---
        ModelApiRequest::ListLayers(response) => {
            let _ = response.send(handle_list_layers(world));
        }
        ModelApiRequest::SetLayerVisibility {
            name,
            visible,
            response,
        } => {
            let _ = response.send(handle_set_layer_visibility(world, &name, visible));
        }
        ModelApiRequest::SetLayerLocked {
            name,
            locked,
            response,
        } => {
            let _ = response.send(handle_set_layer_locked(world, &name, locked));
        }
        ModelApiRequest::AssignLayer {
            element_id,
            layer_name,
            response,
        } => {
            let _ = response.send(handle_assign_layer(world, element_id, &layer_name));
        }
        ModelApiRequest::CreateLayer { name, response } => {
            let _ = response.send(handle_create_layer(world, &name));
        }
        ModelApiRequest::RenameLayer {
            old_name,
            new_name,
            response,
        } => {
            let _ = response.send(handle_rename_layer(world, &old_name, &new_name));
        }
        ModelApiRequest::DeleteLayer { name, response } => {
            let _ = response.send(handle_delete_layer(world, &name));
        }
        // --- Dependency Graph (read-only) ---
        ModelApiRequest::DependencyGraph(response) => {
            let _ = response
                .send(crate::plugins::modeling::dependency_graph::dependency_graph_json(world));
        }
        ModelApiRequest::EntityDependencies {
            element_id,
            response,
        } => {
            let _ = response.send(
                crate::plugins::modeling::dependency_graph::entity_dependencies_json(
                    world, element_id,
                ),
            );
        }
        // --- Materials ---
        ModelApiRequest::ListMaterials(response) => {
            let _ = response.send(handle_list_materials(world));
        }
        ModelApiRequest::GetMaterial { id, response } => {
            let _ = response.send(handle_get_material(world, &id));
        }
        ModelApiRequest::CreateMaterial { request, response } => {
            let _ = response.send(handle_create_material(world, request));
        }
        ModelApiRequest::UpdateMaterial {
            id,
            request,
            response,
        } => {
            let _ = response.send(handle_update_material(world, &id, request));
        }
        ModelApiRequest::DeleteMaterial { id, response } => {
            let _ = response.send(handle_delete_material(world, &id));
        }
        ModelApiRequest::ApplyMaterial { request, response } => {
            let _ = response.send(handle_apply_material(world, request));
        }
        ModelApiRequest::AssignMaterial { request, response } => {
            let _ = response.send(handle_assign_material(world, request));
        }
        ModelApiRequest::RemoveMaterial {
            element_ids,
            response,
        } => {
            let _ = response.send(handle_remove_material(world, element_ids));
        }
        ModelApiRequest::GetMaterialAssignment {
            element_id,
            response,
        } => {
            let _ = response.send(handle_get_material_assignment(world, element_id));
        }
        ModelApiRequest::SetMaterialAssignment { request, response } => {
            let _ = response.send(handle_set_material_assignment(world, request));
        }
        ModelApiRequest::GetTextureMapping { request, response } => {
            let _ = response.send(handle_get_texture_mapping(world, request));
        }
        ModelApiRequest::UpdateTextureMapping { request, response } => {
            let _ = response.send(handle_update_texture_mapping(world, request));
        }
        ModelApiRequest::ResetTextureMapping { request, response } => {
            let _ = response.send(handle_reset_texture_mapping(world, request));
        }
        ModelApiRequest::BimMaterialAssignLayered { request, response } => {
            let _ = response.send(handle_bim_material_assign_layered(world, request));
        }
        ModelApiRequest::BimMaterialAssignConstituents { request, response } => {
            let _ = response.send(handle_bim_material_assign_constituents(world, request));
        }
        ModelApiRequest::BimMaterialGetEffective { request, response } => {
            let _ = response.send(handle_bim_material_get_effective(world, request));
        }
        ModelApiRequest::QuantitySet { request, response } => {
            let _ = response.send(handle_quantity_set(world, request));
        }
        ModelApiRequest::QuantityGet { request, response } => {
            let _ = response.send(handle_quantity_get(world, request));
        }
        ModelApiRequest::QuantityListProvenance { request, response } => {
            let _ = response.send(handle_quantity_list_provenance(world, request));
        }
        ModelApiRequest::QuantityCheckInvariants { request, response } => {
            let _ = response.send(handle_quantity_check_invariants(world, request));
        }
        ModelApiRequest::ListMaterialSpecs { filter, response } => {
            let _ = response.send(handle_list_material_specs(world, filter));
        }
        ModelApiRequest::GetMaterialSpec { asset_id, response } => {
            let _ = response.send(handle_get_material_spec(world, &asset_id));
        }
        ModelApiRequest::CreateMaterialSpec { request, response } => {
            let _ = response.send(handle_create_material_spec(world, request));
        }
        ModelApiRequest::UpdateMaterialSpec {
            asset_id,
            body,
            rationale,
            response,
        } => {
            let _ = response.send(handle_update_material_spec(
                world, &asset_id, body, rationale,
            ));
        }
        ModelApiRequest::SaveMaterialSpec {
            asset_id,
            scope,
            response,
        } => {
            let _ = response.send(handle_save_material_spec(world, &asset_id, &scope));
        }
        ModelApiRequest::PublishMaterialSpec { asset_id, response } => {
            let _ = response.send(handle_publish_material_spec(world, &asset_id));
        }
        ModelApiRequest::DeleteMaterialSpec { asset_id, response } => {
            let _ = response.send(handle_delete_material_spec(world, &asset_id));
        }
        ModelApiRequest::GetLightingScene(response) => {
            let _ = response.send(handle_get_lighting_scene(world));
        }
        ModelApiRequest::ListLights(response) => {
            let _ = response.send(handle_list_lights(world));
        }
        ModelApiRequest::CreateLight { request, response } => {
            let _ = response.send(handle_create_light(world, request));
        }
        ModelApiRequest::UpdateLight { request, response } => {
            let _ = response.send(handle_update_light(world, request));
        }
        ModelApiRequest::DeleteLight {
            element_id,
            response,
        } => {
            let _ = response.send(handle_delete_light(world, element_id));
        }
        ModelApiRequest::SetAmbientLight { request, response } => {
            let _ = response.send(handle_set_ambient_light(world, request));
        }
        ModelApiRequest::RestoreDefaultLightRig { response } => {
            let _ = response.send(handle_restore_default_light_rig(world));
        }
        ModelApiRequest::GetRenderSettings(response) => {
            let _ = response.send(handle_get_render_settings(world));
        }
        ModelApiRequest::GetPerfStats(response) => {
            let _ = response.send(handle_get_perf_stats(world));
        }
        ModelApiRequest::SetRenderSettings { request, response } => {
            let _ = response.send(handle_set_render_settings(world, request));
        }
        ModelApiRequest::GetCamera(response) => {
            let _ = response.send(handle_get_camera(world));
        }
        ModelApiRequest::SetCamera { params, response } => {
            let _ = response.send(handle_set_camera(world, params));
        }
        // --- Selection ---
        ModelApiRequest::GetSelection(response) => {
            let _ = response.send(handle_get_selection(world));
        }
        ModelApiRequest::SetSelection {
            element_ids,
            response,
        } => {
            let _ = response.send(handle_set_selection(world, element_ids));
        }
        ModelApiRequest::ListSubobjects {
            element_id,
            response,
        } => {
            let _ = response.send(handle_list_subobjects(world, element_id));
        }
        ModelApiRequest::GetSubobjectSelection { response } => {
            let _ = response.send(handle_get_subobject_selection(world));
        }
        ModelApiRequest::SetSubobjectSelection { refs, response } => {
            let _ = response.send(handle_set_subobject_selection(world, refs));
        }
        ModelApiRequest::ExpandSubobjectSelection {
            reference,
            mode,
            response,
        } => {
            let _ = response.send(handle_expand_subobject_selection(world, reference, &mode));
        }
        ModelApiRequest::ApplySubobjectEdit {
            reference,
            operation,
            parameters,
            response,
        } => {
            let _ = response.send(handle_apply_subobject_edit(
                world, reference, operation, parameters,
            ));
        }
        // --- Live UX harness ---
        ModelApiRequest::UxObserve { response } => {
            let _ = response.send(crate::plugins::ux_harness::observe_ux(world));
        }
        ModelApiRequest::UxMovePointer { request, response } => {
            let _ = response.send(crate::plugins::ux_harness::enqueue_pointer_move(
                world, request,
            ));
        }
        ModelApiRequest::UxClick { request, response } => {
            let _ = response.send(crate::plugins::ux_harness::enqueue_click(world, request));
        }
        ModelApiRequest::UxDrag { request, response } => {
            let _ = response.send(crate::plugins::ux_harness::enqueue_drag(world, request));
        }
        ModelApiRequest::UxPressKey { request, response } => {
            let _ = response.send(crate::plugins::ux_harness::enqueue_press_key(
                world, request,
            ));
        }
        ModelApiRequest::AlignPreview { request, response } => {
            let _ = response.send(handle_align_preview(world, request));
        }
        ModelApiRequest::AlignExecute { request, response } => {
            let _ = response.send(handle_align_execute(world, request));
        }
        ModelApiRequest::DistributePreview { request, response } => {
            let _ = response.send(handle_distribute_preview(world, request));
        }
        ModelApiRequest::DistributeExecute { request, response } => {
            let _ = response.send(handle_distribute_execute(world, request));
        }
        // --- Face Subdivision ---
        ModelApiRequest::SplitBoxFace {
            element_id,
            face_id,
            split_position,
            response,
        } => {
            let _ = response.send(handle_split_box_face(
                world,
                element_id,
                face_id,
                split_position,
            ));
        }
        // --- Screenshot ---
        ModelApiRequest::TakeScreenshot {
            path,
            include_ui,
            response,
        } => {
            let _ = response.send(handle_take_screenshot(world, &path, include_ui));
        }
        ModelApiRequest::ExportDrawing { path, response } => {
            let _ = response.send(handle_export_drawing(world, &path));
        }
        ModelApiRequest::ExportDraftingSheet {
            path,
            scale_denominator,
            response,
        } => {
            let _ = response.send(handle_export_drafting_sheet(
                world,
                &path,
                scale_denominator,
            ));
        }
        ModelApiRequest::PlaceSheetDimension { request, response } => {
            let _ = response.send(handle_place_sheet_dimension(world, request));
        }
        ModelApiRequest::PlaceDimensionBetweenHandles { request, response } => {
            let _ = response.send(handle_place_dimension_between_handles(world, request));
        }
        ModelApiRequest::SaveProject { path, response } => {
            let _ = response.send(handle_save_project(world, &path));
        }
        ModelApiRequest::FrameModel { response } => {
            let _ = response.send(handle_frame_model(world));
        }
        ModelApiRequest::FrameEntities {
            element_ids,
            response,
        } => {
            let _ = response.send(handle_frame_entities(world, &element_ids));
        }
        ModelApiRequest::LoadProject { path, response } => {
            let _ = response.send(handle_load_project(world, &path));
        }
        // --- Semantic Assembly / Relation ---
        ModelApiRequest::ListVocabulary(response) => {
            let _ = response.send(handle_list_vocabulary(world));
        }
        ModelApiRequest::CreateAssembly { request, response } => {
            let _ = response.send(handle_create_assembly(world, request));
        }
        ModelApiRequest::PreviewSemanticAssemblyFromSelection { request, response } => {
            let _ = response.send(handle_preview_semantic_assembly_from_selection(
                world, request,
            ));
        }
        ModelApiRequest::CreateSemanticAssemblyFromSelection { request, response } => {
            let _ = response.send(handle_create_semantic_assembly_from_selection(
                world, request,
            ));
        }
        ModelApiRequest::GetAssembly {
            element_id,
            response,
        } => {
            let _ = response.send(handle_get_assembly(world, element_id));
        }
        ModelApiRequest::ListAssemblies(response) => {
            let _ = response.send(handle_list_assemblies(world));
        }
        ModelApiRequest::QueryRelations {
            source,
            target,
            relation_type,
            response,
        } => {
            let _ = response.send(handle_query_relations(world, source, target, relation_type));
        }
        ModelApiRequest::ListAssemblyMembers {
            element_id,
            response,
        } => {
            let _ = response.send(handle_list_assembly_members(world, element_id));
        }
        ModelApiRequest::ListDefinitions {
            include_internal,
            response,
        } => {
            let _ = response.send(handle_list_definitions_filtered(world, include_internal));
        }
        ModelApiRequest::GetDefinition {
            definition_id,
            response,
        } => {
            let _ = response.send(handle_get_definition(world, definition_id));
        }
        ModelApiRequest::CreateDefinition { request, response } => {
            let _ = response.send(handle_create_definition(world, request));
        }
        ModelApiRequest::UpdateDefinition { request, response } => {
            let _ = response.send(handle_update_definition(world, request));
        }
        ModelApiRequest::RepresentationDeclare { request, response } => {
            let _ = response.send(handle_representation_declare(world, request));
        }
        ModelApiRequest::RepresentationSetLod { request, response } => {
            let _ = response.send(handle_representation_set_lod(world, request));
        }
        ModelApiRequest::RepresentationSetUpdatePolicy { request, response } => {
            let _ = response.send(handle_representation_set_update_policy(world, request));
        }
        ModelApiRequest::ListDefinitionDrafts(response) => {
            let _ = response.send(handle_list_definition_drafts(world));
        }
        ModelApiRequest::GetDefinitionDraft { draft_id, response } => {
            let _ = response.send(handle_get_definition_draft(world, draft_id));
        }
        ModelApiRequest::OpenDefinitionDraft { request, response } => {
            let _ = response.send(handle_open_definition_draft(world, request));
        }
        ModelApiRequest::CreateDefinitionDraft { request, response } => {
            let _ = response.send(handle_create_definition_draft(world, request));
        }
        ModelApiRequest::DeriveDefinitionDraft { request, response } => {
            let _ = response.send(handle_derive_definition_draft(world, request));
        }
        ModelApiRequest::PatchDefinitionDraft { request, response } => {
            let _ = response.send(handle_patch_definition_draft(world, request));
        }
        ModelApiRequest::PublishDefinitionDraft { draft_id, response } => {
            let _ = response.send(handle_publish_definition_draft(world, draft_id));
        }
        ModelApiRequest::ValidateDefinition { request, response } => {
            let _ = response.send(handle_validate_definition(world, request));
        }
        ModelApiRequest::CompileDefinition { request, response } => {
            let _ = response.send(handle_compile_definition(world, request));
        }
        ModelApiRequest::ExplainDefinition { request, response } => {
            let _ = response.send(handle_explain_definition(world, request));
        }
        ModelApiRequest::ListDefinitionLibraries(response) => {
            let _ = response.send(handle_list_definition_libraries(world));
        }
        ModelApiRequest::GetDefinitionLibrary {
            library_id,
            response,
        } => {
            let _ = response.send(handle_get_definition_library(world, library_id));
        }
        ModelApiRequest::CreateDefinitionLibrary { request, response } => {
            let _ = response.send(handle_create_definition_library(world, request));
        }
        ModelApiRequest::AddDefinitionToLibrary { request, response } => {
            let _ = response.send(handle_add_definition_to_library(world, request));
        }
        ModelApiRequest::ImportDefinitionLibrary { path, response } => {
            let _ = response.send(handle_import_definition_library(world, &path));
        }
        ModelApiRequest::ExportDefinitionLibrary {
            library_id,
            path,
            response,
        } => {
            let _ = response.send(handle_export_definition_library(world, &library_id, &path));
        }
        ModelApiRequest::ListWorkspaceDefinitionLibraries { request, response } => {
            let _ = response.send(handle_list_workspace_definition_libraries(world, request));
        }
        ModelApiRequest::CreateWorkspaceDefinitionLibrary { request, response } => {
            let _ = response.send(handle_create_workspace_definition_library(world, request));
        }
        ModelApiRequest::ImportWorkspaceDefinitionDraft { request, response } => {
            let _ = response.send(handle_import_workspace_definition_draft(world, request));
        }
        ModelApiRequest::UpdateWorkspaceDefinitionDraft { request, response } => {
            let _ = response.send(handle_update_workspace_definition_draft(world, request));
        }
        ModelApiRequest::DeleteWorkspaceDefinitionDraft { request, response } => {
            let _ = response.send(handle_delete_workspace_definition_draft(world, request));
        }
        ModelApiRequest::InstantiateDefinition { request, response } => {
            let _ = response.send(handle_instantiate_definition(world, request));
        }
        ModelApiRequest::InstantiateHostedDefinition { request, response } => {
            let _ = response.send(handle_instantiate_hosted_definition(world, request));
        }
        ModelApiRequest::PlaceOccurrence { request, response } => {
            let _ = response.send(handle_place_occurrence(world, request));
        }
        ModelApiRequest::UpdateOccurrenceOverrides {
            element_id,
            overrides,
            response,
        } => {
            let _ = response.send(handle_update_occurrence_overrides(
                world, element_id, overrides,
            ));
        }
        ModelApiRequest::SetOccurrenceMaterialOverride { request, response } => {
            let _ = response.send(handle_set_occurrence_material_override(world, request));
        }
        ModelApiRequest::ClearOccurrenceMaterialOverride { request, response } => {
            let _ = response.send(handle_clear_occurrence_material_override(world, request));
        }
        ModelApiRequest::MakeOccurrenceUnique { request, response } => {
            let _ = response.send(handle_make_occurrence_unique(world, request));
        }
        ModelApiRequest::ExplainOccurrence {
            element_id,
            response,
        } => {
            let _ = response.send(handle_explain_occurrence(world, element_id));
        }
        ModelApiRequest::ResolveOccurrence {
            element_id,
            response,
        } => {
            let _ = response.send(handle_resolve_occurrence(world, element_id));
        }
        // --- Array ---
        ModelApiRequest::ArrayCreateLinear {
            source_id,
            count,
            spacing,
            response,
        } => {
            let _ = response.send(handle_array_create_linear(world, source_id, count, spacing));
        }
        ModelApiRequest::ArrayCreatePolar {
            source_id,
            count,
            axis,
            total_angle_degrees,
            center,
            response,
        } => {
            let _ = response.send(handle_array_create_polar(
                world,
                source_id,
                count,
                axis,
                total_angle_degrees,
                center,
            ));
        }
        ModelApiRequest::ArrayUpdate {
            element_id,
            count,
            spacing,
            axis,
            total_angle_degrees,
            center,
            response,
        } => {
            let _ = response.send(handle_array_update(
                world,
                element_id,
                count,
                spacing,
                axis,
                total_angle_degrees,
                center,
            ));
        }
        ModelApiRequest::ArrayDissolve {
            element_id,
            response,
        } => {
            let _ = response.send(handle_array_dissolve(world, element_id));
        }
        ModelApiRequest::ArrayGet {
            element_id,
            response,
        } => {
            let _ = response.send(handle_array_get(world, element_id));
        }
        // --- Mirror ---
        ModelApiRequest::MirrorCreate {
            source_id,
            plane_str,
            plane_origin,
            plane_normal,
            merge,
            response,
        } => {
            let _ = response.send(handle_mirror_create(
                world,
                source_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
            ));
        }
        ModelApiRequest::MirrorUpdate {
            element_id,
            plane_str,
            plane_origin,
            plane_normal,
            merge,
            response,
        } => {
            let _ = response.send(handle_mirror_update(
                world,
                element_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
            ));
        }
        ModelApiRequest::MirrorDissolve {
            element_id,
            response,
        } => {
            let _ = response.send(handle_mirror_dissolve(world, element_id));
        }
        ModelApiRequest::MirrorGet {
            element_id,
            response,
        } => {
            let _ = response.send(handle_mirror_get(world, element_id));
        }
        // --- Named Views ---
        ModelApiRequest::ViewList(response) => {
            let _ = response.send(handle_view_list(world));
        }
        ModelApiRequest::ViewSave {
            name,
            description,
            camera_params,
            response,
        } => {
            let _ = response.send(handle_view_save(world, name, description, camera_params));
        }
        ModelApiRequest::ViewRestore { name, response } => {
            let _ = response.send(handle_view_restore(world, name));
        }
        ModelApiRequest::ViewUpdate {
            name,
            new_name,
            description,
            camera_params,
            response,
        } => {
            let _ = response.send(handle_view_update(
                world,
                name,
                new_name,
                description,
                camera_params,
            ));
        }
        ModelApiRequest::ViewDelete { name, response } => {
            let _ = response.send(handle_view_delete(world, name));
        }
        // --- Clipping Planes ---
        ModelApiRequest::ClipPlaneCreate {
            name,
            origin,
            normal,
            active,
            response,
        } => {
            let _ = response.send(handle_clip_plane_create(
                world, name, origin, normal, active,
            ));
        }
        ModelApiRequest::ClipPlaneUpdate {
            element_id,
            name,
            origin,
            normal,
            active,
            response,
        } => {
            let _ = response.send(handle_clip_plane_update(
                world, element_id, name, origin, normal, active,
            ));
        }
        ModelApiRequest::ClipPlaneList(response) => {
            let _ = response.send(handle_clip_plane_list(world));
        }
        ModelApiRequest::ClipPlaneToggle {
            element_id,
            active,
            response,
        } => {
            let _ = response.send(handle_clip_plane_toggle(world, element_id, active));
        }
        // --- Refinement (PP70) ---
        ModelApiRequest::GetRefinementState {
            element_id,
            response,
        } => {
            let _ = response.send(handle_get_refinement_state(world, element_id));
        }
        ModelApiRequest::GetObligations {
            element_id,
            response,
        } => {
            let _ = response.send(handle_get_obligations(world, element_id));
        }
        ModelApiRequest::ResolveObligation { request, response } => {
            let _ = response.send(handle_resolve_obligation(world, request));
        }
        ModelApiRequest::GetAuthoringProvenance {
            element_id,
            response,
        } => {
            let _ = response.send(handle_get_authoring_provenance(world, element_id));
        }
        ModelApiRequest::GetClaimGrounding {
            element_id,
            path,
            response,
        } => {
            let _ = response.send(handle_get_claim_grounding(world, element_id, path));
        }
        ModelApiRequest::PromoteRefinement {
            element_id,
            target_state,
            recipe_id,
            overrides,
            response,
        } => {
            let _ = response.send(handle_promote_refinement(
                world,
                element_id,
                target_state,
                recipe_id,
                overrides,
            ));
        }
        ModelApiRequest::DemoteRefinement {
            element_id,
            target_state,
            response,
        } => {
            let _ = response.send(handle_demote_refinement(world, element_id, target_state));
        }
        ModelApiRequest::InspectRefinementBranches {
            element_id,
            response,
        } => {
            let _ = response.send(handle_inspect_refinement_branches(world, element_id));
        }
        ModelApiRequest::DiscardRefinementBranch {
            parent_element_id,
            child_element_id,
            response,
        } => {
            let _ = response.send(handle_discard_refinement_branch(
                world,
                parent_element_id,
                child_element_id,
            ));
        }
        ModelApiRequest::RunValidation {
            element_id,
            response,
        } => {
            let _ = response.send(handle_run_validation(world, element_id));
        }
        ModelApiRequest::OccurrenceValidateHostFit { request, response } => {
            let _ = response.send(handle_occurrence_validate_host_fit(world, request));
        }
        ModelApiRequest::DefinitionValidateHostContract { request, response } => {
            let _ = response.send(handle_definition_validate_host_contract(world, request));
        }
        ModelApiRequest::ExplainFinding {
            finding_id,
            response,
        } => {
            let _ = response.send(handle_explain_finding(world, finding_id));
        }
        // --- Descriptor discovery (PP71) ---
        ModelApiRequest::ListElementClasses(response) => {
            let _ = response.send(handle_list_element_classes(world));
        }
        ModelApiRequest::GetCapabilitySnapshot { expanded, response } => {
            let _ = response.send(handle_get_capability_snapshot(world, expanded));
        }
        ModelApiRequest::ListRecipeFamilies {
            element_class,
            include_session_drafts,
            response,
        } => {
            let _ = response.send(handle_list_recipe_families_with_options(
                world,
                element_class,
                include_session_drafts,
            ));
        }
        ModelApiRequest::SelectRecipe {
            element_class,
            context,
            response,
        } => {
            let _ = response.send(handle_select_recipe(world, element_class, context));
        }
        ModelApiRequest::DiscoverCuratedPaths { request, response } => {
            let _ = response.send(handle_discover_curated_paths(world, request));
        }
        ModelApiRequest::InstantiateRecipe { request, response } => {
            let _ = response.send(handle_instantiate_recipe(world, *request));
        }
        // --- PP74 ---
        ModelApiRequest::ListConstraints { scope, response } => {
            let _ = response.send(handle_list_constraints(world, scope));
        }
        ModelApiRequest::RunValidationV2 {
            element_id,
            response,
        } => {
            // Force a fresh sweep, then read from the Findings resource.
            crate::plugins::validation::validation_sweep_system(world);
            let _ = response.send(handle_run_validation_v2(world, element_id));
        }
        ModelApiRequest::ExplainFindingV2 {
            finding_id,
            response,
        } => {
            let _ = response.send(handle_explain_finding_v2(world, finding_id));
        }
        ModelApiRequest::PreviewPromotion {
            element_id,
            target_state,
            recipe_id,
            overrides,
            response,
        } => {
            let _ = response.send(handle_preview_promotion(
                world,
                element_id,
                target_state,
                recipe_id,
                overrides,
            ));
        }
        // --- PP75 ---
        ModelApiRequest::ListCatalogProviders(response) => {
            let _ = response.send(handle_list_catalog_providers(world));
        }
        ModelApiRequest::CatalogQuery {
            provider_id,
            filter,
            response,
        } => {
            let _ = response.send(handle_catalog_query(world, provider_id, filter));
        }
        // --- PP76 ---
        ModelApiRequest::ListGenerationPriors {
            scope_filter,
            response,
        } => {
            let _ = response.send(handle_list_generation_priors(world, scope_filter));
        }
        // --- PP78 ---
        ModelApiRequest::ListCorpusGaps(response) => {
            let _ = response.send(handle_list_corpus_gaps(world));
        }
        ModelApiRequest::RequestCorpusExpansion {
            element_class,
            jurisdiction,
            kind,
            rationale,
            response,
        } => {
            let _ = response.send(handle_request_corpus_expansion(
                world,
                element_class,
                jurisdiction,
                kind,
                rationale,
            ));
        }
        ModelApiRequest::LookupSourcePassage {
            passage_ref,
            response,
        } => {
            let _ = response.send(handle_lookup_source_passage(world, passage_ref));
        }
        ModelApiRequest::DraftRulePack {
            chunk_id,
            element_class,
            response,
        } => {
            let _ = response.send(handle_draft_rule_pack(world, chunk_id, element_class));
        }
        ModelApiRequest::CheckRulePackBacklinks(response) => {
            let _ = response.send(handle_check_rule_pack_backlinks(world));
        }
        ModelApiRequest::ListRecipeDrafts {
            target_class,
            status,
            response,
        } => {
            let _ = response.send(handle_list_recipe_drafts(world, target_class, status));
        }
        ModelApiRequest::GetRecipeDraft {
            recipe_draft_id,
            response,
        } => {
            let _ = response.send(handle_get_recipe_draft(world, recipe_draft_id));
        }
        ModelApiRequest::SaveRecipeDraft { request, response } => {
            let _ = response.send(handle_save_recipe_draft(world, request));
        }
        ModelApiRequest::SetRecipeDraftStatus {
            recipe_draft_id,
            status,
            response,
        } => {
            let _ = response.send(handle_set_recipe_draft_status(
                world,
                recipe_draft_id,
                status,
            ));
        }
        ModelApiRequest::ListAssemblyPatternDrafts {
            target_type,
            status,
            response,
        } => {
            let _ = response.send(handle_list_assembly_pattern_drafts(
                world,
                target_type,
                status,
            ));
        }
        ModelApiRequest::GetAssemblyPatternDraft {
            assembly_pattern_draft_id,
            response,
        } => {
            let _ = response.send(handle_get_assembly_pattern_draft(
                world,
                assembly_pattern_draft_id,
            ));
        }
        ModelApiRequest::SaveAssemblyPatternDraft { request, response } => {
            let _ = response.send(handle_save_assembly_pattern_draft(world, request));
        }
        ModelApiRequest::SetAssemblyPatternDraftStatus {
            assembly_pattern_draft_id,
            status,
            response,
        } => {
            let _ = response.send(handle_set_assembly_pattern_draft_status(
                world,
                assembly_pattern_draft_id,
                status,
            ));
        }
        ModelApiRequest::MaterializeLearnedAsset { request, response } => {
            let _ = response.send(handle_materialize_learned_asset(world, request));
        }
        ModelApiRequest::GetAuthoringGuidance(response) => {
            let guidance = world
                .get_resource::<AuthoringGuidance>()
                .cloned()
                .unwrap_or_default();
            let _ = response.send(guidance);
        }
        ModelApiRequest::ListGuidanceCards { task, response } => {
            let _ = response.send(handle_list_guidance_cards(world, task));
        }
        ModelApiRequest::GetGuidanceCard { card_id, response } => {
            let _ = response.send(handle_get_guidance_card(world, card_id));
        }
        ModelApiRequest::ListAgentSkills { filter, response } => {
            let _ = response.send(handle_list_agent_skills(world, filter));
        }
        ModelApiRequest::GetAgentSkill { skill_id, response } => {
            let _ = response.send(handle_get_agent_skill(world, skill_id));
        }
        ModelApiRequest::SaveAgentSkillDraft { request, response } => {
            let _ = response.send(handle_save_agent_skill_draft(world, request));
        }
        ModelApiRequest::ProceduralSessionCreate { request, response } => {
            let r = crate::plugins::procedural_session_mcp::world_create(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ProceduralSessionEval { request, response } => {
            let r = crate::plugins::procedural_session_mcp::world_eval(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ProceduralSessionSnapshot { request, response } => {
            let r = crate::plugins::procedural_session_mcp::world_snapshot(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ProceduralSessionCommit { request, response } => {
            let mut executor = ModelApiStepExecutor;
            let r = crate::plugins::procedural_session_mcp::world_commit_with_executor(
                world,
                request,
                Some(&mut executor),
            );
            let _ = response.send(r);
        }
        ModelApiRequest::ProceduralSessionExport { request, response } => {
            let r = crate::plugins::procedural_session_mcp::world_export(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ParametricListTypes { response } => {
            let r = crate::plugins::parametric_mcp::world_list_types(world);
            let _ = response.send(r);
        }
        ModelApiRequest::ParametricCreate { request, response } => {
            let r = crate::plugins::parametric_mcp::world_create(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ParametricInspect { request, response } => {
            let r = crate::plugins::parametric_mcp::world_inspect(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ParametricSetDriver { request, response } => {
            let r = crate::plugins::parametric_mcp::world_set_driver(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ParametricTransform { request, response } => {
            let r = crate::plugins::parametric_mcp::world_transform(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ParametricExplain { request, response } => {
            let r = crate::plugins::parametric_mcp::world_explain(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::InstallRecipeFromSessionExport { request, response } => {
            let r = handle_install_recipe_from_session_export(world, request);
            let _ = response.send(r);
        }
        ModelApiRequest::ListPersistedRecipes { response } => {
            let r = handle_list_persisted_recipes(world);
            let _ = response.send(r);
        }
        ModelApiRequest::AcquireCorpusPassage { request, response } => {
            let r = handle_acquire_corpus_passage(world, request);
            let _ = response.send(r);
        }
        // --- Geometric validators (Item C) ---
        ModelApiRequest::GetWorldAabb { request, response } => {
            let _ = response.send(handle_get_world_aabb(world, request));
        }
        ModelApiRequest::CheckOverlaps { request, response } => {
            let _ = response.send(handle_check_overlaps(world, request));
        }
        ModelApiRequest::CheckFloating { request, response } => {
            let _ = response.send(handle_check_floating(world, request));
        }
        ModelApiRequest::CheckClearance { request, response } => {
            let _ = response.send(handle_check_clearance(world, request));
        }
    }
}

// -----------------------------------------------------------------------
// Change-2: install_recipe_from_session_export world handler
// -----------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn handle_install_recipe_from_session_export(
    world: &mut World,
    request: InstallRecipeFromSessionExportRequest,
) -> Result<InstallRecipeResult, String> {
    use crate::capability_registry::RecipeFamilyId;
    use crate::curation::provenance::Confidence;
    use crate::curation::{
        AssetId, AssetKindId, CurationMeta, Provenance, RecipeArtifact, RecipeArtifactRegistry,
        RecipeBody, Scope, Trust,
    };
    use crate::plugins::refinement::{AgentId, AuthoringMode};

    // Find the exported script in the session registry.
    let script = {
        let registry = world
            .get_resource::<crate::curation::ProceduralSessionRegistry>()
            .ok_or_else(|| "ProceduralSessionRegistry not available".to_string())?;
        let session_id = crate::curation::SessionId(request.session_id.clone());
        let session = registry
            .get(&session_id)
            .ok_or_else(|| format!("session '{}' not found", request.session_id))?;

        // Find the exported script matching the export_handle asset_id.
        let export_asset_id = AssetId(request.export_handle.clone());
        session
            .exports()
            .iter()
            .find(|e| e.handle.asset_id == export_asset_id)
            .map(|e| e.script.clone())
            .ok_or_else(|| {
                format!(
                    "export handle '{}' not found in session '{}'",
                    request.export_handle, request.session_id
                )
            })?
    };

    let scope_str = request.scope.as_deref().unwrap_or("Project");
    let scope = if scope_str == "Session" {
        Scope::Session
    } else {
        Scope::Project
    };

    let _family_id = RecipeFamilyId(request.family_id.clone());
    let asset_id =
        crate::plugins::knowledge_persistence::installed_recipe_asset_id(&request.family_id);

    let meta = CurationMeta::new(
        asset_id,
        AssetKindId(crate::curation::RECIPE_ARTIFACT_KIND.into()),
        Provenance {
            author: AgentId("mcp_agent".into()),
            confidence: Confidence::Medium,
            lineage: AuthoringMode::Freeform,
            rationale: Some(request.description.clone()),
            jurisdiction: None,
            catalog_dependencies: Vec::new(),
            evidence: Vec::new(),
        },
    )
    .with_scope(scope)
    .with_trust(Trust::Draft);

    let artifact = RecipeArtifact {
        meta,
        body: RecipeBody::AuthoringScript { script },
        parameter_schema: serde_json::Value::Null,
        target_class: request.target_class.clone().unwrap_or_default(),
        supported_refinement_states: request
            .supported_refinement_levels
            .iter()
            .filter_map(|s| crate::plugins::refinement::RefinementState::from_str(s))
            .collect(),
        tests: Vec::new(),
    };

    // Persist to disk if scope != Session.
    let persisted_path = if scope != Scope::Session {
        let path = crate::plugins::knowledge_persistence::persist_recipe(&artifact)?;
        Some(path.display().to_string())
    } else {
        None
    };

    // Insert into the in-memory registry.
    world
        .resource_mut::<RecipeArtifactRegistry>()
        .insert(artifact);

    Ok(InstallRecipeResult {
        family_id: request.family_id,
        scope: scope_str.to_string(),
        persisted_path,
        supported_refinement_levels: request.supported_refinement_levels,
    })
}

// -----------------------------------------------------------------------
// Change-3: list_persisted_recipes world handler
// -----------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn handle_list_persisted_recipes(world: &mut World) -> Vec<PersistedRecipeInfo> {
    let registry = world.get_resource::<crate::curation::RecipeArtifactRegistry>();
    let Some(registry) = registry else {
        return Vec::new();
    };
    registry
        .iter()
        .map(|a| PersistedRecipeInfo {
            family_id: a
                .meta
                .id
                .0
                .strip_prefix("installed_recipe/")
                .unwrap_or(a.meta.id.0.as_str())
                .to_string(),
            asset_id: a.meta.id.0.clone(),
            label: a.meta.id.0.clone(),
            description: a.meta.provenance.rationale.clone().unwrap_or_default(),
            body_kind: if a.body.is_native() {
                "native_fn_ref".into()
            } else {
                "authoring_script".into()
            },
            supported_refinement_levels: a
                .supported_refinement_states
                .iter()
                .map(|s| s.as_str().to_string())
                .collect(),
        })
        .collect()
}

// -----------------------------------------------------------------------
// Change-7: acquire_corpus_passage world handler
// -----------------------------------------------------------------------

#[cfg(feature = "model-api")]
fn handle_acquire_corpus_passage(
    world: &mut World,
    request: AcquireCorpusPassageRequest,
) -> Result<AcquireCorpusPassageResult, String> {
    use crate::capability_registry::PassageRef;
    use crate::plugins::corpus_gap::CorpusPassageRegistry;
    use crate::plugins::knowledge_persistence::{persist_passage, PersistedPassage};

    let persisted = PersistedPassage {
        passage_ref: request.passage_ref.clone(),
        text: request.text.clone(),
        citation: request.citation.clone(),
        source_url: request.source_url.clone(),
        jurisdiction: request.jurisdiction.clone(),
        classification: request.classification.clone(),
        license: request.license.clone(),
        // Acquiring a passage does not auto-promote it to a proactive must-read
        // card; that is a deliberate data-authoring step in the passage JSON.
        proactive_guidance: None,
    };

    let persisted_path = if request.persist {
        let path = persist_passage(&persisted)?;
        Some(path.display().to_string())
    } else {
        None
    };

    let provenance =
        crate::plugins::knowledge_persistence::build_provenance_for_passage(&persisted);
    let passage_ref = PassageRef(request.passage_ref.clone());

    let registry_size = {
        let mut registry = world.resource_mut::<CorpusPassageRegistry>();
        registry.register(passage_ref, request.text, provenance);
        registry.len()
    };

    Ok(AcquireCorpusPassageResult {
        passage_ref: request.passage_ref,
        stored: true,
        registry_size,
        persisted_path,
    })
}
