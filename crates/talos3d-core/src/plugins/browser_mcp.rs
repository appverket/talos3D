#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use bevy::{camera::Projection, ecs::world::EntityRef, prelude::*};
use serde_json::{json, Map, Value};

use crate::{
    capability_registry::CapabilityRegistry,
    plugins::{
        camera::OrbitCamera,
        commands::enqueue_create_boxed_entity,
        cursor::{cursor_window_position, CursorWorldPos, DrawingPlane},
        history::apply_pending_history_commands,
        identity::ElementId,
        tools::ActiveTool,
    },
};

pub struct BrowserMcpPlugin;

impl Plugin for BrowserMcpPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(target_arch = "wasm32")]
        {
            app.add_systems(Startup, install_browser_mcp_executor)
                .add_systems(Update, poll_browser_mcp_requests);
        }

        #[cfg(not(target_arch = "wasm32"))]
        let _ = app;
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "browser_session_info",
            "description": "Return status for the attached Talos3D browser runtime session.",
            "inputSchema": object_schema([])
        }),
        json!({
            "name": "list_entity_types",
            "description": "List authored entity types registered by the active Talos3D runtime.",
            "inputSchema": object_schema([])
        }),
        json!({
            "name": "list_entities",
            "description": "List user-facing authored entities in the active Talos3D model.",
            "inputSchema": object_schema([])
        }),
        json!({
            "name": "get_entity",
            "description": "Return the authored snapshot JSON for one model entity.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "element_id": { "type": "integer", "minimum": 1 }
                },
                "required": ["element_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "model_summary",
            "description": "Summarize entity counts, relation counts, bounding box, and capability metrics.",
            "inputSchema": object_schema([])
        }),
        json!({
            "name": "create_entity",
            "description": "Create any registered authored entity from its Talos3D create-request JSON.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "Registered authored entity type, such as box, wall, opening, guide_line, or dimension_line." }
                },
                "required": ["type"],
                "additionalProperties": true
            }
        }),
        json!({
            "name": "create_box",
            "description": "Create a box primitive in the active Talos3D model.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "center": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "centre": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "size": { "type": "array", "items": { "type": "number", "exclusiveMinimum": 0 }, "minItems": 3, "maxItems": 3 },
                    "half_extents": { "type": "array", "items": { "type": "number", "exclusiveMinimum": 0 }, "minItems": 3, "maxItems": 3 },
                    "rotation": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 }
                },
                "additionalProperties": false
            }
        }),
    ]
}

fn object_schema<const N: usize>(_required: [&str; N]) -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

fn list_entities(world: &mut World) -> Value {
    let registry = world.resource::<CapabilityRegistry>();
    let mut entries = Vec::new();
    let mut query = world.try_query::<EntityRef>().expect("EntityRef query");

    for entity_ref in query.iter(world) {
        let Some(snapshot) = registry.capture_user_facing_snapshot(&entity_ref, world) else {
            continue;
        };
        entries.push(json!({
            "element_id": snapshot.element_id().0,
            "entity_type": snapshot.type_name(),
            "label": snapshot.label(),
        }));
    }

    entries.sort_by_key(|entry| {
        entry
            .get("element_id")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    });
    json!(entries)
}

fn list_entity_types(world: &World) -> Value {
    let mut types: Vec<_> = world
        .resource::<CapabilityRegistry>()
        .factories()
        .iter()
        .map(|factory| factory.type_name())
        .collect();
    types.sort_unstable();
    json!(types)
}

fn get_entity(world: &mut World, element_id: u64) -> Result<Value, String> {
    let element_id = ElementId(element_id);
    let registry = world.resource::<CapabilityRegistry>();
    let mut query = world
        .try_query::<EntityRef>()
        .ok_or_else(|| "Failed to create entity query".to_string())?;
    for entity_ref in query.iter(world) {
        if entity_ref.get::<ElementId>().copied() != Some(element_id) {
            continue;
        }
        let Some(snapshot) = registry.capture_snapshot(&entity_ref, world) else {
            return Err(format!("Entity {} is not an authored entity", element_id.0));
        };
        return Ok(snapshot.to_json());
    }
    Err(format!("Entity {} not found", element_id.0))
}

fn model_summary(world: &World) -> Value {
    let summary = world
        .resource::<CapabilityRegistry>()
        .build_model_summary(world);
    json!({
        "entity_counts": summary.entity_counts,
        "assembly_counts": summary.assembly_counts,
        "relation_counts": summary.relation_counts,
        "bounding_box": bounding_box_json(summary.bounding_points),
        "metrics": summary.metrics,
    })
}

