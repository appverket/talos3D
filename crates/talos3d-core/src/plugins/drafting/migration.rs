//! Legacy-to-drafting migration.
//!
//! When a project is loaded, if it contains legacy `dimension_annotations`
//! (from the old `dimension_line.rs` plugin) but no `drafting_annotations`,
//! this module synthesises a `drafting_annotations` payload so the new
//! plugin's sync system picks up the dims on the next tick and creates the
//! corresponding entities. The legacy key is NOT removed — the old plugin
//! continues to operate on it and the two remain in sync through their
//! respective sync systems.
//!
//! Run once on project load (typically via a Startup system or after the
//! persistence plugin restores document metadata).

use bevy::prelude::*;
use serde_json::{json, Value};

use crate::plugins::{
    dimension_line::DIMENSION_ANNOTATIONS_KEY,
    document_properties::DocumentProperties,
    identity::ElementIdAllocator,
};

use super::plugin::DRAFTING_ANNOTATIONS_KEY;

/// One-shot migration: populate `drafting_annotations` from any legacy
/// `dimension_annotations` the document might carry. Idempotent: does nothing
/// if a `drafting_annotations` entry already exists.
pub fn migrate_legacy_dimensions(
    mut props: ResMut<DocumentProperties>,
    allocator: Res<ElementIdAllocator>,
) {
    if props.domain_defaults.contains_key(DRAFTING_ANNOTATIONS_KEY) {
        return;
    }
    let Some(legacy) = props.domain_defaults.get(DIMENSION_ANNOTATIONS_KEY).cloned() else {
        return;
    };
    let Some(new_list) = translate_legacy_list(&legacy, &allocator) else {
        return;
    };
    props
        .domain_defaults
        .insert(DRAFTING_ANNOTATIONS_KEY.to_string(), new_list);
}

fn translate_legacy_list(legacy: &Value, allocator: &ElementIdAllocator) -> Option<Value> {
    let arr = legacy.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(converted) = translate_legacy_one(item, allocator) {
            out.push(converted);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

fn translate_legacy_one(legacy: &Value, allocator: &ElementIdAllocator) -> Option<Value> {
    let obj = legacy.as_object()?;
    let start = obj.get("start")?.clone();
    let end = obj.get("end")?.clone();
    // Legacy `offset` is a scalar distance; our new model uses Vec3.
    // Convert to a +y offset of the requested magnitude as a best guess.
    let offset_scalar = obj
        .get("offset")
        .and_then(Value::as_f64)
        .unwrap_or(0.5) as f32;

    // Use a fresh ID rather than colliding with the legacy one. Both keys
    // will continue to coexist and both sync systems will drive their own
    // entities — users can delete the old ones when ready.
    let element_id = allocator.next_id();

    Some(json!({
        "element_id": element_id,
        "kind": { "Linear": { "direction": [1.0, 0.0, 0.0] } },
        "a": start,
        "b": end,
        "offset": [0.0, offset_scalar, 0.0],
        "style_name": "architectural_metric",
        "text_override": obj.get("label").cloned().and_then(|v| v.as_str().map(String::from)),
        "visible": obj.get("visible").and_then(Value::as_bool).unwrap_or(true),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::*;

    #[test]
    fn translates_empty_to_none() {
        let allocator = ElementIdAllocator::default();
        assert!(translate_legacy_list(&json!([]), &allocator).is_none());
    }

    #[test]
    fn translates_single_legacy_dim() {
        let allocator = ElementIdAllocator::default();
        let legacy = json!([{
            "element_id": 7,
            "start": [0.0, 0.0, 0.0],
            "end": [4.572, 0.0, 0.0],
            "offset": 0.635,
            "extension": 0.15,
            "visible": true,
            "label": null,
            "display_unit": null,
            "precision": null,
            "length": 4.572,
            "line_point": [2.286, 0.635, 0.0]
        }]);
        let result = translate_legacy_list(&legacy, &allocator).expect("migrated");
        let arr = result.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        let first = &arr[0];
        assert_eq!(first["style_name"], "architectural_metric");
        assert_eq!(first["kind"]["Linear"]["direction"], json!([1.0, 0.0, 0.0]));
    }

    #[test]
    fn idempotent_when_new_key_already_present() {
        let mut app = App::new();
        app.init_resource::<DocumentProperties>()
            .init_resource::<ElementIdAllocator>();
        {
            let mut props = app.world_mut().resource_mut::<DocumentProperties>();
            props.domain_defaults.insert(
                DIMENSION_ANNOTATIONS_KEY.to_string(),
                json!([{"element_id": 1, "start": [0,0,0], "end": [1,0,0], "offset": 0.1}]),
            );
            props.domain_defaults.insert(
                DRAFTING_ANNOTATIONS_KEY.to_string(),
                json!([{"already": "here"}]),
            );
        }
        app.add_systems(Startup, migrate_legacy_dimensions);
        app.update();
        let props = app.world().resource::<DocumentProperties>();
        assert_eq!(
            props.domain_defaults.get(DRAFTING_ANNOTATIONS_KEY),
            Some(&json!([{"already": "here"}])),
            "should not overwrite existing drafting_annotations"
        );
    }
}
