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

    // Determine refinement state: use Conceptual as the default for parametric
    // geometry (lowest claim, requires no obligation resolution).
    let refinement_state = RefinementState::Conceptual;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let element_ids: Vec<u64> = members
        .into_iter()
        .enumerate()
        .map(|(member_index, m)| {
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
                    state: refinement_state,
                },
                AuthoringProvenance {
                    mode: AuthoringMode::Freeform,
                    rationale: Some(format!(
                        "parametric type '{type_id}' instance {instance_id} member {member_index}"
                    )),
                },
                claim_grounding,
            ));

            eid.0
        })
        .collect();

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
}