fn bounding_box_json(points: Vec<Vec3>) -> Value {
    if points.is_empty() {
        return Value::Null;
    }
    let mut min = points[0];
    let mut max = points[0];
    for point in points.into_iter().skip(1) {
        min = min.min(point);
        max = max.max(point);
    }
    json!({
        "min": [min.x, min.y, min.z],
        "max": [max.x, max.y, max.z],
    })
}

fn create_entity(world: &mut World, request: Value) -> Result<Value, String> {
    let object = request
        .as_object()
        .ok_or_else(|| "create_entity expects a JSON object".to_string())?;
    let entity_type = required_string(object, "type")?.to_ascii_lowercase();
    let registry = world.resource::<CapabilityRegistry>();
    let factory = registry.factory_for(&entity_type).ok_or_else(|| {
        let mut valid_types: Vec<&str> = registry
            .factories()
            .iter()
            .map(|factory| factory.type_name())
            .collect();
        valid_types.sort_unstable();
        format!(
            "Invalid entity type '{entity_type}'. Valid types: {}",
            valid_types.join(", ")
        )
    })?;
    let snapshot = factory.from_create_request(world, &request)?;
    let element_id = snapshot.element_id();
    enqueue_create_boxed_entity(world, snapshot);
    apply_pending_history_commands(world);

    Ok(json!({
        "element_id": element_id.0,
        "entity": get_entity(world, element_id.0)?
    }))
}

fn create_box(world: &mut World, args: Value) -> Result<Value, String> {
    let object = args
        .as_object()
        .ok_or_else(|| "create_box expects a JSON object".to_string())?;
    let center = object
        .get("center")
        .or_else(|| object.get("centre"))
        .cloned()
        .unwrap_or_else(|| json!([0.0, 0.0, 0.0]));

    let half_extents = match (object.get("half_extents"), object.get("size")) {
        (Some(_), Some(_)) => {
            return Err("create_box expects either `size` or `half_extents`, not both".to_string())
        }
        (Some(value), None) => value.clone(),
        (None, Some(value)) => {
            let size = vec3_from_value(value, "size")?;
            json!([size[0] * 0.5, size[1] * 0.5, size[2] * 0.5])
        }
        (None, None) => return Err("create_box requires either `size` or `half_extents`".into()),
    };

    let mut request = Map::new();
    request.insert("type".into(), Value::String("box".into()));
    request.insert("centre".into(), center);
    request.insert("half_extents".into(), half_extents);
    if let Some(rotation) = object.get("rotation") {
        request.insert("rotation".into(), rotation.clone());
    }

    create_entity(world, Value::Object(request))
}

fn browser_session_info(world: &mut World) -> Value {
    let cursor_raw = world
        .get_resource::<CursorWorldPos>()
        .and_then(|cursor| cursor.raw);
    let cursor_snapped = world
        .get_resource::<CursorWorldPos>()
        .and_then(|cursor| cursor.snapped);
    let drawing_plane = world.get_resource::<DrawingPlane>().cloned();
    let active_tool = world
        .get_resource::<State<ActiveTool>>()
        .map(|tool| format!("{:?}", tool.get()));
    let pointer = browser_pointer_info(world);
    let camera = browser_camera_info(world);

    json!({
        "executor_available": true,
        "runtime": "talos3d-browser",
        "url": browser_location_href(),
        "active_tool": active_tool,
        "pointer": pointer,
        "camera": camera,
        "cursor_world": {
            "raw": cursor_raw.map(vec3_json),
            "snapped": cursor_snapped.map(vec3_json),
        },
        "drawing_plane": drawing_plane.as_ref().map(|plane| json!({
            "origin": vec3_json(plane.origin),
            "normal": vec3_json(plane.normal),
            "tangent": vec3_json(plane.tangent),
            "bitangent": vec3_json(plane.bitangent),
        })),
    })
}

fn browser_camera_info(world: &mut World) -> Value {
    let mut camera_query =
        world.query::<(&Camera, &GlobalTransform, &Projection, Option<&OrbitCamera>)>();
    let Some((camera, transform, projection, orbit)) = camera_query
        .iter(world)
        .find(|(camera, _, _, orbit)| camera.is_active && orbit.is_some())
        .or_else(|| camera_query.iter(world).next())
    else {
        return Value::Null;
    };

    json!({
        "viewport": camera.logical_viewport_rect().map(|rect| json!({
            "min": vec2_json(rect.min),
            "max": vec2_json(rect.max),
        })),
        "projection": projection_json(projection),
        "transform": {
            "translation": vec3_json(transform.translation()),
            "forward": vec3_json(Vec3::from(transform.forward())),
            "up": vec3_json(Vec3::from(transform.up())),
        },
        "orbit": orbit.map(|orbit| json!({
            "focus": vec3_json(orbit.focus),
            "radius": orbit.radius,
            "yaw": orbit.yaw,
            "pitch": orbit.pitch,
            "projection_mode": format!("{:?}", orbit.projection_mode),
            "orthographic_scale": orbit.orthographic_scale,
            "focal_length_mm": orbit.focal_length_mm,
        })),
    })
}

