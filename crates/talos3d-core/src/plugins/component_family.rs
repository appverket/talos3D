//! Reusable component-family schemas and Definition parameter operations.
//!
//! Domain packs supply family assets, templates and parametric types. This
//! module owns the shared parsing, validation, driver mapping and Definition
//! instantiation contract; it contains no bundled domain content.

use std::collections::BTreeMap;

use crate::{
    plugins::{
        modeling::definition::{
            Definition, DefinitionId, DefinitionLibraryId, OverridePolicy, ParamType, ParameterDef,
            ParameterMetadata,
        },
        units::ParameterUnit,
    },
    relational::registry::ParametricRegistry,
};
use bevy::prelude::Resource;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentFamilyAssetFile {
    pub version: u32,
    #[serde(default)]
    pub families: Vec<ComponentFamily>,
}

impl ComponentFamilyAssetFile {
    pub const VERSION: u32 = 1;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentFamily {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub definition_library_id: String,
    pub root_definition_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parametric_type_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub default_spec: Value,
    #[serde(default)]
    pub parameters: Vec<ComponentFamilyParameter>,
}

impl ComponentFamily {
    pub fn definition_library_id(&self) -> DefinitionLibraryId {
        DefinitionLibraryId(self.definition_library_id.clone())
    }

    pub fn root_definition_id(&self) -> DefinitionId {
        DefinitionId(self.root_definition_id.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentFamilyParameter {
    pub name: String,
    pub label: String,
    pub value_type: ComponentFamilyValueType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<ParameterUnit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default = "default_true")]
    pub geometry_affecting: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parametric_driver: Option<ParametricDriverBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentFamilyValueType {
    Number,
    Boolean,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParametricDriverBinding {
    pub driver: String,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero_when_false: Option<String>,
}

#[derive(Debug, Clone, Default, Resource)]
pub struct ComponentFamilyRegistry {
    families: BTreeMap<String, ComponentFamily>,
}

impl ComponentFamilyRegistry {
    pub fn insert(&mut self, family: ComponentFamily) {
        self.families.insert(family.id.clone(), family);
    }

    pub fn get(&self, family_id: &str) -> Option<&ComponentFamily> {
        self.families.get(family_id)
    }

    pub fn list(&self) -> Vec<&ComponentFamily> {
        self.families.values().collect()
    }

    pub fn search(&self, query: &str) -> Vec<&ComponentFamily> {
        let query = query.to_lowercase();
        self.families
            .values()
            .filter(|family| {
                family.id.to_lowercase().contains(&query)
                    || family.title.to_lowercase().contains(&query)
                    || family.summary.to_lowercase().contains(&query)
                    || family
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query))
            })
            .collect()
    }
}

/// Parse one versioned asset file. Bundling and registration belong to the pack.
pub fn parse_component_family_asset(
    name: &str,
    json: &str,
) -> Result<Vec<ComponentFamily>, String> {
    let file: ComponentFamilyAssetFile =
        serde_json::from_str(json).map_err(|error| format!("failed to parse {name}: {error}"))?;
    if file.version != ComponentFamilyAssetFile::VERSION {
        return Err(format!(
            "unsupported ComponentFamilyAssetFile version {} in {name}",
            file.version
        ));
    }
    Ok(file.families)
}

pub fn parse_component_family_spec(
    family: &ComponentFamily,
    parameters: &Value,
) -> Result<Value, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "Component-family commands expect a JSON object".to_string())?;
    let mut spec = family.default_spec.clone();
    if !spec.is_object() {
        spec = Value::Object(Map::new());
    }

    if let Some(input_spec) = object.get("spec") {
        let input_spec = input_spec
            .as_object()
            .ok_or_else(|| "'spec' must be a JSON object".to_string())?;
        let spec_object = spec.as_object_mut().expect("checked object above");
        for (key, value) in input_spec {
            spec_object.insert(key.clone(), value.clone());
        }
    }
    let spec_object = spec.as_object_mut().expect("checked object above");
    for parameter in &family.parameters {
        if let Some(value) = object.get(&parameter.name) {
            spec_object.insert(parameter.name.clone(), value.clone());
        }
    }
    if let Some(name) = object.get("name") {
        spec_object.insert("name".to_string(), name.clone());
    }

