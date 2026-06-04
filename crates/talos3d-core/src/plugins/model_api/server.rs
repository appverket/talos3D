use super::*;

#[cfg(feature = "model-api")]
#[derive(Debug, Clone)]
pub(super) struct ModelApiServer {
    pub(super) sender: mpsc::Sender<ModelApiRequest>,
    pub(super) tool_router: ToolRouter<Self>,
}

/// Ensure a tool's top-level input schema is a valid `type: object` schema and
/// that no nested JSON-Schema node is a bare boolean (which strict MCP clients
/// reject). Operates in place on the rmcp `JsonObject` (a `serde_json::Map`).
#[cfg(feature = "model-api")]
fn sanitize_tool_input_schema(schema: &mut serde_json::Map<String, serde_json::Value>) {
    schema
        .entry("type")
        .or_insert_with(|| serde_json::Value::String("object".to_string()));
    let mut wrapper = serde_json::Value::Object(std::mem::take(schema));
    sanitize_schema_node(&mut wrapper);
    if let serde_json::Value::Object(map) = wrapper {
        *schema = map;
    }
}

/// Recursively rewrite boolean JSON-Schema nodes (`true`/`false`) into object
/// schemas, recursing only into positions that actually hold subschemas so
/// non-schema data (enum/const/required/default) is never touched.
#[cfg(feature = "model-api")]
fn sanitize_schema_node(node: &mut serde_json::Value) {
    match node {
        serde_json::Value::Bool(allowed) => {
            let mut map = serde_json::Map::new();
            if !*allowed {
                map.insert(
                    "not".to_string(),
                    serde_json::Value::Object(serde_json::Map::new()),
                );
            }
            *node = serde_json::Value::Object(map);
        }
        serde_json::Value::Object(map) => {
            for key in ["properties", "$defs", "definitions"] {
                if let Some(serde_json::Value::Object(children)) = map.get_mut(key) {
                    for child in children.values_mut() {
                        sanitize_schema_node(child);
                    }
                }
            }
            for key in [
                "additionalProperties",
                "not",
                "contains",
                "propertyNames",
                "if",
                "then",
                "else",
            ] {
                if let Some(child) = map.get_mut(key) {
                    sanitize_schema_node(child);
                }
            }
            if let Some(items) = map.get_mut("items") {
                match items {
                    serde_json::Value::Array(arr) => {
                        for child in arr.iter_mut() {
                            sanitize_schema_node(child);
                        }
                    }
                    other => sanitize_schema_node(other),
                }
            }
            for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
                if let Some(serde_json::Value::Array(arr)) = map.get_mut(key) {
                    for child in arr.iter_mut() {
                        sanitize_schema_node(child);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(feature = "model-api")]
impl ModelApiServer {
    pub(super) fn new(sender: mpsc::Sender<ModelApiRequest>) -> Self {
        let mut tool_router = Self::tool_router();
        // Sanitize every tool's input schema so the catalog is valid for strict
        // MCP clients (Claude Code / the Anthropic tool API). schemars renders
        // `serde_json::Value` fields as boolean (`true`) or type-less schemas,
        // and a single such schema makes strict clients silently drop the
        // ENTIRE server's tool list. We ensure the top level is `type: object`
        // and rewrite boolean JSON-Schema nodes into their object equivalents.
        for route in tool_router.map.values_mut() {
            let schema = std::sync::Arc::make_mut(&mut route.attr.input_schema);
            sanitize_tool_input_schema(schema);
        }
        Self {
            sender,
            tool_router,
        }
    }

    async fn request_get_instance_info(&self) -> Result<InstanceInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetInstanceInfo(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_list_entities(&self) -> Result<Vec<EntityEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListEntities(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_entity(
        &self,
        element_id: u64,
    ) -> Result<Option<serde_json::Value>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetEntity {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_entity_details(
        &self,
        element_id: u64,
    ) -> Result<Option<EntityDetails>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetEntityDetails {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_model_summary(&self) -> Result<ModelSummary, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ModelSummary(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_outline_tree(&self) -> Result<Value, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::OutlineTree(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_list_importers(&self) -> Result<Vec<ImporterDescriptor>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListImporters(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_create_entity(&self, json: Value) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateEntity { json, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_import_file(
        &self,
        path: String,
        format_hint: Option<String>,
    ) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ImportFile {
                path,
                format_hint,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_accept_semantic_shadow_candidate(
        &self,
        request: AcceptSemanticShadowCandidateRequest,
    ) -> ApiResult<EntityDetails> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AcceptSemanticShadowCandidate { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_handles(&self, element_id: u64) -> ApiResult<Vec<HandleInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListHandles {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_property_set_get(
        &self,
        element_id: u64,
        set_name: String,
        property_name: String,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimPropertySetGet {
                element_id,
                set_name,
                property_name,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_property_set_set(
        &self,
        element_id: u64,
        definition_id: String,
        set_name: String,
        property_name: String,
        value: Value,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimPropertySetSet {
                element_id,
                definition_id,
                set_name,
                property_name,
                value,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_exchange_identity_assign(
        &self,
        element_id: u64,
        system: String,
        exchange_id: String,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimExchangeIdentityAssign {
                element_id,
                system,
                exchange_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_exchange_identity_get(
        &self,
        element_id: u64,
        system: String,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimExchangeIdentityGet {
                element_id,
                system,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_exchange_identity_list(&self, element_id: u64) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimExchangeIdentityList {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_void_declare_for_definition(
        &self,
        definition_id: String,
        declaration: Value,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimVoidDeclareForDefinition {
                definition_id,
                declaration,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_void_plan_placement(
        &self,
        filling_definition: String,
        host_element_id: u64,
        filling_element_id: u64,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimVoidPlanPlacement {
                filling_definition,
                host_element_id,
                filling_element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_spatial_assign(
        &self,
        child_element_id: u64,
        container_element_id: u64,
        container_kind: String,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimSpatialAssign {
                child_element_id,
                container_element_id,
                container_kind,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_spatial_list_kind_registry(&self) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimSpatialListKindRegistry { response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_document_properties(&self) -> Result<serde_json::Value, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDocumentProperties(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_document_properties(
        &self,
        partial: serde_json::Value,
    ) -> ApiResult<serde_json::Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetDocumentProperties { partial, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_toolbars(&self) -> Result<Vec<ToolbarDetails>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListToolbars(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_toolbar_layout(
        &self,
        updates: Vec<ToolbarLayoutUpdate>,
    ) -> ApiResult<Vec<ToolbarDetails>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetToolbarLayout { updates, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_commands(&self) -> Result<Value, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListCommands(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_invoke_command(
        &self,
        command_id: String,
        parameters: Value,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InvokeCommand {
                command_id,
                parameters,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_prepare_site_surface(
        &self,
        request: PrepareSiteSurfaceRequest,
    ) -> ApiResult<crate::plugins::command_registry::CommandResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PrepareSiteSurface { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_terrain_cut_fill_analysis(
        &self,
        request: TerrainCutFillAnalysisRequest,
    ) -> ApiResult<crate::plugins::command_registry::CommandResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::TerrainCutFillAnalysis { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_terrain_elevation_at(
        &self,
        request: TerrainElevationAtRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::TerrainElevationAt { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_editing_context(&self) -> Result<EditingContextInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetEditingContext(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_enter_group(&self, element_id: u64) -> ApiResult<EditingContextInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::EnterGroup {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_exit_group(&self) -> ApiResult<EditingContextInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExitGroup(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_group_members(
        &self,
        element_id: u64,
    ) -> ApiResult<Vec<GroupMemberEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListGroupMembers {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Layer Management ---

    async fn request_list_layers(&self) -> Result<Vec<LayerInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListLayers(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_layer_visibility(
        &self,
        name: String,
        visible: bool,
    ) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetLayerVisibility {
                name,
                visible,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_layer_locked(
        &self,
        name: String,
        locked: bool,
    ) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetLayerLocked {
                name,
                locked,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_assign_layer(
        &self,
        element_id: u64,
        layer_name: String,
    ) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AssignLayer {
                element_id,
                layer_name,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_layer(&self, name: String) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateLayer { name, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_rename_layer(
        &self,
        old_name: String,
        new_name: String,
    ) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RenameLayer {
                old_name,
                new_name,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_delete_layer(&self, name: String) -> ApiResult<Vec<LayerInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeleteLayer { name, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_dependency_graph(&self) -> Result<Value, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DependencyGraph(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_entity_dependencies(&self, element_id: u64) -> Result<Value, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::EntityDependencies {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    // --- Named Views ---

    async fn request_view_list(&self) -> Result<Vec<NamedViewInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewList(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_view_save(
        &self,
        name: String,
        description: Option<String>,
        camera_params: Option<CameraParams>,
    ) -> ApiResult<NamedViewInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewSave {
                name,
                description,
                camera_params,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_view_restore(&self, name: String) -> ApiResult<NamedViewInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewRestore { name, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_view_update(
        &self,
        name: String,
        new_name: Option<String>,
        description: Option<String>,
        camera_params: Option<CameraParams>,
    ) -> ApiResult<NamedViewInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewUpdate {
                name,
                new_name,
                description,
                camera_params,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_view_delete(&self, name: String) -> ApiResult<()> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ViewDelete { name, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Clipping Planes ---

    async fn request_clip_plane_create(
        &self,
        name: String,
        origin: [f32; 3],
        normal: [f32; 3],
        active: bool,
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneCreate {
                name,
                origin,
                normal,
                active,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_clip_plane_update(
        &self,
        element_id: u64,
        name: Option<String>,
        origin: Option<[f32; 3]>,
        normal: Option<[f32; 3]>,
        active: Option<bool>,
    ) -> ApiResult<ClipPlaneInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneUpdate {
                element_id,
                name,
                origin,
                normal,
                active,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_clip_plane_list(&self) -> Result<Vec<ClipPlaneInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneList(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_clip_plane_toggle(
        &self,
        element_id: u64,
        active: bool,
    ) -> ApiResult<ClipPlaneInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClipPlaneToggle {
                element_id,
                active,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Materials ---

    async fn request_list_materials(&self) -> Result<Vec<MaterialInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListMaterials(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_material(&self, id: String) -> ApiResult<MaterialInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetMaterial { id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_material(
        &self,
        request: CreateMaterialRequest,
    ) -> ApiResult<MaterialInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateMaterial { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_material(
        &self,
        id: String,
        request: CreateMaterialRequest,
    ) -> ApiResult<MaterialInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateMaterial {
                id,
                request,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_delete_material(&self, id: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeleteMaterial { id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_apply_material(&self, request: ApplyMaterialRequest) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ApplyMaterial { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_assign_material(
        &self,
        request: AssignMaterialRequest,
    ) -> ApiResult<AssignMaterialResponse> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AssignMaterial { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_remove_material(&self, element_ids: Vec<u64>) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RemoveMaterial {
                element_ids,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_material_assignment(
        &self,
        element_id: u64,
    ) -> ApiResult<EntityMaterialAssignmentInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetMaterialAssignment {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_material_assignment(
        &self,
        request: SetMaterialAssignmentRequest,
    ) -> ApiResult<Vec<EntityMaterialAssignmentInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetMaterialAssignment { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_texture_mapping(
        &self,
        request: GetTextureMappingRequest,
    ) -> ApiResult<TextureMappingInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetTextureMapping { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_texture_mapping(
        &self,
        request: UpdateTextureMappingRequest,
    ) -> ApiResult<TextureMappingInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateTextureMapping { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_reset_texture_mapping(
        &self,
        request: ResetTextureMappingRequest,
    ) -> ApiResult<TextureMappingInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ResetTextureMapping { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_material_assign_layered(
        &self,
        request: BimMaterialAssignLayeredRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimMaterialAssignLayered { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_material_assign_constituents(
        &self,
        request: BimMaterialAssignConstituentsRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimMaterialAssignConstituents { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_bim_material_get_effective(
        &self,
        request: BimMaterialGetEffectiveRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::BimMaterialGetEffective { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_quantity_set(&self, request: QuantitySetRequest) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::QuantitySet { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_quantity_get(&self, request: QuantityGetRequest) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::QuantityGet { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_quantity_list_provenance(
        &self,
        request: QuantityListProvenanceRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::QuantityListProvenance { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_quantity_check_invariants(
        &self,
        request: QuantityCheckInvariantsRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::QuantityCheckInvariants { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_material_specs(
        &self,
        filter: ListMaterialSpecsFilter,
    ) -> ApiResult<Vec<MaterialSpecInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListMaterialSpecs { filter, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_material_spec(&self, asset_id: String) -> ApiResult<MaterialSpecInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetMaterialSpec { asset_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_material_spec(
        &self,
        request: DraftMaterialSpecRequest,
    ) -> ApiResult<MaterialSpecInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateMaterialSpec { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_material_spec(
        &self,
        asset_id: String,
        body: MaterialSpecBody,
        rationale: Option<String>,
    ) -> ApiResult<MaterialSpecInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateMaterialSpec {
                asset_id,
                body,
                rationale,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_save_material_spec(
        &self,
        asset_id: String,
        scope: String,
    ) -> ApiResult<MaterialSpecInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SaveMaterialSpec {
                asset_id,
                scope,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_publish_material_spec(&self, asset_id: String) -> ApiResult<MaterialSpecInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PublishMaterialSpec { asset_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_delete_material_spec(&self, asset_id: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeleteMaterialSpec { asset_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_lighting_scene(&self) -> Result<LightingSceneInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetLightingScene(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_list_lights(&self) -> Result<Vec<SceneLightInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListLights(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_create_light(&self, request: CreateLightRequest) -> ApiResult<SceneLightInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateLight { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_light(&self, request: UpdateLightRequest) -> ApiResult<SceneLightInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateLight { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_delete_light(&self, element_id: u64) -> ApiResult<usize> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeleteLight {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_ambient_light(
        &self,
        request: AmbientLightUpdateRequest,
    ) -> ApiResult<AmbientLightInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetAmbientLight { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_restore_default_light_rig(&self) -> ApiResult<Vec<SceneLightInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RestoreDefaultLightRig { response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_render_settings(&self) -> Result<RenderSettingsInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetRenderSettings(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_render_settings(
        &self,
        request: RenderSettingsUpdateRequest,
    ) -> ApiResult<RenderSettingsInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetRenderSettings { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_camera(&self) -> Result<CameraStateInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetCamera(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_camera(&self, params: CameraParams) -> ApiResult<CameraStateInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetCamera { params, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Selection ---

    async fn request_get_selection(&self) -> Result<Vec<u64>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetSelection(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_set_selection(&self, element_ids: Vec<u64>) -> ApiResult<Vec<u64>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetSelection {
                element_ids,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_ux_observe(&self) -> ApiResult<crate::plugins::ux_harness::UxHarnessSnapshot> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UxObserve { response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_ux_move_pointer(
        &self,
        request: crate::plugins::ux_harness::UxPointerMoveRequest,
    ) -> ApiResult<crate::plugins::ux_harness::UxInputResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UxMovePointer { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_ux_click(
        &self,
        request: crate::plugins::ux_harness::UxClickRequest,
    ) -> ApiResult<crate::plugins::ux_harness::UxInputResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UxClick { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_ux_drag(
        &self,
        request: crate::plugins::ux_harness::UxDragRequest,
    ) -> ApiResult<crate::plugins::ux_harness::UxInputResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UxDrag { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_ux_press_key(
        &self,
        request: crate::plugins::ux_harness::UxPressKeyRequest,
    ) -> ApiResult<crate::plugins::ux_harness::UxInputResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UxPressKey { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_align_preview(
        &self,
        request: AlignRequest,
    ) -> ApiResult<Vec<SpatialPreviewEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AlignPreview { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_align_execute(
        &self,
        request: AlignRequest,
    ) -> ApiResult<Vec<SpatialPreviewEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AlignExecute { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_distribute_preview(
        &self,
        request: DistributeRequest,
    ) -> ApiResult<Vec<SpatialPreviewEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DistributePreview { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_distribute_execute(
        &self,
        request: DistributeRequest,
    ) -> ApiResult<Vec<SpatialPreviewEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DistributeExecute { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Face Subdivision ---

    async fn request_split_box_face(
        &self,
        element_id: u64,
        face_id: u32,
        split_position: f32,
    ) -> ApiResult<SplitResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SplitBoxFace {
                element_id,
                face_id,
                split_position,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Screenshot ---

    async fn request_take_screenshot(&self, path: String, include_ui: bool) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::TakeScreenshot {
                path,
                include_ui,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        let saved_path = receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())??;

        wait_for_written_file(&saved_path).await?;
        Ok(saved_path)
    }

    async fn request_export_drawing(&self, path: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExportDrawing { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        let saved_path = receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())??;

        wait_for_written_file(&saved_path).await?;
        Ok(saved_path)
    }

    async fn request_export_drafting_sheet(
        &self,
        path: String,
        scale_denominator: Option<f32>,
    ) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExportDraftingSheet {
                path,
                scale_denominator,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        let saved_path = receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())??;

        wait_for_written_file(&saved_path).await?;
        Ok(saved_path)
    }

    async fn request_place_sheet_dimension(
        &self,
        request: PlaceSheetDimensionRequest,
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PlaceSheetDimension { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_place_dimension_between_handles(
        &self,
        request: PlaceDimensionBetweenHandlesRequest,
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PlaceDimensionBetweenHandles { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_save_project(&self, path: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SaveProject { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_frame_model(&self) -> ApiResult<BoundingBox> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::FrameModel { response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_frame_entities(&self, element_ids: Vec<u64>) -> ApiResult<BoundingBox> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::FrameEntities {
                element_ids,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_load_project(&self, path: String) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::LoadProject { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Semantic Assembly / Relation requests ---

    async fn request_list_vocabulary(&self) -> Result<VocabularyInfo, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListVocabulary(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_create_assembly(
        &self,
        request: CreateAssemblyRequest,
    ) -> ApiResult<CreateAssemblyResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateAssembly { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_assembly(&self, element_id: u64) -> ApiResult<AssemblyDetails> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetAssembly {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_assemblies(&self) -> Result<Vec<AssemblyEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListAssemblies(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_query_relations(
        &self,
        source: Option<u64>,
        target: Option<u64>,
        relation_type: Option<String>,
    ) -> Result<Vec<RelationEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::QueryRelations {
                source,
                target,
                relation_type,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_list_assembly_members(
        &self,
        element_id: u64,
    ) -> ApiResult<Vec<AssemblyMemberEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListAssemblyMembers {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Refinement requests (PP70) ---

    async fn request_get_refinement_state(
        &self,
        element_id: u64,
    ) -> ApiResult<RefinementStateInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetRefinementState {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_obligations(&self, element_id: u64) -> ApiResult<Vec<ObligationInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetObligations {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_resolve_obligation(
        &self,
        request: super::request::ResolveObligationRequest,
    ) -> ApiResult<super::request::ResolveObligationResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ResolveObligation { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_authoring_provenance(
        &self,
        element_id: u64,
    ) -> ApiResult<AuthoringProvenanceInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetAuthoringProvenance {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_claim_grounding(
        &self,
        element_id: u64,
        path: Option<String>,
    ) -> ApiResult<Vec<ClaimGroundingEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetClaimGrounding {
                element_id,
                path,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_promote_refinement(
        &self,
        element_id: u64,
        target_state: String,
        recipe_id: Option<String>,
        overrides: serde_json::Value,
    ) -> ApiResult<PromoteRefinementResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PromoteRefinement {
                element_id,
                target_state,
                recipe_id,
                overrides,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_demote_refinement(
        &self,
        element_id: u64,
        target_state: String,
    ) -> ApiResult<DemoteRefinementResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DemoteRefinement {
                element_id,
                target_state,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_inspect_refinement_branches(
        &self,
        element_id: u64,
    ) -> ApiResult<Vec<RefinementBranchApiInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InspectRefinementBranches {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_discard_refinement_branch(
        &self,
        parent_element_id: u64,
        child_element_id: u64,
    ) -> ApiResult<DiscardRefinementBranchResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DiscardRefinementBranch {
                parent_element_id,
                child_element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_run_validation(
        &self,
        element_id: u64,
    ) -> ApiResult<Vec<ValidationFindingInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RunValidation {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_explain_finding(&self, finding_id: String) -> ApiResult<serde_json::Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExplainFinding {
                finding_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_occurrence_validate_host_fit(
        &self,
        request: ValidateHostFitRequest,
    ) -> ApiResult<HostingValidationResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::OccurrenceValidateHostFit { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_definition_validate_host_contract(
        &self,
        request: ValidateDefinitionHostContractRequest,
    ) -> ApiResult<HostingValidationResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DefinitionValidateHostContract { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Descriptor discovery requests (PP71) ---

    async fn request_list_element_classes(&self) -> Vec<ElementClassInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .sender
            .send(ModelApiRequest::ListElementClasses(response));
        receiver.await.unwrap_or_default()
    }

    async fn request_get_capability_snapshot(&self, expanded: bool) -> CapabilitySnapshotInfo {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .sender
            .send(ModelApiRequest::GetCapabilitySnapshot { expanded, response });
        receiver
            .await
            .unwrap_or_else(|_| CapabilitySnapshotInfo::empty(expanded))
    }

    async fn request_list_recipe_families(
        &self,
        element_class: Option<String>,
        include_session_drafts: bool,
    ) -> Vec<RecipeFamilyInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self.sender.send(ModelApiRequest::ListRecipeFamilies {
            element_class,
            include_session_drafts,
            response,
        });
        receiver.await.unwrap_or_default()
    }

    async fn request_select_recipe(
        &self,
        element_class: String,
        context: serde_json::Value,
    ) -> ApiResult<Vec<RecipeRankingInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SelectRecipe {
                element_class,
                context,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_discover_curated_paths(
        &self,
        request: CuratedPathDiscoveryRequest,
    ) -> ApiResult<CuratedPathDiscoveryInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DiscoverCuratedPaths { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_instantiate_recipe(
        &self,
        request: InstantiateRecipeRequest,
    ) -> ApiResult<InstantiateRecipeResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InstantiateRecipe {
                request: Box::new(request),
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- PP74 requests ---

    async fn request_list_constraints(&self, scope: Option<String>) -> Vec<ConstraintInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .sender
            .send(ModelApiRequest::ListConstraints { scope, response });
        receiver.await.unwrap_or_default()
    }

    // --- PP75 requests ---

    async fn request_list_catalog_providers(&self) -> Vec<CatalogProviderInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .sender
            .send(ModelApiRequest::ListCatalogProviders(response));
        receiver.await.unwrap_or_default()
    }

    // --- PP76 requests ---

    async fn request_list_generation_priors(
        &self,
        scope_filter: Option<serde_json::Value>,
    ) -> Vec<GenerationPriorInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self.sender.send(ModelApiRequest::ListGenerationPriors {
            scope_filter,
            response,
        });
        receiver.await.unwrap_or_default()
    }

    // --- PP78 requests ---

    async fn request_list_corpus_gaps(&self) -> Vec<CorpusGapInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self.sender.send(ModelApiRequest::ListCorpusGaps(response));
        receiver.await.unwrap_or_default()
    }

    async fn request_request_corpus_expansion(
        &self,
        element_class: Option<String>,
        jurisdiction: Option<String>,
        kind: String,
        rationale: String,
    ) -> ApiResult<CorpusGapInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RequestCorpusExpansion {
                element_class,
                jurisdiction,
                kind,
                rationale,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_lookup_source_passage(
        &self,
        passage_ref: String,
    ) -> ApiResult<PassageLookupInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::LookupSourcePassage {
                passage_ref,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_draft_rule_pack(
        &self,
        chunk_id: String,
        element_class: String,
    ) -> ApiResult<DraftRulePackInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DraftRulePack {
                chunk_id,
                element_class,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_check_rule_pack_backlinks(&self) -> BacklinkCheckReportInfo {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .sender
            .send(ModelApiRequest::CheckRulePackBacklinks(response));
        receiver.await.unwrap_or_else(|_| BacklinkCheckReportInfo {
            total: 0,
            resolved: 0,
            broken: Vec::new(),
        })
    }

    async fn request_list_recipe_drafts(
        &self,
        target_class: Option<String>,
        status: Option<String>,
    ) -> ApiResult<Vec<RecipeDraftInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListRecipeDrafts {
                target_class,
                status,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_recipe_draft(
        &self,
        recipe_draft_id: String,
    ) -> ApiResult<RecipeDraftInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetRecipeDraft {
                recipe_draft_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_save_recipe_draft(
        &self,
        request: SaveRecipeDraftRequest,
    ) -> ApiResult<RecipeDraftInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SaveRecipeDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_recipe_draft_status(
        &self,
        recipe_draft_id: String,
        status: String,
    ) -> ApiResult<RecipeDraftInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetRecipeDraftStatus {
                recipe_draft_id,
                status,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_assembly_pattern_drafts(
        &self,
        target_type: Option<String>,
        status: Option<String>,
    ) -> ApiResult<Vec<AssemblyPatternDraftInfo>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListAssemblyPatternDrafts {
                target_type,
                status,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_assembly_pattern_draft(
        &self,
        assembly_pattern_draft_id: String,
    ) -> ApiResult<AssemblyPatternDraftInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetAssemblyPatternDraft {
                assembly_pattern_draft_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_save_assembly_pattern_draft(
        &self,
        request: SaveAssemblyPatternDraftRequest,
    ) -> ApiResult<AssemblyPatternDraftInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SaveAssemblyPatternDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_assembly_pattern_draft_status(
        &self,
        assembly_pattern_draft_id: String,
        status: String,
    ) -> ApiResult<AssemblyPatternDraftInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetAssemblyPatternDraftStatus {
                assembly_pattern_draft_id,
                status,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_materialize_learned_asset(
        &self,
        request: MaterializeLearnedAssetRequest,
    ) -> ApiResult<MaterializeLearnedAssetResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MaterializeLearnedAsset { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_catalog_query(
        &self,
        provider_id: String,
        filter: serde_json::Value,
    ) -> ApiResult<Vec<CatalogRowInfo>> {
        let (response, receiver) = oneshot::channel();
        let _ = self.sender.send(ModelApiRequest::CatalogQuery {
            provider_id,
            filter,
            response,
        });
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_guidance_cards(&self, task: Option<String>) -> Vec<GuidanceCardInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self
            .sender
            .send(ModelApiRequest::ListGuidanceCards { task, response });
        receiver.await.unwrap_or_default()
    }

    async fn request_get_guidance_card(&self, card_id: String) -> ApiResult<GuidanceCardInfo> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetGuidanceCard { card_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_run_validation_v2(
        &self,
        element_id: Option<u64>,
    ) -> Vec<ValidationFindingInfo> {
        let (response, receiver) = oneshot::channel();
        let _ = self.sender.send(ModelApiRequest::RunValidationV2 {
            element_id,
            response,
        });
        receiver.await.unwrap_or_default()
    }

    async fn request_explain_finding_v2(&self, finding_id: String) -> ApiResult<serde_json::Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExplainFindingV2 {
                finding_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_preview_promotion(
        &self,
        element_id: u64,
        target_state: String,
        recipe_id: Option<String>,
        overrides: serde_json::Value,
    ) -> ApiResult<PreviewPromotionResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PreviewPromotion {
                element_id,
                target_state,
                recipe_id,
                overrides,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    #[allow(dead_code)]
    async fn request_list_definitions(&self) -> Result<Vec<DefinitionEntry>, String> {
        self.request_list_definitions_opt(false).await
    }

    async fn request_list_definitions_opt(
        &self,
        include_internal: bool,
    ) -> Result<Vec<DefinitionEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListDefinitions {
                include_internal,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_definition(&self, definition_id: String) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDefinition {
                definition_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_definition(&self, request: Value) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_definition(&self, request: Value) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_representation_declare(
        &self,
        request: RepresentationDeclareRequest,
    ) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RepresentationDeclare { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_representation_set_lod(
        &self,
        request: RepresentationSetLodRequest,
    ) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RepresentationSetLod { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_representation_set_update_policy(
        &self,
        request: RepresentationSetUpdatePolicyRequest,
    ) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::RepresentationSetUpdatePolicy { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_definition_drafts(&self) -> Result<Vec<DefinitionDraftEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListDefinitionDrafts(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_definition_draft(
        &self,
        draft_id: String,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDefinitionDraft { draft_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_open_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::OpenDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_derive_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeriveDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_patch_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionDraftEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PatchDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_publish_definition_draft(
        &self,
        draft_id: String,
    ) -> ApiResult<DefinitionEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PublishDefinitionDraft { draft_id, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_validate_definition(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionValidationResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ValidateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_compile_definition(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionCompileResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CompileDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_explain_definition(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionExplainResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExplainDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_definition_libraries(
        &self,
    ) -> Result<Vec<DefinitionLibraryEntry>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListDefinitionLibraries(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_get_definition_library(&self, library_id: String) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetDefinitionLibrary {
                library_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_definition_library(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateDefinitionLibrary { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_add_definition_to_library(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AddDefinitionToLibrary { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_import_definition_library(
        &self,
        path: String,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ImportDefinitionLibrary { path, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_export_definition_library(
        &self,
        library_id: String,
        path: String,
    ) -> ApiResult<String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExportDefinitionLibrary {
                library_id,
                path,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_workspace_definition_libraries(
        &self,
        request: Value,
    ) -> ApiResult<Vec<DefinitionLibraryEntry>> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListWorkspaceDefinitionLibraries { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_create_workspace_definition_library(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::CreateWorkspaceDefinitionLibrary { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_import_workspace_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ImportWorkspaceDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_workspace_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateWorkspaceDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_delete_workspace_definition_draft(
        &self,
        request: Value,
    ) -> ApiResult<DefinitionLibraryEntry> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::DeleteWorkspaceDefinitionDraft { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_instantiate_definition(
        &self,
        request: Value,
    ) -> ApiResult<InstantiateDefinitionResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InstantiateDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_instantiate_hosted_definition(
        &self,
        request: Value,
    ) -> ApiResult<InstantiateDefinitionResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InstantiateHostedDefinition { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_place_occurrence(&self, request: Value) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::PlaceOccurrence { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_update_occurrence_overrides(
        &self,
        element_id: u64,
        overrides: Value,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::UpdateOccurrenceOverrides {
                element_id,
                overrides,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_set_occurrence_material_override(
        &self,
        request: SetOccurrenceMaterialOverrideRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::SetOccurrenceMaterialOverride { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_clear_occurrence_material_override(
        &self,
        request: ClearOccurrenceMaterialOverrideRequest,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ClearOccurrenceMaterialOverride { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_make_occurrence_unique(
        &self,
        request: OccurrenceMakeUniqueRequest,
    ) -> ApiResult<MakeOccurrenceUniqueResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MakeOccurrenceUnique { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_explain_occurrence(
        &self,
        element_id: u64,
    ) -> ApiResult<OccurrenceExplainResult> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ExplainOccurrence {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_resolve_occurrence(&self, element_id: u64) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ResolveOccurrence {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Array requests ---

    async fn request_array_create_linear(
        &self,
        source_id: u64,
        count: u32,
        spacing: [f32; 3],
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayCreateLinear {
                source_id,
                count,
                spacing,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_create_polar(
        &self,
        source_id: u64,
        count: u32,
        axis: [f32; 3],
        total_angle_degrees: f32,
        center: [f32; 3],
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayCreatePolar {
                source_id,
                count,
                axis,
                total_angle_degrees,
                center,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_update(
        &self,
        element_id: u64,
        count: Option<u32>,
        spacing: Option<[f32; 3]>,
        axis: Option<[f32; 3]>,
        total_angle_degrees: Option<f32>,
        center: Option<[f32; 3]>,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayUpdate {
                element_id,
                count,
                spacing,
                axis,
                total_angle_degrees,
                center,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_dissolve(&self, element_id: u64) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayDissolve {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_array_get(&self, element_id: u64) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ArrayGet {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Mirror requests ---

    async fn request_mirror_create(
        &self,
        source_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
    ) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorCreate {
                source_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_mirror_update(
        &self,
        element_id: u64,
        plane_str: Option<String>,
        plane_origin: Option<[f32; 3]>,
        plane_normal: Option<[f32; 3]>,
        merge: Option<bool>,
    ) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorUpdate {
                element_id,
                plane_str,
                plane_origin,
                plane_normal,
                merge,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_mirror_dissolve(&self, element_id: u64) -> ApiResult<u64> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorDissolve {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_mirror_get(&self, element_id: u64) -> ApiResult<Value> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::MirrorGet {
                element_id,
                response,
            })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_get_authoring_guidance(&self) -> Result<AuthoringGuidance, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::GetAuthoringGuidance(response))
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    // --- Semantic Procedural Session (ADR-051, PP-SPS-3) ---

    async fn request_procedural_session_create(
        &self,
        request: crate::plugins::procedural_session_mcp::SessionCreateRequest,
    ) -> Result<crate::plugins::procedural_session_mcp::SessionCreateResponse, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ProceduralSessionCreate { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_procedural_session_eval(
        &self,
        request: crate::plugins::procedural_session_mcp::SessionEvalRequest,
    ) -> Result<crate::curation::EvalReport, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ProceduralSessionEval { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
            .map_err(|e| format!("{e}"))
    }

    async fn request_procedural_session_snapshot(
        &self,
        request: crate::plugins::procedural_session_mcp::SessionSnapshotRequest,
    ) -> Result<crate::curation::SessionSnapshot, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ProceduralSessionSnapshot { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
            .map_err(|e| format!("{e}"))
    }

    async fn request_procedural_session_commit(
        &self,
        request: crate::plugins::procedural_session_mcp::SessionCommitRequest,
    ) -> Result<crate::curation::CommitReport, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ProceduralSessionCommit { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
            .map_err(|e| format!("{e}"))
    }

    async fn request_procedural_session_export(
        &self,
        request: crate::plugins::procedural_session_mcp::SessionExportRequest,
    ) -> Result<crate::curation::ExportHandle, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ProceduralSessionExport { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
            .map_err(|e| format!("{e}"))
    }

    // --- Parametric components (RELATIONAL_PARAMETRIC_SUBSTRATE, PP-RPS-7 UX) ---

    async fn request_parametric_list_types(
        &self,
    ) -> Result<Vec<crate::plugins::parametric_mcp::ParametricTypeInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ParametricListTypes { response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())
    }

    async fn request_parametric_create(
        &self,
        request: crate::plugins::parametric_mcp::CreateParametricRequest,
    ) -> Result<crate::plugins::parametric_mcp::CreateParametricResponse, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ParametricCreate { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_parametric_inspect(
        &self,
        request: crate::plugins::parametric_mcp::InspectParametricRequest,
    ) -> Result<crate::relational::registry::ParametricSnapshot, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ParametricInspect { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_parametric_set_driver(
        &self,
        request: crate::plugins::parametric_mcp::SetParametricDriverRequest,
    ) -> Result<crate::plugins::parametric_mcp::SetDriverResponse, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ParametricSetDriver { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_parametric_transform(
        &self,
        request: crate::plugins::parametric_mcp::ParametricTransformRequest,
    ) -> Result<crate::relational::transform::TransformOutcome, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ParametricTransform { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_parametric_explain(
        &self,
        request: crate::plugins::parametric_mcp::ExplainParametricRequest,
    ) -> Result<crate::plugins::parametric_mcp::ExplainParametricResponse, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ParametricExplain { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    // --- Knowledge persistence bridges (Change-2 / Change-3 / Change-7) ---

    async fn request_install_recipe_from_session_export(
        &self,
        request: super::request::InstallRecipeFromSessionExportRequest,
    ) -> Result<super::request::InstallRecipeResult, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::InstallRecipeFromSessionExport { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }

    async fn request_list_persisted_recipes(
        &self,
    ) -> Result<Vec<super::request::PersistedRecipeInfo>, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::ListPersistedRecipes { response })
            .map_err(|_| "model API request channel closed".to_string())?;
        let recipes = receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?;
        Ok(recipes)
    }

    async fn request_acquire_corpus_passage(
        &self,
        request: super::request::AcquireCorpusPassageRequest,
    ) -> Result<super::request::AcquireCorpusPassageResult, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(ModelApiRequest::AcquireCorpusPassage { request, response })
            .map_err(|_| "model API request channel closed".to_string())?;
        receiver
            .await
            .map_err(|_| "model API response channel closed".to_string())?
    }
}

#[cfg(feature = "model-api")]
fn json_tool_result<T: Serialize>(value: T) -> Result<CallToolResult, McpError> {
    let content = Content::json(value)?;
    Ok(CallToolResult::success(vec![content]))
}

#[cfg(feature = "model-api")]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for ModelApiServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some("Read and write access to the Talos3D authored model.".into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GetEntityRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DeleteEntitiesRequest {
    pub(super) element_ids: Vec<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct FrameEntitiesRequest {
    pub(super) element_ids: Vec<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportFileRequest {
    pub(super) path: String,
    pub(super) format_hint: Option<String>,
}

/// Explicit schema for `TransformToolRequest::value`: a number (rotate degrees /
/// scale factor / axis move distance) or a 3-number `[x,y,z]` vector (free move).
/// Replaces the type-less schema that `serde_json::Value` would otherwise emit,
/// which strict MCP tool-call serializers drop.
#[cfg(feature = "model-api")]
fn transform_value_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "description": "Number (rotate degrees, scale factor, or axis move distance) or a [x,y,z] array of 3 numbers (free move).",
        "oneOf": [
            { "type": "number" },
            { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 }
        ]
    })
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformToolRequest {
    pub element_ids: Vec<u64>,
    pub operation: String,
    pub axis: Option<String>,
    /// rotate: angle in degrees (number); scale: factor (number); move: distance
    /// along `axis` (number) or a free `[dx,dy,dz]` vector (array of 3 numbers).
    #[cfg_attr(
        feature = "model-api",
        schemars(schema_with = "transform_value_schema")
    )]
    pub value: Value,
    /// Optional world-space pivot `[x,y,z]` for `rotate`. When given, the
    /// selection is rotated rigidly about this point (e.g. a wing's junction
    /// corner) instead of the world origin — so a whole assembly orients in
    /// place. Ignored for non-rotate operations.
    #[serde(default)]
    pub pivot: Option<[f64; 3]>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetPropertyRequest {
    pub(super) element_id: u64,
    pub(super) property_name: String,
    pub(super) value: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ListHandlesRequest {
    pub(super) element_id: u64,
}

/// ADR-026 Phase 6a: read a single BIM property-set value from an
/// authored entity.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimPropertySetGetRequest {
    pub(super) element_id: u64,
    pub(super) set_name: String,
    pub(super) property_name: String,
}

/// ADR-026 Phase 6a: write a single BIM property-set value on an
/// authored entity. The write is schema-validated against the
/// `PropertySetSchemaRegistry` for the entity's Definition; type
/// mismatches and unknown set/property names are rejected.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimPropertySetSetRequest {
    pub(super) element_id: u64,
    /// Definition id used to look up the property-set schema. Must
    /// be the id of a Definition that has registered schemas via
    /// `PropertySetSchemaRegistry`.
    pub(super) definition_id: String,
    pub(super) set_name: String,
    pub(super) property_name: String,
    /// Property value, encoded with the standard `PropertyValue`
    /// JSON shape (`{"number": 0.18}` / `{"text": "REI60"}` /
    /// `{"boolean": true}` / `{"integer": 60}` / `{"enum": "A1"}`
    /// / `{"json": ...}`).
    pub(super) value: Value,
}

/// ADR-026 Phase 6b: assign a stable BIM exchange identifier to an
/// authored entity if the system slot is currently empty.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimExchangeIdentityAssignRequest {
    pub(super) element_id: u64,
    /// Exchange system label: `ifc`, `revit`, `dwg`, `cobie`, or a
    /// custom system name.
    pub(super) system: String,
    pub(super) exchange_id: String,
}

/// ADR-026 Phase 6b: read a single BIM exchange identifier.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimExchangeIdentityGetRequest {
    pub(super) element_id: u64,
    /// Exchange system label: `ifc`, `revit`, `dwg`, `cobie`, or a
    /// custom system name.
    pub(super) system: String,
}

/// ADR-026 Phase 6b: list all exchange identifiers assigned to an entity.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimExchangeIdentityListRequest {
    pub(super) element_id: u64,
}

/// ADR-026 Phase 6f: write a `VoidDeclaration` into a
/// Definition interface so placing that Definition cuts a void in its
/// host.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimVoidDeclareForDefinitionRequest {
    pub(super) definition_id: String,
    /// JSON-encoded `VoidDeclaration` (shape, placement, exchange_role).
    pub(super) declaration: Value,
}

/// ADR-026 Phase 6f: plan an atomic void placement.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimVoidPlanPlacementRequest {
    pub(super) filling_definition: String,
    pub(super) host_element_id: u64,
    pub(super) filling_element_id: u64,
}

/// ADR-026 Phase 6g: assign an entity to a spatial container.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BimSpatialAssignRequest {
    pub(super) child_element_id: u64,
    pub(super) container_element_id: u64,
    /// Kind label of the container (`"storey"`, `"space"`, …). Must
    /// be registered in the `SpatialContainerKindRegistry`.
    pub(super) container_kind: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetToolbarLayoutRequest {
    pub(super) updates: Vec<ToolbarLayoutUpdate>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct InvokeCommandRequest {
    pub(super) command_id: String,
    #[serde(default)]
    pub(super) parameters: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareSiteSurfaceRequest {
    #[serde(default)]
    pub source_element_ids: Vec<u64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub delete_source: bool,
    #[serde(default = "default_true")]
    pub center_at_origin: bool,
    #[serde(default)]
    pub contour_layers: Vec<String>,
    #[serde(default)]
    pub join_tolerance: Option<f32>,
    #[serde(default)]
    pub drape_sample_spacing: Option<f32>,
    #[serde(default)]
    pub max_triangle_area: Option<f32>,
    #[serde(default)]
    pub minimum_angle: Option<f32>,
    #[serde(default)]
    pub contour_interval: Option<f32>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerrainCutFillAnalysisRequest {
    pub existing_surface_id: u64,
    #[serde(default)]
    pub proposed_surface_id: Option<u64>,
    #[serde(default)]
    pub datum_y: Option<f32>,
    #[serde(default)]
    pub sample_spacing: Option<f32>,
    #[serde(default)]
    pub boundary: Vec<[f32; 2]>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerrainElevationAtRequest {
    pub x: f32,
    pub z: f32,
}

pub(super) fn default_true() -> bool {
    true
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct EnterGroupRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ListGroupMembersRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetLayerVisibilityRequest {
    pub(super) name: String,
    pub(super) visible: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetLayerLockedRequest {
    pub(super) name: String,
    pub(super) locked: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AssignLayerRequest {
    pub(super) element_id: u64,
    pub(super) layer_name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CreateLayerRequest {
    pub(super) name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RenameLayerRequest {
    pub(super) old_name: String,
    pub(super) new_name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DeleteLayerRequest {
    pub(super) name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct EntityDependenciesRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GetMaterialRequest {
    pub(super) id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct UpdateMaterialRequest {
    pub(super) id: String,
    #[serde(flatten)]
    pub(super) material: CreateMaterialRequest,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DeleteMaterialRequest {
    pub(super) id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RemoveMaterialRequest {
    pub(super) element_ids: Vec<u64>,
}

// --- Named View request types ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ViewSaveRequest {
    pub(super) name: String,
    pub(super) description: Option<String>,
    #[serde(flatten)]
    pub(super) camera: Option<CameraParams>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ViewRestoreRequest {
    pub(super) name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ViewUpdateRequest {
    pub(super) name: String,
    pub(super) new_name: Option<String>,
    pub(super) description: Option<String>,
    #[serde(flatten)]
    pub(super) camera: Option<CameraParams>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ViewDeleteRequest {
    pub(super) name: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetSelectionRequest {
    pub(super) element_ids: Vec<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct AlignRequest {
    pub(super) element_ids: Vec<u64>,
    pub(super) axis: String,
    pub(super) mode: String,
    pub(super) reference_element_id: Option<u64>,
    pub(super) reference_value: Option<f32>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct DistributeRequest {
    pub(super) element_ids: Vec<u64>,
    pub(super) axis: String,
    pub(super) mode: String,
    pub(super) value: Option<f32>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct SpatialPreviewEntry {
    pub(super) element_id: u64,
    pub(super) current_position: [f32; 3],
    pub(super) proposed_position: [f32; 3],
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SplitBoxFaceRequest {
    pub(super) element_id: u64,
    pub(super) face_id: u32,
    /// Split position from 0.0 to 1.0 along the split axis.
    pub(super) split_position: f32,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TakeScreenshotRequest {
    /// File path to save the screenshot. Defaults to /tmp/talos_screenshot.png.
    #[serde(default = "default_screenshot_path")]
    pub(super) path: String,
    /// Include egui app chrome and panels instead of returning the cropped modeling viewport.
    #[serde(default)]
    pub(super) include_ui: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExportDrawingRequest {
    /// File path to save the exported drawing. Supports PNG, PDF, SVG, and the `svd` alias.
    #[serde(default = "crate::plugins::drawing_export::default_drawing_export_path")]
    pub(super) path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExportDraftingSheetRequest {
    /// File path to save the drafting sheet. Extension decides the format
    /// (svg, pdf, dxf, png).
    pub path: String,
    /// Architectural drawing scale denominator (e.g. 50 for a 1:50 plan).
    /// Defaults to 50 if omitted.
    #[serde(default)]
    pub scale_denominator: Option<f32>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExportFidelityRequest {
    /// Optional surface id, e.g. drawing.png, drawing.pdf, drawing.svg,
    /// drawing.dxf, or definition_library.json. Omit with `path` to infer from
    /// extension; omit both to list every known export surface.
    #[serde(default)]
    pub(super) surface: Option<String>,
    /// Optional file path whose extension is used to infer the export surface.
    #[serde(default)]
    pub(super) path: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PlaceSheetDimensionRequest {
    /// Paper-mm endpoint A in the current sheet's 2D frame.
    pub a: [f32; 2],
    /// Paper-mm endpoint B.
    pub b: [f32; 2],
    /// Paper-mm offset vector from the midpoint of a..b to the dim line.
    /// Use e.g. `[0, -15]` for "15 mm below" or `[15, 0]` for "15 mm right".
    pub offset: [f32; 2],
    /// Optional dim style name. Defaults to the registry's current default.
    #[serde(default)]
    pub style: Option<String>,
    /// Optional text override. If unset, the dim renders the measured value
    /// using the style's number format.
    #[serde(default)]
    pub text_override: Option<String>,
    /// Drawing scale denominator used to interpret the paper-mm inputs.
    /// Defaults to 50 (i.e. 1:50).
    #[serde(default)]
    pub scale_denominator: Option<f32>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SaveProjectRequest {
    pub(super) path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct LoadProjectRequest {
    pub(super) path: String,
}

// --- Clipping Plane request types ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ClipPlaneCreateRequest {
    /// Display name for the clipping plane.
    #[serde(default = "default_clip_plane_name")]
    pub(super) name: String,
    /// Point on the plane in world space `[x, y, z]`. Defaults to origin.
    #[serde(default)]
    pub(super) origin: [f32; 3],
    /// Normal pointing toward the visible side `[x, y, z]`. Defaults to `[0,1,0]` (up).
    #[serde(default = "default_clip_plane_normal")]
    pub(super) normal: [f32; 3],
    /// Whether the plane is active immediately. Defaults to `true`.
    #[serde(default = "default_true")]
    pub(super) active: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ClipPlaneUpdateRequest {
    pub(super) element_id: u64,
    pub(super) name: Option<String>,
    pub(super) origin: Option<[f32; 3]>,
    pub(super) normal: Option<[f32; 3]>,
    pub(super) active: Option<bool>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ClipPlaneToggleRequest {
    pub(super) element_id: u64,
    pub(super) active: bool,
}

#[cfg(feature = "model-api")]
fn default_clip_plane_name() -> String {
    "Section".to_string()
}

#[cfg(feature = "model-api")]
fn default_clip_plane_normal() -> [f32; 3] {
    [0.0, 1.0, 0.0]
}

#[cfg(feature = "model-api")]
fn default_screenshot_path() -> String {
    "/tmp/talos_screenshot.png".to_string()
}

#[cfg(feature = "model-api")]
async fn wait_for_written_file(path: &str) -> Result<(), String> {
    const ATTEMPTS: usize = 600;
    const POLL_INTERVAL_MS: u64 = 100;
    const STABLE_POLLS_REQUIRED: usize = 3;

    let mut last_size = None;
    let mut stable_polls = 0usize;

    for _ in 0..ATTEMPTS {
        match std::fs::metadata(path).map(|metadata| metadata.len()) {
            Ok(size) if size > 0 => {
                if last_size == Some(size) {
                    stable_polls += 1;
                } else {
                    last_size = Some(size);
                    stable_polls = 1;
                }
                if stable_polls >= STABLE_POLLS_REQUIRED {
                    return Ok(());
                }
            }
            _ => {
                last_size = None;
                stable_polls = 0;
            }
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }

    Err(format!(
        "Viewport export was requested but '{path}' was not written within {} ms",
        ATTEMPTS as u64 * POLL_INTERVAL_MS
    ))
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ToolbarLayoutUpdate {
    pub(super) toolbar_id: String,
    pub(super) dock: Option<String>,
    pub(super) order: Option<u32>,
    pub(super) visible: Option<bool>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct HandlePosition {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) z: f32,
}

#[cfg(feature = "model-api")]
impl From<Vec3> for HandlePosition {
    fn from(position: Vec3) -> Self {
        Self {
            x: position.x,
            y: position.y,
            z: position.z,
        }
    }
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandleInfo {
    pub(super) id: String,
    pub(super) position: HandlePosition,
    pub(super) kind: String,
    pub(super) label: String,
}

// --- Element class / recipe family types (PP71) ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct CapabilitySnapshotRequest {
    /// Include full ids and a larger gap list for diagnostic sessions.
    #[serde(default)]
    pub(super) expanded: bool,
}

/// Bounded dynamic-knowledge discovery snapshot (PP-DKC-1).
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilitySnapshotInfo {
    pub snapshot_version: u32,
    pub expanded: bool,
    pub size_budget_bytes: usize,
    pub estimated_json_bytes: usize,
    pub summary: CapabilitySnapshotSummary,
    pub computed: CapabilitySnapshotComputed,
    pub evidence_backed: Vec<CapabilitySnapshotFact>,
    pub guidance_overrides: Vec<CapabilitySnapshotFact>,
    pub no_curated_paths: Vec<NoCuratedPathInfo>,
    pub must_read_guidance_card_ids: Vec<String>,
    pub next_tools: Vec<String>,
}

impl CapabilitySnapshotInfo {
    fn empty(expanded: bool) -> Self {
        Self {
            snapshot_version: 1,
            expanded,
            size_budget_bytes: 12 * 1024,
            estimated_json_bytes: 0,
            summary: CapabilitySnapshotSummary::default(),
            computed: CapabilitySnapshotComputed::default(),
            evidence_backed: Vec::new(),
            guidance_overrides: Vec::new(),
            no_curated_paths: Vec::new(),
            must_read_guidance_card_ids: Vec::new(),
            next_tools: vec![
                "list_element_classes".into(),
                "select_recipe".into(),
                "request_corpus_expansion".into(),
                "save_recipe_draft".into(),
                "materialize_learned_asset".into(),
            ],
        }
    }
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CapabilitySnapshotSummary {
    pub element_class_count: usize,
    pub recipe_family_count: usize,
    pub session_recipe_draft_count: usize,
    pub assembly_pattern_draft_count: usize,
    pub parametric_type_count: usize,
    pub catalog_provider_count: usize,
    pub generation_prior_count: usize,
    pub constraint_count: usize,
    pub corpus_gap_count: usize,
    pub source_count: usize,
    pub curated_manifest_count: usize,
    pub material_spec_count: usize,
    pub no_curated_path_count: usize,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CapabilitySnapshotComputed {
    pub element_class_ids: Vec<String>,
    pub recipe_family_ids: Vec<String>,
    pub session_recipe_draft_ids: Vec<String>,
    pub assembly_pattern_draft_ids: Vec<String>,
    pub parametric_type_ids: Vec<String>,
    pub catalog_provider_ids: Vec<String>,
    pub generation_prior_ids: Vec<String>,
    pub constraint_ids: Vec<String>,
    pub corpus_gap_ids: Vec<String>,
    pub source_ids: Vec<String>,
    pub curated_manifest_ids: Vec<String>,
    pub material_spec_ids: Vec<String>,
    pub maturity_flags: Vec<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilitySnapshotFact {
    /// `computed`, `evidence_backed`, or `guidance_override`.
    pub classification: String,
    pub id: String,
    pub summary: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoCuratedPathInfo {
    pub element_class: String,
    pub missing_artifact_kind: String,
    pub suggested_next_tool: String,
    #[serde(default)]
    pub gap_record_is_terminal: bool,
    #[serde(default)]
    pub required_next_tools: Vec<String>,
    #[serde(default)]
    pub completion_criteria: Vec<String>,
    pub guidance_card_ids: Vec<String>,
    pub related_installed_or_learned_asset_ids: Vec<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ElementClassInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub semantic_roles: Vec<String>,
    /// Per-refinement-state obligation ladder: what must be resolved before an
    /// entity of this class can legitimately claim each level. Ordered from
    /// `Conceptual` upward; only states that carry obligations or
    /// promotion-critical claim paths are included. An MCP-only agent reads this
    /// to know the per-level requirements without source access.
    #[serde(default)]
    pub obligations_by_state: Vec<ClassStateObligationsInfo>,
}

/// The obligations and promotion-critical claim paths an element class requires
/// at one refinement state.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassStateObligationsInfo {
    /// Refinement state label (`Conceptual` … `FabricationReady`).
    pub refinement_state: String,
    /// Class-minimum obligations that must be resolved by this state.
    pub obligations: Vec<ClassObligationTemplateInfo>,
    /// Claim paths that are promotion-critical at this state (must be grounded
    /// before promotion).
    pub promotion_critical_paths: Vec<String>,
}

/// A single class-minimum obligation template (no live status).
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassObligationTemplateInfo {
    pub id: String,
    pub role: String,
    pub required_by_state: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeParameterInfo {
    pub name: String,
    pub value_schema: serde_json::Value,
    pub default: Option<serde_json::Value>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeFamilyInfo {
    pub id: String,
    pub target_class: String,
    pub label: String,
    pub description: String,
    pub supported_refinement_levels: Vec<String>,
    pub parameters: Vec<RecipeParameterInfo>,
    #[serde(default)]
    pub is_session_draft: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeRankingInfo {
    pub id: String,
    pub target_class: String,
    pub label: String,
    /// Tie weight — 1.0 for all viable recipes in PP71 (real priors land in PP76).
    pub weight: f32,
    #[serde(default)]
    pub is_session_draft: bool,
    /// True only when this ranking can emit geometry through the named execution path.
    #[serde(default)]
    pub executable: bool,
    /// MCP tool that can materialise this ranking, when executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_path: Option<String>,
    /// Declared parameters (name, value schema, default) the agent can pass
    /// to `instantiate_recipe`. Surfaced here so a caller can discover the
    /// accepted parameters up front instead of probing by trial-and-error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<RecipeParameterInfo>,
    /// One-line hint for how to instantiate or materialise this path.
    pub how_to_instantiate: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeDraftInfo {
    pub id: String,
    pub curation: CurationAssetInfo,
    pub label: String,
    pub description: String,
    pub target_class: String,
    pub supported_refinement_levels: Vec<String>,
    pub parameters: Vec<RecipeParameterInfo>,
    pub jurisdiction: Option<String>,
    pub gap_id: Option<String>,
    pub source_passage_refs: Vec<String>,
    #[serde(default)]
    pub evidence_slots: Vec<crate::plugins::knowledge_assets::EvidenceSlot>,
    #[serde(default)]
    pub runtime_claims: Vec<crate::plugins::knowledge_assets::RuntimeCapabilityClaim>,
    pub acquisition_context: serde_json::Value,
    pub draft_script: serde_json::Value,
    pub notes: Vec<String>,
    pub status: String,
    pub consultable: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefinementStateInfo {
    pub element_id: u64,
    pub state: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObligationInfo {
    pub id: String,
    pub role: String,
    pub required_by_state: String,
    /// `"Unresolved"`, `"SatisfiedBy:<id>"`, `"Deferred:<reason>"`, or `"Waived:<rationale>"`.
    pub status: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthoringProvenanceInfo {
    pub element_id: u64,
    /// `"Freeform"`, `"ViaRecipe:<id>"`, `"Imported:<ref>"`, or `"Refined:<id>"`.
    pub mode: String,
    pub rationale: Option<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimGroundingEntry {
    pub path: String,
    /// JSON-encoded `Grounding` variant.
    pub grounding: serde_json::Value,
    pub set_at: i64,
    pub set_by: Option<String>,
    /// Always `false` in PP70; PP74 wires in element-class descriptor merge.
    pub is_promotion_critical: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidationFindingInfo {
    pub finding_id: String,
    pub entity_element_id: u64,
    pub validator: String,
    pub severity: String,
    pub message: String,
    pub rationale: String,
    pub obligation_id: Option<String>,
    /// Backlink to the source passage that grounds this finding, if the
    /// constraint set one. Agents follow it with `lookup_source_passage` to read
    /// the construction knowledge behind the finding. Carrying it here is what
    /// makes the passage corpus reactively discoverable from a failed check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passage_ref: Option<String>,
}

// --- PP74 response types ---

/// Info for a single registered constraint, returned by `list_constraints`.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConstraintInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub default_severity: String,
    pub rationale: String,
    /// Element classes this constraint applies to (empty = all).
    pub element_classes: Vec<String>,
    /// Required refinement state filter (`None` = any state).
    pub required_state: Option<String>,
}

// --- PP75 response types ---

/// Summary of a registered catalog provider, returned by `list_catalog_providers`.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogProviderInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    /// `CatalogCategory::as_str()` — e.g. `"dimensional_lumber"`.
    pub category: String,
    pub region: Option<String>,
    /// `LicenseTag::as_str()` — e.g. `"cc0"`.
    pub license: String,
    pub source_version: String,
}

/// A single row returned by `catalog_query`.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogRowInfo {
    pub row_id: String,
    /// `CatalogCategory::as_str()`.
    pub category: String,
    pub data: serde_json::Value,
    /// `LicenseTag::as_str()`.
    pub license: String,
    pub source_version: String,
}

// --- PP76 response types ---

/// Summary of a registered generation prior, returned by `list_generation_priors`.
///
/// Does not carry the `prior_fn` closure — use the descriptor directly when
/// you need to evaluate the prior at runtime.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationPriorInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    /// Serialised `PriorScope` as a JSON object (includes `"kind"` discriminant).
    pub scope: serde_json::Value,
    /// `LicenseTag::as_str()` from the descriptor's `source_provenance`.
    pub license: String,
    /// Version label from the descriptor's `source_provenance`.
    pub source_version: String,
}

// --- PP78 response types ---

/// Serialisable summary of a [`CorpusGap`] entry, returned by corpus-ops MCP tools.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CorpusGapInfo {
    pub id: String,
    pub element_class: Option<String>,
    pub kind: Option<String>,
    pub jurisdiction: Option<String>,
    pub missing_artifact_kind: String,
    pub context: serde_json::Value,
    pub reported_by: String,
    pub reported_at: i64,
    /// Open until a usable knowledge asset is installed and, for authoring tasks,
    /// proven executable. Creating this record is not itself completion.
    pub status: String,
    #[serde(default)]
    pub gap_record_is_terminal: bool,
    #[serde(default)]
    pub required_next_tools: Vec<String>,
    #[serde(default)]
    pub completion_criteria: Vec<String>,
}

/// Serialisable passage lookup result, returned by `lookup_source_passage`.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PassageLookupInfo {
    pub passage_ref: String,
    pub text: String,
    pub source: String,
    pub source_version: String,
    pub jurisdiction: Option<String>,
    /// `LicenseTag::as_str()` label.
    pub license: String,
}

/// Scaffolded rule-pack draft, returned by `draft_rule_pack`.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DraftRulePackInfo {
    /// Rust source skeleton (not compilable as-is — human must fill in the
    /// validator body).
    pub rust_skeleton: String,
    /// The passage ref used as the backlink in the skeleton.
    pub backlink: String,
    /// Human-readable notes for the author.
    pub notes: Vec<String>,
}

/// Backlink check summary, returned by `check_rule_pack_backlinks`.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BacklinkCheckReportInfo {
    pub total: usize,
    pub resolved: usize,
    pub broken: Vec<BrokenBacklinkInfo>,
}

/// A single unresolvable backlink entry in a [`BacklinkCheckReportInfo`].
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrokenBacklinkInfo {
    pub constraint_id: String,
    pub passage_ref: String,
}

/// Result of a `preview_promotion` call.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewPromotionResult {
    pub element_id: u64,
    pub would_transition_to: String,
    /// Obligations that would be present after promotion.
    pub obligation_set: Vec<ObligationInfo>,
    /// Validator findings that would be produced after promotion.
    pub findings: Vec<ValidationFindingInfo>,
    pub plan: RefinementPromotionPlanInfo,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefinementPromotionPlanInfo {
    pub plan_id: String,
    pub target: RefinementPromotionTargetInfo,
    pub affected_scope: RefinementPromotionScopeInfo,
    pub current_state: String,
    pub target_state: String,
    pub recipe_id: Option<String>,
    pub default_commit_policy: String,
    pub supported_commit_policies: Vec<String>,
    pub changed_entities: Vec<PromotionPlanEntityChangeInfo>,
    pub generated_entities: Vec<PromotionPlanEntityChangeInfo>,
    pub parked_entities: Vec<PromotionPlanEntityChangeInfo>,
    pub removed_entities: Vec<PromotionPlanEntityChangeInfo>,
    pub obligations: Vec<ObligationInfo>,
    pub validators: Vec<PromotionPlanValidatorInfo>,
    pub missing_inputs: Vec<String>,
    pub findings: Vec<ValidationFindingInfo>,
    pub derived_graph_additions: Vec<String>,
    pub can_commit: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefinementPromotionTargetInfo {
    pub kind: String,
    pub root_element_id: u64,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefinementPromotionScopeInfo {
    pub root_element_id: u64,
    pub default_selected_element_ids: Vec<u64>,
    pub editable: bool,
    pub project_wide: bool,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromotionPlanEntityChangeInfo {
    pub element_id: Option<u64>,
    pub action: String,
    pub reason: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromotionPlanValidatorInfo {
    pub id: String,
    pub label: String,
    pub role: String,
    pub default_severity: String,
    pub required_state: Option<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromoteRefinementResult {
    pub element_id: u64,
    pub previous_state: String,
    pub new_state: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DemoteRefinementResult {
    pub element_id: u64,
    pub previous_state: String,
    pub new_state: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefinementBranchApiInfo {
    pub root_element_id: u64,
    pub parent_element_id: u64,
    pub child_element_id: u64,
    pub target_state: String,
    pub recipe_id: Option<String>,
    pub status: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscardRefinementBranchResult {
    pub parent_element_id: u64,
    pub child_element_id: u64,
    pub discarded_element_ids: Vec<u64>,
}

// --- Refinement request types (PP70) ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RefinementEntityRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ClaimGroundingRequest {
    pub(super) element_id: u64,
    pub(super) path: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PromoteRefinementRequest {
    pub(super) element_id: u64,
    pub(super) target_state: String,
    pub(super) recipe_id: Option<String>,
    pub(super) overrides: Option<serde_json::Value>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DemoteRefinementRequest {
    pub(super) element_id: u64,
    pub(super) target_state: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct InspectRefinementBranchesRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DiscardRefinementBranchRequest {
    pub(super) parent_element_id: u64,
    pub(super) child_element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExplainFindingRequest {
    pub(super) finding_id: String,
}

// --- PP71 request parameter structs ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ListRecipeFamiliesRequest {
    /// Filter to this element class id, or omit for all families.
    pub(super) element_class: Option<String>,
    /// Include session-installed recipe drafts in the response.
    #[serde(default)]
    pub(super) include_session_drafts: bool,
}

/// Request for the `instantiate_recipe` convenience tool.
///
/// Creates a semantic element of `target_class` and immediately runs
/// the named recipe's `generate` function to populate geometry.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct InstantiateRecipeRequest {
    /// Recipe family id (from `select_recipe` / `list_recipe_families`).
    pub(super) family_id: String,
    /// Element class of the root semantic entity to create (e.g. `"wall_assembly"`).
    pub(super) target_class: String,
    /// Recipe parameters (e.g. `{"length_mm": 4000, "height_mm": 2700}`).
    /// Driver keys and units are recipe-specific; consult `list_recipe_families`
    /// for the parameter schema.
    #[serde(default)]
    #[cfg_attr(
        feature = "model-api",
        schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")
    )]
    pub(super) parameters: serde_json::Value,
    /// Optional placement. `translate` is in **metres** (world coordinates).
    #[serde(default)]
    pub(super) placement: Option<InstantiateRecipePlacement>,
    /// Target refinement state (default `"Constructible"`).
    pub(super) target_state: Option<String>,
}

/// Placement override for `instantiate_recipe`.
#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct InstantiateRecipePlacement {
    /// World-space translation in metres.
    #[serde(default)]
    pub(super) translate: [f64; 3],
    /// Euler angles in degrees (XYZ order), applied to the whole generated
    /// result. The recipe lays out its sub-elements in the host frame, so e.g.
    /// `[0,90,0]` turns an X-running wall into a Z-running one — use this to
    /// place the perpendicular walls of a rectangular building.
    #[serde(default)]
    pub(super) rotate_euler_deg: [f64; 3],
}

/// Response from `instantiate_recipe`.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstantiateRecipeResult {
    /// Element id of the root semantic entity that was created.
    pub root_element_id: u64,
    /// All element ids created by the recipe `generate` function (may be empty if
    /// the recipe generates no sub-elements, but `root_element_id` is always present).
    pub created_element_ids: Vec<u64>,
    /// The refinement state the root entity now occupies.
    pub state: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SelectRecipeRequest {
    pub(super) element_class: String,
    /// Context object — expected keys: `target_state` (required), `jurisdiction` (optional).
    #[serde(default)]
    pub(super) context: serde_json::Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CuratedPathDiscoveryRequest {
    /// `recipe`, `parametric`, `definition`, or `prior`. Defaults to `recipe`.
    #[serde(default)]
    pub(super) path_kind: Option<String>,
    /// Element class or concept id the caller wants to author.
    #[serde(default)]
    pub(super) element_class: Option<String>,
    #[serde(default)]
    pub(super) context: serde_json::Value,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CuratedPathDiscoveryInfo {
    pub path_kind: String,
    pub element_class: Option<String>,
    pub recipe_rankings: Vec<RecipeRankingInfo>,
    pub parametric_types: Vec<crate::plugins::parametric_mcp::ParametricTypeInfo>,
    pub definition_assets: Vec<DefinitionPathInfo>,
    pub curated_assets: Vec<CuratedAssetPathInfo>,
    pub generation_priors: Vec<GenerationPriorInfo>,
    pub related_asset_ids: Vec<String>,
    pub no_curated_path: Option<NoCuratedPathInfo>,
    /// Present when the requested `element_class` is not a registered semantic
    /// element class but is a recognised native modeling term (e.g. `door` /
    /// `window`, which are authored as `opening` entities, not element classes).
    /// When set, `no_curated_path` is `None`: this is not a corpus gap, it is a
    /// pointer to the existing native path. Keeps discovery in agreement with
    /// the `create_box` / `create_entity` semantic-annotation surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_class_term: Option<NonClassTermInfo>,
    pub suggested_next_tool: String,
    pub guidance_card_ids: Vec<String>,
}

/// Describes a requested term that is not a registered element class but does
/// have a native modeling path. Returned by `discover_curated_paths` instead of
/// a spurious `NoCuratedPath` gap.
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NonClassTermInfo {
    pub term: String,
    /// Human/agent-readable explanation of why this is not an element class and
    /// what to do instead.
    pub message: String,
    /// Native entity type(s) this term is authored as (e.g. `["opening"]`).
    pub native_entity_types: Vec<String>,
    /// Curated assembly pattern id(s) that already cover this term.
    pub assembly_pattern_ids: Vec<String>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CuratedAssetPathInfo {
    pub asset_id: String,
    pub manifest_kind: String,
    pub label: Option<String>,
    pub executable: bool,
    pub how_to_use: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionPathInfo {
    pub library_id: String,
    pub library_name: String,
    pub definition_id: String,
    pub name: String,
    pub definition_kind: String,
    pub how_to_instantiate: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct ListGuidanceCardsRequest {
    pub(super) task: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GetGuidanceCardRequest {
    pub(super) card_id: String,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuidanceCardInfo {
    pub id: String,
    pub title: String,
    pub task_tags: Vec<String>,
    pub summary: String,
    pub referenced_tool_ids: Vec<String>,
    pub next_card_ids: Vec<String>,
    pub json_examples: Vec<serde_json::Value>,
}

// --- PP74 request parameter structs ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ListConstraintsRequest {
    /// Optional scope filter. Currently ignored in PP74 (all constraints returned).
    pub(super) scope: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RunValidationV2Request {
    /// Element id to validate, or omit / `null` for whole model.
    pub(super) element_id: Option<u64>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ExplainFindingV2Request {
    pub(super) finding_id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PreviewPromotionRequest {
    pub(super) element_id: u64,
    pub(super) target_state: String,
    pub(super) recipe_id: Option<String>,
    #[serde(default)]
    pub(super) overrides: serde_json::Value,
}

// --- PP75 request parameter structs ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CatalogQueryRequest {
    /// Id of the provider to query (as returned by `list_catalog_providers`).
    pub(super) provider_id: String,
    /// Arbitrary JSON filter object. PP75: ignored by all providers (all rows returned).
    #[serde(default)]
    pub(super) filter: serde_json::Value,
}

// --- PP76 request parameter structs ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct ListGenerationPriorsRequest {
    /// Optional scope filter. Recognised keys: `element_class` (string),
    /// `claim_path` (string). Absent or empty object returns all priors.
    #[serde(default)]
    pub(super) scope_filter: Option<serde_json::Value>,
}

// --- PP78 request parameter structs ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct RequestCorpusExpansionRequest {
    pub(super) element_class: Option<String>,
    pub(super) jurisdiction: Option<String>,
    /// What kind of artifact is missing: `"rule_pack"`, `"catalog"`, `"passage"`, …
    pub(super) kind: String,
    /// Free-form rationale for the request.
    pub(super) rationale: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct LookupSourcePassageRequest {
    pub(super) passage_ref: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DraftRulePackRequest {
    /// The passage ref / chunk id to anchor the skeleton.
    pub(super) chunk_id: String,
    /// The element class the validator will apply to.
    pub(super) element_class: String,
}

// --- PP92 request parameter structs ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct ListRecipeDraftsRequest {
    pub(super) target_class: Option<String>,
    pub(super) status: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GetRecipeDraftRequest {
    pub(super) recipe_draft_id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SaveRecipeDraftRequest {
    pub(super) recipe_draft_id: Option<String>,
    /// Curation semantic scope. Defaults to project so acquired knowledge can
    /// survive project save/load and later MCP sessions.
    pub(super) scope: Option<String>,
    pub(super) label: String,
    pub(super) description: String,
    pub(super) target_class: String,
    #[serde(default)]
    pub(super) supported_refinement_levels: Vec<String>,
    #[serde(default)]
    pub(super) parameters: Vec<RecipeParameterInfo>,
    pub(super) jurisdiction: Option<String>,
    pub(super) gap_id: Option<String>,
    #[serde(default)]
    pub(super) source_passage_refs: Vec<String>,
    #[serde(default)]
    pub(super) evidence_slots: Vec<crate::plugins::knowledge_assets::EvidenceSlot>,
    #[serde(default)]
    pub(super) runtime_claims: Vec<crate::plugins::knowledge_assets::RuntimeCapabilityClaim>,
    #[serde(default)]
    pub(super) acquisition_context: serde_json::Value,
    #[serde(default)]
    pub(super) draft_script: serde_json::Value,
    #[serde(default)]
    pub(super) notes: Vec<String>,
    pub(super) status: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetRecipeDraftStatusRequest {
    pub(super) recipe_draft_id: String,
    pub(super) status: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct ListAssemblyPatternDraftsRequest {
    pub(super) target_type: Option<String>,
    pub(super) status: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GetAssemblyPatternDraftRequest {
    pub(super) assembly_pattern_draft_id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SaveAssemblyPatternDraftRequest {
    pub(super) assembly_pattern_draft_id: Option<String>,
    /// Curation semantic scope. Defaults to project so acquired knowledge can
    /// survive project save/load and later MCP sessions.
    pub(super) scope: Option<String>,
    pub(super) label: String,
    pub(super) description: String,
    #[serde(default)]
    pub(super) target_types: Vec<String>,
    pub(super) axis: String,
    #[serde(default)]
    pub(super) layers: Vec<AssemblyPatternLayerInfo>,
    #[serde(default)]
    pub(super) relation_rules: Vec<AssemblyPatternRelationRuleInfo>,
    #[serde(default)]
    pub(super) root_layer_ids: Vec<String>,
    #[serde(default)]
    pub(super) requires_support_path: bool,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) parameter_schema: serde_json::Value,
    pub(super) jurisdiction: Option<String>,
    pub(super) gap_id: Option<String>,
    #[serde(default)]
    pub(super) source_passage_refs: Vec<String>,
    #[serde(default)]
    pub(super) evidence_slots: Vec<crate::plugins::knowledge_assets::EvidenceSlot>,
    #[serde(default)]
    pub(super) runtime_claims: Vec<crate::plugins::knowledge_assets::RuntimeCapabilityClaim>,
    #[serde(default)]
    pub(super) acquisition_context: serde_json::Value,
    #[serde(default)]
    pub(super) notes: Vec<String>,
    pub(super) status: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetAssemblyPatternDraftStatusRequest {
    pub(super) assembly_pattern_draft_id: String,
    pub(super) status: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaterializeLearnedAssetRequest {
    pub(super) asset_id: String,
    #[serde(default)]
    pub(super) overrides: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) placement: Option<crate::relational::registry::Placement>,
}

#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MaterializeLearnedAssetResult {
    pub asset_id: String,
    pub execution_path: String,
    pub element_ids: Vec<u64>,
    pub evidence_backed_claim_ids: Vec<String>,
    pub last_verified: i64,
}

// --- Assembly / Relation request types ---

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAssemblyRequest {
    pub assembly_type: String,
    pub label: String,
    pub members: Vec<AssemblyMemberRefRequest>,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub relations: Vec<CreateRelationRequest>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyMemberRefRequest {
    pub target: u64,
    pub role: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRelationRequest {
    pub source: u64,
    pub target: u64,
    #[serde(rename = "type")]
    pub relation_type: String,
    #[serde(default)]
    pub parameters: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GetAssemblyRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct QueryRelationsRequest {
    pub(super) source: Option<u64>,
    pub(super) target: Option<u64>,
    pub(super) relation_type: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ListAssemblyMembersRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DefinitionGetRequest {
    pub(super) definition_id: String,
    #[serde(default)]
    pub(super) library_id: Option<String>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DefinitionDraftIdRequest {
    pub(super) draft_id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DefinitionLibraryGetRequest {
    pub(super) library_id: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DefinitionLibraryPathRequest {
    pub(super) path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DefinitionLibraryExportRequest {
    pub(super) library_id: String,
    pub(super) path: String,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OccurrenceUpdateOverridesRequest {
    pub(super) element_id: u64,
    pub(super) overrides: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OccurrenceMakeUniqueRequest {
    pub(super) element_id: u64,
    pub(super) name: Option<String>,
    #[serde(default = "default_copy_definition_dependencies")]
    pub(super) copy_dependencies: bool,
}

#[cfg(feature = "model-api")]
fn default_copy_definition_dependencies() -> bool {
    true
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OccurrenceResolveRequest {
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateHostFitRequest {
    pub(super) contract_kind: String,
    pub(super) host_element_id: u64,
    pub(super) hosted_element_id: u64,
    #[serde(default)]
    pub(super) contract_parameters: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateDefinitionHostContractRequest {
    pub(super) definition_id: String,
    pub(super) contract_kind: String,
    pub(super) host_element_id: u64,
    pub(super) hosted_element_id: u64,
    #[serde(default)]
    pub(super) contract_parameters: Value,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ArrayCreateLinearRequest {
    /// Source entity ID to array.
    pub(super) source: u64,
    /// Number of copies (includes the original). Minimum 2.
    pub(super) count: u32,
    /// Spacing vector [x, y, z] — direction × distance between successive copies.
    pub(super) spacing: [f32; 3],
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ArrayCreatePolarRequest {
    /// Source entity ID to array.
    pub(super) source: u64,
    /// Number of copies (includes the original). Minimum 2.
    pub(super) count: u32,
    /// Rotation axis [x, y, z]. Defaults to [0, 1, 0] (Y axis).
    pub(super) axis: Option<[f32; 3]>,
    /// Total sweep angle in degrees. Defaults to 360.0 (full circle).
    pub(super) total_angle_degrees: Option<f32>,
    /// Centre point of rotation [x, y, z]. Defaults to [0, 0, 0].
    pub(super) center: Option<[f32; 3]>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ArrayUpdateRequest {
    /// Element ID of the array node to update.
    pub(super) element_id: u64,
    /// New copy count (minimum 2).
    pub(super) count: Option<u32>,
    /// New spacing vector [x, y, z] (linear array only).
    pub(super) spacing: Option<[f32; 3]>,
    /// New rotation axis [x, y, z] (polar array only).
    pub(super) axis: Option<[f32; 3]>,
    /// New total angle in degrees (polar array only).
    pub(super) total_angle_degrees: Option<f32>,
    /// New centre of rotation [x, y, z] (polar array only).
    pub(super) center: Option<[f32; 3]>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ArrayEntityRequest {
    /// Element ID of the array node.
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct MirrorCreateRequest {
    /// Source entity ID to mirror.
    pub(super) source: u64,
    /// Mirror plane shortcut: "XY", "XZ", or "YZ". Takes priority over plane_origin/plane_normal.
    pub(super) plane: Option<String>,
    /// Mirror plane origin [x, y, z]. Used when `plane` is not set.
    pub(super) plane_origin: Option<[f32; 3]>,
    /// Mirror plane normal [x, y, z]. Used when `plane` is not set.
    pub(super) plane_normal: Option<[f32; 3]>,
    /// Whether to merge vertices at the seam (default: false).
    #[serde(default)]
    pub(super) merge: bool,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct MirrorUpdateRequest {
    /// Element ID of the MirrorNode to update.
    pub(super) element_id: u64,
    /// Mirror plane shortcut: "XY", "XZ", or "YZ".
    pub(super) plane: Option<String>,
    /// Mirror plane origin [x, y, z].
    pub(super) plane_origin: Option<[f32; 3]>,
    /// Mirror plane normal [x, y, z].
    pub(super) plane_normal: Option<[f32; 3]>,
    /// Whether to merge vertices at the seam.
    pub(super) merge: Option<bool>,
}

#[cfg(feature = "model-api")]
#[cfg_attr(feature = "model-api", derive(JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct MirrorEntityRequest {
    /// Element ID of the MirrorNode.
    pub(super) element_id: u64,
}

#[cfg(feature = "model-api")]
#[tool_router(router = tool_router)]
impl ModelApiServer {
    #[tool(
        name = "get_instance_info",
        description = "Get runtime identification for this Talos3D instance, including instance_id, MCP HTTP port, URL, registry manifest path, and pid."
    )]
    pub(super) async fn get_instance_info_tool(&self) -> Result<CallToolResult, McpError> {
        let info = self
            .request_get_instance_info()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "list_entities",
        description = "List all authored entities in the model."
    )]
    pub(super) async fn list_entities_tool(&self) -> Result<CallToolResult, McpError> {
        let entities = self
            .request_list_entities()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(entities)
    }

    #[tool(
        name = "get_entity",
        description = "Get a full entity snapshot by element ID."
    )]
    pub(super) async fn get_entity_tool(
        &self,
        Parameters(params): Parameters<GetEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let snapshot = self
            .request_get_entity(params.element_id)
            .await
            .map_err(|error| McpError::internal_error(error, None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("entity {} not found", params.element_id), None)
            })?;
        json_tool_result(snapshot)
    }

    #[tool(
        name = "get_entity_details",
        description = "Get an entity snapshot plus a normalized property list by element ID."
    )]
    pub(super) async fn get_entity_details_tool(
        &self,
        Parameters(params): Parameters<GetEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let details = self
            .request_get_entity_details(params.element_id)
            .await
            .map_err(|error| McpError::internal_error(error, None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("entity {} not found", params.element_id), None)
            })?;
        json_tool_result(details)
    }

    #[tool(
        name = "model_summary",
        description = "Get aggregate information about the authored model."
    )]
    pub(super) async fn model_summary_tool(&self) -> Result<CallToolResult, McpError> {
        let summary = self
            .request_model_summary()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(summary)
    }

    #[tool(
        name = "outline_tree",
        description = "Get the model's aggregation structure as a nested tree (the same structure shown in the Outliner panel). Returns `{\"roots\":[{element_id,label,kind,children}]}` where `kind` is one of group/occurrence/part/leaf; groups nest their members and compound occurrences nest their generated parts."
    )]
    pub(super) async fn outline_tree_tool(&self) -> Result<CallToolResult, McpError> {
        let outline = self
            .request_outline_tree()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(outline)
    }

    #[tool(
        name = "list_importers",
        description = "List all registered file importers."
    )]
    pub(super) async fn list_importers_tool(&self) -> Result<CallToolResult, McpError> {
        let importers = self
            .request_list_importers()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(importers)
    }

    #[tool(
        name = "create_entity",
        description = "Create an authored entity from a typed JSON object through the command pipeline. Returns CommandResult."
    )]
    pub(super) async fn create_entity_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_invoke_command(CMD_MODEL_API_CREATE_ENTITY.to_string(), json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "create_box",
        description = "Create a box primitive through the command pipeline. Accepts `center` (or `centre`) plus either `size` or `half_extents`, with optional quaternion `rotation`. Returns CommandResult."
    )]
    pub(super) async fn create_box_tool(
        &self,
        Parameters(params): Parameters<CreateBoxRequest>,
    ) -> Result<CallToolResult, McpError> {
        let parameters = serde_json::to_value(params)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let result = self
            .request_invoke_command(CMD_MODEL_API_CREATE_BOX.to_string(), parameters)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "import_file",
        description = "Import a supported file from disk and return the created entity IDs."
    )]
    pub(super) async fn import_file_tool(
        &self,
        Parameters(params): Parameters<ImportFileRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_ids = self
            .request_import_file(params.path, params.format_hint)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_ids)
    }

    #[tool(
        name = "semantic_shadow.accept_candidate",
        description = "Accept an inferred semantic shadow candidate for an imported entity and write the resulting native semantic annotation through the model API path."
    )]
    pub(super) async fn accept_semantic_shadow_candidate_tool(
        &self,
        Parameters(params): Parameters<AcceptSemanticShadowCandidateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let details = self
            .request_accept_semantic_shadow_candidate(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(details)
    }

    #[tool(
        name = "delete_entities",
        description = "Delete one or more entities by element ID through the command pipeline. Returns CommandResult."
    )]
    pub(super) async fn delete_entities_tool(
        &self,
        Parameters(params): Parameters<DeleteEntitiesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let parameters = serde_json::to_value(params)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let result = self
            .request_invoke_command(CMD_MODEL_API_DELETE_ENTITIES.to_string(), parameters)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "transform",
        description = "Move, rotate, or scale entities through the command pipeline. Returns CommandResult. \
LOCAL FRAMES (ADR-058): if an element_id is a GROUP, the whole assembly transforms as a rigid unit — every \
nested member moves/rotates together about `pivot` (default: the group's frame origin, else its bounds centre) \
and the group's local authoring frame is recorded. This is how you give a whole wing its angle in one call: \
build the wing axis-aligned inside a group, then rotate the GROUP about the junction corner — every wall and \
gable inherits the angle, so gable ends can never be left at the wrong angle. rotate `value` is degrees; \
`pivot` is world-space [x,y,z]."
    )]
    pub(super) async fn transform_tool(
        &self,
        Parameters(params): Parameters<TransformToolRequest>,
    ) -> Result<CallToolResult, McpError> {
        let parameters = serde_json::to_value(params)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let result = self
            .request_invoke_command(CMD_MODEL_API_TRANSFORM_ENTITIES.to_string(), parameters)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "set_property",
        description = "Set a single authored property on an entity through the command pipeline. Returns CommandResult."
    )]
    pub(super) async fn set_property_tool(
        &self,
        Parameters(params): Parameters<SetPropertyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let parameters = serde_json::to_value(params)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let result = self
            .request_invoke_command(CMD_MODEL_API_SET_ENTITY_PROPERTY.to_string(), parameters)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "set_entity_property",
        description = "Set a single authored property on an entity through the command pipeline. Returns CommandResult."
    )]
    pub(super) async fn set_entity_property_tool(
        &self,
        Parameters(params): Parameters<SetPropertyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let parameters = serde_json::to_value(params)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let result = self
            .request_invoke_command(CMD_MODEL_API_SET_ENTITY_PROPERTY.to_string(), parameters)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "list_handles",
        description = "List the read-only edit handles for an entity."
    )]
    pub(super) async fn list_handles_tool(
        &self,
        Parameters(params): Parameters<ListHandlesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let handles = self
            .request_list_handles(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(handles)
    }

    #[tool(
        name = "bim_property_set.get",
        description = "ADR-026 Phase 6a: read a single BIM property-set value from an entity. \
                       Returns the typed PropertyValue JSON (e.g. {\"text\": \"REI60\"}) or \
                       null if the entity has no PropertySetMap or the requested property is \
                       not authored."
    )]
    pub(super) async fn bim_property_set_get_tool(
        &self,
        Parameters(params): Parameters<BimPropertySetGetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_property_set_get(params.element_id, params.set_name, params.property_name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "bim_property_set.set",
        description = "ADR-026 Phase 6a: write a single BIM property-set value on an entity. \
                       The value is schema-validated against the PropertySetSchemaRegistry \
                       for the given definition_id; type mismatches and unknown set/property \
                       names are rejected. Per ADR-026 §1 this write does NOT invalidate the \
                       mesh cache. Returns the prior value (or null) for rollback / diff."
    )]
    pub(super) async fn bim_property_set_set_tool(
        &self,
        Parameters(params): Parameters<BimPropertySetSetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let prior = self
            .request_bim_property_set_set(
                params.element_id,
                params.definition_id,
                params.set_name,
                params.property_name,
                params.value,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(prior)
    }

    #[tool(
        name = "bim_exchange_identity.assign",
        description = "ADR-026 Phase 6b: assign a stable BIM exchange identifier to an entity. \
                       Uses assign-if-absent semantics: if the requested system already has an \
                       id, the call errors instead of regenerating it. Returns null on success."
    )]
    pub(super) async fn bim_exchange_identity_assign_tool(
        &self,
        Parameters(params): Parameters<BimExchangeIdentityAssignRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_exchange_identity_assign(
                params.element_id,
                params.system,
                params.exchange_id,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "bim_exchange_identity.get",
        description = "ADR-026 Phase 6b: read one exchange identifier from an entity for a \
                       system label (`ifc`, `revit`, `dwg`, `cobie`, or custom). Returns the \
                       id string or null if the entity has no id for that system."
    )]
    pub(super) async fn bim_exchange_identity_get_tool(
        &self,
        Parameters(params): Parameters<BimExchangeIdentityGetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_exchange_identity_get(params.element_id, params.system)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "bim_exchange_identity.list",
        description = "ADR-026 Phase 6b: list all stable BIM exchange identifiers assigned to \
                       an entity. Returns an object keyed by exchange system label."
    )]
    pub(super) async fn bim_exchange_identity_list_tool(
        &self,
        Parameters(params): Parameters<BimExchangeIdentityListRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_exchange_identity_list(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "bim_void.declare_for_definition",
        description = "ADR-026 Phase 6f: write a VoidDeclaration into a Definition's interface so \
                       placing that Definition cuts a void in its host. The declaration is the \
                       JSON shape of `VoidDeclaration` (kind=Rectangular|Profile, placement, \
                       exchange_role). Returns the prior inline declaration if any was previously \
                       declared (for diff / rollback), or null."
    )]
    pub(super) async fn bim_void_declare_for_definition_tool(
        &self,
        Parameters(params): Parameters<BimVoidDeclareForDefinitionRequest>,
    ) -> Result<CallToolResult, McpError> {
        let prior = self
            .request_bim_void_declare_for_definition(params.definition_id, params.declaration)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(prior)
    }

    #[tool(
        name = "bim_void.plan_placement",
        description = "ADR-026 Phase 6f: plan an atomic void placement. Validates that the \
                       filling Definition has an inline VoidDeclaration, that host != filling, \
                       and returns the planned OpeningContext + VoidLink components plus the \
                       freshly-allocated opening element id. The caller applies the plan in a \
                       single command per ADR-026 §Consequences."
    )]
    pub(super) async fn bim_void_plan_placement_tool(
        &self,
        Parameters(params): Parameters<BimVoidPlanPlacementRequest>,
    ) -> Result<CallToolResult, McpError> {
        let plan = self
            .request_bim_void_plan_placement(
                params.filling_definition,
                params.host_element_id,
                params.filling_element_id,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(plan)
    }

    #[tool(
        name = "bim_spatial.assign",
        description = "ADR-026 Phase 6g: assign an entity to a spatial container, enforcing the \
                       three-invariant contract (registered kind, no cycles, single-parent). \
                       Inserts a SpatialMembership component on the child entity on success. \
                       Returns null on success; errors out if the kind is unregistered, the \
                       child already has a parent, or the assignment would create a cycle."
    )]
    pub(super) async fn bim_spatial_assign_tool(
        &self,
        Parameters(params): Parameters<BimSpatialAssignRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_spatial_assign(
                params.child_element_id,
                params.container_element_id,
                params.container_kind,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "bim_spatial.list_kind_registry",
        description = "ADR-026 Phase 6g: list the registered spatial container kinds. Returns a \
                       JSON array of strings (e.g. [\"storey\", \"space\", \"zone\"])."
    )]
    pub(super) async fn bim_spatial_list_kind_registry_tool(
        &self,
    ) -> Result<CallToolResult, McpError> {
        let kinds = self
            .request_bim_spatial_list_kind_registry()
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(kinds)
    }

    #[tool(
        name = "get_document_properties",
        description = "Get the current document properties (units, grid, snap, domain defaults)."
    )]
    pub(super) async fn get_document_properties_tool(&self) -> Result<CallToolResult, McpError> {
        let props = self
            .request_get_document_properties()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(props)
    }

    #[tool(
        name = "set_document_properties",
        description = "Merge partial JSON into document properties. Only provided fields are updated."
    )]
    pub(super) async fn set_document_properties_tool(
        &self,
        Parameters(partial): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let updated = self
            .request_set_document_properties(partial)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(updated)
    }

    #[tool(
        name = "list_toolbars",
        description = "List registered toolbars, their sections, and current layout state."
    )]
    pub(super) async fn list_toolbars_tool(&self) -> Result<CallToolResult, McpError> {
        let toolbars = self
            .request_list_toolbars()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(toolbars)
    }

    #[tool(
        name = "set_toolbar_layout",
        description = "Update toolbar dock, order, or visibility and return the resulting layout."
    )]
    pub(super) async fn set_toolbar_layout_tool(
        &self,
        Parameters(params): Parameters<SetToolbarLayoutRequest>,
    ) -> Result<CallToolResult, McpError> {
        let toolbars = self
            .request_set_toolbar_layout(params.updates)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(toolbars)
    }

    #[tool(
        name = "list_commands",
        description = "List all registered commands with their descriptors, parameter schemas, and capability ownership."
    )]
    pub(super) async fn list_commands_tool(&self) -> Result<CallToolResult, McpError> {
        let commands = self
            .request_list_commands()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(commands)
    }

    #[tool(
        name = "invoke_command",
        description = "Execute a registered command by ID with optional parameters. Returns a CommandResult with created/modified/deleted entity IDs."
    )]
    pub(super) async fn invoke_command_tool(
        &self,
        Parameters(params): Parameters<InvokeCommandRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_invoke_command(params.command_id, params.parameters)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "prepare_site_surface",
        description = "Repair selected or explicitly listed contour entities, create elevation curves, and generate a draped terrain surface. This wraps the terrain.prepare_site_surface command in a dedicated MCP tool."
    )]
    pub(super) async fn prepare_site_surface_tool(
        &self,
        Parameters(params): Parameters<PrepareSiteSurfaceRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_prepare_site_surface(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "cut_fill_analysis",
        description = "Compute terrain cut, fill, and net volumes between an existing terrain surface and either a proposed terrain surface or horizontal datum. This wraps the terrain.cut_fill_analysis command in a dedicated MCP tool."
    )]
    pub(super) async fn cut_fill_analysis_tool(
        &self,
        Parameters(params): Parameters<TerrainCutFillAnalysisRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_terrain_cut_fill_analysis(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "elevation_at",
        description = "Return terrain elevation at world X/Z coordinates using the registered TerrainProvider."
    )]
    pub(super) async fn elevation_at_tool(
        &self,
        Parameters(params): Parameters<TerrainElevationAtRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_terrain_elevation_at(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "get_editing_context",
        description = "Get the current group editing context: whether at root or inside a group, with a breadcrumb path."
    )]
    pub(super) async fn get_editing_context_tool(&self) -> Result<CallToolResult, McpError> {
        let context = self
            .request_get_editing_context()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(context)
    }

    #[tool(
        name = "enter_group",
        description = "Enter a group for editing — work in its LOCAL coordinate frame (ADR-058). While inside, \
geometry you author is interpreted in the group's rectified local frame and composed to world by the group's \
frame, AND is auto-added to the group. So to build an angled volume: create_entity {type:group, \
frame_rotate_euler_deg:[0,deg,0], frame_origin:[x,y,z]} (or rotate the group later with `transform`), enter it, \
then author everything AXIS-ALIGNED in clean local coords — the frame carries the angle. get_editing_context \
reports the active frame. Returns the updated editing context. Call exit_group when done."
    )]
    pub(super) async fn enter_group_tool(
        &self,
        Parameters(params): Parameters<EnterGroupRequest>,
    ) -> Result<CallToolResult, McpError> {
        let context = self
            .request_enter_group(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(context)
    }

    #[tool(
        name = "exit_group",
        description = "Exit the current group editing context and return to its parent. At root level this is a no-op."
    )]
    pub(super) async fn exit_group_tool(&self) -> Result<CallToolResult, McpError> {
        let context = self
            .request_exit_group()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(context)
    }

    #[tool(
        name = "list_group_members",
        description = "List the direct members of a group by element ID."
    )]
    pub(super) async fn list_group_members_tool(
        &self,
        Parameters(params): Parameters<ListGroupMembersRequest>,
    ) -> Result<CallToolResult, McpError> {
        let members = self
            .request_list_group_members(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(members)
    }

    // --- Layer Management ---

    #[tool(
        name = "list_layers",
        description = "List all layers with their visibility, locked state, color, and whether each is the active layer."
    )]
    pub(super) async fn list_layers_tool(&self) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_list_layers()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "set_layer_visibility",
        description = "Toggle a layer's visibility on or off."
    )]
    pub(super) async fn set_layer_visibility_tool(
        &self,
        Parameters(params): Parameters<SetLayerVisibilityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_set_layer_visibility(params.name, params.visible)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "set_layer_locked",
        description = "Toggle a layer's locked state. Locked layers block selection and editing."
    )]
    pub(super) async fn set_layer_locked_tool(
        &self,
        Parameters(params): Parameters<SetLayerLockedRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_set_layer_locked(params.name, params.locked)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "assign_layer",
        description = "Move an entity to a different layer by name."
    )]
    pub(super) async fn assign_layer_tool(
        &self,
        Parameters(params): Parameters<AssignLayerRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_assign_layer(params.element_id, params.layer_name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "create_layer",
        description = "Create a new layer. Returns the updated layer list."
    )]
    pub(super) async fn create_layer_tool(
        &self,
        Parameters(params): Parameters<CreateLayerRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_create_layer(params.name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "rename_layer",
        description = "Rename a layer and move every object on it onto the new name. The Default layer cannot be renamed. Returns the updated layer list."
    )]
    pub(super) async fn rename_layer_tool(
        &self,
        Parameters(params): Parameters<RenameLayerRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_rename_layer(params.old_name, params.new_name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    #[tool(
        name = "delete_layer",
        description = "Delete a layer, moving any objects on it back to the Default layer. The Default layer cannot be deleted. Returns the updated layer list."
    )]
    pub(super) async fn delete_layer_tool(
        &self,
        Parameters(params): Parameters<DeleteLayerRequest>,
    ) -> Result<CallToolResult, McpError> {
        let layers = self
            .request_delete_layer(params.name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(layers)
    }

    // --- Dependency Graph (read-only) ---

    #[tool(
        name = "dependency_graph",
        description = "Get the model's change-propagation dependency graph (ADR-007). Returns `{nodes:[{element_id,label,depends_on_count,dependent_count}], edges:[{dependent,dependency,role}], topological_order, has_cycle, node_count, edge_count}`. An edge `dependent → dependency` means the dependent must be re-evaluated when the dependency changes; `topological_order` is null when `has_cycle` is true."
    )]
    pub(super) async fn dependency_graph_tool(&self) -> Result<CallToolResult, McpError> {
        let graph = self
            .request_dependency_graph()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(graph)
    }

    #[tool(
        name = "entity_dependencies",
        description = "Inspect one entity's place in the dependency graph: `{element_id, label, depends_on:[{element_id,label,role}], dependents:[{element_id,label}], propagates_to:[{element_id,label}]}`. `depends_on` is what it directly consumes, `dependents` is what directly consumes it, and `propagates_to` is the full transitive set that a change to it would dirty."
    )]
    pub(super) async fn entity_dependencies_tool(
        &self,
        Parameters(params): Parameters<EntityDependenciesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let deps = self
            .request_entity_dependencies(params.element_id)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(deps)
    }

    // --- Named Views ---

    #[tool(name = "view_list", description = "List all named views.")]
    pub(super) async fn view_list_tool(&self) -> Result<CallToolResult, McpError> {
        let views = self
            .request_view_list()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(views)
    }

    #[tool(
        name = "view_save",
        description = "Save the current camera position as a named view, or save explicit camera parameters."
    )]
    pub(super) async fn view_save_tool(
        &self,
        Parameters(params): Parameters<ViewSaveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let view = self
            .request_view_save(params.name, params.description, params.camera)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(view)
    }

    #[tool(
        name = "view_restore",
        description = "Restore the camera to a previously saved named view."
    )]
    pub(super) async fn view_restore_tool(
        &self,
        Parameters(params): Parameters<ViewRestoreRequest>,
    ) -> Result<CallToolResult, McpError> {
        let view = self
            .request_view_restore(params.name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(view)
    }

    #[tool(
        name = "view_update",
        description = "Update a named view's name, description, or camera parameters."
    )]
    pub(super) async fn view_update_tool(
        &self,
        Parameters(params): Parameters<ViewUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let view = self
            .request_view_update(
                params.name,
                params.new_name,
                params.description,
                params.camera,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(view)
    }

    #[tool(name = "view_delete", description = "Delete a named view by name.")]
    pub(super) async fn view_delete_tool(
        &self,
        Parameters(params): Parameters<ViewDeleteRequest>,
    ) -> Result<CallToolResult, McpError> {
        self.request_view_delete(params.name)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(serde_json::json!({"ok": true}))
    }

    // --- Clipping Planes ---

    #[tool(
        name = "clip_plane_create",
        description = "Create a section-view clipping plane as drawing metadata. Geometry on the side opposite to the normal is hidden. Returns the new element_id."
    )]
    pub(super) async fn clip_plane_create_tool(
        &self,
        Parameters(params): Parameters<ClipPlaneCreateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_clip_plane_create(params.name, params.origin, params.normal, params.active)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(serde_json::json!({ "element_id": element_id }))
    }

    #[tool(
        name = "clip_plane_update",
        description = "Update a section-view clipping plane's name, origin, normal, or active state."
    )]
    pub(super) async fn clip_plane_update_tool(
        &self,
        Parameters(params): Parameters<ClipPlaneUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_clip_plane_update(
                params.element_id,
                params.name,
                params.origin,
                params.normal,
                params.active,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "clip_plane_list",
        description = "List all section-view clipping planes and their active state."
    )]
    pub(super) async fn clip_plane_list_tool(&self) -> Result<CallToolResult, McpError> {
        let planes = self
            .request_clip_plane_list()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(planes)
    }

    #[tool(
        name = "clip_plane_toggle",
        description = "Activate or deactivate a section-view clipping plane by element_id."
    )]
    pub(super) async fn clip_plane_toggle_tool(
        &self,
        Parameters(params): Parameters<ClipPlaneToggleRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_clip_plane_toggle(params.element_id, params.active)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    // --- Materials ---

    #[tool(
        name = "list_materials",
        description = "List all materials in the project registry. Returns id, name, PBR properties, texture paths, and UV tiling."
    )]
    pub(super) async fn list_materials_tool(&self) -> Result<CallToolResult, McpError> {
        let materials = self
            .request_list_materials()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(materials)
    }

    #[tool(
        name = "get_material",
        description = "Get full details for a specific material by id."
    )]
    pub(super) async fn get_material_tool(
        &self,
        Parameters(params): Parameters<GetMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let material = self
            .request_get_material(params.id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(material)
    }

    #[tool(
        name = "create_material",
        description = "Create a new material in the project registry. Specify PBR properties (base_color as [r,g,b,a], perceptual_roughness, metallic, reflectance, emissive as [r,g,b]), alpha_mode (opaque/blend/mask), UV tiling (uv_scale as [x,y], uv_rotation_deg), and optional texture file paths."
    )]
    pub(super) async fn create_material_tool(
        &self,
        Parameters(params): Parameters<CreateMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let material = self
            .request_create_material(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(material)
    }

    #[tool(
        name = "update_material",
        description = "Update an existing material's properties. Takes the same fields as create_material plus the material id."
    )]
    pub(super) async fn update_material_tool(
        &self,
        Parameters(params): Parameters<UpdateMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let material = self
            .request_update_material(params.id, params.material)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(material)
    }

    #[tool(
        name = "delete_material",
        description = "Delete a material from the registry and remove its assignment from all entities."
    )]
    pub(super) async fn delete_material_tool(
        &self,
        Parameters(params): Parameters<DeleteMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let id = self
            .request_delete_material(params.id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(id)
    }

    #[tool(
        name = "apply_material",
        description = "Apply a material to one or more entities by element_id. Pass material_id and element_ids array."
    )]
    pub(super) async fn apply_material_tool(
        &self,
        Parameters(params): Parameters<ApplyMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let applied = self
            .request_apply_material(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(applied)
    }

    #[tool(
        name = "assign_material",
        description = "Assign a material to one or more entities. Pass material_id to use an existing registry material, or pass base_color and/or texture references such as base_color_texture: { asset: { path } } with optional name, perceptual_roughness, and metallic to create a project material and assign it."
    )]
    pub(super) async fn assign_material_tool(
        &self,
        Parameters(params): Parameters<AssignMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let assigned = self
            .request_assign_material(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(assigned)
    }

    #[tool(
        name = "remove_material_assignment",
        description = "Remove the material assignment from entities, reverting them to the default material."
    )]
    pub(super) async fn remove_material_tool(
        &self,
        Parameters(params): Parameters<RemoveMaterialRequest>,
    ) -> Result<CallToolResult, McpError> {
        let removed = self
            .request_remove_material(params.element_ids)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(removed)
    }

    #[tool(
        name = "get_material_assignment",
        description = "Get the authored material assignment for one entity by element_id."
    )]
    pub(super) async fn get_material_assignment_tool(
        &self,
        Parameters(params): Parameters<GetMaterialAssignmentRequest>,
    ) -> Result<CallToolResult, McpError> {
        let assignment = self
            .request_get_material_assignment(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(assignment)
    }

    #[tool(
        name = "set_material_assignment",
        description = "Set a typed material assignment for one or more entities. Supports single bindings and ordered layer sets."
    )]
    pub(super) async fn set_material_assignment_tool(
        &self,
        Parameters(params): Parameters<SetMaterialAssignmentRequest>,
    ) -> Result<CallToolResult, McpError> {
        let updated = self
            .request_set_material_assignment(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(updated)
    }

    #[tool(
        name = "get_texture_mapping",
        description = "Inspect texture mapping for either a material_id default or an element_id assignment. Exactly one target is required; element targets include UV diagnostics."
    )]
    pub(super) async fn get_texture_mapping_tool(
        &self,
        Parameters(params): Parameters<GetTextureMappingRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_get_texture_mapping(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "update_texture_mapping",
        description = "Patch texture mapping for either a material_id default or an element_id assignment override. Exactly one target is required. Mapping supports projection, uv_scale, uv_offset, uv_rotation_deg, flip_u, flip_v, and blend_sharpness."
    )]
    pub(super) async fn update_texture_mapping_tool(
        &self,
        Parameters(params): Parameters<UpdateTextureMappingRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_update_texture_mapping(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "reset_texture_mapping",
        description = "Reset texture mapping for either a material_id default or an element_id assignment override. Material targets reset to renderer defaults; element targets clear the assignment override."
    )]
    pub(super) async fn reset_texture_mapping_tool(
        &self,
        Parameters(params): Parameters<ResetTextureMappingRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_reset_texture_mapping(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "bim_material.assign_layered",
        description = "ADR-026 Phase 6d: assign a BIM layered material build-up. \
                       Provide exactly one target: definition_id for the type-level default, \
                       or element_id for an occurrence override."
    )]
    pub(super) async fn bim_material_assign_layered_tool(
        &self,
        Parameters(params): Parameters<BimMaterialAssignLayeredRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_material_assign_layered(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "bim_material.assign_constituents",
        description = "ADR-026 Phase 6d: assign a BIM constituent material set. \
                       Provide exactly one target: definition_id for the type-level default, \
                       or element_id for an occurrence override. Fractions must sum to 1.0."
    )]
    pub(super) async fn bim_material_assign_constituents_tool(
        &self,
        Parameters(params): Parameters<BimMaterialAssignConstituentsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_material_assign_constituents(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "bim_material.get_effective",
        description = "ADR-026 Phase 6d: resolve the effective BIM material assignment. \
                       With element_id, returns occurrence override first and then definition default. \
                       With definition_id, returns the definition default."
    )]
    pub(super) async fn bim_material_get_effective_tool(
        &self,
        Parameters(params): Parameters<BimMaterialGetEffectiveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_bim_material_get_effective(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "quantity.set",
        description = "ADR-026 Phase 6e: set one typed quantity on an entity. \
                       For per-material quantities, pass material plus field \
                       volume_m3, area_m2, length_m, mass_kg, or count."
    )]
    pub(super) async fn quantity_set_tool(
        &self,
        Parameters(params): Parameters<QuantitySetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_quantity_set(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "quantity.get",
        description = "ADR-026 Phase 6e: get an entity QuantitySet, or one field. \
                       Omit field for the full set; pass material to read a per-material field."
    )]
    pub(super) async fn quantity_get_tool(
        &self,
        Parameters(params): Parameters<QuantityGetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_quantity_get(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "quantity.list_provenance",
        description = "ADR-026 Phase 6e: list provenance records for all set primary and material quantities on an entity."
    )]
    pub(super) async fn quantity_list_provenance_tool(
        &self,
        Parameters(params): Parameters<QuantityListProvenanceRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_quantity_list_provenance(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "quantity.check_invariants",
        description = "ADR-026 Phase 6e: check gross/net, opening deduction, and grounded-provenance invariants for an entity QuantitySet."
    )]
    pub(super) async fn quantity_check_invariants_tool(
        &self,
        Parameters(params): Parameters<QuantityCheckInvariantsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .request_quantity_check_invariants(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(value)
    }

    #[tool(
        name = "list_material_specs",
        description = "List curated construction material specs. Optional filters: scope, trust, classification."
    )]
    pub(super) async fn list_material_specs_tool(
        &self,
        Parameters(params): Parameters<ListMaterialSpecsFilter>,
    ) -> Result<CallToolResult, McpError> {
        let specs = self
            .request_list_material_specs(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(specs)
    }

    #[tool(
        name = "get_material_spec",
        description = "Get a curated material spec by asset_id."
    )]
    pub(super) async fn get_material_spec_tool(
        &self,
        Parameters(params): Parameters<GetMaterialSpecRequest>,
    ) -> Result<CallToolResult, McpError> {
        let spec = self
            .request_get_material_spec(params.asset_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(spec)
    }

    #[tool(
        name = "create_material_spec",
        description = "Create a project-scope draft MaterialSpec. Provide body plus optional asset_id, author, and rationale."
    )]
    pub(super) async fn create_material_spec_tool(
        &self,
        Parameters(params): Parameters<DraftMaterialSpecRequest>,
    ) -> Result<CallToolResult, McpError> {
        let spec = self
            .request_create_material_spec(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(spec)
    }

    #[tool(
        name = "update_material_spec",
        description = "Replace the body of an existing MaterialSpec draft."
    )]
    pub(super) async fn update_material_spec_tool(
        &self,
        Parameters(params): Parameters<UpdateMaterialSpecRequest>,
    ) -> Result<CallToolResult, McpError> {
        let spec = self
            .request_update_material_spec(params.asset_id, params.body, params.rationale)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(spec)
    }

    #[tool(
        name = "save_material_spec",
        description = "Change the scope of a MaterialSpec draft or project asset."
    )]
    pub(super) async fn save_material_spec_tool(
        &self,
        Parameters(params): Parameters<SaveMaterialSpecRequest>,
    ) -> Result<CallToolResult, McpError> {
        let spec = self
            .request_save_material_spec(params.asset_id, params.scope)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(spec)
    }

    #[tool(
        name = "publish_material_spec",
        description = "Publish a MaterialSpec when its publication-policy floor passes."
    )]
    pub(super) async fn publish_material_spec_tool(
        &self,
        Parameters(params): Parameters<GetMaterialSpecRequest>,
    ) -> Result<CallToolResult, McpError> {
        let spec = self
            .request_publish_material_spec(params.asset_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(spec)
    }

    #[tool(
        name = "delete_material_spec",
        description = "Delete a non-shipped MaterialSpec by asset_id."
    )]
    pub(super) async fn delete_material_spec_tool(
        &self,
        Parameters(params): Parameters<DeleteMaterialSpecRequest>,
    ) -> Result<CallToolResult, McpError> {
        let asset_id = self
            .request_delete_material_spec(params.asset_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(asset_id)
    }

    #[tool(
        name = "get_lighting_scene",
        description = "Get ambient scene lighting settings and all authored light entities."
    )]
    pub(super) async fn get_lighting_scene_tool(&self) -> Result<CallToolResult, McpError> {
        let lighting = self
            .request_get_lighting_scene()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(lighting)
    }

    #[tool(
        name = "list_lights",
        description = "List all authored light entities in the current scene."
    )]
    pub(super) async fn list_lights_tool(&self) -> Result<CallToolResult, McpError> {
        let lights = self
            .request_list_lights()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(lights)
    }

    #[tool(
        name = "create_light",
        description = "Create an authored light entity. kind must be directional, point, or spot."
    )]
    pub(super) async fn create_light_tool(
        &self,
        Parameters(params): Parameters<CreateLightRequest>,
    ) -> Result<CallToolResult, McpError> {
        let light = self
            .request_create_light(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(light)
    }

    #[tool(
        name = "place_guide_line",
        description = "Create a construction guide line from an anchor plus direction, a through point, or an angle relative to a reference direction on a plane."
    )]
    pub(super) async fn place_guide_line_tool(
        &self,
        Parameters(params): Parameters<PlaceGuideLineRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_create_entity(create_guide_line_request_json(&params))
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "place_dimension_line",
        description = "Create a drawing dimension annotation from start and end points, then place the visible dimension line with line_point or offset. Optionally override extension, units, and precision."
    )]
    pub(super) async fn place_dimension_line_tool(
        &self,
        Parameters(params): Parameters<PlaceDimensionLineRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_create_entity(create_dimension_line_request_json(&params))
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "place_dimension_between_handles",
        description = "Create a dimension line between two authored handles, such as `corner_0` and `corner_1` on a box. Use `list_handles` to discover stable handle ids, then place the visible line with `line_point` or `offset`."
    )]
    pub(super) async fn place_dimension_between_handles_tool(
        &self,
        Parameters(params): Parameters<PlaceDimensionBetweenHandlesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_place_dimension_between_handles(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "boolean_union",
        description = "Combine two solids into one by adding their volumes together. Both operands become hidden and a new combined solid is created. The result preserves the parametric operands so either can still be edited."
    )]
    pub(super) async fn boolean_union_tool(
        &self,
        Parameters(params): Parameters<BooleanOperationRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_create_entity(boolean_request_json(params.base, params.tool, "union"))
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "boolean_difference",
        description = "Subtract the tool solid from the base solid. The tool volume is removed from the base. Both operands become hidden and a new result solid is created. Use this for cutting holes, openings, recesses, or any subtractive operation."
    )]
    pub(super) async fn boolean_difference_tool(
        &self,
        Parameters(params): Parameters<BooleanOperationRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_create_entity(boolean_request_json(params.base, params.tool, "difference"))
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "boolean_intersection",
        description = "Keep only the volume where two solids overlap. Both operands become hidden and a new result solid containing only the shared volume is created."
    )]
    pub(super) async fn boolean_intersection_tool(
        &self,
        Parameters(params): Parameters<BooleanOperationRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_create_entity(boolean_request_json(
                params.base,
                params.tool,
                "intersection",
            ))
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "update_light",
        description = "Update an authored light entity by element_id."
    )]
    pub(super) async fn update_light_tool(
        &self,
        Parameters(params): Parameters<UpdateLightRequest>,
    ) -> Result<CallToolResult, McpError> {
        let light = self
            .request_update_light(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(light)
    }

    #[tool(
        name = "delete_light",
        description = "Delete an authored light entity by element_id."
    )]
    pub(super) async fn delete_light_tool(
        &self,
        Parameters(params): Parameters<DeleteLightRequest>,
    ) -> Result<CallToolResult, McpError> {
        let deleted = self
            .request_delete_light(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(deleted)
    }

    #[tool(
        name = "set_ambient_light",
        description = "Update ambient scene lighting without changing authored light entities."
    )]
    pub(super) async fn set_ambient_light_tool(
        &self,
        Parameters(params): Parameters<AmbientLightUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let ambient = self
            .request_set_ambient_light(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(ambient)
    }

    #[tool(
        name = "restore_default_light_rig",
        description = "Replace existing authored lights with the default daylight rig."
    )]
    pub(super) async fn restore_default_light_rig_tool(&self) -> Result<CallToolResult, McpError> {
        let lights = self
            .request_restore_default_light_rig()
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(lights)
    }

    #[tool(
        name = "get_render_settings",
        description = "Get the current viewport renderer settings, including tonemapping, exposure, post-processing, drawing overlays, grid visibility, paper fill, X-ray surface transparency, and background color."
    )]
    pub(super) async fn get_render_settings_tool(&self) -> Result<CallToolResult, McpError> {
        let settings = self
            .request_get_render_settings()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(settings)
    }

    #[tool(
        name = "set_render_settings",
        description = "Update viewport renderer settings. Pass any subset of tonemapping, exposure, post-processing, drawing overlays, grid visibility, paper fill, X-ray surface transparency, and background color fields."
    )]
    pub(super) async fn set_render_settings_tool(
        &self,
        Parameters(params): Parameters<RenderSettingsUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let settings = self
            .request_set_render_settings(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(settings)
    }

    #[tool(
        name = "get_camera",
        description = "Get the live orbit camera state for the active viewport."
    )]
    pub(super) async fn get_camera_tool(&self) -> Result<CallToolResult, McpError> {
        let camera = self
            .request_get_camera()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(camera)
    }

    #[tool(
        name = "set_camera",
        description = "Update the live orbit camera directly. Any omitted fields keep their current value."
    )]
    pub(super) async fn set_camera_tool(
        &self,
        Parameters(params): Parameters<CameraParams>,
    ) -> Result<CallToolResult, McpError> {
        let camera = self
            .request_set_camera(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(camera)
    }

    // --- Selection ---

    #[tool(
        name = "get_selection",
        description = "Get the element IDs of all currently selected entities."
    )]
    pub(super) async fn get_selection_tool(&self) -> Result<CallToolResult, McpError> {
        let selection = self
            .request_get_selection()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(selection)
    }

    #[tool(
        name = "set_selection",
        description = "Replace the current selection with the given element IDs."
    )]
    pub(super) async fn set_selection_tool(
        &self,
        Parameters(params): Parameters<SetSelectionRequest>,
    ) -> Result<CallToolResult, McpError> {
        let selection = self
            .request_set_selection(params.element_ids)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(selection)
    }

    #[tool(
        name = "ux_observe",
        description = "Observe the live UX state: pending injected input steps, selected element IDs, primary window/cursor coordinates, and projected screen bounds for user-facing model entities."
    )]
    pub(super) async fn ux_observe_tool(&self) -> Result<CallToolResult, McpError> {
        let snapshot = self
            .request_ux_observe()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(snapshot)
    }

    #[tool(
        name = "ux_move_pointer",
        description = "Queue a pointer move through the live Bevy input path using primary-window logical pixel coordinates."
    )]
    pub(super) async fn ux_move_pointer_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::ux_harness::UxPointerMoveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_ux_move_pointer(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "ux_click",
        description = "Queue a pointer move, mouse press, and mouse release through the live Bevy input path. Coordinates are primary-window logical pixels."
    )]
    pub(super) async fn ux_click_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::ux_harness::UxClickRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_ux_click(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "ux_drag",
        description = "Queue a live pointer drag through the Bevy input path. Use primary-window logical pixel coordinates, optional button left/right/middle, and optional step count."
    )]
    pub(super) async fn ux_drag_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::ux_harness::UxDragRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_ux_drag(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "ux_press_key",
        description = "Queue a key press and release through the live Bevy input path. Supports letters, KeyG-style names, Escape, Delete, Backspace, Enter, ShiftLeft, and ShiftRight."
    )]
    pub(super) async fn ux_press_key_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::ux_harness::UxPressKeyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_ux_press_key(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "align_preview",
        description = "Preview multi-entity axis alignment without applying it. Supports min, max, or center alignment on x, y, or z."
    )]
    pub(super) async fn align_preview_tool(
        &self,
        Parameters(params): Parameters<AlignRequest>,
    ) -> Result<CallToolResult, McpError> {
        let preview = self
            .request_align_preview(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(preview)
    }

    #[tool(
        name = "align_execute",
        description = "Align multiple entities along x, y, or z using min, max, or center semantics. Returns the applied positions."
    )]
    pub(super) async fn align_execute_tool(
        &self,
        Parameters(params): Parameters<AlignRequest>,
    ) -> Result<CallToolResult, McpError> {
        let preview = self
            .request_align_execute(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(preview)
    }

    #[tool(
        name = "distribute_preview",
        description = "Preview equal spacing or equal gap distribution along x, y, or z without applying it."
    )]
    pub(super) async fn distribute_preview_tool(
        &self,
        Parameters(params): Parameters<DistributeRequest>,
    ) -> Result<CallToolResult, McpError> {
        let preview = self
            .request_distribute_preview(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(preview)
    }

    #[tool(
        name = "distribute_execute",
        description = "Distribute multiple entities along x, y, or z using equal center spacing or equal edge gaps. Returns the applied positions."
    )]
    pub(super) async fn distribute_execute_tool(
        &self,
        Parameters(params): Parameters<DistributeRequest>,
    ) -> Result<CallToolResult, McpError> {
        let preview = self
            .request_distribute_execute(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(preview)
    }

    // --- Face Subdivision ---

    #[tool(
        name = "split_box_face",
        description = "Split a box entity into two boxes along a face axis. face_id 0-5 maps to -X,+X,-Y,+Y,-Z,+Z. split_position is 0.0-1.0 along the split axis. Returns the new element IDs for the two boxes and the CompositeSolid group."
    )]
    pub(super) async fn split_box_face_tool(
        &self,
        Parameters(params): Parameters<SplitBoxFaceRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_split_box_face(params.element_id, params.face_id, params.split_position)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- Screenshot ---

    #[tool(
        name = "take_screenshot",
        description = "Capture a screenshot and save it to disk. By default the image is cropped to the active modeling viewport so app chrome is excluded, while authored viewport annotations such as dimensions remain visible. Pass include_ui=true to capture the full app window with egui panels and chrome for UX validation. Raster formats save as images; PDF and SVG embed the captured image."
    )]
    pub(super) async fn take_screenshot_tool(
        &self,
        Parameters(params): Parameters<TakeScreenshotRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_take_screenshot(params.path, params.include_ui)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    #[tool(
        name = "export_drawing",
        description = "Export the current cropped drawing viewport to PNG, PDF, or SVG. SVG is also accepted via the legacy `svd` file extension alias. Returns the file path where the drawing was saved."
    )]
    pub(super) async fn export_drawing_tool(
        &self,
        Parameters(params): Parameters<ExportDrawingRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_export_drawing(params.path)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    #[tool(
        name = "export_drafting_sheet",
        description = "Capture the current orthographic camera into a paper-mm DraftingSheet and export it. Extension selects the writer: .svg (paper-mm native), .pdf, .dxf (mm), or .png. Optional `scale_denominator` sets the drawing scale (1:N), defaulting to 1:50. Refuses perspective cameras."
    )]
    pub(super) async fn export_drafting_sheet_tool(
        &self,
        Parameters(params): Parameters<ExportDraftingSheetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_export_drafting_sheet(params.path, params.scale_denominator)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    #[tool(
        name = "export.fidelity.describe",
        description = "Describe what an export surface preserves, degrades, or omits. Accepts optional surface or path; omit both to list all known manifests."
    )]
    pub(super) async fn export_fidelity_describe_tool(
        &self,
        Parameters(params): Parameters<ExportFidelityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let manifests = if let Some(surface) = params.surface {
            vec![
                crate::plugins::export_fidelity::export_fidelity_manifest_for_surface(&surface)
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            format!("unknown export fidelity surface: {surface}"),
                            None,
                        )
                    })?,
            ]
        } else if let Some(path) = params.path {
            vec![
                crate::plugins::export_fidelity::export_fidelity_manifest_for_path(&path)
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            format!("could not infer export fidelity surface from path: {path}"),
                            None,
                        )
                    })?,
            ]
        } else {
            crate::plugins::export_fidelity::all_export_fidelity_manifests()
        };
        json_tool_result(manifests)
    }

    #[tool(
        name = "place_sheet_dimension",
        description = "Place a linear dimension in paper-millimetre coordinates on the DraftingSheet captured from the current orthographic view. `a`, `b`, and `offset` are 2D paper-mm vectors in the sheet's frame; they are inverse-projected to world-space and stored as a regular drafting_dimension. Refuses perspective cameras. Returns the created element id."
    )]
    pub(super) async fn place_sheet_dimension_tool(
        &self,
        Parameters(params): Parameters<PlaceSheetDimensionRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_place_sheet_dimension(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "frame_model",
        description = "Frame the orbit camera around the authored model and return the fitted bounding box."
    )]
    pub(super) async fn frame_model_tool(&self) -> Result<CallToolResult, McpError> {
        let bounds = self
            .request_frame_model()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(bounds)
    }

    #[tool(
        name = "frame_entities",
        description = "Frame the orbit camera around the given authored entities and return the fitted bounding box."
    )]
    pub(super) async fn frame_entities_tool(
        &self,
        Parameters(params): Parameters<FrameEntitiesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let bounds = self
            .request_frame_entities(params.element_ids)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(bounds)
    }

    #[tool(
        name = "save_project",
        description = "Save the current Talos3D project to a specific path on disk and return the resolved file path."
    )]
    pub(super) async fn save_project_tool(
        &self,
        Parameters(params): Parameters<SaveProjectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_save_project(params.path)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    #[tool(
        name = "load_project",
        description = "Load a Talos3D project from a specific path on disk and return the resolved file path."
    )]
    pub(super) async fn load_project_tool(
        &self,
        Parameters(params): Parameters<LoadProjectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_load_project(params.path)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(serde_json::json!({ "path": path }))
    }

    // --- Semantic Assembly / Relation tools ---

    #[tool(
        name = "list_vocabulary",
        description = "List all registered assembly types, reusable assembly patterns, and relation types. This is how AI discovers what domain concepts are available."
    )]
    pub(super) async fn list_vocabulary_tool(&self) -> Result<CallToolResult, McpError> {
        let vocab = self
            .request_list_vocabulary()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(vocab)
    }

    #[tool(
        name = "create_assembly",
        description = "Create a semantic assembly with typed members and optionally create relations. The entire operation is one undoable unit."
    )]
    pub(super) async fn create_assembly_tool(
        &self,
        Parameters(params): Parameters<CreateAssemblyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_create_assembly(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "get_assembly",
        description = "Get full details of a semantic assembly by element ID, including members enriched with entity type and label."
    )]
    pub(super) async fn get_assembly_tool(
        &self,
        Parameters(params): Parameters<GetAssemblyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let details = self
            .request_get_assembly(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(details)
    }

    #[tool(
        name = "list_assemblies",
        description = "List all semantic assemblies in the model with their type, label, and member count."
    )]
    pub(super) async fn list_assemblies_tool(&self) -> Result<CallToolResult, McpError> {
        let assemblies = self
            .request_list_assemblies()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(assemblies)
    }

    #[tool(
        name = "query_relations",
        description = "Query semantic relations, optionally filtering by source element ID, target element ID, or relation type."
    )]
    pub(super) async fn query_relations_tool(
        &self,
        Parameters(params): Parameters<QueryRelationsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let relations = self
            .request_query_relations(params.source, params.target, params.relation_type)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(relations)
    }

    #[tool(
        name = "list_assembly_members",
        description = "List the members of a specific assembly with their roles, types, and labels."
    )]
    pub(super) async fn list_assembly_members_tool(
        &self,
        Parameters(params): Parameters<ListAssemblyMembersRequest>,
    ) -> Result<CallToolResult, McpError> {
        let members = self
            .request_list_assembly_members(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(members)
    }

    // --- Refinement tools (PP70) ---

    #[tool(
        name = "get_refinement_state",
        description = "Get the declared refinement maturity of an entity. Returns one of: Conceptual, Schematic, Constructible, Detailed, FabricationReady."
    )]
    pub(super) async fn get_refinement_state_tool(
        &self,
        Parameters(params): Parameters<RefinementEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_get_refinement_state(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "get_obligations",
        description = "Get the obligation list for an entity, showing what sub-elements or claims must be resolved at each refinement state."
    )]
    pub(super) async fn get_obligations_tool(
        &self,
        Parameters(params): Parameters<RefinementEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let obligations = self
            .request_get_obligations(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(obligations)
    }

    #[tool(
        name = "resolve_obligation",
        description = "Record that a per-entity class obligation is met (SatisfiedBy a sub-element), \
            or explicitly Deferred / Waived with a reason. \
            \n\nPromotion to a refinement level gates on every in-force obligation \
            (required_by_state <= target_state) being resolved. An Unresolved obligation \
            blocks the promotion; use this tool to clear the block before calling \
            promote_refinement. \
            \n\nResolution variants: \
            `{ satisfied_by: { element_id: <u64> } }` — child element satisfies the obligation; \
            `{ deferred: { reason: \"<string>\" } }` — obligation intentionally deferred; \
            `{ waived: { rationale: \"<string>\" } }` — obligation explicitly out of scope. \
            \n\nThe change is recorded through the history pipeline and is undoable. \
            Returns the updated obligation set for the entity."
    )]
    pub(super) async fn resolve_obligation_tool(
        &self,
        Parameters(params): Parameters<super::request::ResolveObligationRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_resolve_obligation(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "get_authoring_provenance",
        description = "Get the authoring provenance for an entity — how it was created (Freeform, ViaRecipe, Imported, or Refined from a coarser entity)."
    )]
    pub(super) async fn get_authoring_provenance_tool(
        &self,
        Parameters(params): Parameters<RefinementEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let provenance = self
            .request_get_authoring_provenance(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(provenance)
    }

    #[tool(
        name = "get_claim_grounding",
        description = "Get per-claim grounding for an entity, optionally filtered to a specific claim path. The is_promotion_critical flag is false in PP70 (element-class descriptors land in PP74)."
    )]
    pub(super) async fn get_claim_grounding_tool(
        &self,
        Parameters(params): Parameters<ClaimGroundingRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entries = self
            .request_get_claim_grounding(params.element_id, params.path)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entries)
    }

    #[tool(
        name = "promote_refinement",
        description = "Promote an entity to a higher refinement state. When `recipe_id` is \
            supplied, the named recipe's `generate` function runs synchronously, creating \
            sub-element geometry (occurrences, relations, etc.) and satisfying obligations. \
            Available recipes are discovered via `select_recipe` or `list_recipe_families`; \
            prefer `instantiate_recipe` to create a new element AND run its recipe in one call. \
            The promotion is undoable."
    )]
    pub(super) async fn promote_refinement_tool(
        &self,
        Parameters(params): Parameters<PromoteRefinementRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_promote_refinement(
                params.element_id,
                params.target_state,
                params.recipe_id,
                params.overrides.unwrap_or_default(),
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "demote_refinement",
        description = "Demote an entity to a lower refinement state. Generated refinement branches are parked, not deleted, so authored overrides can be reactivated later. The demotion is undoable."
    )]
    pub(super) async fn demote_refinement_tool(
        &self,
        Parameters(params): Parameters<DemoteRefinementRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_demote_refinement(params.element_id, params.target_state)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "inspect_refinement_branches",
        description = "List active and parked refinement branches for an entity. Parked branches are dormant but inspectable and can be reactivated by promoting to the same target state."
    )]
    pub(super) async fn inspect_refinement_branches_tool(
        &self,
        Parameters(params): Parameters<InspectRefinementBranchesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_inspect_refinement_branches(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "discard_refinement_branch",
        description = "Permanently discard a parked refinement branch by parent_element_id and child_element_id. Active branches must be demoted/parked before they can be discarded."
    )]
    pub(super) async fn discard_refinement_branch_tool(
        &self,
        Parameters(params): Parameters<DiscardRefinementBranchRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_discard_refinement_branch(params.parent_element_id, params.child_element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "run_validation",
        description = "Run the registered validators against an entity and return findings. In PP70 this runs only the DeclaredStateRequiresResolvedObligations validator."
    )]
    pub(super) async fn run_validation_tool(
        &self,
        Parameters(params): Parameters<RefinementEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let findings = self
            .request_run_validation(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(findings)
    }

    #[tool(
        name = "explain_finding",
        description = "Return the rationale for a specific validator finding by finding_id."
    )]
    pub(super) async fn explain_finding_tool(
        &self,
        Parameters(params): Parameters<ExplainFindingRequest>,
    ) -> Result<CallToolResult, McpError> {
        let explanation = self
            .request_explain_finding(params.finding_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(explanation)
    }

    // --- Descriptor discovery tools (PP71) ---

    #[tool(
        name = "list_element_classes",
        description = "List all registered element classes (e.g. wall_assembly, foundation_system). \
            Each entry includes the id, label, description, and semantic roles."
    )]
    pub(super) async fn list_element_classes_tool(&self) -> Result<CallToolResult, McpError> {
        let classes = self.request_list_element_classes().await;
        json_tool_result(classes)
    }

    #[tool(
        name = "get_capability_snapshot",
        description = "Return a bounded dynamic-knowledge capability index for progressive MCP discovery. \
            The default response is compact: counts, short id lists, first-class no-curated-path summaries, \
            and up to five must-read guidance card ids. Pass expanded=true for diagnostics."
    )]
    pub(super) async fn get_capability_snapshot_tool(
        &self,
        Parameters(params): Parameters<CapabilitySnapshotRequest>,
    ) -> Result<CallToolResult, McpError> {
        let snapshot = self.request_get_capability_snapshot(params.expanded).await;
        json_tool_result(snapshot)
    }

    #[tool(
        name = "list_recipe_families",
        description = "List registered recipe families. Pass element_class to filter to a specific \
            class (e.g. 'wall_assembly'). Each entry includes the id, label, parameters, and \
            supported refinement levels. Set include_session_drafts=true to append installed \
            session drafts."
    )]
    pub(super) async fn list_recipe_families_tool(
        &self,
        Parameters(params): Parameters<ListRecipeFamiliesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let families = self
            .request_list_recipe_families(params.element_class, params.include_session_drafts)
            .await;
        json_tool_result(families)
    }

    #[tool(
        name = "select_recipe",
        description = "Return viable recipe families for an element class, ranked by weight. \
            In PP71 all viable recipes tie at 1.0 (real priors land in PP76). \
            Viable means the recipe's supported_refinement_levels includes the target_state. \
            Context schema: { target_state: string, jurisdiction?: string, \
            include_session_drafts?: bool }."
    )]
    pub(super) async fn select_recipe_tool(
        &self,
        Parameters(params): Parameters<SelectRecipeRequest>,
    ) -> Result<CallToolResult, McpError> {
        let ranking = self
            .request_select_recipe(params.element_class, params.context)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(ranking)
    }

    #[tool(
        name = "discover_curated_paths",
        description = "Additive dynamic-knowledge discovery endpoint. Returns existing curated \
            paths plus an explicit NoCuratedPath gap candidate when recipe, parametric, or prior \
            coverage is missing. Existing select/list tools remain compatible."
    )]
    pub(super) async fn discover_curated_paths_tool(
        &self,
        Parameters(params): Parameters<CuratedPathDiscoveryRequest>,
    ) -> Result<CallToolResult, McpError> {
        let discovery = self
            .request_discover_curated_paths(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(discovery)
    }

    #[tool(
        name = "instantiate_recipe",
        description = "Create a semantic element and immediately run a curated recipe to generate \
            its sub-element geometry in a single call. This is the preferred one-call alternative \
            to the two-step pattern of `create_entity` + `promote_refinement { recipe_id }`. \
            \n\nRequired: `family_id` (recipe family id from `select_recipe`), `target_class` \
            (element class, e.g. `\"wall_assembly\"`), `parameters` (recipe-specific driver map, \
            e.g. `{\"length_mm\": 4000, \"height_mm\": 2700, \"thickness_mm\": 140}`). \
            \nOptional: `placement` (`{ translate: [x,y,z] }` in **metres**), \
            `target_state` (default `\"Constructible\"`). \
            \n\nReturns `{ root_element_id, created_element_ids, state }`. After this call, \
            use `frame_entities([root_element_id])` to verify geometry was placed."
    )]
    pub(super) async fn instantiate_recipe_tool(
        &self,
        Parameters(params): Parameters<InstantiateRecipeRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_instantiate_recipe(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- PP74: Constraint layer tools ---

    #[tool(
        name = "list_constraints",
        description = "List all registered constraint descriptors. Each entry includes the id, \
            label, description, default_severity, rationale, and applicability filter. Pass \
            scope to filter (not yet interpreted in PP74 — all constraints returned)."
    )]
    pub(super) async fn list_constraints_tool(
        &self,
        Parameters(params): Parameters<ListConstraintsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let constraints = self.request_list_constraints(params.scope).await;
        json_tool_result(constraints)
    }

    #[tool(
        name = "run_validation_v2",
        description = "Run all registered constraints against an entity (or the whole model if \
            element_id is omitted). Returns findings from the PP74 orchestration engine. \
            Forces a fresh sweep before returning."
    )]
    pub(super) async fn run_validation_v2_tool(
        &self,
        Parameters(params): Parameters<RunValidationV2Request>,
    ) -> Result<CallToolResult, McpError> {
        let findings = self.request_run_validation_v2(params.element_id).await;
        json_tool_result(findings)
    }

    #[tool(
        name = "explain_finding_v2",
        description = "Look up a finding by its finding_id and return the full rationale, \
            constraint id, subject entity, and backlink. Reads from the Findings cache \
            populated by the last validation sweep."
    )]
    pub(super) async fn explain_finding_v2_tool(
        &self,
        Parameters(params): Parameters<ExplainFindingV2Request>,
    ) -> Result<CallToolResult, McpError> {
        let explanation = self
            .request_explain_finding_v2(params.finding_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(explanation)
    }

    #[tool(
        name = "preview_promotion",
        description = "Preview the obligation set and validation findings that would result from \
            promoting an entity to a target state, without permanently mutating the world. \
            Returns a read-only promotion plan and leaves the active graph unchanged."
    )]
    pub(super) async fn preview_promotion_tool(
        &self,
        Parameters(params): Parameters<PreviewPromotionRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_preview_promotion(
                params.element_id,
                params.target_state,
                params.recipe_id,
                params.overrides,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- PP75: Catalog providers ---

    #[tool(
        name = "list_catalog_providers",
        description = "List all registered catalog providers. Each entry includes the id, label, \
            description, category, region, license, and source_version."
    )]
    pub(super) async fn list_catalog_providers_tool(&self) -> Result<CallToolResult, McpError> {
        let providers = self.request_list_catalog_providers().await;
        json_tool_result(providers)
    }

    // --- PP76: Generation priors ---

    #[tool(
        name = "list_generation_priors",
        description = "List all registered generation priors. Each entry includes id, label, \
            description, scope (as a JSON object with a 'kind' discriminant), license, and \
            source_version. Pass an optional scope_filter object with 'element_class' or \
            'claim_path' keys to narrow the results; omit it to return all priors."
    )]
    pub(super) async fn list_generation_priors_tool(
        &self,
        Parameters(params): Parameters<ListGenerationPriorsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let priors = self
            .request_list_generation_priors(params.scope_filter)
            .await;
        json_tool_result(priors)
    }

    #[tool(
        name = "catalog_query",
        description = "Query a catalog provider by id and return matching rows. Pass an empty \
            filter object `{}` to retrieve all rows. PP75: filter is accepted but not yet \
            interpreted — all rows are returned regardless."
    )]
    pub(super) async fn catalog_query_tool(
        &self,
        Parameters(params): Parameters<CatalogQueryRequest>,
    ) -> Result<CallToolResult, McpError> {
        let rows = self
            .request_catalog_query(params.provider_id, params.filter)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(rows)
    }

    // --- PP78: Corpus operations ---

    #[tool(
        name = "list_corpus_gaps",
        description = "List all unresolved corpus gaps. Each entry names the element class, \
            jurisdiction, the kind of missing artifact, and who reported it. Gaps are pushed \
            by agents via request_corpus_expansion or automatically by validators."
    )]
    pub(super) async fn list_corpus_gaps_tool(&self) -> Result<CallToolResult, McpError> {
        let gaps = self.request_list_corpus_gaps().await;
        json_tool_result(gaps)
    }

    #[tool(
        name = "request_corpus_expansion",
        description = "Push a corpus-gap record requesting new coverage. Returns the created \
            open CorpusGapInfo record plus required closeout tools and completion criteria. \
            This records the missing knowledge; it does not close the gap. element_class and \
            jurisdiction are optional; kind is required (e.g. 'rule_pack', 'catalog', \
            'passage', 'recipe'); rationale is a free-form explanation."
    )]
    pub(super) async fn request_corpus_expansion_tool(
        &self,
        Parameters(params): Parameters<RequestCorpusExpansionRequest>,
    ) -> Result<CallToolResult, McpError> {
        let gap = self
            .request_request_corpus_expansion(
                params.element_class,
                params.jurisdiction,
                params.kind,
                params.rationale,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(gap)
    }

    #[tool(
        name = "lookup_source_passage",
        description = "Look up the text and provenance of a corpus passage by its passage_ref \
            (e.g. 'BBR_8:22_riser_max'). Returns an error if the passage is not registered."
    )]
    pub(super) async fn lookup_source_passage_tool(
        &self,
        Parameters(params): Parameters<LookupSourcePassageRequest>,
    ) -> Result<CallToolResult, McpError> {
        let info = self
            .request_lookup_source_passage(params.passage_ref)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(info)
    }

    #[tool(
        name = "draft_rule_pack",
        description = "Scaffold a Rust validator skeleton anchored to a corpus passage. \
            chunk_id must match a passage registered in CorpusPassageRegistry; \
            element_class names the ECS element class the validator will target. \
            Returns a rust_skeleton string (template — human must fill in the body), \
            a backlink ref, and editorial notes."
    )]
    pub(super) async fn draft_rule_pack_tool(
        &self,
        Parameters(params): Parameters<DraftRulePackRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_draft_rule_pack(params.chunk_id, params.element_class)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "check_rule_pack_backlinks",
        description = "Check whether every registered constraint's source_backlink resolves \
            against the CorpusPassageRegistry. Returns total, resolved, and broken counts. \
            Intended as a CI validation step — broken backlinks mean a passage was removed \
            or never ingested."
    )]
    pub(super) async fn check_rule_pack_backlinks_tool(&self) -> Result<CallToolResult, McpError> {
        let report = self.request_check_rule_pack_backlinks().await;
        json_tool_result(report)
    }

    #[tool(
        name = "list_recipe_drafts",
        description = "List session-scoped recipe drafts captured for dynamic recipe learning. \
            Pass target_class or status to filter. Status values: gap_detected, sourced, \
            drafted, validated, installed."
    )]
    pub(super) async fn list_recipe_drafts_tool(
        &self,
        Parameters(params): Parameters<ListRecipeDraftsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let drafts = self
            .request_list_recipe_drafts(params.target_class, params.status)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(drafts)
    }

    #[tool(
        name = "get_recipe_draft",
        description = "Get one session recipe draft by id, including linked gap id, source \
            passage refs, draft script payload, notes, and current status."
    )]
    pub(super) async fn get_recipe_draft_tool(
        &self,
        Parameters(params): Parameters<GetRecipeDraftRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_get_recipe_draft(params.recipe_draft_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "save_recipe_draft",
        description = "Create or update a session recipe draft. This stores acquisition context, \
            linked corpus gap/source passages, parameter shape, and an opaque draft_script payload. \
            If recipe_draft_id is omitted a new session id is allocated. A draft closes an authoring \
            gap only when it is installed, has an evidence-backed geometry_emission runtime claim, \
            and can be materialized with materialize_learned_asset."
    )]
    pub(super) async fn save_recipe_draft_tool(
        &self,
        Parameters(params): Parameters<SaveRecipeDraftRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_save_recipe_draft(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "set_recipe_draft_status",
        description = "Update a session recipe draft status. Use installed to make a draft \
            consultable from list_recipe_families/select_recipe when the caller opts in. \
            Installed does not imply executable; select_recipe reports executable=false unless \
            materialize_learned_asset can replay the learned asset."
    )]
    pub(super) async fn set_recipe_draft_status_tool(
        &self,
        Parameters(params): Parameters<SetRecipeDraftStatusRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_set_recipe_draft_status(params.recipe_draft_id, params.status)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "list_assembly_pattern_drafts",
        description = "List session-scoped assembly pattern drafts for layered construction knowledge. \
            Pass target_type or status to filter. Status values: gap_detected, sourced, drafted, \
            validated, installed."
    )]
    pub(super) async fn list_assembly_pattern_drafts_tool(
        &self,
        Parameters(params): Parameters<ListAssemblyPatternDraftsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let drafts = self
            .request_list_assembly_pattern_drafts(params.target_type, params.status)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(drafts)
    }

    #[tool(
        name = "get_assembly_pattern_draft",
        description = "Get one session assembly pattern draft by id, including ordered layers, \
            relation rules, linked gap/source refs, and current status."
    )]
    pub(super) async fn get_assembly_pattern_draft_tool(
        &self,
        Parameters(params): Parameters<GetAssemblyPatternDraftRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_get_assembly_pattern_draft(params.assembly_pattern_draft_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "save_assembly_pattern_draft",
        description = "Create or update a session assembly pattern draft. This stores ordered layers, \
            relation rules, support-root hints, linked corpus gaps/source passages, and acquisition \
            context. If assembly_pattern_draft_id is omitted a new session id is allocated."
    )]
    pub(super) async fn save_assembly_pattern_draft_tool(
        &self,
        Parameters(params): Parameters<SaveAssemblyPatternDraftRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_save_assembly_pattern_draft(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "set_assembly_pattern_draft_status",
        description = "Update a session assembly pattern draft status. Use installed to make a \
            draft consultable from list_vocabulary in the current running app."
    )]
    pub(super) async fn set_assembly_pattern_draft_status_tool(
        &self,
        Parameters(params): Parameters<SetAssemblyPatternDraftStatusRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_set_assembly_pattern_draft_status(
                params.assembly_pattern_draft_id,
                params.status,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "materialize_learned_asset",
        description = "Materialize an executable learned asset through its approved execution path. \
            The first implementation supports recipe draft assets whose draft_script contains a \
            `parametric_create` request and whose runtime geometry claim has EvidenceRef plus \
            last_verified."
    )]
    pub(super) async fn materialize_learned_asset_tool(
        &self,
        Parameters(params): Parameters<MaterializeLearnedAssetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_materialize_learned_asset(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "list_guidance_cards",
        description = "List bounded progressive guidance cards. Pass task to filter by relevance; \
            use get_guidance_card to fetch one card by id instead of loading monolithic guidance."
    )]
    pub(super) async fn list_guidance_cards_tool(
        &self,
        Parameters(params): Parameters<ListGuidanceCardsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let cards = self.request_list_guidance_cards(params.task).await;
        json_tool_result(cards)
    }

    #[tool(
        name = "get_guidance_card",
        description = "Fetch one progressive guidance card by id, including referenced tool ids \
            and schema-shaped JSON examples."
    )]
    pub(super) async fn get_guidance_card_tool(
        &self,
        Parameters(params): Parameters<GetGuidanceCardRequest>,
    ) -> Result<CallToolResult, McpError> {
        let card = self
            .request_get_guidance_card(params.card_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(card)
    }

    #[tool(
        name = "definition.list",
        description = "List reusable definitions in the document. By default only public \
                       families (PublicRoot + PublicVariant) are returned. Pass \
                       include_internal=true to also include internal implementation parts \
                       such as truss members and window parts."
    )]
    pub(super) async fn definition_list_tool(
        &self,
        Parameters(params): Parameters<DefinitionListParams>,
    ) -> Result<CallToolResult, McpError> {
        let definitions = self
            .request_list_definitions_opt(params.include_internal)
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(definitions)
    }

    #[tool(
        name = "definition.get",
        description = "Get a definition by its definition_id. Returns both the raw stored definition and the effective inherited definition."
    )]
    pub(super) async fn definition_get_tool(
        &self,
        Parameters(params): Parameters<DefinitionGetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_get_definition(params.definition_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.create",
        description = "Create a new reusable definition. Requires: name. Optionally: base_definition_id, definition_kind, parameters, void_declaration, evaluators, representations, compound, width_param/depth_param/height_param fallback fields, and domain_data."
    )]
    pub(super) async fn definition_create_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_create_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.update",
        description = "Update an existing definition. Requires: definition_id. Optionally: name, base_definition_id, definition_kind, parameters, void_declaration, evaluators, representations, compound, and domain_data. Bumps definition_version and propagates changes to all linked occurrences."
    )]
    pub(super) async fn definition_update_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_update_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "representation.declare",
        description = "ADR-026 Phase 6c: declare or replace a representation on a definition. \
                       Requires definition_id and kind; role defaults to Body. \
                       Optional lod and update_policy set explicit representation metadata."
    )]
    pub(super) async fn representation_declare_tool(
        &self,
        Parameters(params): Parameters<RepresentationDeclareRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_representation_declare(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "representation.set_lod",
        description = "ADR-026 Phase 6c: set the LevelOfDetail for an existing representation. \
                       Requires definition_id, kind, and lod; provide role when kind alone is ambiguous."
    )]
    pub(super) async fn representation_set_lod_tool(
        &self,
        Parameters(params): Parameters<RepresentationSetLodRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_representation_set_lod(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "representation.set_update_policy",
        description = "ADR-026 Phase 6c: set the UpdatePolicy for an existing representation. \
                       Requires definition_id, kind, and update_policy; provide role when kind alone is ambiguous."
    )]
    pub(super) async fn representation_set_update_policy_tool(
        &self,
        Parameters(params): Parameters<RepresentationSetUpdatePolicyRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_representation_set_update_policy(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.draft.list",
        description = "List all open definition drafts."
    )]
    pub(super) async fn definition_draft_list_tool(&self) -> Result<CallToolResult, McpError> {
        let drafts = self
            .request_list_definition_drafts()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(drafts)
    }

    #[tool(
        name = "definition.draft.get",
        description = "Get a definition draft by draft_id."
    )]
    pub(super) async fn definition_draft_get_tool(
        &self,
        Parameters(params): Parameters<DefinitionDraftIdRequest>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_get_definition_draft(params.draft_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.open",
        description = "Open an existing definition as a draft for editing. Requires: definition_id. Optionally: library_id."
    )]
    pub(super) async fn definition_draft_open_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_open_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.create",
        description = "Create a new definition draft. Same payload shape as definition/create, but stored only as an editable draft until published."
    )]
    pub(super) async fn definition_draft_create_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_create_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.derive",
        description = "Create a derived definition draft from an existing definition. Requires: definition_id. Optionally: library_id and name."
    )]
    pub(super) async fn definition_draft_derive_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_derive_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.patch",
        description = "Apply one or more patch operations to a definition draft. Requires: draft_id and either patch or patches."
    )]
    pub(super) async fn definition_draft_patch_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self
            .request_patch_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(draft)
    }

    #[tool(
        name = "definition.draft.publish",
        description = "Validate and publish a definition draft into the document. Requires: draft_id."
    )]
    pub(super) async fn definition_draft_publish_tool(
        &self,
        Parameters(params): Parameters<DefinitionDraftIdRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_publish_definition_draft(params.draft_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.validate",
        description = "Validate either a draft or a published definition. Requires either draft_id or definition_id. Optionally: library_id for library definitions."
    )]
    pub(super) async fn definition_validate_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_validate_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.validate_host_contract",
        description = "Validate a hosted Definition against a registered hosting contract. Requires: definition_id, contract_kind, host_element_id, hosted_element_id. Optional: contract_parameters."
    )]
    pub(super) async fn definition_validate_host_contract_tool(
        &self,
        Parameters(params): Parameters<ValidateDefinitionHostContractRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_definition_validate_host_contract(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.compile",
        description = "Compile a dependency summary for either a draft or a published definition. Requires either draft_id or definition_id. Optionally: library_id for library definitions."
    )]
    pub(super) async fn definition_compile_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_compile_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.explain",
        description = "Explain either a draft or a published definition, including effective inherited shape and dependency summary. Requires either draft_id or definition_id. Optionally: library_id for library definitions."
    )]
    pub(super) async fn definition_explain_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_explain_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.library.list",
        description = "List reusable definition libraries available to the current document."
    )]
    pub(super) async fn definition_library_list_tool(&self) -> Result<CallToolResult, McpError> {
        let libraries = self
            .request_list_definition_libraries()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(libraries)
    }

    #[tool(
        name = "definition.library.get",
        description = "Get a definition library by library_id, including the definitions it contains."
    )]
    pub(super) async fn definition_library_get_tool(
        &self,
        Parameters(params): Parameters<DefinitionLibraryGetRequest>,
    ) -> Result<CallToolResult, McpError> {
        let library = self
            .request_get_definition_library(params.library_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(library)
    }

    #[tool(
        name = "definition.library.create",
        description = "Create a new definition library. Requires: name. Optionally: scope (\"DocumentLocal\"|\"ExternalFile\"), source_path, tags."
    )]
    pub(super) async fn definition_library_create_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_create_definition_library(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.add_definition",
        description = "Copy a document definition into a library. Requires: library_id, definition_id."
    )]
    pub(super) async fn definition_library_add_definition_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_add_definition_to_library(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.import",
        description = "Import a definition library JSON file into the current document context. Requires: path."
    )]
    pub(super) async fn definition_library_import_tool(
        &self,
        Parameters(params): Parameters<DefinitionLibraryPathRequest>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_import_definition_library(params.path)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.export",
        description = "Export a definition library JSON file. Requires: library_id, path."
    )]
    pub(super) async fn definition_library_export_tool(
        &self,
        Parameters(params): Parameters<DefinitionLibraryExportRequest>,
    ) -> Result<CallToolResult, McpError> {
        let path = self
            .request_export_definition_library(params.library_id, params.path)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(path)
    }

    #[tool(
        name = "definition.library.workspace.list",
        description = "List workspace definition libraries under an existing .talos3d/libraries root. Accepts workspace_root or start_dir."
    )]
    pub(super) async fn definition_library_workspace_list_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entries = self
            .request_list_workspace_definition_libraries(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entries)
    }

    #[tool(
        name = "definition.library.workspace.create",
        description = "Create a workspace definition library JSON file under an existing .talos3d/libraries root. Requires: workspace_root, name."
    )]
    pub(super) async fn definition_library_workspace_create_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_create_workspace_definition_library(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.workspace.import_draft",
        description = "Import a validated Definition draft into a workspace library. Requires: library_id, draft_id."
    )]
    pub(super) async fn definition_library_workspace_import_draft_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_import_workspace_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.workspace.update_draft",
        description = "Replace a workspace library draft definition with a validated Definition draft. Requires: library_id, draft_id."
    )]
    pub(super) async fn definition_library_workspace_update_draft_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_update_workspace_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.library.workspace.delete_draft",
        description = "Delete a draft definition from a workspace library. Requires: library_id, definition_id."
    )]
    pub(super) async fn definition_library_workspace_delete_draft_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let entry = self
            .request_delete_workspace_definition_draft(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(entry)
    }

    #[tool(
        name = "definition.instantiate",
        description = "Instantiate a definition into the model. Requires: definition_id. Optionally: library_id (imports from library first if needed), overrides, label, offset, domain_data."
    )]
    pub(super) async fn definition_instantiate_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_instantiate_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "definition.instantiate_hosted",
        description = "Instantiate a hosted definition into the model. Requires: definition_id and hosting. Optionally: library_id, overrides, label, offset, and domain_data. Hosting may provide host_element_id, opening_element_id, wall_thickness, relation_type, relation_parameters, and anchors keyed by anchor id."
    )]
    pub(super) async fn definition_instantiate_hosted_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_instantiate_hosted_definition(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.place",
        description = "Place an occurrence of a definition. Requires: definition_id. Optionally: overrides, label, offset, and domain_data."
    )]
    pub(super) async fn occurrence_place_tool(
        &self,
        Parameters(json): Parameters<Value>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_place_occurrence(json)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "occurrence.update_overrides",
        description = "Update the parameter overrides on an existing occurrence. Requires: element_id (u64), overrides (object mapping param names to values)."
    )]
    pub(super) async fn occurrence_update_overrides_tool(
        &self,
        Parameters(params): Parameters<OccurrenceUpdateOverridesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_update_occurrence_overrides(params.element_id, params.overrides)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.set_material_override",
        description = "Set a typed material override on an occurrence. Requires: element_id and assignment. The override shadows the Definition material assignment."
    )]
    pub(super) async fn occurrence_set_material_override_tool(
        &self,
        Parameters(params): Parameters<SetOccurrenceMaterialOverrideRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_set_occurrence_material_override(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.clear_material_override",
        description = "Clear an occurrence material override so it inherits the Definition material assignment. Requires: element_id."
    )]
    pub(super) async fn occurrence_clear_material_override_tool(
        &self,
        Parameters(params): Parameters<ClearOccurrenceMaterialOverrideRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_clear_occurrence_material_override(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.make_unique",
        description = "Detach one occurrence from its shared definition by copying its effective definition tree and repointing only that occurrence at the new initially-identical definition. Requires: element_id. Optional: name, copy_dependencies (default true)."
    )]
    pub(super) async fn occurrence_make_unique_tool(
        &self,
        Parameters(params): Parameters<OccurrenceMakeUniqueRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_make_occurrence_unique(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.validate_host_fit",
        description = "Validate a hosted occurrence against a registered hosting contract. Requires: contract_kind, host_element_id, hosted_element_id. Optional: contract_parameters."
    )]
    pub(super) async fn occurrence_validate_host_fit_tool(
        &self,
        Parameters(params): Parameters<ValidateHostFitRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_occurrence_validate_host_fit(params)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.resolve",
        description = "Resolve and return the effective parameter values for an occurrence, including provenance (DefinitionDefault or OccurrenceOverride). Requires: element_id (u64)."
    )]
    pub(super) async fn occurrence_resolve_tool(
        &self,
        Parameters(params): Parameters<OccurrenceResolveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_resolve_occurrence(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "occurrence.explain",
        description = "Explain a placed occurrence for agent inspection. Returns resolved parameters, anchors, and generated compound slot parts. Requires: element_id (u64)."
    )]
    pub(super) async fn occurrence_explain_tool(
        &self,
        Parameters(params): Parameters<OccurrenceResolveRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_explain_occurrence(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- Array tools ---

    #[tool(
        name = "array_create_linear",
        description = "Create a linear array of N copies of a source entity, spaced evenly along a direction vector."
    )]
    pub(super) async fn array_create_linear_tool(
        &self,
        Parameters(params): Parameters<ArrayCreateLinearRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_array_create_linear(params.source, params.count, params.spacing)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "array_create_polar",
        description = "Create a polar (rotational) array of N copies of a source entity, distributed around an axis."
    )]
    pub(super) async fn array_create_polar_tool(
        &self,
        Parameters(params): Parameters<ArrayCreatePolarRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_array_create_polar(
                params.source,
                params.count,
                params.axis.unwrap_or([0.0, 1.0, 0.0]),
                params.total_angle_degrees.unwrap_or(360.0),
                params.center.unwrap_or([0.0, 0.0, 0.0]),
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "array_update",
        description = "Update the count, spacing, axis, angle, or center of an array node."
    )]
    pub(super) async fn array_update_tool(
        &self,
        Parameters(params): Parameters<ArrayUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_array_update(
                params.element_id,
                params.count,
                params.spacing,
                params.axis,
                params.total_angle_degrees,
                params.center,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "array_dissolve",
        description = "Convert an array node into an independent entity, breaking the link to its source."
    )]
    pub(super) async fn array_dissolve_tool(
        &self,
        Parameters(params): Parameters<ArrayEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let new_id = self
            .request_array_dissolve(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(new_id)
    }

    #[tool(
        name = "array_get",
        description = "Get the parameters of an array node (source, count, spacing or axis/angle/center)."
    )]
    pub(super) async fn array_get_tool(
        &self,
        Parameters(params): Parameters<ArrayEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_array_get(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    // --- Mirror tools ---

    #[tool(
        name = "mirror_create",
        description = "Create a mirror geometry node that reflects a source entity across a plane. The mirror maintains a live dependency on the source. Returns the new element_id."
    )]
    pub(super) async fn mirror_create_tool(
        &self,
        Parameters(params): Parameters<MirrorCreateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let element_id = self
            .request_mirror_create(
                params.source,
                params.plane,
                params.plane_origin,
                params.plane_normal,
                Some(params.merge),
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(element_id)
    }

    #[tool(
        name = "mirror_update",
        description = "Update the mirror plane or merge setting of a MirrorNode entity."
    )]
    pub(super) async fn mirror_update_tool(
        &self,
        Parameters(params): Parameters<MirrorUpdateRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_mirror_update(
                params.element_id,
                params.plane,
                params.plane_origin,
                params.plane_normal,
                params.merge,
            )
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "mirror_dissolve",
        description = "Break the live link of a MirrorNode, converting it to an independent triangle mesh entity with the current reflected geometry. Returns the new entity's element_id."
    )]
    pub(super) async fn mirror_dissolve_tool(
        &self,
        Parameters(params): Parameters<MirrorEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let new_id = self
            .request_mirror_dissolve(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(new_id)
    }

    #[tool(
        name = "mirror_get",
        description = "Get the mirror parameters (source entity, plane origin, plane normal, merge) of a MirrorNode entity."
    )]
    pub(super) async fn mirror_get_tool(
        &self,
        Parameters(params): Parameters<MirrorEntityRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_mirror_get(params.element_id)
            .await
            .map_err(|error| McpError::invalid_params(error, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "get_authoring_guidance",
        description = "Return the canonical Talos3D-owned authoring guidance (COMPONENT_STRUCTURE). Call this immediately after connecting so you know how to structure reusable Definitions, derived variants, and singletons, and how they compose with progressive refinement. The `prompt_text` markdown is authoritative; the `component_structure` struct is a supplementary policy view."
    )]
    pub(super) async fn get_authoring_guidance_tool(&self) -> Result<CallToolResult, McpError> {
        let guidance = self
            .request_get_authoring_guidance()
            .await
            .map_err(|error| McpError::internal_error(error, None))?;
        json_tool_result(guidance)
    }

    // --- Semantic Procedural Session (ADR-051, PP-SPS-3) ---

    #[tool(
        name = "procedural_session.create",
        description = "Open a stateful, validated scratchpad for a MULTI-STEP authoring sequence — repeated placements, datum-derived layouts, atomic multi-call edits, or recipe authoring. Prefer this over streaming individual Model-API mutations when steps share parameters, depend on each other's outputs (bindings), or must commit atomically. Declares refinement target, stage transition, MutationScope, and allowed tools up front. Returns session_id and a session-scoped guidance overlay. See ADR-051 and the procedural-session orientation in get_authoring_guidance for full when/why guidance."
    )]
    pub(super) async fn procedural_session_create_tool(
        &self,
        Parameters(params): Parameters<
            crate::plugins::procedural_session_mcp::SessionCreateRequest,
        >,
    ) -> Result<CallToolResult, McpError> {
        let response = self
            .request_procedural_session_create(params)
            .await
            .map_err(|e| McpError::internal_error(e, None))?;
        json_tool_result(response)
    }

    #[tool(
        name = "procedural_session.eval",
        description = "Append or preview ONE step in an open session — the inner loop for assembling a multi-step authoring sequence. Modes: bind_only (type-check + append), dry_run (project expected commands, obligations, and findings WITHOUT appending — use this to explore cheaply), dry_run_and_bind. Steps reference prior step outputs by binding, so you do not recompute coordinates by hand. Type-checked against the registered capability/command descriptors; enforces the session's MutationScope."
    )]
    pub(super) async fn procedural_session_eval_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::procedural_session_mcp::SessionEvalRequest>,
    ) -> Result<CallToolResult, McpError> {
        let report = self
            .request_procedural_session_eval(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(report)
    }

    #[tool(
        name = "procedural_session.snapshot",
        description = "Inspect an open session before committing: declared spec, accumulated AuthoringScript, live bindings between steps, outstanding obligations, accrued findings, and a recent audit excerpt. Use between eval iterations to confirm the script is what you intend, or before commit to confirm there are no blocking findings."
    )]
    pub(super) async fn procedural_session_snapshot_tool(
        &self,
        Parameters(params): Parameters<
            crate::plugins::procedural_session_mcp::SessionSnapshotRequest,
        >,
    ) -> Result<CallToolResult, McpError> {
        let snap = self
            .request_procedural_session_snapshot(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(snap)
    }

    #[tool(
        name = "procedural_session.commit",
        description = "Flush the session's accumulated AuthoringScript to the world through the command queue (ADR-002 / ADR-011) — this is the step that actually mutates the model. Policies: require_clean (refuse on any finding), accept_with_waivers, accept_partial (commit clean prefix, return carry-over). Returns enqueued command ids, post-commit findings, remaining obligations, and an optional in-line export handle. Render and inspect the result before declaring done — a clean commit report is not evidence the geometry is right."
    )]
    pub(super) async fn procedural_session_commit_tool(
        &self,
        Parameters(params): Parameters<
            crate::plugins::procedural_session_mcp::SessionCommitRequest,
        >,
    ) -> Result<CallToolResult, McpError> {
        let report = self
            .request_procedural_session_commit(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(report)
    }

    #[tool(
        name = "procedural_session.export",
        description = "Freeze the session's accumulated AuthoringScript as a curated artifact (kind `recipe.authoring_script.v1`) and return an `export_handle`. This is the first half of publishing a recipe: export only FREEZES the script — it does NOT make it callable. To turn it into a reusable, executable recipe you MUST then call `install_recipe_from_session_export` with the returned handle; only after install can `instantiate_recipe`/`promote_refinement` replay it and `discover_curated_paths`/`select_recipe` surface it to future sessions. Do not hand-write recipe JSON; build the sequence in a session, validate via eval/snapshot, export, then install. The session remains re-exportable until close."
    )]
    pub(super) async fn procedural_session_export_tool(
        &self,
        Parameters(params): Parameters<
            crate::plugins::procedural_session_mcp::SessionExportRequest,
        >,
    ) -> Result<CallToolResult, McpError> {
        let handle = self
            .request_procedural_session_export(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(handle)
    }

    // --- Parametric components (RELATIONAL_PARAMETRIC_SUBSTRATE, PP-RPS-7 UX) ---

    #[tool(
        name = "parametric.list_types",
        description = "List the registered parametric component types (e.g. trusses, windows). Each entry has an `id` (use it with parametric.create) and a human `label`. Parametric types are a DERIVATION substrate: they compute driver-driven values (lengths, counts, member geometry) but do NOT by themselves place renderable geometry in the model. They are not a substitute for a geometry-emitting recipe — see RELATIONAL_PARAMETRIC_SUBSTRATE and the ADR-042 anti-bluff gate in get_authoring_guidance."
    )]
    pub(super) async fn parametric_list_types_tool(&self) -> Result<CallToolResult, McpError> {
        let types = self
            .request_parametric_list_types()
            .await
            .map_err(|e| McpError::internal_error(e, None))?;
        json_tool_result(types)
    }

    #[tool(
        name = "parametric.create",
        description = "Instantiate a parametric component of the given `type_id` (from \
        parametric.list_types). Returns a `CreateParametricResponse` containing: `snapshot` \
        (instance_id, drivers, derived values) and `element_ids` (scene entity IDs for spawned \
        geometry, empty when the type has no declarative representation). Types that carry a \
        `representation` will emit real renderable geometry — one ProfileExtrusion per member — \
        accessible by element_id. Types without a representation are derivation-only; use \
        request_corpus_expansion to add a representation as DATA rather than hand-rolling \
        primitives."
    )]
    pub(super) async fn parametric_create_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::parametric_mcp::CreateParametricRequest>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .request_parametric_create(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(resp)
    }

    #[tool(
        name = "parametric.inspect",
        description = "Return the current snapshot of a parametric instance: its type label, all driver values (with editability), and all derived values. Use this to see what can be edited and what those edits will affect."
    )]
    pub(super) async fn parametric_inspect_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::parametric_mcp::InspectParametricRequest>,
    ) -> Result<CallToolResult, McpError> {
        let snap = self
            .request_parametric_inspect(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(snap)
    }

    #[tool(
        name = "parametric.set_driver",
        description = "Edit one editable driver of a parametric instance by name and value, then re-derive. Returns a propagation report listing which derived values changed. Read-only drivers are refused. This is the primary way to resize/reshape a parametric system semantically."
    )]
    pub(super) async fn parametric_set_driver_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::parametric_mcp::SetParametricDriverRequest>,
    ) -> Result<CallToolResult, McpError> {
        let report = self
            .request_parametric_set_driver(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(report)
    }

    #[tool(
        name = "parametric.transform",
        description = "Apply a transform gesture (e.g. SetExtent along an axis) to a parametric instance. If the axis is bound to a driver the gesture becomes a smart driver-edit (resizing re-derives dependents); otherwise it is refused rather than silently breaking the parametric relationships. Returns the transform outcome."
    )]
    pub(super) async fn parametric_transform_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::parametric_mcp::ParametricTransformRequest>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = self
            .request_parametric_transform(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(outcome)
    }

    #[tool(
        name = "parametric.explain",
        description = "Explain a derived parameter of a parametric instance: returns the dependency trace through the graph and the controlling drivers that ultimately determine its value. Use this to understand why a value is what it is and which driver to edit to change it."
    )]
    pub(super) async fn parametric_explain_tool(
        &self,
        Parameters(params): Parameters<crate::plugins::parametric_mcp::ExplainParametricRequest>,
    ) -> Result<CallToolResult, McpError> {
        let response = self
            .request_parametric_explain(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(response)
    }

    // --- Knowledge persistence tools (Change-2 / Change-3 / Change-7) ---

    #[tool(
        name = "install_recipe_from_session_export",
        description = "Install an `AuthoringScript` exported from a procedural session as a durable, \
        executable recipe in the `RecipeArtifactRegistry`. After installation the recipe is callable \
        via `instantiate_recipe` and `promote_refinement` by its `family_id`. Supply `scope: \
        \"Project\"` (default) to persist to `~/.talos3d/knowledge/recipes/` and survive restarts; \
        supply `scope: \"Session\"` for an in-memory-only install. \
        \n\n\
        Workflow: (1) build the script in a procedural session, (2) commit it, (3) call \
        `procedural_session.export` to freeze it as an artifact, (4) call this tool with the \
        returned `export_handle` to make it executable. Returns `{ family_id, scope, \
        persisted_path, supported_refinement_levels }`."
    )]
    pub(super) async fn install_recipe_from_session_export_tool(
        &self,
        Parameters(params): Parameters<super::request::InstallRecipeFromSessionExportRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_install_recipe_from_session_export(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(result)
    }

    #[tool(
        name = "list_persisted_recipes",
        description = "List all recipes currently loaded in the `RecipeArtifactRegistry` — both \
        shipped (native) recipes and user-installed `AuthoringScript` recipes. Returns an array of \
        `{ family_id, asset_id, label, description, body_kind, supported_refinement_levels }`. \
        Use this after `install_recipe_from_session_export` to confirm the recipe is registered \
        and discoverable."
    )]
    pub(super) async fn list_persisted_recipes_tool(&self) -> Result<CallToolResult, McpError> {
        let recipes = self
            .request_list_persisted_recipes()
            .await
            .map_err(|e| McpError::internal_error(e, None))?;
        json_tool_result(recipes)
    }

    #[tool(
        name = "acquire_corpus_passage",
        description = "Store a plain-text passage from an external source (a code section, \
        regulation excerpt, manufacturer specification, or other knowledge fragment) into the \
        `CorpusPassageRegistry` so it becomes available as grounding for future curation work. \
        With `persist: true` (default) the passage is also written to \
        `~/.talos3d/knowledge/passages/<passage_ref>.json` and reloaded on next startup. \
        \n\n\
        Required: `passage_ref` (stable id), `citation` (source name), `text` (plain-text body). \
        Optional: `source_url`, `jurisdiction` (ISO 3166-1 alpha-2), `classification`, \
        `license` (`cc0`, `public_record`, `boverket_public`, `icc_cite_only`, \
        `standards_body_citation_only`). \
        \n\n\
        Returns `{ passage_ref, stored, registry_size, persisted_path }`."
    )]
    pub(super) async fn acquire_corpus_passage_tool(
        &self,
        Parameters(params): Parameters<super::request::AcquireCorpusPassageRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .request_acquire_corpus_passage(params)
            .await
            .map_err(|e| McpError::invalid_params(e, None))?;
        json_tool_result(result)
    }
}
