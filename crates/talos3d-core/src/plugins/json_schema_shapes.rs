//! Explicit JSON-Schema shapes for MCP request fields stored as `serde_json::Value`.
//!
//! schemars renders a bare `Value` as a type-less schema. MCP clients then have no
//! declared shape and may serialize an object argument as a JSON string, which the
//! handler correctly rejects. Every served request field holding a `Value` must
//! declare its accepted shape with one of these helpers via
//! `#[schemars(schema_with = "...")]`; `served_tool_parameters_declare_a_type`
//! enforces this for the whole advertised catalog.

use schemars::{json_schema, Schema, SchemaGenerator};

/// A JSON object whose keys are defined by the tool's own contract.
pub fn object(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object" })
}

/// Optional JSON object (absent or `null` keeps the tool default).
pub fn optional_object(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": ["object", "null"] })
}

/// A JSON array of objects.
pub fn object_array(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "array", "items": { "type": "object" } })
}

/// Any JSON value; the tool validates it against a runtime-declared type.
pub fn any_value(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": ["string", "number", "integer", "boolean", "object", "array", "null"] })
}
