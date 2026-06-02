//! World-side service for the parametric component MCP surface (PP-RPS-7 UX).
//!
//! The transport-neutral operations behind the `parametric.*` MCP tools:
//! list registered types, create an instance, inspect (drivers + derived),
//! set a driver (edit + propagation report), apply a transform gesture
//! (edit-by-gesture or refuse), and explain a dependency trace. These operate on
//! the core `ParametricRegistry` + `ParametricStore` resources; domain crates
//! (architecture) register the actual component types (window, truss).

use std::collections::BTreeMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugins::commands::despawn_by_element_id;
use crate::plugins::identity::{ElementId, ElementIdAllocator};
use crate::plugins::modeling::{
    mesh_generation::NeedsMesh,
    primitives::ShapeRotation,
    profile::{Profile2d, ProfileExtrusion, ProfileSegment},
};
use crate::plugins::refinement::{
    AuthoringMode, AuthoringProvenance, ClaimGrounding, ClaimPath, ClaimRecord, Grounding,
    HeuristicTag, RefinementState, RefinementStateComponent,
};
use crate::relational::graph::NodeId;
use crate::relational::param_expr::{ScalarExpr, Unit};
use crate::relational::registry::{
    ParametricRegistry, ParametricRepresentation, ParametricSnapshot, ParametricStore,
    ParametricTypeDef, Placement,
};
use crate::relational::transform::{TransformAxis, TransformGesture, TransformOutcome};

// --- request / response DTOs -----------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricTypeInfo {
    pub id: String,
    pub label: String,
}

/// Bevy ECS component that back-references a spawned geometry entity to its
/// parametric instance. Attached to every entity emitted by
/// `synthesize_parametric_geometry`, enabling stable member identity and
/// re-synthesis on driver edits.
#[derive(Component, Debug, Clone)]
pub struct ParametricInstanceRef {
    pub instance_id: u64,
    pub member_index: u32,
    pub member_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[derive(Default)]
pub struct CreateParametricRequest {
    /// ID of a registered parametric type (from `parametric.list_types`).
    /// May be omitted when `representation` is provided (inline dynamic authoring).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub type_id: String,
    /// Driver override values applied on top of the type's defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Value>,
    /// World-space placement of the instance (defaults to origin, no rotation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,

