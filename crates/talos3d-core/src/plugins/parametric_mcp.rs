//! World-side service for the parametric component MCP surface (PP-RPS-7 UX).
//!
//! The transport-neutral operations behind the `parametric.*` MCP tools:
//! list registered types, create an instance, inspect (drivers + derived),
//! set a driver (edit + propagation report), apply a transform gesture
//! (edit-by-gesture or refuse), and explain a dependency trace. These operate on
//! the core `ParametricRegistry` + `ParametricStore` resources; domain crates
//! (architecture) register the actual component types (window, truss).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::relational::graph::NodeId;
use crate::relational::registry::{
    ParametricRegistry, ParametricSnapshot, ParametricStore,
};
use crate::relational::transform::{TransformAxis, TransformGesture, TransformOutcome};

// --- request / response DTOs -----------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ParametricTypeInfo {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct CreateParametricRequest {
    pub type_id: String,
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

pub fn world_create(world: &mut World, req: CreateParametricRequest) -> Result<ParametricSnapshot, String> {
    if world
        .get_resource::<ParametricRegistry>()
        .map(|r| r.get(&req.type_id).is_none())
        .unwrap_or(true)
    {
        return Err(format!("unknown parametric type '{}'", req.type_id));
    }
    let id = {
        let mut store = world.resource_mut::<ParametricStore>();
        store.instantiate(&req.type_id)
    };
    world_inspect(world, InspectParametricRequest { instance_id: id })
}

pub fn world_inspect(world: &mut World, req: InspectParametricRequest) -> Result<ParametricSnapshot, String> {
    let reg = world
        .get_resource::<ParametricRegistry>()
        .cloned()
        .unwrap_or_default();
    let store = world.resource::<ParametricStore>();
    store
        .snapshot(&reg, req.instance_id)
        .ok_or_else(|| format!("unknown parametric instance {}", req.instance_id))
}

pub fn world_set_driver(
    world: &mut World,
    req: SetParametricDriverRequest,
) -> Result<crate::relational::service::PropagationReport, String> {
    let reg = world
        .get_resource::<ParametricRegistry>()
        .cloned()
        .unwrap_or_default();
    let mut store = world.resource_mut::<ParametricStore>();
    store
        .set_driver(&reg, req.instance_id, &req.name, req.value)
        .map_err(|e| format!("{e}"))
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
    use crate::relational::registry::ParametricTypeDef;
    use crate::relational::transform::{TransformAxis, TransformBindings, TransformGesture};
    use std::collections::BTreeMap;
    use serde_json::json;

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
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(ParametricMcpPlugin);
        let mut reg = app.world_mut().resource_mut::<ParametricRegistry>();
        reg.register(box_type());
        app
    }

    #[test]
    fn list_create_inspect_edit_explain_over_world() {
        let mut app = app();
        let w = app.world_mut();
        // list
        let types = world_list_types(w);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].id, "test.box");
        // create
        let snap = world_create(w, CreateParametricRequest { type_id: "test.box".into() }).unwrap();
        let id = snap.instance_id;
        assert_eq!(snap.derived["half"], json!(500.0));
        // edit via set_driver
        let report = world_set_driver(
            w,
            SetParametricDriverRequest { instance_id: id, name: "width".into(), value: json!(1600.0) },
        )
        .unwrap();
        assert!(report.changed_derived.contains_key("half"));
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
        assert_eq!(world_inspect(w, InspectParametricRequest { instance_id: id }).unwrap().derived["half"], json!(1000.0));
        // explain
        let ex = world_explain(w, ExplainParametricRequest { instance_id: id, param: "half".into() }).unwrap();
        assert!(ex.controlling_drivers.contains(&"width".to_string()));
        // unknown type / instance errors
        assert!(world_create(w, CreateParametricRequest { type_id: "nope".into() }).is_err());
        assert!(world_inspect(w, InspectParametricRequest { instance_id: 999 }).is_err());
    }
}
