//! BIM property sets (ADR-026 Phase 6a).
//!
//! `PropertySetSchema` and `PropertySetMap` provide a metadata
//! channel that is **structurally separate** from
//! `ParameterSchema` / `OverrideMap`:
//!
//! - `ParameterSchema` carries Evaluator-driving inputs (width,
//!   height, frame_depth, …). Changes invalidate the mesh cache.
//! - `PropertySetSchema` carries non-geometric authored properties
//!   (fire rating, thermal transmittance, manufacturer, product
//!   code, …). Changes never affect geometry.
//!
//! The hard invariant from ADR-026 §1 — *"a change to
//! `PropertySetMap` must never set `mesh_dirty = true`"* — is
//! enforced architecturally rather than by convention: the
//! property-set state lives in its own Bevy Component
//! (`PropertySetMap`) on the same entity as `OccurrenceIdentity`,
//! and the schema lives in a Bevy Resource
//! (`PropertySetSchemaRegistry`) keyed by `DefinitionId`. The
//! geometry-evaluation pipeline never observes either of them.
//!
//! Mutations emit a `PropertySetChanged` event. The event is
//! intentionally not consumed by `mesh_generation` — only by
//! validation, export, and the ACE inspection surfaces.
//!
//! IFC terminology must not appear in this module: it is
//! domain-neutral. The IFC export pack maps Talos contracts to IFC
//! schemas. See ADR-026 §BIM Mapping Reference for the table.

use std::collections::HashMap;

#[cfg(test)]
use bevy::ecs::message::Messages;
use bevy::ecs::message::Message;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugins::identity::ElementId;
use crate::plugins::modeling::definition::DefinitionId;

// ---------------------------------------------------------------------------
// Property value + type system
// ---------------------------------------------------------------------------

/// Typed value carried in a `PropertySetMap` cell. Kept narrow on
/// purpose — BIM property values are a small typed set, not arbitrary
/// JSON. Use `Json` only for export-pack-specific extensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyValue {
    /// Numeric scalar in the unit declared by the
    /// `PropertyDef::unit` field.
    Number(f64),
    /// Integer-valued property (counts, ratings, codes that are
    /// integer-only).
    Integer(i64),
    /// Boolean property (load-bearing yes/no, fire-rated yes/no, …).
    Boolean(bool),
    /// Free-text property (manufacturer, product code, label, …).
    Text(String),
    /// Enumerated property — must match one of the
    /// `PropertyDef::value_type::Enum` allowed values.
    Enum(String),
    /// Free JSON for extensions; export packs map this to
    /// schema-specific shapes.
    Json(Value),
}

impl PropertyValue {
    /// Discriminator string for debug / error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Number(_) => "number",
            Self::Integer(_) => "integer",
            Self::Boolean(_) => "boolean",
            Self::Text(_) => "text",
            Self::Enum(_) => "enum",
            Self::Json(_) => "json",
        }
    }
}

/// Allowed shape of a property's value, declared by the schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertyValueType {
    Number,
    Integer,
    Boolean,
    Text,
    /// Enum with a closed set of allowed string values.
    Enum { allowed: Vec<String> },
    /// Free JSON; schema-only check is "is JSON".
    Json,
}