    validate_component_family_spec(family, &spec)?;
    Ok(spec)
}

pub fn component_family_spec_from_definition(
    family: &ComponentFamily,
    definition: &Definition,
) -> Result<Value, String> {
    let mut spec = family.default_spec.clone();
    if !spec.is_object() {
        spec = Value::Object(Map::new());
    }
    let spec_object = spec.as_object_mut().expect("object ensured above");
    spec_object.insert("name".to_string(), Value::String(definition.name.clone()));
    for parameter in &family.parameters {
        if let Some(definition_parameter) = definition.interface.parameters.get(&parameter.name) {
            spec_object.insert(
                parameter.name.clone(),
                definition_parameter.default_value.clone(),
            );
        }
    }
    validate_component_family_spec(family, &spec)?;
    Ok(spec)
}

pub fn instantiate_component_family_definition(
    family: &ComponentFamily,
    template: &Definition,
    spec: &Value,
) -> Result<Definition, String> {
    let mut definition = template.clone();
    definition.id = DefinitionId::new();
    definition.base_definition_id = None;
    apply_component_family_spec_to_definition(family, &mut definition, spec)?;
    Ok(definition)
}

pub fn apply_component_family_spec_to_definition(
    family: &ComponentFamily,
    definition: &mut Definition,
    spec: &Value,
) -> Result<(), String> {
    validate_component_family_spec(family, spec)?;
    let spec_object = spec
        .as_object()
        .ok_or_else(|| "'spec' must be a JSON object".to_string())?;
    if let Some(name) = spec_object.get("name").and_then(Value::as_str) {
        definition.name = name.to_string();
    }

    for family_parameter in &family.parameters {
        let Some(value) = spec_object.get(&family_parameter.name) else {
            continue;
        };
        let parameter_index = definition
            .interface
            .parameters
            .0
            .iter()
            .position(|parameter| parameter.name == family_parameter.name);
        let parameter_index = if let Some(index) = parameter_index {
            index
        } else {
            definition
                .interface
                .parameters
                .0
                .push(parameter_def_from_family_param(
                    family_parameter,
                    value.clone(),
                ));
            definition.interface.parameters.0.len() - 1
        };
        let parameter = definition
            .interface
            .parameters
            .0
            .get_mut(parameter_index)
            .expect("parameter index is in range");
        validate_value_type(&family_parameter.value_type, value, &family_parameter.name)?;
        parameter.default_value = value.clone();
        parameter.geometry_affecting = family_parameter.geometry_affecting;
    }
    Ok(())
}

pub fn derive_component_family_parameters(
    registry: &ParametricRegistry,
    family: &ComponentFamily,
    spec: &Value,
) -> Result<Value, String> {
    let Some(parametric_type_id) = family.parametric_type_id.as_deref() else {
        return Ok(Value::Object(Map::new()));
    };
    let parametric = registry
        .get(parametric_type_id)
        .ok_or_else(|| format!("Parametric type '{parametric_type_id}' is not registered"))?;
    let drivers = parametric_driver_values(family, spec)?;
    Ok(Value::Object(
        parametric
            .derive(&drivers)
            .into_iter()
            .collect::<Map<String, Value>>(),
    ))
}

fn parametric_driver_values(
    family: &ComponentFamily,
    spec: &Value,
) -> Result<BTreeMap<String, Value>, String> {
    let spec_object = spec
        .as_object()
        .ok_or_else(|| "'spec' must be a JSON object".to_string())?;
    let mut drivers = BTreeMap::new();
    for parameter in &family.parameters {
        let Some(binding) = &parameter.parametric_driver else {
            continue;
        };
        let mut value = spec_object
            .get(&parameter.name)
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                format!(
                    "Parameter '{}' must be numeric to drive '{}'",
                    parameter.name, binding.driver
                )
            })?;
        if let Some(flag_name) = binding.zero_when_false.as_deref() {
            if !spec_object
                .get(flag_name)
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                value = 0.0;
            }
        }
        drivers.insert(binding.driver.clone(), Value::from(value * binding.scale));
    }
    Ok(drivers)
}