fn projection_json(projection: &Projection) -> Value {
    match projection {
        Projection::Perspective(perspective) => json!({
            "type": "perspective",
            "aspect_ratio": perspective.aspect_ratio,
            "fov": perspective.fov,
            "near": perspective.near,
            "far": perspective.far,
        }),
        Projection::Orthographic(orthographic) => json!({
            "type": "orthographic",
            "scale": orthographic.scale,
            "near": orthographic.near,
            "far": orthographic.far,
        }),
        Projection::Custom(_) => json!({
            "type": "custom",
        }),
    }
}

fn browser_pointer_info(world: &mut World) -> Value {
    let window_cursor = {
        let mut window_query = world.query_filtered::<&Window, With<bevy::window::PrimaryWindow>>();
        let Some(window) = window_query.iter(world).next() else {
            return Value::Null;
        };
        let Some(window_cursor) = cursor_window_position(window) else {
            return Value::Null;
        };
        window_cursor
    };

    let mut camera_query = world.query::<(&Camera, Option<&OrbitCamera>)>();
    let viewport_cursor = camera_query
        .iter(world)
        .find(|(camera, orbit)| camera.is_active && orbit.is_some())
        .or_else(|| camera_query.iter(world).next())
        .map(|(camera, _)| match camera.logical_viewport_rect() {
            Some(rect) => window_cursor - rect.min,
            None => window_cursor,
        });

    json!({
        "window": vec2_json(window_cursor),
        "viewport": viewport_cursor.map(vec2_json),
    })
}

fn vec2_json(value: Vec2) -> Value {
    json!([value.x, value.y])
}

fn vec3_json(value: Vec3) -> Value {
    json!([value.x, value.y, value.z])
}

fn dispatch_tool(world: &mut World, tool_name: &str, args: Value) -> Result<Value, String> {
    match tool_name {
        "browser_session_info" => Ok(browser_session_info(world)),
        "list_entity_types" => Ok(list_entity_types(world)),
        "list_entities" => Ok(list_entities(world)),
        "get_entity" => {
            let element_id = args
                .get("element_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| "get_entity requires `element_id`".to_string())?;
            get_entity(world, element_id)
        }
        "model_summary" => Ok(model_summary(world)),
        "create_entity" => create_entity(world, args),
        "create_box" => create_box(world, args),
        other => Err(format!("Unknown Talos3D browser MCP tool: {other}")),
    }
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("Missing required string `{key}`"))
}