impl PropertyValueType {
    /// Validate that a `PropertyValue` matches this type. Returns
    /// `Ok(())` on match; otherwise a human-readable error.
    pub fn validate(&self, value: &PropertyValue) -> Result<(), String> {
        match (self, value) {
            (Self::Number, PropertyValue::Number(_)) => Ok(()),
            (Self::Integer, PropertyValue::Integer(_)) => Ok(()),
            (Self::Boolean, PropertyValue::Boolean(_)) => Ok(()),
            (Self::Text, PropertyValue::Text(_)) => Ok(()),
            (Self::Enum { allowed }, PropertyValue::Enum(s)) => {
                if allowed.iter().any(|a| a == s) {
                    Ok(())
                } else {
                    Err(format!(
                        "enum value '{s}' not in allowed set [{}]",
                        allowed.join(", ")
                    ))
                }
            }
            (Self::Json, PropertyValue::Json(_)) => Ok(()),
            (expected, got) => Err(format!(
                "property type mismatch: schema declares {:?}, value is {}",
                expected,
                got.kind()
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Export profiles
// ---------------------------------------------------------------------------

/// Identifier of a BIM exchange profile this property is required by.
/// Free-form so domain packs can declare their own
/// (`"IFC4"`, `"COBie"`, `"NL/SfB"`, etc.) without core knowing the
/// profile vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ExportProfile(pub String);

impl ExportProfile {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Schema declarations
// ---------------------------------------------------------------------------

/// A single property's declaration inside a `PropertySetSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyDef {
    pub name: String,
    pub value_type: PropertyValueType,
    /// Optional unit string (`"W/(m²·K)"`, `"min"`, `"mm"`, …).
    /// Schema-only metadata; not parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Export profiles that require this property to be set on
    /// every Occurrence using the schema. Used by export-readiness
    /// checks; absence here means the property is optional.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_for: Vec<ExportProfile>,
    /// Free-form description for property-editor UIs and AI agent
    /// prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl PropertyDef {
    pub fn new(name: impl Into<String>, value_type: PropertyValueType) -> Self {
        Self {
            name: name.into(),
            value_type,
            unit: None,
            required_for: Vec::new(),
            description: None,
        }
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn required_for(mut self, profile: ExportProfile) -> Self {
        self.required_for.push(profile);
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// One named property set on a `Definition`.
///
/// Conventionally named after the BIM concept (`"Pset_WallCommon"`,
/// `"Pset_DoorCommon"`, `"COBie.Type"`), but the name is opaque to
/// core; export packs decide how to map it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertySetSchema {
    pub name: String,
    pub properties: Vec<PropertyDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl PropertySetSchema {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            properties: Vec::new(),
            description: None,
        }
    }

    pub fn with_property(mut self, property: PropertyDef) -> Self {
        self.properties.push(property);
        self
    }

    pub fn property(&self, name: &str) -> Option<&PropertyDef> {
        self.properties.iter().find(|p| p.name == name)
    }
}

/// Bevy resource: per-`DefinitionId` map of property-set schemas.
///
/// Definitions register their schemas at plugin build time (or via
/// the upcoming `definition.set_property_set_schemas` MCP tool).
/// Occurrence-level reads consult this registry to validate
/// `PropertySetMap` writes against their Definition's schema.
#[derive(Resource, Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct PropertySetSchemaRegistry {
    pub by_definition: HashMap<DefinitionId, Vec<PropertySetSchema>>,
}

impl PropertySetSchemaRegistry {
    pub fn register(
        &mut self,
        definition_id: DefinitionId,
        schemas: Vec<PropertySetSchema>,
    ) {
        self.by_definition.insert(definition_id, schemas);
    }

    pub fn schemas_for(&self, definition_id: &DefinitionId) -> &[PropertySetSchema] {
        self.by_definition
            .get(definition_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn schema_property(
        &self,
        definition_id: &DefinitionId,
        set_name: &str,
        property_name: &str,
    ) -> Option<&PropertyDef> {
        self.schemas_for(definition_id)
            .iter()
            .find(|s| s.name == set_name)
            .and_then(|s| s.property(property_name))
    }
}

// ---------------------------------------------------------------------------
// Per-occurrence map
// ---------------------------------------------------------------------------

/// Bevy component carrying the per-Occurrence values for the
/// property sets declared by the Definition's schemas.
///
/// Lives on the same entity as `OccurrenceIdentity` but is a
/// separate component so the geometry-evaluation pipeline (which
/// queries for `OccurrenceIdentity` + parameter overrides) does not
/// depend on or react to property-set changes.
#[derive(Component, Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertySetMap {
    /// `set_name → (property_name → PropertyValue)`.
    pub sets: HashMap<String, HashMap<String, PropertyValue>>,
}

impl PropertySetMap {
    /// Read a single property value, if present.
    pub fn get(&self, set_name: &str, property_name: &str) -> Option<&PropertyValue> {
        self.sets.get(set_name).and_then(|s| s.get(property_name))
    }

    /// Write a single property value, returning the previous value
    /// if any. Validation against the Definition's schema is the
    /// caller's responsibility — see [`set_property_validated`].
    pub fn set(
        &mut self,
        set_name: impl Into<String>,
        property_name: impl Into<String>,
        value: PropertyValue,
    ) -> Option<PropertyValue> {
        self.sets
            .entry(set_name.into())
            .or_default()
            .insert(property_name.into(), value)
    }

    /// Remove a single property value, returning the prior value
    /// if any. Empty sets are pruned to keep iteration deterministic.
    pub fn remove(&mut self, set_name: &str, property_name: &str) -> Option<PropertyValue> {
        let prior = self
            .sets
            .get_mut(set_name)
            .and_then(|s| s.remove(property_name));
        if let Some(s) = self.sets.get(set_name) {
            if s.is_empty() {
                self.sets.remove(set_name);
            }
        }
        prior
    }

    /// Iterate over every `(set_name, property_name, value)`
    /// triple. Iteration order is not guaranteed; sort the
    /// collected output if a stable order is needed.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, &PropertyValue)> {
        self.sets
            .iter()
            .flat_map(|(set, props)| props.iter().map(move |(name, v)| (set.as_str(), name.as_str(), v)))
    }

    /// Number of property values across all sets.
    pub fn property_count(&self) -> usize {
        self.sets.values().map(HashMap::len).sum()
    }

    /// Returns the list of `(set_name, property_name)` pairs that
    /// are declared `required_for: [profile]` in the Definition's
    /// schemas but missing from this map. Used by export-readiness
    /// checks; an empty result means the Occurrence is complete for
    /// the named profile.
    pub fn missing_required_for_profile(
        &self,
        schemas: &[PropertySetSchema],
        profile: &ExportProfile,
    ) -> Vec<(String, String)> {
        let mut missing = Vec::new();
        for schema in schemas {
            for prop in &schema.properties {
                if !prop.required_for.iter().any(|p| p == profile) {
                    continue;
                }
                if self.get(&schema.name, &prop.name).is_none() {
                    missing.push((schema.name.clone(), prop.name.clone()));
                }
            }
        }
        missing.sort();
        missing
    }
}

/// Set a property with full validation against the registered
/// schema. Returns the prior value on success, or an error if:
///
/// - The Definition has no schemas registered.
/// - The named set does not exist on the Definition.
/// - The named property does not exist in the set.
/// - The provided value's type does not match the property's
///   declared `value_type`.
pub fn set_property_validated(
    map: &mut PropertySetMap,
    registry: &PropertySetSchemaRegistry,
    definition_id: &DefinitionId,
    set_name: &str,
    property_name: &str,
    value: PropertyValue,
) -> Result<Option<PropertyValue>, String> {
    let prop = registry
        .schema_property(definition_id, set_name, property_name)
        .ok_or_else(|| {
            format!(
                "no property '{property_name}' in set '{set_name}' for definition '{}'",
                definition_id.0
            )
        })?;
    prop.value_type.validate(&value)?;
    Ok(map.set(set_name.to_string(), property_name.to_string(), value))
}

// ---------------------------------------------------------------------------
// Change event
// ---------------------------------------------------------------------------

/// Message emitted on every authored mutation of a `PropertySetMap`.
///
/// Critically: `mesh_generation` does NOT consume this message. Only
/// validation, export, and AI-inspection surfaces do. This is the
/// architectural enforcement of ADR-026's hard invariant that
/// property-set changes do not invalidate geometry.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct PropertySetChanged {
    pub element_id: ElementId,
    pub set_name: String,
    pub property_name: String,
    pub kind: PropertySetChangeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropertySetChangeKind {
    /// Newly authored — no prior value.
    Created,
    /// Updated in place — the prior value is included.
    Updated { prior: PropertyValue },
    /// Removed — the prior value is included.
    Removed { prior: PropertyValue },
}

// ---------------------------------------------------------------------------
// Bevy plugin
// ---------------------------------------------------------------------------

/// Registers the `PropertySetSchemaRegistry` resource and the
/// `PropertySetChanged` event channel.
///
/// No systems are added; property-set mutation is callsite-driven
/// (the `set_property_validated` helper, the upcoming MCP tools, and
/// any UI-side property editor are responsible for emitting
/// `PropertySetChanged` events when they mutate a `PropertySetMap`).
pub struct PropertySetsPlugin;

impl Plugin for PropertySetsPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<PropertySetSchemaRegistry>() {
            app.init_resource::<PropertySetSchemaRegistry>();
        }
        app.add_message::<PropertySetChanged>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fire_rating_def() -> PropertyDef {
        PropertyDef::new("FireRating", PropertyValueType::Text)
            .required_for(ExportProfile::new("IFC4"))
            .with_description("EN-13501-1 fire-resistance rating")
    }

    fn wall_common_schema() -> PropertySetSchema {
        PropertySetSchema::new("Pset_WallCommon")
            .with_property(fire_rating_def())
            .with_property(
                PropertyDef::new("LoadBearing", PropertyValueType::Boolean)
                    .required_for(ExportProfile::new("IFC4")),
            )
            .with_property(
                PropertyDef::new("ThermalTransmittance", PropertyValueType::Number)
                    .with_unit("W/(m²·K)"),
            )
    }

    fn registry_with_wall_schemas() -> (DefinitionId, PropertySetSchemaRegistry) {
        let def = DefinitionId("wall.light_frame_v1".to_string());
        let mut reg = PropertySetSchemaRegistry::default();
        reg.register(def.clone(), vec![wall_common_schema()]);
        (def, reg)
    }

    #[test]
    fn property_value_type_validates_matching_value() {
        assert!(PropertyValueType::Text
            .validate(&PropertyValue::Text("REI60".into()))
            .is_ok());
        assert!(PropertyValueType::Number
            .validate(&PropertyValue::Number(0.18))
            .is_ok());
        assert!(PropertyValueType::Boolean
            .validate(&PropertyValue::Boolean(true))
            .is_ok());
    }

    #[test]
    fn property_value_type_rejects_mismatched_value() {
        let err = PropertyValueType::Number
            .validate(&PropertyValue::Text("nope".into()))
            .unwrap_err();
        assert!(err.contains("type mismatch"));
    }

    #[test]
    fn enum_property_rejects_value_outside_allowed_set() {
        let ty = PropertyValueType::Enum {
            allowed: vec!["A1".into(), "A2".into(), "B".into()],
        };
        assert!(ty.validate(&PropertyValue::Enum("A1".into())).is_ok());
        let err = ty.validate(&PropertyValue::Enum("D".into())).unwrap_err();
        assert!(err.contains("'D'"));
        assert!(err.contains("allowed"));
    }

    #[test]
    fn registry_register_and_lookup() {
        let (def, reg) = registry_with_wall_schemas();
        let schemas = reg.schemas_for(&def);
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "Pset_WallCommon");
    }

    #[test]
    fn registry_schema_property_lookup_finds_declared_property() {
        let (def, reg) = registry_with_wall_schemas();
        let prop = reg
            .schema_property(&def, "Pset_WallCommon", "FireRating")
            .unwrap();
        assert_eq!(prop.name, "FireRating");
    }

    #[test]
    fn registry_schema_property_lookup_returns_none_for_unknown_property() {
        let (def, reg) = registry_with_wall_schemas();
        assert!(reg
            .schema_property(&def, "Pset_WallCommon", "Unknown")
            .is_none());
        assert!(reg
            .schema_property(&def, "Pset_DoesNotExist", "FireRating")
            .is_none());
    }

    #[test]
    fn property_set_map_set_and_get_round_trip() {
        let mut map = PropertySetMap::default();
        let prior = map.set(
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("REI60".into()),
        );
        assert!(prior.is_none());
        let v = map.get("Pset_WallCommon", "FireRating").unwrap();
        assert_eq!(v, &PropertyValue::Text("REI60".into()));
    }

    #[test]
    fn property_set_map_set_returns_prior_on_overwrite() {
        let mut map = PropertySetMap::default();
        map.set(
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("EI30".into()),
        );
        let prior = map.set(
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("REI60".into()),
        );
        assert_eq!(prior, Some(PropertyValue::Text("EI30".into())));
    }

    #[test]
    fn property_set_map_remove_prunes_empty_set() {
        let mut map = PropertySetMap::default();
        map.set(
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("REI60".into()),
        );
        let prior = map.remove("Pset_WallCommon", "FireRating");
        assert!(prior.is_some());
        // Empty set should be pruned.
        assert!(!map.sets.contains_key("Pset_WallCommon"));
    }

    #[test]
    fn property_count_aggregates_across_sets() {
        let mut map = PropertySetMap::default();
        map.set("a", "p1", PropertyValue::Boolean(true));
        map.set("a", "p2", PropertyValue::Number(1.0));
        map.set("b", "p1", PropertyValue::Text("x".into()));
        assert_eq!(map.property_count(), 3);
    }

    #[test]
    fn set_property_validated_accepts_well_typed_value() {
        let (def, reg) = registry_with_wall_schemas();
        let mut map = PropertySetMap::default();
        let prior = set_property_validated(
            &mut map,
            &reg,
            &def,
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("REI60".into()),
        )
        .unwrap();
        assert!(prior.is_none());
        assert_eq!(
            map.get("Pset_WallCommon", "FireRating").unwrap(),
            &PropertyValue::Text("REI60".into())
        );
    }

    #[test]
    fn set_property_validated_rejects_wrong_type() {
        let (def, reg) = registry_with_wall_schemas();
        let mut map = PropertySetMap::default();
        let err = set_property_validated(
            &mut map,
            &reg,
            &def,
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Number(60.0),
        )
        .unwrap_err();
        assert!(err.contains("type mismatch"));
        // Map must not have been mutated.
        assert!(map.sets.is_empty());
    }

    #[test]
    fn set_property_validated_rejects_unknown_property() {
        let (def, reg) = registry_with_wall_schemas();
        let mut map = PropertySetMap::default();
        let err = set_property_validated(
            &mut map,
            &reg,
            &def,
            "Pset_WallCommon",
            "DoesNotExist",
            PropertyValue::Text("x".into()),
        )
        .unwrap_err();
        assert!(err.contains("DoesNotExist"));
    }

    #[test]
    fn missing_required_for_profile_finds_unset_required_properties() {
        let schemas = vec![wall_common_schema()];
        let mut map = PropertySetMap::default();
        // Set only one of the two IFC4-required props.
        map.set(
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("REI60".into()),
        );
        let missing = map.missing_required_for_profile(&schemas, &ExportProfile::new("IFC4"));
        // LoadBearing is required for IFC4 and unset.
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, "Pset_WallCommon");
        assert_eq!(missing[0].1, "LoadBearing");
    }

    #[test]
    fn missing_required_for_profile_empty_when_complete() {
        let schemas = vec![wall_common_schema()];
        let mut map = PropertySetMap::default();
        map.set(
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("REI60".into()),
        );
        map.set(
            "Pset_WallCommon",
            "LoadBearing",
            PropertyValue::Boolean(true),
        );
        let missing = map.missing_required_for_profile(&schemas, &ExportProfile::new("IFC4"));
        assert!(missing.is_empty());
    }

    #[test]
    fn missing_required_for_profile_ignores_other_profiles() {
        let schemas = vec![wall_common_schema()];
        let map = PropertySetMap::default();
        // IFC4 has 2 required; an unrelated profile has 0.
        let missing = map.missing_required_for_profile(&schemas, &ExportProfile::new("COBie"));
        assert!(missing.is_empty());
    }

    #[test]
    fn schema_round_trips_through_json() {
        let schema = wall_common_schema();
        let json = serde_json::to_string(&schema).unwrap();
        let parsed: PropertySetSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, schema);
    }

    #[test]
    fn map_round_trips_through_json() {
        let mut map = PropertySetMap::default();
        map.set(
            "Pset_WallCommon",
            "FireRating",
            PropertyValue::Text("REI60".into()),
        );
        map.set(
            "Pset_WallCommon",
            "LoadBearing",
            PropertyValue::Boolean(true),
        );
        let json = serde_json::to_string(&map).unwrap();
        let parsed: PropertySetMap = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, map);
    }

    #[test]
    fn property_set_change_event_serialises_kind_via_debug() {
        // PropertySetChanged is emitted by mutators; this test
        // confirms the Created / Updated / Removed kinds are
        // distinguishable.
        let created = PropertySetChangeKind::Created;
        let updated = PropertySetChangeKind::Updated {
            prior: PropertyValue::Text("EI30".into()),
        };
        let removed = PropertySetChangeKind::Removed {
            prior: PropertyValue::Boolean(false),
        };
        assert!(matches!(created, PropertySetChangeKind::Created));
        assert!(matches!(updated, PropertySetChangeKind::Updated { .. }));
        assert!(matches!(removed, PropertySetChangeKind::Removed { .. }));
    }

    #[test]
    fn plugin_registers_resource_and_event() {
        let mut app = App::new();
        app.add_plugins(PropertySetsPlugin);
        assert!(app
            .world()
            .contains_resource::<PropertySetSchemaRegistry>());
        // Confirm the message channel was registered.
        assert!(app
            .world()
            .contains_resource::<Messages<PropertySetChanged>>());
    }
}