fn validate_component_family_spec(family: &ComponentFamily, spec: &Value) -> Result<(), String> {
    let spec_object = spec
        .as_object()
        .ok_or_else(|| "'spec' must be a JSON object".to_string())?;
    for parameter in &family.parameters {
        let Some(value) = spec_object.get(&parameter.name) else {
            return Err(format!(
                "Missing component-family parameter '{}'",
                parameter.name
            ));
        };
        validate_value_type(&parameter.value_type, value, &parameter.name)?;
        validate_numeric_bound(value, parameter.min.as_ref(), true, &parameter.name)?;
        validate_numeric_bound(value, parameter.max.as_ref(), false, &parameter.name)?;
    }
    Ok(())
}

fn validate_value_type(
    value_type: &ComponentFamilyValueType,
    value: &Value,
    name: &str,
) -> Result<(), String> {
    let valid = match value_type {
        ComponentFamilyValueType::Number => value.as_f64().is_some(),
        ComponentFamilyValueType::Boolean => value.as_bool().is_some(),
        ComponentFamilyValueType::Text => value.as_str().is_some(),
    };
    if valid {
        Ok(())
    } else {
        Err(format!("Parameter '{name}' has the wrong value type"))
    }
}

fn validate_numeric_bound(
    value: &Value,
    bound: Option<&Value>,
    is_minimum: bool,
    name: &str,
) -> Result<(), String> {
    let (Some(value), Some(bound)) = (value.as_f64(), bound.and_then(Value::as_f64)) else {
        return Ok(());
    };
    let violates = if is_minimum {
        value < bound
    } else {
        value > bound
    };
    if violates {
        let relation = if is_minimum { "at least" } else { "at most" };
        Err(format!("Parameter '{name}' must be {relation} {bound}"))
    } else {
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_scale() -> f64 {
    1.0
}

fn parameter_def_from_family_param(
    parameter: &ComponentFamilyParameter,
    default_value: Value,
) -> ParameterDef {
    ParameterDef {
        name: parameter.name.clone(),
        param_type: param_type_from_value_type(&parameter.value_type),
        default_value,
        override_policy: OverridePolicy::Overridable,
        geometry_affecting: parameter.geometry_affecting,
        metadata: ParameterMetadata {
            unit: parameter.unit.clone(),
            min: parameter.min.clone(),
            max: parameter.max.clone(),
            step: parameter.step.clone(),
            category: parameter.category.clone(),
            ..ParameterMetadata::default()
        },
    }
}

fn param_type_from_value_type(value_type: &ComponentFamilyValueType) -> ParamType {
    match value_type {
        ComponentFamilyValueType::Number => ParamType::Numeric,
        ComponentFamilyValueType::Boolean => ParamType::Boolean,
        ComponentFamilyValueType::Text => ParamType::StringVal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn family() -> ComponentFamily {
        serde_json::from_value(json!({
            "id": "mechanical.fixture.adjustable", "title": "Adjustable fixture",
            "definition_library_id": "mechanical.templates", "root_definition_id": "fixture",
            "default_spec": {"length": 100.0, "enabled": true, "finish": "matte"},
            "parameters": [
                {"name": "length", "label": "Length", "value_type": "number",
                 "unit": "mm", "min": 10.0, "max": 500.0,
                 "parametric_driver": {"driver": "travel", "scale": 0.001, "zero_when_false": "enabled"}},
                {"name": "enabled", "label": "Enabled", "value_type": "boolean"},
                {"name": "finish", "label": "Finish", "value_type": "text", "geometry_affecting": false}
            ]
        })).unwrap()
    }

    fn template() -> Definition {
        serde_json::from_value(json!({
            "id": "fixture", "name": "Fixture", "definition_kind": "Solid",
            "definition_version": 1, "interface": {"parameters": []}
        }))
        .unwrap()
    }

    #[test]
    fn versioned_assets_reject_incompatible_versions() {
        let json = json!({"version": 1, "families": [family()]}).to_string();
        assert_eq!(
            parse_component_family_asset("fixture.json", &json).unwrap()[0].id,
            family().id
        );
        let invalid = json!({"version": 2, "families": []}).to_string();
        assert!(parse_component_family_asset("fixture.json", &invalid)
            .unwrap_err()
            .contains("version 2"));
        assert!(parse_component_family_asset("fixture.json", "{")
            .unwrap_err()
            .contains("fixture.json"));
    }

    #[test]
    fn flat_parameters_override_nested_spec_and_defaults() {
        let spec = parse_component_family_spec(
            &family(),
            &json!({
                "spec": {"length": 200.0, "finish": "gloss"}, "length": 300.0, "name": "Fixture A"
            }),
        )
        .unwrap();
        assert_eq!(
            spec,
            json!({"length": 300.0, "finish": "gloss", "enabled": true, "name": "Fixture A"})
        );
        assert!(parse_component_family_spec(&family(), &json!({"length": 9.0})).is_err());
        assert!(parse_component_family_spec(&family(), &json!({"length": 501.0})).is_err());
        assert!(parse_component_family_spec(&family(), &json!({"enabled": "yes"})).is_err());
    }

    #[test]
    fn instances_preserve_typed_metadata_and_leave_template_unchanged() {
        let family = family();
        let template = template();
        let spec =
            parse_component_family_spec(&family, &json!({"length": 250.0, "name": "Fixture A"}))
                .unwrap();
        let instance = instantiate_component_family_definition(&family, &template, &spec).unwrap();
        assert_ne!(instance.id, template.id);
        assert_eq!(template.name, "Fixture");
        assert!(template.interface.parameters.0.is_empty());
        assert_eq!(instance.name, "Fixture A");
        let parameter = instance.interface.parameters.get("length").unwrap();
        assert_eq!(parameter.metadata.unit, family.parameters[0].unit);
        assert_eq!(
            serde_json::to_value(&parameter.metadata.unit).unwrap(),
            json!({"dimension": "length", "unit": "mm"})
        );
        assert!(
            !instance
                .interface
                .parameters
                .get("finish")
                .unwrap()
                .geometry_affecting
        );
        assert_eq!(
            component_family_spec_from_definition(&family, &instance).unwrap(),
            spec
        );
    }

    #[test]
    fn invalid_spec_cannot_partially_edit_a_definition() {
        let mut definition = template();
        let before = serde_json::to_value(&definition).unwrap();
        assert!(apply_component_family_spec_to_definition(
            &family(),
            &mut definition,
            &json!({"name": "Changed", "length": 1.0, "enabled": true, "finish": "matte"})
        )
        .is_err());
        assert_eq!(serde_json::to_value(&definition).unwrap(), before);
    }

    #[test]
    fn driver_mapping_scales_and_honors_boolean_gate() {
        let family = family();
        let spec = parse_component_family_spec(&family, &json!({"length": 250.0})).unwrap();
        assert_eq!(
            parametric_driver_values(&family, &spec).unwrap()["travel"],
            json!(0.25)
        );
        let spec = parse_component_family_spec(&family, &json!({"enabled": false})).unwrap();
        assert_eq!(
            parametric_driver_values(&family, &spec).unwrap()["travel"],
            json!(0.0)
        );
        assert!(
            derive_component_family_parameters(&ParametricRegistry::default(), &family, &spec)
                .unwrap()
                .as_object()
                .unwrap()
                .is_empty()
        );
        let mut driven = family;
        driven.parametric_type_id = Some("unregistered".into());
        assert!(
            derive_component_family_parameters(&ParametricRegistry::default(), &driven, &spec)
                .is_err()
        );
    }
}