fn vec3_from_value(value: &Value, key: &str) -> Result<[f64; 3], String> {
    let Some(items) = value.as_array() else {
        return Err(format!(
            "`{key}` must be an array of three positive numbers"
        ));
    };
    if items.len() != 3 {
        return Err(format!("`{key}` must have exactly three values"));
    }
    let mut out = [0.0; 3];
    for (index, item) in items.iter().enumerate() {
        let number = item
            .as_f64()
            .ok_or_else(|| format!("`{key}` values must be numbers"))?;
        if !number.is_finite() || number <= 0.0 {
            return Err(format!(
                "`{key}` values must be finite and greater than zero"
            ));
        }
        out[index] = number;
    }
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
mod wasm_bridge {
    use super::*;
    use js_sys::{Function, Promise, Reflect};
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};
    use wasm_bindgen::prelude::*;

    pub(super) struct BrowserMcpRequest {
        pub kind: BrowserMcpRequestKind,
        pub resolve: Function,
        pub reject: Function,
    }

    pub(super) enum BrowserMcpRequestKind {
        ListTools,
        CallTool { tool_name: String, arguments: Value },
    }

    thread_local! {
        static REQUESTS: RefCell<VecDeque<BrowserMcpRequest>> = RefCell::new(VecDeque::new());
    }

    pub(super) fn install_executor() {
        let Some(window) = web_sys::window() else {
            return;
        };

        let executor = js_sys::Object::new();
        let list_tools = Closure::<dyn Fn() -> Promise>::wrap(Box::new(|| {
            enqueue_promise(BrowserMcpRequestKind::ListTools)
        }));
        let call_tool = Closure::<dyn Fn(JsValue, JsValue) -> Promise>::wrap(Box::new(
            |tool_name: JsValue, args: JsValue| {
                let tool_name = tool_name.as_string().unwrap_or_default();
                let arguments = match js_value_to_json(args) {
                    Ok(value) => value,
                    Err(error) => {
                        return Promise::reject(&js_error(&error));
                    }
                };
                enqueue_promise(BrowserMcpRequestKind::CallTool {
                    tool_name,
                    arguments,
                })
            },
        ));

        let _ = Reflect::set(
            &executor,
            &JsValue::from_str("listTools"),
            list_tools.as_ref(),
        );
        let _ = Reflect::set(
            &executor,
            &JsValue::from_str("callTool"),
            call_tool.as_ref(),
        );
        let _ = Reflect::set(window.as_ref(), &JsValue::from_str("talos3dMcp"), &executor);

        list_tools.forget();
        call_tool.forget();
    }

    fn enqueue_promise(kind: BrowserMcpRequestKind) -> Promise {
        let shared = Rc::new(RefCell::new(Some(kind)));
        Promise::new(&mut {
            let shared = shared.clone();
            move |resolve, reject| {
                let Some(kind) = shared.borrow_mut().take() else {
                    let _ = reject.call1(&JsValue::NULL, &js_error("MCP request already consumed"));
                    return;
                };
                REQUESTS.with(|requests| {
                    requests.borrow_mut().push_back(BrowserMcpRequest {
                        kind,
                        resolve,
                        reject,
                    });
                });
            }
        })
    }

    pub(super) fn drain_requests() -> Vec<BrowserMcpRequest> {
        REQUESTS.with(|requests| requests.borrow_mut().drain(..).collect())
    }

    pub(super) fn resolve(request: BrowserMcpRequest, result: Result<Value, String>) {
        match result.and_then(json_to_js) {
            Ok(value) => {
                let _ = request.resolve.call1(&JsValue::NULL, &value);
            }
            Err(error) => {
                let _ = request.reject.call1(&JsValue::NULL, &js_error(&error));
            }
        }
    }

    fn js_value_to_json(value: JsValue) -> Result<Value, String> {
        if value.is_null() || value.is_undefined() {
            return Ok(json!({}));
        }
        if let Some(text) = value.as_string() {
            return serde_json::from_str(&text).map_err(|error| error.to_string());
        }
        let text = js_sys::JSON::stringify(&value)
            .map_err(|_| "Failed to stringify JavaScript MCP arguments".to_string())?
            .as_string()
            .ok_or_else(|| "Failed to stringify JavaScript MCP arguments".to_string())?;
        serde_json::from_str(&text).map_err(|error| error.to_string())
    }

    fn json_to_js(value: Value) -> Result<JsValue, String> {
        js_sys::JSON::parse(&value.to_string()).map_err(|_| "Failed to encode MCP result".into())
    }

    fn js_error(message: &str) -> JsValue {
        js_sys::Error::new(message).into()
    }
}

#[cfg(target_arch = "wasm32")]
fn install_browser_mcp_executor() {
    wasm_bridge::install_executor();
}

#[cfg(target_arch = "wasm32")]
fn poll_browser_mcp_requests(world: &mut World) {
    for request in wasm_bridge::drain_requests() {
        let result = match &request.kind {
            wasm_bridge::BrowserMcpRequestKind::ListTools => Ok(json!({
                "tools": tool_definitions(),
                "session": browser_session_info(world),
            })),
            wasm_bridge::BrowserMcpRequestKind::CallTool {
                tool_name,
                arguments,
            } => dispatch_tool(world, tool_name, arguments.clone()),
        };
        wasm_bridge::resolve(request, result);
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_location_href() -> Option<String> {
    web_sys::window().and_then(|window| window.location().href().ok())
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_location_href() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_include_mutating_tools() {
        let names: Vec<_> = tool_definitions()
            .into_iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();

        assert!(names.contains(&"browser_session_info".to_string()));
        assert!(names.contains(&"create_entity".to_string()));
        assert!(names.contains(&"create_box".to_string()));
        assert!(names.contains(&"list_entities".to_string()));
        assert!(names.contains(&"model_summary".to_string()));
    }

    #[test]
    fn create_box_rejects_ambiguous_dimensions() {
        let mut world = World::new();
        let result = create_box(
            &mut world,
            json!({
                "size": [1.0, 1.0, 1.0],
                "half_extents": [0.5, 0.5, 0.5]
            }),
        );

        assert!(result
            .expect_err("ambiguous dimensions should be rejected")
            .contains("either `size` or `half_extents`"));
    }
}