    // ---- Dynamic authoring (inline definition) fields ----
    /// When supplied, an ephemeral `ParametricTypeDef` is registered on the fly
    /// and instantiated. `type_id` is used as a label hint if provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub representation: Option<ParametricRepresentation>,
    /// Default driver values for the inline definition.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defaults: BTreeMap<String, f64>,
    /// Driver unit declarations for the inline definition.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub driver_units: BTreeMap<String, Unit>,
    /// Derived-value expressions for the inline definition.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub derivations: BTreeMap<String, ScalarExpr>,
    /// Human-readable label for the inline ephemeral type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct InspectParametricRequest {
    pub instance_id: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SetParametricDriverRequest {
    pub instance_id: u64,
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricTransformRequest {
    pub instance_id: u64,
    pub axis: TransformAxis,
    pub gesture: TransformGesture,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ExplainParametricRequest {
    pub instance_id: u64,
    pub param: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ExplainParametricResponse {
    pub instance_id: u64,
    pub param: String,
    pub controlling_drivers: Vec<String>,
    pub trace: Vec<NodeId>,
}

/// Response from `parametric.create` when the type carries a `representation`.
/// `snapshot` gives the full driver/derived state; `element_ids` lists every
/// scene entity that was spawned (one per evaluated member). When the type has
/// no representation, `element_ids` is empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CreateParametricResponse {
    pub snapshot: ParametricSnapshot,
    /// Element IDs of the spawned geometry entities (selectable in the scene).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub element_ids: Vec<u64>,
}

// --- world handlers --------------------------------------------------------

/// Return only public parametric types for MCP discovery.
///
/// Types with `public: false` are internal evaluator inputs that back a
/// `Definition` — they must not appear in `parametric.list_types` responses
/// because the same component is already discoverable through
/// `definition.list`.
pub fn world_list_types(world: &mut World) -> Vec<ParametricTypeInfo> {
    let Some(reg) = world.get_resource::<ParametricRegistry>() else {
        return Vec::new();
    };
    reg.list_public()
        .into_iter()
        .map(|(id, label)| ParametricTypeInfo { id, label })
        .collect()
}

pub fn world_create(
    world: &mut World,
    req: CreateParametricRequest,
) -> Result<CreateParametricResponse, String> {
    // Clone the registry so we can release the borrow before mutating the store.
    let mut reg = world
        .get_resource::<ParametricRegistry>()
        .cloned()
        .ok_or_else(|| "parametric registry not initialised".to_string())?;

    let placement = req.placement.clone().unwrap_or_default();

    // Resolve type_id: either a registered type or an inline ephemeral definition.
    let effective_type_id = if req.representation.is_some() {
        // Dynamic authoring: build and register an ephemeral ParametricTypeDef.
        use crate::relational::component::{ComponentParams, DriverPolicy};

        let label = req
            .label
            .clone()
            .or_else(|| (!req.type_id.is_empty()).then(|| req.type_id.clone()))
            .unwrap_or_else(|| "ephemeral".to_string());

        let mut params = ComponentParams::default();
        let mut driver_units_map = BTreeMap::new();
        let mut defaults_map = BTreeMap::new();

        for (name, unit) in &req.driver_units {
            params = params.driver(name, DriverPolicy::Editable);
            driver_units_map.insert(name.clone(), *unit);
        }
        for (name, val) in &req.defaults {
            defaults_map.insert(name.clone(), *val);
            // Register as a driver if not already in driver_units.
            if !req.driver_units.contains_key(name) {
                params = params.driver(name, DriverPolicy::Editable);
                driver_units_map.insert(name.clone(), Unit::Dimensionless);
            }
        }
        for name in req.derivations.keys() {
            params = params.derived(name);
        }

        let ephemeral_def = ParametricTypeDef {
            id: if req.type_id.is_empty() {
                String::new()
            } else {
                req.type_id.clone()
            },
            label,
            params,
            driver_units: driver_units_map,
            defaults: defaults_map,
            derivations: req.derivations.clone(),
            transform: crate::relational::transform::TransformBindings::default(),
            public: false,
            representation: req.representation.clone(),
        };
        let eid = reg.register_ephemeral(ephemeral_def);
        // Write the mutated registry back to the world.
        world.insert_resource(reg.clone());
        eid
    } else {
        if req.type_id.is_empty() {
            return Err(
                "type_id is required when no inline representation is provided".to_string(),
            );
        }
        if reg.get(&req.type_id).is_none() {
            return Err(format!("unknown parametric type '{}'", req.type_id));
        }
        req.type_id.clone()
    };

    let instance_id = {
        let mut store = world.resource_mut::<ParametricStore>();
        store.instantiate_with(&effective_type_id, req.overrides.clone(), placement)
    };

    // Synthesise representation geometry using the effective (post-override) drivers.
    let element_ids = synthesize_parametric_geometry(world, &reg, &effective_type_id, instance_id)?;

    // Record spawned element ids on the instance.
    {
        let mut store = world.resource_mut::<ParametricStore>();
        store.set_geometry(instance_id, element_ids.clone());
    }

    let snapshot = world_inspect(world, InspectParametricRequest { instance_id })?;

    Ok(CreateParametricResponse {
        snapshot,
        element_ids,
    })
}

/// Materialise one `ProfileExtrusion` entity per evaluated representation
/// member. Each entity gets a fresh `ElementId` so it is selectable in the
/// scene.
///
/// Applies the instance's `placement` to each member:
/// - `world_translate = placement_translate + placement_rot * member_translate`
/// - `world_rotation  = placement_rot * member_rot`
///
/// When a `ParametricMember` carries a `semantic` annotation the spawned
/// entity receives `ElementClassAssignment`, `RefinementStateComponent`, and
/// `SemanticIntent` components via the same `apply_semantic_annotation` path
/// used by `create_box`. Members without an annotation keep prior behaviour
/// (no regression).
///
/// Returns the list of allocated element IDs (empty when the type has no
/// representation), or `Err` if any member expression fails to evaluate.
fn synthesize_parametric_geometry(
    world: &mut World,
    reg: &ParametricRegistry,
    type_id: &str,
    instance_id: u64,
) -> Result<Vec<u64>, String> {
    let Some(type_def) = reg.get(type_id) else {
        return Ok(Vec::new());
    };

    // Capture the original member descriptors (for semantic annotations) before
    // the immutable borrow of type_def is released.
    let original_members: Option<Vec<crate::relational::registry::ParametricMemberSemantic>> =
        type_def.representation.as_ref().map(|repr| {
            repr.members
                .iter()
                .map(|m| {
                    m.semantic
                        .clone()
                        .unwrap_or_default()
                })
                .collect()
        });

    // Read effective overrides from the stored instance.
    let (overrides, placement) = {
        let store = world.resource::<ParametricStore>();
        match store.get(instance_id) {
            Some(inst) => (inst.overrides.clone(), inst.placement.clone()),
            None => (BTreeMap::new(), Placement::default()),
        }
    };

    let Some(result) = type_def.evaluate_representation(&overrides) else {
        return Ok(Vec::new());
    };
    let members = result?;

    // Build the placement rotation quaternion once.
    let pl_rx = (placement.rotate_euler_deg[0] as f32).to_radians();
    let pl_ry = (placement.rotate_euler_deg[1] as f32).to_radians();
    let pl_rz = (placement.rotate_euler_deg[2] as f32).to_radians();
    let placement_rot = Quat::from_euler(EulerRot::XYZ, pl_rx, pl_ry, pl_rz);
    // Member geometry is authored in millimetres (parametric drivers are
    // `*_mm`) and converted mm -> m below. PLACEMENT, however, is "where in the
    // world" and is expressed in METRES (world units) — consistent with
    // create_box and instantiate_recipe placement, so an agent never mixes units.
    const MM_TO_M: f32 = 0.001;
    let placement_translate = Vec3::new(
        placement.translate[0] as f32,
        placement.translate[1] as f32,
        placement.translate[2] as f32,
    );

    // Determine default refinement state for members that carry no semantic
    // annotation (lowest claim, requires no obligation resolution).
    let default_refinement_state = RefinementState::Conceptual;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Pair each evaluated member with its original semantic descriptor (if any).
    // `original_members` is `Some` iff the type has a representation, and in
    // that case it has exactly one entry per member (built from the same slice).
    let semantics: Vec<Option<crate::relational::registry::ParametricMemberSemantic>> =
        match original_members {
            Some(sem_vec) => sem_vec
                .into_iter()
                .map(|s| {
                    // Only propagate when at least one meaningful field is set.
                    let has_content = s.element_class.is_some()
                        || s.refinement_state.is_some()
                        || !s.parameters.is_null()
                        || s.rationale.is_some();
                    has_content.then_some(s)
                })
                .collect(),
            None => vec![None; members.len()],
        };

    let mut element_ids: Vec<u64> = Vec::with_capacity(members.len());
    for (member_index, (m, maybe_semantic)) in members.into_iter().zip(semantics).enumerate() {
        let eid = world.resource::<ElementIdAllocator>().next_id();

        // Sizes mm → m. height (Y) drives extrusion height; width
        // (X) and depth (Z) form the rectangular profile.
        let width = m.size[0] as f32 * MM_TO_M;
        let height = m.size[1] as f32 * MM_TO_M;
        let depth = m.size[2] as f32 * MM_TO_M;

        // Per-member local rotation.
        let rx = (m.rotate_euler_deg[0] as f32).to_radians();
        let ry = (m.rotate_euler_deg[1] as f32).to_radians();
        let rz = (m.rotate_euler_deg[2] as f32).to_radians();
        let member_rot = Quat::from_euler(EulerRot::XYZ, rx, ry, rz);

        // Compose placement: world_rot = placement_rot * member_rot
        let world_rot = placement_rot * member_rot;

        // Compose translation: world_pos = placement_translate + placement_rot * member_translate
        let member_translate = Vec3::new(
            m.translate[0] as f32 * MM_TO_M,
            m.translate[1] as f32 * MM_TO_M,
            m.translate[2] as f32 * MM_TO_M,
        );
        let world_translate = placement_translate + placement_rot * member_translate;

        // Profile: an arbitrary extruded polygon when the member supplies
        // `profile_xz` points (mm, in the u→X / v→Z profile plane), else the
        // default rectangle(width, depth). Polygon profiles keep the
        // representation general (e.g. a gable triangle) with no per-shape code.
        let profile = if m.profile_xz.len() >= 3 {
            let p = |pt: &[f64; 2]| Vec2::new(pt[0] as f32 * MM_TO_M, pt[1] as f32 * MM_TO_M);
            let start = p(&m.profile_xz[0]);
            let segments = m.profile_xz[1..]
                .iter()
                .map(|pt| ProfileSegment::LineTo { to: p(pt) })
                .collect();
            Profile2d { start, segments }
        } else {
            Profile2d::rectangle(width, depth)
        };
        let extrusion = ProfileExtrusion {
            centre: world_translate,
            profile,
            height,
        };

        // Build a ClaimGrounding that records this entity was generated by
        // a parametric type, with the type_id as the source.
        let mut claims = std::collections::HashMap::new();
        claims.insert(
            ClaimPath("parametric_source".to_string()),
            ClaimRecord {
                grounding: Grounding::LLMHeuristic {
                    rationale: format!(
                        "geometry generated by parametric type '{type_id}' instance {instance_id}"
                    ),
                    heuristic_tag: HeuristicTag(format!("parametric:{type_id}")),
                },
                set_at: now_secs,
                set_by: None,
            },
        );
        let claim_grounding = ClaimGrounding { claims };

        world.spawn((
            eid,
            extrusion,
            ShapeRotation(world_rot),
            NeedsMesh,
            Visibility::Visible,
            GlobalTransform::default(),
            ParametricInstanceRef {
                instance_id,
                member_index: member_index as u32,
                member_label: m.label.clone(),
            },
            RefinementStateComponent {
                state: default_refinement_state,
            },
            AuthoringProvenance {
                mode: AuthoringMode::Freeform,
                rationale: Some(format!(
                    "parametric type '{type_id}' instance {instance_id} member {member_index}"
                )),
            },
            claim_grounding,
        ));

        // Apply per-member semantic annotation (element_class, refinement_state,
        // parameters) if the member declared one.  Uses the same
        // `validate_semantic_annotation` + `apply_semantic_annotation` path as
        // `create_box` so semantics are consistent across all creation surfaces.
        // Gated on `model-api` because the annotation helpers live in that module.
        #[cfg(feature = "model-api")]
        if let Some(sem) = maybe_semantic {
            let annotation = crate::plugins::model_api::SemanticEntityAnnotationRequest {
                element_class: sem.element_class,
                refinement_state: sem.refinement_state,
                parameters: sem.parameters,
                rationale: sem.rationale,
                unresolved_decisions: Vec::new(),
                source_refs: Vec::new(),
            };
            let validated =
                crate::plugins::model_api::validate_semantic_annotation(world, &annotation)?;
            crate::plugins::model_api::apply_semantic_annotation(
                world, eid, annotation, validated,
            )?;
        }

        element_ids.push(eid.0);
    }

    Ok(element_ids)
}

pub fn world_inspect(
    world: &mut World,
    req: InspectParametricRequest,
) -> Result<ParametricSnapshot, String> {
    let reg = world
        .get_resource::<ParametricRegistry>()
        .cloned()
        .unwrap_or_default();
    let store = world.resource::<ParametricStore>();
    store
        .snapshot(&reg, req.instance_id)
        .ok_or_else(|| format!("unknown parametric instance {}", req.instance_id))
}

/// Response from `parametric.set_driver` when the instance has live geometry:
/// the propagation report plus the new element ids of re-synthesised entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct SetDriverResponse {
    pub propagation: crate::relational::service::PropagationReport,
    /// New element IDs after re-synthesis. Empty when the type has no representation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub element_ids: Vec<u64>,
}

pub fn world_set_driver(
    world: &mut World,
    req: SetParametricDriverRequest,
) -> Result<SetDriverResponse, String> {
    let reg = world
        .get_resource::<ParametricRegistry>()
        .cloned()
        .unwrap_or_default();

    // Apply the driver edit.
    let propagation = {
        let mut store = world.resource_mut::<ParametricStore>();
        store
            .set_driver(&reg, req.instance_id, &req.name, req.value)
            .map_err(|e| format!("{e}"))?
    };

    // Re-synthesise geometry if this instance has a representation.
    let element_ids = world_resynthesize(world, &reg, req.instance_id)?;

    Ok(SetDriverResponse {
        propagation,
        element_ids,
    })
}

/// Despawn all existing geometry entities for an instance, then re-synthesise
/// from the current effective drivers + placement. Updates `instance.geometry`.
///
/// Returns the new element ids (empty if the type has no representation).
fn world_resynthesize(
    world: &mut World,
    reg: &ParametricRegistry,
    instance_id: u64,
) -> Result<Vec<u64>, String> {
    // Read the previous geometry ids and type_id without holding a borrow.
    let (old_geometry, type_id) = {
        let store = world.resource::<ParametricStore>();
        match store.get(instance_id) {
            Some(inst) => (inst.geometry.clone(), inst.type_id.clone()),
            None => return Ok(Vec::new()),
        }
    };

    // Despawn previous geometry entities.
    for &eid_raw in &old_geometry {
        despawn_by_element_id(world, ElementId(eid_raw));
    }

    // Re-synthesise with updated overrides + same placement.
    let new_ids = synthesize_parametric_geometry(world, reg, &type_id, instance_id)?;

    // Record updated geometry ids.
    {
        let mut store = world.resource_mut::<ParametricStore>();
        store.set_geometry(instance_id, new_ids.clone());
    }

    Ok(new_ids)
}

pub fn world_transform(
    world: &mut World,
    req: ParametricTransformRequest,
) -> Result<TransformOutcome, String> {
    let reg = world
        .get_resource::<ParametricRegistry>()
        .cloned()
        .unwrap_or_default();
    let mut store = world.resource_mut::<ParametricStore>();
    store
        .transform(&reg, req.instance_id, req.axis, req.gesture)
        .ok_or_else(|| format!("unknown parametric instance {}", req.instance_id))
}

pub fn world_explain(
    world: &mut World,
    req: ExplainParametricRequest,
) -> Result<ExplainParametricResponse, String> {
    let reg = world
        .get_resource::<ParametricRegistry>()
        .cloned()
        .unwrap_or_default();
    let store = world.resource::<ParametricStore>();
    let trace = store
        .explain(&reg, req.instance_id, &req.param)
        .ok_or_else(|| format!("unknown parametric instance {}", req.instance_id))?;
    let controlling_drivers = trace
        .iter()
        .filter_map(|n| match n {
            NodeId::ComponentParam { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    Ok(ExplainParametricResponse {
        instance_id: req.instance_id,
        param: req.param,
        controlling_drivers,
        trace,
    })
}

/// Ensures the parametric registry/store resources exist (domain plugins
/// populate the registry). Mirrors `relational::registry::ParametricPlugin`.
pub struct ParametricMcpPlugin;

impl Plugin for ParametricMcpPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(crate::relational::registry::ParametricPlugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relational::component::{ComponentParams, DriverPolicy};
    use crate::relational::param_expr::{Quantity, ScalarExpr, Unit};
    use crate::relational::registry::{
        ParametricMember, ParametricRepresentation, ParametricTypeDef, Placement,
    };
    use crate::relational::transform::{TransformAxis, TransformBindings, TransformGesture};
    use serde_json::json;
    use std::collections::BTreeMap;

    // ---- helpers -----------------------------------------------------------

    fn zero_exprs() -> [ScalarExpr; 3] {
        [
            ScalarExpr::lit(Quantity::num(0.0)),
            ScalarExpr::lit(Quantity::num(0.0)),
            ScalarExpr::lit(Quantity::num(0.0)),
        ]
    }

    fn box_type() -> ParametricTypeDef {
        let params = ComponentParams::default()
            .driver("width", DriverPolicy::Editable)
            .derived("half");
        let mut driver_units = BTreeMap::new();
        driver_units.insert("width".into(), Unit::Mm);
        let mut defaults = BTreeMap::new();
        defaults.insert("width".into(), 1000.0);
        let mut d = BTreeMap::new();
        d.insert(
            "half".into(),
            ScalarExpr::Div {
                lhs: Box::new(ScalarExpr::param("width")),
                rhs: Box::new(ScalarExpr::lit(Quantity::num(2.0))),
            },
        );
        ParametricTypeDef {
            id: "test.box".into(),
            label: "Test Box".into(),
            params,
            driver_units,
            defaults,
            derivations: d,
            transform: TransformBindings::default().bind(TransformAxis::X, "width"),
            public: true,
            representation: None,
        }
    }

    /// A type with a two-member representation driven by `width`.
    fn repr_box_type() -> ParametricTypeDef {
        let params = ComponentParams::default().driver("width", DriverPolicy::Editable);
        let mut driver_units = BTreeMap::new();
        driver_units.insert("width".into(), Unit::Mm);
        let mut defaults = BTreeMap::new();
        defaults.insert("width".into(), 1000.0);

        let m0 = ParametricMember {
            size: [
                ScalarExpr::param("width"),
                ScalarExpr::lit(Quantity::mm(500.0)),
                ScalarExpr::lit(Quantity::mm(100.0)),
            ],
            translate: zero_exprs(),
            rotate_euler_deg: zero_exprs(),
            profile_xz: Vec::new(),
            label: Some("member_a".into()),
            semantic: None,
        };
        let m1 = ParametricMember {
            size: [
                ScalarExpr::param("width"),
                ScalarExpr::lit(Quantity::mm(500.0)),
                ScalarExpr::lit(Quantity::mm(100.0)),
            ],
            translate: [
                ScalarExpr::param("width"),
                ScalarExpr::lit(Quantity::mm(0.0)),
                ScalarExpr::lit(Quantity::mm(0.0)),
            ],
            rotate_euler_deg: zero_exprs(),
            profile_xz: Vec::new(),
            label: Some("member_b".into()),
            semantic: None,
        };

        ParametricTypeDef {
            id: "test.repr_box".into(),
            label: "Repr Box".into(),
            params,
            driver_units,
            defaults,
            derivations: BTreeMap::new(),
            transform: TransformBindings::default().bind(TransformAxis::X, "width"),
            public: true,
            representation: Some(ParametricRepresentation {
                members: vec![m0, m1],
            }),
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(ParametricMcpPlugin);
        // Initialise the ElementIdAllocator required by synthesize_parametric_geometry.
        app.init_resource::<crate::plugins::identity::ElementIdAllocator>();
        {
            let mut reg = app.world_mut().resource_mut::<ParametricRegistry>();
            reg.register(box_type());
            reg.register(repr_box_type());
        }
        app
    }

    // ---- original round-trip test ------------------------------------------

    #[test]
    fn list_create_inspect_edit_explain_over_world() {
        let mut app = app();
        let w = app.world_mut();
        // list — only public types
        let types = world_list_types(w);
        assert_eq!(types.len(), 2);
        // create (no representation — element_ids empty)
        let resp = world_create(
            w,
            CreateParametricRequest {
                type_id: "test.box".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let id = resp.snapshot.instance_id;
        assert_eq!(resp.snapshot.derived["half"], json!(500.0));
        assert!(
            resp.element_ids.is_empty(),
            "box_type has no representation"
        );
        // edit via set_driver
        let dr = world_set_driver(
            w,
            SetParametricDriverRequest {
                instance_id: id,
                name: "width".into(),
                value: json!(1600.0),
            },
        )
        .unwrap();
        assert!(dr.propagation.changed_derived.contains_key("half"));
        // inspect reflects the edit
        let snap2 = world_inspect(w, InspectParametricRequest { instance_id: id }).unwrap();
        assert_eq!(snap2.derived["half"], json!(800.0));
        // transform gesture
        let out = world_transform(
            w,
            ParametricTransformRequest {
                instance_id: id,
                axis: TransformAxis::X,
                gesture: TransformGesture::SetExtent { value: 2000.0 },
            },
        )
        .unwrap();
        assert!(matches!(out, TransformOutcome::DriverEdit { .. }));
        assert_eq!(
            world_inspect(w, InspectParametricRequest { instance_id: id })
                .unwrap()
                .derived["half"],
            json!(1000.0)
        );
        // explain
        let ex = world_explain(
            w,
            ExplainParametricRequest {
                instance_id: id,
                param: "half".into(),
            },
        )
        .unwrap();
        assert!(ex.controlling_drivers.contains(&"width".to_string()));
        // unknown type / instance errors
        assert!(world_create(
            w,
            CreateParametricRequest {
                type_id: "nope".into(),
                ..Default::default()
            }
        )
        .is_err());
        assert!(world_inspect(w, InspectParametricRequest { instance_id: 999 }).is_err());
    }

    // ---- P1a: override-driven size + placement -----------------------------

    #[test]
    fn create_with_overrides_produces_correct_geometry() {
        let mut app = app();
        let w = app.world_mut();

        let mut overrides = BTreeMap::new();
        overrides.insert("width".to_string(), json!(2000.0));

        let resp = world_create(
            w,
            CreateParametricRequest {
                type_id: "test.repr_box".into(),
                overrides,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            resp.element_ids.len(),
            2,
            "repr_box has 2 members, should emit 2 entities"
        );
        // Snapshot must reflect the override.
        assert_eq!(resp.snapshot.drivers["width"], json!(2000.0));
    }

    #[test]
    fn placement_is_stored_on_instance() {
        let mut app = app();
        let w = app.world_mut();

        let pl = Placement {
            translate: [500.0, 0.0, 0.0],
            rotate_euler_deg: [0.0, 90.0, 0.0],
        };
        let resp = world_create(
            w,
            CreateParametricRequest {
                type_id: "test.repr_box".into(),
                placement: Some(pl.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let store = w.resource::<ParametricStore>();
        let inst = store.get(resp.snapshot.instance_id).unwrap();
        assert_eq!(inst.placement, pl);
    }

    // ---- P1a: re-synthesis on set_driver -----------------------------------

    #[test]
    fn set_driver_resynthesize_replaces_geometry() {
        let mut app = app();
        let w = app.world_mut();

        let resp = world_create(
            w,
            CreateParametricRequest {
                type_id: "test.repr_box".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let instance_id = resp.snapshot.instance_id;
        let old_ids = resp.element_ids.clone();
        assert_eq!(old_ids.len(), 2);

        // Edit a driver — should despawn old geometry and spawn new.
        let dr = world_set_driver(
            w,
            SetParametricDriverRequest {
                instance_id,
                name: "width".into(),
                value: json!(3000.0),
            },
        )
        .unwrap();

        let new_ids = dr.element_ids;
        assert_eq!(new_ids.len(), 2, "should still have 2 members");

        // Old element ids must be gone from the world.
        for old_eid in &old_ids {
            let mut q = w.query::<&crate::plugins::identity::ElementId>();
            let still_exists = q.iter(w).any(|e| e.0 == *old_eid);
            assert!(
                !still_exists,
                "old entity {old_eid} should have been despawned"
            );
        }

        // New ids should differ.
        assert_ne!(old_ids, new_ids, "new element ids should differ from old");

        // Instance geometry list must be updated.
        let store = w.resource::<ParametricStore>();
        assert_eq!(store.get(instance_id).unwrap().geometry, new_ids);
    }

    // ---- P2a: provenance components are attached ---------------------------

    #[test]
    fn synthesised_entities_carry_provenance_components() {
        let mut app = app();
        let w = app.world_mut();

        let resp = world_create(
            w,
            CreateParametricRequest {
                type_id: "test.repr_box".into(),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(!resp.element_ids.is_empty());
        let first_eid = resp.element_ids[0];

        // Find the entity.
        let mut q = w.query::<(
            &crate::plugins::identity::ElementId,
            &RefinementStateComponent,
            &AuthoringProvenance,
            &ClaimGrounding,
            &ParametricInstanceRef,
        )>();
        let found = q.iter(w).find(|(e, _, _, _, _)| e.0 == first_eid);

        let (_, refinement, provenance, grounding, inst_ref) =
            found.expect("entity should carry provenance components");

        assert_eq!(refinement.state, RefinementState::Conceptual);
        assert!(
            provenance
                .rationale
                .as_ref()
                .unwrap()
                .contains("test.repr_box"),
            "provenance should mention the type id"
        );
        assert!(
            grounding
                .claims
                .contains_key(&ClaimPath("parametric_source".to_string())),
            "claim grounding should have parametric_source entry"
        );
        assert_eq!(inst_ref.instance_id, resp.snapshot.instance_id);
        assert_eq!(inst_ref.member_index, 0);
    }

    // ---- P3 / item 8: inline representation (dynamic authoring) -----------

    #[test]
    fn inline_representation_creates_ephemeral_type_and_geometry() {
        let mut app = app();
        let w = app.world_mut();

        // Build an inline 3-member representation without pre-registering a type.
        let members = vec![
            ParametricMember {
                size: [
                    ScalarExpr::lit(Quantity::mm(1000.0)),
                    ScalarExpr::lit(Quantity::mm(200.0)),
                    ScalarExpr::lit(Quantity::mm(100.0)),
                ],
                translate: [
                    ScalarExpr::lit(Quantity::mm(0.0)),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                ],
                rotate_euler_deg: [
                    ScalarExpr::lit(Quantity::num(0.0)),
                    ScalarExpr::lit(Quantity::num(0.0)),
                    ScalarExpr::lit(Quantity::num(0.0)),
                ],
                profile_xz: Vec::new(),
                label: Some("inline_0".into()),
                semantic: None,
            },
            ParametricMember {
                size: [
                    ScalarExpr::lit(Quantity::mm(500.0)),
                    ScalarExpr::lit(Quantity::mm(200.0)),
                    ScalarExpr::lit(Quantity::mm(100.0)),
                ],
                translate: [
                    ScalarExpr::lit(Quantity::mm(1000.0)),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                ],
                rotate_euler_deg: [
                    ScalarExpr::lit(Quantity::num(0.0)),
                    ScalarExpr::lit(Quantity::num(0.0)),
                    ScalarExpr::lit(Quantity::num(0.0)),
                ],
                profile_xz: Vec::new(),
                label: Some("inline_1".into()),
                semantic: None,
            },
            ParametricMember {
                size: [
                    ScalarExpr::lit(Quantity::mm(200.0)),
                    ScalarExpr::lit(Quantity::mm(200.0)),
                    ScalarExpr::lit(Quantity::mm(100.0)),
                ],
                translate: [
                    ScalarExpr::lit(Quantity::mm(1500.0)),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                ],
                rotate_euler_deg: [
                    ScalarExpr::lit(Quantity::num(0.0)),
                    ScalarExpr::lit(Quantity::num(0.0)),
                    ScalarExpr::lit(Quantity::num(0.0)),
                ],
                profile_xz: Vec::new(),
                label: Some("inline_2".into()),
                semantic: None,
            },
        ];

        let resp = world_create(
            w,
            CreateParametricRequest {
                type_id: String::new(),
                representation: Some(ParametricRepresentation { members }),
                label: Some("inline_test".into()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            resp.element_ids.len(),
            3,
            "should produce 3 entities for 3 members"
        );

        // The ephemeral type must not be listed as public.
        let public_types = world_list_types(w);
        assert!(
            public_types.iter().all(|t| !t.id.starts_with("ephemeral.")),
            "ephemeral types must not appear in list_types"
        );
    }

    // ---- Semantic annotation propagation tests (require model-api) ----------

    #[cfg(feature = "model-api")]
    mod semantic {
        use super::*;
        use crate::capability_registry::{
            CapabilityRegistry, ElementClassAssignment, ElementClassDescriptor, ElementClassId,
        };
        use crate::plugins::refinement::{RefinementState, RefinementStateComponent, SemanticIntent};
        use crate::relational::registry::ParametricMemberSemantic;

        /// Register a minimal test element class so `validate_semantic_annotation`
        /// resolves it. Uses a domain-neutral name; no architecture nouns.
        fn register_test_element_class(world: &mut World, class_id: &str) {
            let mut reg = world
                .get_resource_mut::<CapabilityRegistry>()
                .expect("CapabilityRegistry must be present");
            reg.register_element_class(ElementClassDescriptor {
                id: ElementClassId(class_id.to_string()),
                label: class_id.to_string(),
                description: "test class".to_string(),
                semantic_roles: Vec::new(),
                class_min_obligations: std::collections::HashMap::new(),
                class_min_promotion_critical_paths: std::collections::HashMap::new(),
                parameter_schema: serde_json::Value::Null,
            });
        }

        /// Minimal app with ParametricMcpPlugin + CapabilityRegistry.
        fn app_with_registry() -> App {
            let mut app = super::app();
            app.init_resource::<CapabilityRegistry>();
            app
        }

        /// Build a one-member parametric type whose member carries a semantic annotation.
        fn annotated_member_type(semantic: ParametricMemberSemantic) -> ParametricTypeDef {
            let params = ComponentParams::default().driver("width", DriverPolicy::Editable);
            let mut driver_units = BTreeMap::new();
            driver_units.insert("width".into(), Unit::Mm);
            let mut defaults = BTreeMap::new();
            defaults.insert("width".into(), 1000.0);

            let member = ParametricMember {
                size: [
                    ScalarExpr::param("width"),
                    ScalarExpr::lit(Quantity::mm(200.0)),
                    ScalarExpr::lit(Quantity::mm(100.0)),
                ],
                translate: super::zero_exprs(),
                rotate_euler_deg: super::zero_exprs(),
                profile_xz: Vec::new(),
                label: Some("annotated_member".into()),
                semantic: Some(semantic),
            };

            ParametricTypeDef {
                id: "test.annotated".into(),
                label: "Annotated Type".into(),
                params,
                driver_units,
                defaults,
                derivations: BTreeMap::new(),
                transform: crate::relational::transform::TransformBindings::default(),
                public: true,
                representation: Some(ParametricRepresentation {
                    members: vec![member],
                }),
            }
        }

        // P-SEM-1: annotated member propagates element_class, refinement_state,
        // and parameters to the spawned entity.
        #[test]
        fn annotated_member_entity_carries_semantic_components() {
            let mut app = app_with_registry();
            let w = app.world_mut();

            register_test_element_class(w, "test_structural_member");

            let sem = ParametricMemberSemantic {
                element_class: Some("test_structural_member".into()),
                refinement_state: Some("Schematic".into()),
                parameters: serde_json::json!({ "construction_role": "chord" }),
                rationale: Some("annotated for test".into()),
            };
            {
                let mut reg = w.resource_mut::<ParametricRegistry>();
                reg.register(annotated_member_type(sem));
            }

            let resp = world_create(
                w,
                CreateParametricRequest {
                    type_id: "test.annotated".into(),
                    ..Default::default()
                },
            )
            .unwrap();

            assert_eq!(resp.element_ids.len(), 1, "one member => one entity");
            let eid_raw = resp.element_ids[0];

            // Locate the entity.
            let entity = {
                let mut q = w.query::<(Entity, &crate::plugins::identity::ElementId)>();
                q.iter(w)
                    .find_map(|(e, id)| (id.0 == eid_raw).then_some(e))
                    .expect("entity must exist")
            };

            // ElementClassAssignment must be present and match the declared class.
            let assignment = w
                .get::<ElementClassAssignment>(entity)
                .expect("ElementClassAssignment must be attached");
            assert_eq!(
                assignment.element_class.0, "test_structural_member",
                "element_class must match the declared annotation"
            );

            // RefinementStateComponent must reflect the declared state, NOT Conceptual.
            let refinement = w
                .get::<RefinementStateComponent>(entity)
                .expect("RefinementStateComponent must be attached");
            assert_eq!(
                refinement.state,
                RefinementState::Schematic,
                "refinement_state must be Schematic, not the default Conceptual"
            );

            // SemanticIntent must carry the declared parameters key.
            let intent = w
                .get::<SemanticIntent>(entity)
                .expect("SemanticIntent must be attached");
            assert_eq!(
                intent.parameters.get("construction_role").and_then(|v| v.as_str()),
                Some("chord"),
                "parameters must contain construction_role=chord"
            );
        }

        // P-SEM-2: member WITHOUT annotation keeps prior behaviour —
        // no ElementClassAssignment, default Conceptual refinement state.
        #[test]
        fn unannotated_member_entity_has_no_class_assignment() {
            let mut app = app_with_registry();
            let w = app.world_mut();

            // Use the pre-registered repr_box type (no semantic on any member).
            let resp = world_create(
                w,
                CreateParametricRequest {
                    type_id: "test.repr_box".into(),
                    ..Default::default()
                },
            )
            .unwrap();

            assert!(!resp.element_ids.is_empty());
            let eid_raw = resp.element_ids[0];

            let entity = {
                let mut q = w.query::<(Entity, &crate::plugins::identity::ElementId)>();
                q.iter(w)
                    .find_map(|(e, id)| (id.0 == eid_raw).then_some(e))
                    .expect("entity must exist")
            };

            // No ElementClassAssignment — no class declared.
            assert!(
                w.get::<ElementClassAssignment>(entity).is_none(),
                "unannotated member must not receive ElementClassAssignment"
            );

            // Refinement state stays at the default Conceptual.
            let refinement = w
                .get::<RefinementStateComponent>(entity)
                .expect("RefinementStateComponent must be present (set by synthesizer)");
            assert_eq!(
                refinement.state,
                RefinementState::Conceptual,
                "unannotated member refinement_state must remain Conceptual"
            );

            // No SemanticIntent inserted.
            assert!(
                w.get::<SemanticIntent>(entity).is_none(),
                "unannotated member must not receive SemanticIntent"
            );
        }

        // P-SEM-3: mixed members — first carries annotation, second does not.
        // Verifies the zip logic applies the annotation only to the right entity.
        #[test]
        fn mixed_members_annotation_targets_correct_entity() {
            let mut app = app_with_registry();
            let w = app.world_mut();

            register_test_element_class(w, "test_element_a");

            let params = ComponentParams::default().driver("width", DriverPolicy::Editable);
            let mut driver_units = BTreeMap::new();
            driver_units.insert("width".into(), Unit::Mm);
            let mut defaults = BTreeMap::new();
            defaults.insert("width".into(), 500.0);

            let m0 = ParametricMember {
                size: [
                    ScalarExpr::param("width"),
                    ScalarExpr::lit(Quantity::mm(100.0)),
                    ScalarExpr::lit(Quantity::mm(50.0)),
                ],
                translate: super::zero_exprs(),
                rotate_euler_deg: super::zero_exprs(),
                profile_xz: Vec::new(),
                label: Some("annotated".into()),
                semantic: Some(ParametricMemberSemantic {
                    element_class: Some("test_element_a".into()),
                    refinement_state: Some("Constructible".into()),
                    parameters: serde_json::json!({ "construction_role": "primary" }),
                    rationale: None,
                }),
            };
            let m1 = ParametricMember {
                size: [
                    ScalarExpr::param("width"),
                    ScalarExpr::lit(Quantity::mm(100.0)),
                    ScalarExpr::lit(Quantity::mm(50.0)),
                ],
                translate: [
                    ScalarExpr::param("width"),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                    ScalarExpr::lit(Quantity::mm(0.0)),
                ],
                rotate_euler_deg: super::zero_exprs(),
                profile_xz: Vec::new(),
                label: Some("plain".into()),
                semantic: None,
            };

            let mixed_type = ParametricTypeDef {
                id: "test.mixed".into(),
                label: "Mixed".into(),
                params,
                driver_units,
                defaults,
                derivations: BTreeMap::new(),
                transform: crate::relational::transform::TransformBindings::default(),
                public: true,
                representation: Some(ParametricRepresentation {
                    members: vec![m0, m1],
                }),
            };
            {
                let mut reg = w.resource_mut::<ParametricRegistry>();
                reg.register(mixed_type);
            }

            let resp = world_create(
                w,
                CreateParametricRequest {
                    type_id: "test.mixed".into(),
                    ..Default::default()
                },
            )
            .unwrap();

            assert_eq!(resp.element_ids.len(), 2);
            let (eid0, eid1) = (resp.element_ids[0], resp.element_ids[1]);

            let mut find_entity = |eid_raw: u64| {
                let mut q = w.query::<(Entity, &crate::plugins::identity::ElementId)>();
                q.iter(w)
                    .find_map(|(e, id)| (id.0 == eid_raw).then_some(e))
                    .expect("entity must exist")
            };

            let e0 = find_entity(eid0);
            let e1 = find_entity(eid1);

            // First entity (m0) — annotated.
            assert!(
                w.get::<ElementClassAssignment>(e0).is_some(),
                "annotated member must have ElementClassAssignment"
            );
            assert_eq!(
                w.get::<RefinementStateComponent>(e0).unwrap().state,
                RefinementState::Constructible,
            );

            // Second entity (m1) — plain.
            assert!(
                w.get::<ElementClassAssignment>(e1).is_none(),
                "plain member must not have ElementClassAssignment"
            );
            assert_eq!(
                w.get::<RefinementStateComponent>(e1).unwrap().state,
                RefinementState::Conceptual,
            );
        }

        // P-SEM-4: invalid element_class in annotation returns an Err.
        #[test]
        fn invalid_element_class_annotation_returns_error() {
            let mut app = app_with_registry();
            let w = app.world_mut();
            // Do NOT register "nonexistent_class".

            let sem = ParametricMemberSemantic {
                element_class: Some("nonexistent_class".into()),
                refinement_state: None,
                parameters: serde_json::Value::Null,
                rationale: None,
            };
            {
                let mut reg = w.resource_mut::<ParametricRegistry>();
                reg.register(annotated_member_type(sem));
            }

            let result = world_create(
                w,
                CreateParametricRequest {
                    type_id: "test.annotated".into(),
                    ..Default::default()
                },
            );
            assert!(result.is_err(), "unknown element_class must return Err");
        }

        // P-SEM-5: serde round-trip — a ParametricMember with semantic serialises
        // and deserialises back to the same value (wire compat).
        #[test]
        fn parametric_member_semantic_serde_round_trip() {
            let member = ParametricMember {
                size: [
                    ScalarExpr::lit(Quantity::mm(100.0)),
                    ScalarExpr::lit(Quantity::mm(200.0)),
                    ScalarExpr::lit(Quantity::mm(50.0)),
                ],
                translate: super::zero_exprs(),
                rotate_euler_deg: super::zero_exprs(),
                profile_xz: Vec::new(),
                label: Some("rt_member".into()),
                semantic: Some(ParametricMemberSemantic {
                    element_class: Some("test_rt_class".into()),
                    refinement_state: Some("Detailed".into()),
                    parameters: serde_json::json!({ "construction_role": "secondary" }),
                    rationale: Some("round-trip test".into()),
                }),
            };

            let json = serde_json::to_string(&member).expect("serialise");
            let back: ParametricMember = serde_json::from_str(&json).expect("deserialise");

            let sem = back.semantic.expect("semantic must survive round-trip");
            assert_eq!(sem.element_class.as_deref(), Some("test_rt_class"));
            assert_eq!(sem.refinement_state.as_deref(), Some("Detailed"));
            assert_eq!(
                sem.parameters.get("construction_role").and_then(|v| v.as_str()),
                Some("secondary")
            );
        }

        // P-SEM-6: a ParametricMember WITHOUT `semantic` field serialises to JSON
        // that omits the `semantic` key entirely (no-annotation capsules stay
        // backward-compatible).
        #[test]
        fn member_without_semantic_serialises_without_key() {
            let member = ParametricMember {
                size: [
                    ScalarExpr::lit(Quantity::mm(100.0)),
                    ScalarExpr::lit(Quantity::mm(200.0)),
                    ScalarExpr::lit(Quantity::mm(50.0)),
                ],
                translate: super::zero_exprs(),
                rotate_euler_deg: super::zero_exprs(),
                profile_xz: Vec::new(),
                label: None,
                semantic: None,
            };

            let json = serde_json::to_string(&member).expect("serialise");
            assert!(
                !json.contains("\"semantic\""),
                "semantic key must be absent when None: {json}"
            );

            // Deserialise the serialised JSON back (without `semantic` key) —
            // must succeed and yield semantic=None. This validates that the
            // skip_serializing_if=Option::is_none + serde(default) pair is
            // correct for backward-compatible round-trips.
            let from_old: ParametricMember =
                serde_json::from_str(&json).expect("must deserialise without semantic key");
            assert!(
                from_old.semantic.is_none(),
                "member without semantic must deserialise with semantic=None"
            );
        }
    }
}
