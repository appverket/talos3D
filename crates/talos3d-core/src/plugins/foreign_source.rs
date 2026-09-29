//! Retained foreign input is a project asset, never a second component authority.
//! Adaptation and interpretation are pure drafts for the shared edit-plan carrier.
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use super::{
    authored_edit_plan::{capture_snapshot, modifiers::EditPlanDraft, PlanContext},
    identity::{ElementId, ElementIdAllocator},
    modeling::{
        definition::{DefinitionId, DefinitionRegistry},
        dependency_graph::EntityDependencies,
        group::GroupMembers,
        occurrence::{occurrence_geometry_part_count, OccurrenceIdentity, OccurrenceSnapshot},
        primitives::TriangleMesh,
        snapshots::TriangleMeshSnapshot,
    },
    semantic_shadow::{semantic_shadow_for_import_request, SemanticShadow},
};

pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_PROJECT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceMetadata {
    pub name: String,
    pub format: String,
    pub format_release: String,
    pub producer: String,
    pub producer_version: String,
    pub license: String,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Degradation {
    NoAuthoritativeSemantics,
    NoEditableHistory,
    MaterialsAndUvNotMapped,
    Float32Geometry,
    FanTriangulation,
    UnsupportedRecords,
    NoAdapter,
    AdapterFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAssessment {
    pub adapter: String,
    pub reference_geometry_available: bool,
    pub degradation: Vec<Degradation>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceArtifact {
    pub digest: String,
    pub metadata: SourceMetadata,
    pub bytes: Vec<u8>,
    pub assessment: SourceAssessment,
}
impl SourceArtifact {
    pub fn manifest(&self) -> Value {
        json!({"digest":self.digest,"digest_algorithm":"blake3","byte_count":self.bytes.len(),"metadata":self.metadata,
            "assessment":self.assessment,"authority":"retained_input_only",
            "next_tool":"get_foreign_sources"})
    }
    fn validate(&self) -> Result<(), String> {
        if self.bytes.is_empty() || self.bytes.len() > MAX_SOURCE_BYTES {
            return Err("Source must contain 1..65536 bytes".into());
        }
        if self.digest != blake3::hash(&self.bytes).to_hex().as_str() {
            return Err("Foreign source digest does not match retained bytes".into());
        }
        for text in [
            &self.metadata.name,
            &self.metadata.format,
            &self.metadata.format_release,
            &self.metadata.producer,
            &self.metadata.producer_version,
            &self.metadata.license,
            &self.metadata.provenance,
        ] {
            if text.trim().is_empty() || text.len() > 1024 {
                return Err("Source metadata fields require 1..1024 bytes; record unknown values explicitly".into());
            }
        }
        if self.assessment.detail.len() > 4096
            || self.assessment.adapter.len() > 128
            || self.assessment.degradation.len() > 16
        {
            return Err("Source assessment exceeds bounded metadata budget".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Resource, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceArtifacts(pub BTreeMap<String, SourceArtifact>);
impl SourceArtifacts {
    pub fn validate(&self) -> Result<(), String> {
        if self.0.len() > MAX_ARTIFACTS
            || self.0.values().map(|s| s.bytes.len()).sum::<usize>() > MAX_PROJECT_BYTES
        {
            return Err(
                "Retained foreign sources exceed project budget (128 sources / 4 MiB)".into(),
            );
        }
        for (digest, source) in &self.0 {
            source.validate()?;
            if digest != &source.digest {
                return Err("Foreign source key/digest mismatch".into());
            }
        }
        Ok(())
    }
}

/// Captured project-resource change, alongside authored entity snapshots.
#[derive(Debug, Clone)]
pub struct SourceArtifactChange {
    pub digest: String,
    pub before: Option<SourceArtifact>,
    pub after: Option<SourceArtifact>,
}
impl SourceArtifactChange {
    pub fn manifest(&self) -> Value {
        json!({"digest":self.digest,"before":self.before.as_ref().map(SourceArtifact::manifest),
            "after":self.after.as_ref().map(SourceArtifact::manifest)})
    }
    pub fn retained_bytes(&self) -> usize {
        self.before
            .iter()
            .chain(self.after.iter())
            .map(|a| a.bytes.len())
            .sum()
    }
    pub fn matches_before(&self, world: &World) -> bool {
        world
            .get_resource::<SourceArtifacts>()
            .and_then(|r| r.0.get(&self.digest))
            == self.before.as_ref()
    }
    pub fn apply(&self, world: &mut World, undo: bool) {
        if self.before == self.after {
            return;
        }
        world.init_resource::<SourceArtifacts>();
        let mut assets = world.resource_mut::<SourceArtifacts>();
        match if undo { &self.before } else { &self.after } {
            Some(source) => {
                assets.0.insert(self.digest.clone(), source.clone());
            }
            None => {
                assets.0.remove(&self.digest);
            }
        }
    }
}
pub fn validate_changes(world: &World, changes: &[SourceArtifactChange]) -> Result<(), String> {
    let mut assets = world
        .get_resource::<SourceArtifacts>()
        .cloned()
        .unwrap_or_default();
    let mut seen = BTreeSet::new();
    for change in changes {
        if !seen.insert(&change.digest) || !change.matches_before(world) {
            return Err("Duplicate or stale foreign source change".into());
        }
        // Published sources are immutable. Removal is allowed only as reversal of
        // a captured creation, not as a new resource-edit request.
        if (change.before.is_some() && change.before != change.after) || change.after.is_none() {
            return Err("Foreign source proposals may only retain a new artifact or assert an unchanged immutable source".into());
        }
        let source = change.after.as_ref().unwrap();
        if source.digest != change.digest {
            return Err("Source change digest mismatch".into());
        }
        assets.0.insert(change.digest.clone(), source.clone());
    }
    assets.validate()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceLengthUnit {
    Millimetre,
    Centimetre,
    Metre,
    Inch,
    Foot,
}
impl SourceLengthUnit {
    fn metres(self) -> f32 {
        match self {
            Self::Millimetre => 0.001,
            Self::Centimetre => 0.01,
            Self::Metre => 1.,
            Self::Inch => 0.0254,
            Self::Foot => 0.3048,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceFrame {
    YUpRightHanded,
    ZUpRightHanded,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourcePlacement {
    pub length_unit: SourceLengthUnit,
    pub frame: SourceFrame,
    pub world_translation_m: [f32; 3],
    pub world_rotation_xyzw: [f32; 4],
}
fn rotation(value: [f32; 4]) -> Result<Quat, String> {
    let q = Quat::from_array(value);
    if !q.is_finite() || (q.length_squared() - 1.).abs() > 0.001 {
        return Err("Supply a finite unit rotation quaternion [x,y,z,w]".into());
    }
    Ok(q.normalize())
}
impl SourcePlacement {
    fn transform(&self) -> Result<(Vec3, Quat, f32), String> {
        let offset = Vec3::from_array(self.world_translation_m);
        if !offset.is_finite() {
            return Err("World translation must be finite metres".into());
        }
        let frame = match self.frame {
            SourceFrame::YUpRightHanded => Quat::IDENTITY,
            SourceFrame::ZUpRightHanded => Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        };
        Ok((
            offset,
            rotation(self.world_rotation_xyzw)? * frame,
            self.length_unit.metres(),
        ))
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SourceReference {
    pub digest: String,
    pub part_index: usize,
    pub part_name: String,
    pub import_placement: SourcePlacement,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeInterpretation {
    pub source: SourceReference,
    pub rationale: String,
    pub uncertainty: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ForeignReferenceRequest {
    pub metadata: SourceMetadata,
    pub bytes: Vec<u8>,
    pub placement: SourcePlacement,
}
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ForeignNativeRequest {
    pub reference_element_id: u64,
    pub definition_id: String,
    pub overrides: BTreeMap<String, Value>,
    pub label: String,
    pub world_translation_m: [f32; 3],
    pub world_rotation_xyzw: [f32; 4],
    pub rationale: String,
    pub uncertainty: Vec<String>,
}

fn reference_geometry(
    request: &ForeignReferenceRequest,
) -> Result<Vec<(TriangleMesh, Value)>, String> {
    let text =
        std::str::from_utf8(&request.bytes).map_err(|_| "OBJ adapter requires UTF-8 input")?;
    let parts = crate::importers::obj::parse_obj_requests(text)?;
    if parts.len() > 64 {
        return Err("OBJ adapter supports at most 64 parts".into());
    }
    let (offset, q, scale) = request.placement.transform()?;
    parts
        .into_iter()
        .map(|part| {
            let mut mesh: TriangleMesh = serde_json::from_value(part.clone())
                .map_err(|e| format!("Invalid OBJ geometry: {e}"))?;
            if mesh.vertices.len() > 32768 || mesh.faces.len() > 32768 {
                return Err("OBJ part exceeds geometry budget".into());
            }
            for v in &mut mesh.vertices {
                *v = offset + q * (*v * scale);
                if !v.is_finite() {
                    return Err("OBJ contains non-finite or overflowing geometry".into());
                }
            }
            for face in &mesh.faces {
                let [a, b, c] = face.map(|i| mesh.vertices.get(i as usize).copied());
                let (Some(a), Some(b), Some(c)) = (a, b, c) else {
                    return Err("OBJ contains invalid indices".into());
                };
                if (b - a).cross(c - a).length_squared() <= 1e-16 {
                    return Err("OBJ contains degenerate triangles at world precision".into());
                }
            }
            if let Some(normals) = &mut mesh.normals {
                for n in normals {
                    if !n.is_finite() || n.length_squared() <= 1e-16 {
                        return Err("OBJ contains invalid normals".into());
                    }
                    *n = (q * *n).normalize();
                }
            }
            Ok((mesh, part))
        })
        .collect()
}

pub fn reference_draft(
    world: &World,
    request: ForeignReferenceRequest,
) -> Result<EditPlanDraft, String> {
    request.placement.transform()?;
    // Validate the bounded source before parsing or allocating derived geometry.
    let mut source = SourceArtifact {
        digest: blake3::hash(&request.bytes).to_hex().to_string(),
        metadata: request.metadata.clone(),
        bytes: request.bytes.clone(),
        assessment: SourceAssessment {
            adapter: "obj-reference-v1".into(),
            reference_geometry_available: false,
            degradation: vec![
                Degradation::NoAuthoritativeSemantics,
                Degradation::NoEditableHistory,
            ],
            detail: String::new(),
        },
    };
    source.validate()?;
    let geometry = if request.metadata.format.eq_ignore_ascii_case("obj") {
        match reference_geometry(&request) {
            Ok(parts) => parts,
            Err(reason) => {
                source
                    .assessment
                    .degradation
                    .push(Degradation::AdapterFailed);
                source.assessment.detail = reason;
                vec![]
            }
        }
    } else {
        source.assessment.adapter = "none".into();
        source.assessment.degradation.push(Degradation::NoAdapter);
        source.assessment.detail =
            "No adapter for this format; exact source retained without geometry".into();
        vec![]
    };
    if source.assessment.adapter == "obj-reference-v1" {
        let unsupported: BTreeSet<_> = String::from_utf8_lossy(&request.bytes)
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|k| !matches!(*k, "v" | "vn" | "f" | "o" | "g") && !k.starts_with('#'))
            .map(str::to_owned)
            .collect();
        if !unsupported.is_empty() {
            source
                .assessment
                .degradation
                .push(Degradation::UnsupportedRecords);
            source.assessment.detail.push_str(&format!(
                " Ignored OBJ records: {}",
                unsupported
                    .into_iter()
                    .take(16)
                    .map(|s| s.chars().take(40).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    source.assessment.reference_geometry_available = !geometry.is_empty();
    if !geometry.is_empty() {
        source.assessment.degradation.extend([
            Degradation::MaterialsAndUvNotMapped,
            Degradation::Float32Geometry,
            Degradation::FanTriangulation,
        ]);
    }
    let existing = world
        .get_resource::<SourceArtifacts>()
        .and_then(|r| r.0.get(&source.digest));
    if existing.is_some_and(|old| old != &source) {
        return Err("These source bytes already have different metadata or adapter assessment; inspect the retained artifact and resolve the discrepancy explicitly".into());
    }
    let changes = if existing.is_none() {
        vec![SourceArtifactChange {
            digest: source.digest.clone(),
            before: None,
            after: Some(source.clone()),
        }]
    } else {
        vec![SourceArtifactChange {
            digest: source.digest.clone(),
            before: Some(source.clone()),
            after: Some(source.clone()),
        }]
    };
    let first = world
        .get_resource::<ElementIdAllocator>()
        .ok_or("No identity allocator")?
        .next_value();
    let mut after = vec![];
    for (index, (primitive, part)) in geometry.into_iter().enumerate() {
        let id = first
            .checked_add(index as u64)
            .filter(|v| *v < u64::MAX)
            .ok_or("Identity capacity exhausted")?;
        let mut shadow = semantic_shadow_for_import_request(
            &source.metadata.format,
            Some(&source.metadata.name),
            &part,
        )
        .ok_or("Missing reference annotation")?;
        shadow.source.retained_source = Some(SourceReference {
            digest: source.digest.clone(),
            part_index: index,
            part_name: primitive
                .name
                .clone()
                .unwrap_or_else(|| format!("part_{index}")),
            import_placement: request.placement.clone(),
        });
        after.push(
            TriangleMeshSnapshot {
                element_id: ElementId(id),
                primitive,
                layer: Some(super::layers::DEFAULT_LAYER_NAME.into()),
                material_assignment: None,
                semantic_shadow: Some(shadow),
            }
            .into(),
        );
    }
    Ok(EditPlanDraft {
        context:PlanContext {planner_id:"core.foreign_reference".into(),planner_version:1,request_kind:"core.foreign_reference".into(),
            mutation_scope:format!("retain source {} and {} reference parts",source.digest,after.len()),
            intent:"Retain foreign input and review its reference geometry".into(),
            assumptions:vec!["Units, frame, producer, release and license are supplied assertions, not inferred evidence".into()],
            unresolved_decisions:vec!["Foreign geometry has no authoritative native semantics or construction claim".into()],
            findings:vec![serde_json::to_string(&source.assessment).unwrap()],..Default::default()},
        before:vec![],after,semantic_intents:Default::default(),source_artifacts:changes,
    })
}

pub fn native_draft(world: &World, request: ForeignNativeRequest) -> Result<EditPlanDraft, String> {
    if request.overrides.len() > 128
        || request.label.is_empty()
        || request.label.len() > 256
        || request.rationale.trim().is_empty()
        || request.rationale.len() > 2048
        || request.uncertainty.is_empty()
        || request.uncertainty.len() > 16
        || request
            .uncertainty
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 512)
    {
        return Err(
            "Supply a bounded label, rationale, controls and 1..16 explicit unresolved assumptions"
                .into(),
        );
    }
    let id = ElementId(request.reference_element_id);
    if super::modeling::linked_model::linked_model_source_owner(world, id).is_some() {
        return Err("Linked source content requires an explicit source-document migration".into());
    }
    let before = capture_snapshot(world, id).ok_or("Reference element not found")?;
    let mesh = before
        .0
        .as_any()
        .downcast_ref::<TriangleMeshSnapshot>()
        .ok_or("Only a retained foreign reference mesh may be interpreted")?;
    let source = mesh
        .semantic_shadow
        .as_ref()
        .and_then(|s| s.source.retained_source.clone())
        .ok_or("Reference has no retained source identity")?;
    let retained = world
        .get_resource::<SourceArtifacts>()
        .and_then(|r| r.0.get(&source.digest))
        .ok_or("Retained source artifact is missing")?;
    retained.validate()?;
    // This bounded mapping has no reseating or group-repair planner. Refuse
    // connected references instead of silently orphaning their relationships.
    if let Some(mut q) = world.try_query::<EntityRef>() {
        for entity in q.iter(world) {
            if entity
                .get::<GroupMembers>()
                .is_some_and(|g| g.member_ids.contains(&id))
                || entity
                    .get::<EntityDependencies>()
                    .is_some_and(|d| d.edges.iter().any(|e| e.on == id))
                || entity
                    .get::<crate::semantics::components::SemanticBindings>()
                    .is_some_and(|b| b.bindings.iter().any(|b| b.anchor_publisher == id.0))
            {
                return Err("Reference has authored dependents; a capability-owned relationship migration is required".into());
            }
            if entity.get::<ElementId>() == Some(&id)
                && (entity.contains::<crate::semantics::components::ConceptAssignment>()
                    || entity.contains::<crate::semantics::components::PublishedAnchors>()
                    || entity.contains::<crate::semantics::components::SemanticBindings>()
                    || entity.contains::<super::refinement::SemanticIntent>()
                    || entity.contains::<super::refinement::ObligationSet>()
                    || entity.contains::<super::refinement::RefinementStateComponent>()
                    || entity.contains::<crate::capability_registry::ElementClassAssignment>()
                    || entity
                        .get::<EntityDependencies>()
                        .is_some_and(|d| !d.edges.is_empty()))
            {
                return Err(
                    "Reference has authored semantic claims; explicit claim migration is required"
                        .into(),
                );
            }
        }
    }
    let registry = world
        .get_resource::<DefinitionRegistry>()
        .ok_or("No native Definition registry")?;
    let definition = registry
        .get(&DefinitionId(request.definition_id.clone()))
        .ok_or(
            "Native Definition is not registered; discover and import a curated Definition first",
        )?;
    let mut identity =
        OccurrenceIdentity::new(definition.id.clone(), definition.definition_version);
    for (key, value) in request.overrides {
        identity.overrides.set(key, value);
    }
    registry.validate_overrides(&identity.definition_id, &identity.overrides)?;
    if occurrence_geometry_part_count(registry, &identity)? == 0 {
        return Err(
            "Native interpretation must resolve visible geometry; reference retained".into(),
        );
    }
    identity.material_override = mesh.material_assignment.clone();
    identity.foreign_interpretation = Some(NativeInterpretation {
        source,
        rationale: request.rationale.clone(),
        uncertainty: request.uncertainty.clone(),
    });
    let new_id = world
        .get_resource::<ElementIdAllocator>()
        .ok_or("No identity allocator")?
        .next_value();
    if new_id == u64::MAX {
        return Err("Identity capacity exhausted".into());
    }
    let mut occurrence = OccurrenceSnapshot::new(ElementId(new_id), identity, request.label);
    occurrence.layer = mesh.layer.clone();
    occurrence.offset = Vec3::from_array(request.world_translation_m);
    occurrence.rotation = rotation(request.world_rotation_xyzw)?;
    if !occurrence.offset.is_finite() {
        return Err("Native placement must be finite metres".into());
    }
    Ok(EditPlanDraft {
        context: PlanContext {
            planner_id: "core.foreign_native_mapping".into(),
            planner_version: 1,
            request_kind: "core.foreign_native_mapping".into(),
            mutation_scope: format!(
                "replace reference {} with native occurrence {}",
                id.0, new_id
            ),
            intent: request.rationale,
            assumptions: vec![
                "New native interpretation; foreign intent and controls have not been recovered"
                    .into(),
            ],
            unresolved_decisions: request.uncertainty,
            ..Default::default()
        },
        before: vec![before],
        after: vec![occurrence.into()],
        semantic_intents: Default::default(),
        source_artifacts: vec![SourceArtifactChange {
            digest: retained.digest.clone(),
            before: Some(retained.clone()),
            after: Some(retained.clone()),
        }],
    })
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetForeignSourcesRequest {
    pub digest: Option<String>,
    #[serde(default)]
    pub include_bytes: bool,
}
pub fn inspect_sources(world: &World, request: GetForeignSourcesRequest) -> Result<Value, String> {
    if request.include_bytes && request.digest.is_none() {
        return Err("Exact bytes require one digest".into());
    }
    let assets = world.get_resource::<SourceArtifacts>();
    let mut sources = vec![];
    for artifact in assets
        .into_iter()
        .flat_map(|a| a.0.values())
        .filter(|a| request.digest.as_ref().is_none_or(|d| d == &a.digest))
    {
        let mut value = artifact.manifest();
        if request.include_bytes {
            value["bytes"] = json!(artifact.bytes);
        }
        sources.push(value);
    }
    if request.digest.is_some() && sources.is_empty() {
        return Err("Retained source digest not found".into());
    }
    Ok(
        json!({"sources":sources,"authority":"retained_input_only","source_byte_limit":MAX_SOURCE_BYTES,
        "next_step":"list_edit_requests: core.foreign_reference or core.foreign_native_mapping; preview before apply"}),
    )
}

#[cfg(feature = "model-api")]
pub fn register_requests(world: &mut World) {
    use super::authored_edit_plan::requests::{EditRequestDescriptor, EditRequestRegistry};
    world.init_resource::<SourceArtifacts>();
    let mut registry = world.resource_mut::<EditRequestRegistry>();
    registry.register(EditRequestDescriptor::new("core.foreign_reference",1,
        "Retain exact foreign source bytes (1..65536) with explicit units/frame/provenance and preview OBJ reference parts. Unsupported/failed adapters propose source-only retention with typed losses. Read get_foreign_sources; discard previews to reject; apply once through history.",
        serde_json::to_value(schemars::schema_for!(ForeignReferenceRequest)).unwrap(),
        |world,value|reference_draft(world,serde_json::from_value(value).map_err(|e|format!("Invalid foreign reference: {e}"))?))).expect("unique reference request");
    registry.register(EditRequestDescriptor::new("core.foreign_native_mapping",1,
        "Preview replacing one unconnected foreign reference part with an already registered native Definition and explicit controls/placement. Rationale and uncertainty are required. Retains source and unselected parts. This is a new hypothesis, not recovered intent or refinement. Apply once; undo restores the reference.",
        serde_json::to_value(schemars::schema_for!(ForeignNativeRequest)).unwrap(),
        |world,value|native_draft(world,serde_json::from_value(value).map_err(|e|format!("Invalid native interpretation: {e}"))?))).expect("unique interpretation request");
}

pub fn explanation_notes(
    world: &World,
    entity: Entity,
) -> Vec<super::design_explanation::ExplanationNote> {
    use super::design_explanation::{ExplanationNote, ExplanationRow};
    let interpretation = world
        .get::<OccurrenceIdentity>(entity)
        .and_then(|i| i.foreign_interpretation.as_ref());
    let reference = interpretation.map(|i| &i.source).or_else(|| {
        world
            .get::<SemanticShadow>(entity)
            .and_then(|s| s.source.retained_source.as_ref())
    });
    let Some(reference) = reference else {
        return vec![];
    };
    let mut notes = vec![ExplanationNote::Source(ExplanationRow {
        label: "Retained foreign source".into(),
        text: reference.part_name.clone(),
        details: json!({"digest":reference.digest,"part_index":reference.part_index,"import_placement":reference.import_placement,"next_tool":"get_foreign_sources","authority":"input_context_only"}),
    })];
    if let Some(i) = interpretation {
        notes.push(ExplanationNote::Source(ExplanationRow {
            label: "Native interpretation".into(),
            text: i.rationale.clone(),
            details: json!({"recovered_foreign_intent":false,"refinement_claim":false}),
        }));
        for text in &i.uncertainty {
            notes.push(ExplanationNote::Unresolved(ExplanationRow {
                label: "Interpretation uncertainty".into(),
                text: text.clone(),
                details: json!({"instance_obligation":false}),
            }));
        }
    }
    if interpretation.is_none() {
        if let Some(shadow) = world.get::<SemanticShadow>(entity) {
            for item in shadow
                .candidates
                .iter()
                .flat_map(|c| c.unresolved_decisions.iter())
                .take(16)
            {
                notes.push(ExplanationNote::Unresolved(ExplanationRow {
                    label: "Source uncertainty".into(), text: item.question.clone(),
                    details: json!({"id":item.id,"reason":item.reason,"grounding":item.grounding,"authority":"source_context","instance_obligation":false}),
                }));
            }
        }
    }
    if let Some(a) = world
        .get_resource::<SourceArtifacts>()
        .and_then(|r| r.0.get(&reference.digest))
    {
        notes.push(ExplanationNote::Evidence(ExplanationRow {label:"Import limits".into(),text:"Retained input does not establish native meaning, editable history or construction validity.".into(),details:a.manifest()}));
    } else {
        notes.push(ExplanationNote::Unresolved(ExplanationRow {
            label: "Missing source".into(),
            text: "Retained source is absent; provenance is unresolved.".into(),
            details: json!({"digest":reference.digest}),
        }));
    }
    notes
}
