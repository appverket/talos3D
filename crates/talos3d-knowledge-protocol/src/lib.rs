//! Shared wire contract; no domain facts, engine, transport or release authority.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const SCHEMA: &str = "talos3d.knowledge-asset.v1";
pub const MAX_ASSET_BYTES: usize = 1_048_576;
/// Portable kinds and their path kinds. The last six are registry kinds: the
/// payload is serialisable registry data (descriptors or graph records) that the
/// runtime installs into the registry compiled plugins also fill.
pub const KINDS: [(&str, &str); 11] = [
    ("parametric_type.v1", "parametric"),
    ("definition_library.v1", "definition"),
    ("recipe.v1", "recipe"),
    ("corpus_passage.v1", "passage"),
    ("catalog.v1", "catalog"),
    ("element_class.v1", "element_class"),
    ("generation_prior.v1", "prior"),
    ("assembly_type.v1", "assembly_type"),
    ("relation_type.v1", "relation_type"),
    ("assembly_pattern.v1", "assembly_pattern"),
    ("concept_graph.v1", "concept_graph"),
];
pub fn portable_path_kind(kind: &str) -> Option<&'static str> {
    KINDS
        .iter()
        .find_map(|(k, path)| (*k == kind).then_some(*path))
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = constrain_envelope)]
pub struct Asset {
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub element_classes: Vec<String>,
    pub path_kind: String,
    pub required_tool_ids: Vec<String>,
    pub payload: Value,
    pub applicability: String,
    pub limitations: Vec<String>,
    pub evidence: Vec<String>,
    pub redistribution_rights: String,
    pub contains_private_context: bool,
}

/// Only protocol-owned expectations are returned, never rejected private content.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Violation {
    pub pointer: String,
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct InvalidEnvelope {
    pub error: &'static str,
    pub contract: &'static str,
    pub violations: Vec<Violation>,
}
impl std::fmt::Display for InvalidEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(self).map_err(|_| std::fmt::Error)?
        )
    }
}
impl std::error::Error for InvalidEnvelope {}

impl Asset {
    pub fn validate_envelope(&self) -> Result<(), InvalidEnvelope> {
        let mut violations = Vec::new();
        let mut reject = |pointer: &str, code, expected| {
            violations.push(Violation {
                pointer: pointer.into(),
                code,
                expected,
            })
        };
        if self.schema != SCHEMA {
            reject("/schema", "unsupported_schema", Some(json!(SCHEMA)));
        }
        match portable_path_kind(&self.kind) {
            None => reject(
                "/kind",
                "unsupported_kind",
                Some(json!(KINDS.map(|(kind, _)| kind))),
            ),
            Some(expected) if self.path_kind != expected => {
                reject("/path_kind", "kind_path_mismatch", Some(json!(expected)))
            }
            _ => (),
        }
        if self.contains_private_context {
            reject(
                "/contains_private_context",
                "private_context_forbidden",
                Some(json!(false)),
            );
        }
        if self.id.is_empty()
            || self.id.len() > 160
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
        {
            reject(
                "/id",
                "invalid_identifier",
                Some(json!({"pattern":"^[A-Za-z0-9._/-]{1,160}$"})),
            );
        }
        for (field, value) in [
            ("title", &self.title),
            ("description", &self.description),
            ("applicability", &self.applicability),
            ("redistribution_rights", &self.redistribution_rights),
        ] {
            if value.trim().is_empty() {
                reject(&format!("/{field}"), "nonblank_required", None);
            }
        }
        if self.evidence.is_empty() {
            reject("/evidence", "evidence_required", None);
        }
        for (i, value) in self.evidence.iter().enumerate() {
            if value.trim().is_empty() {
                reject(&format!("/evidence/{i}"), "nonblank_required", None);
            }
        }
        for (field, count) in [
            ("element_classes", self.element_classes.len()),
            ("required_tool_ids", self.required_tool_ids.len()),
        ] {
            if count > 64 {
                reject(
                    &format!("/{field}"),
                    "too_many_entries",
                    Some(json!({"maxItems":64})),
                );
            }
        }
        if !self.payload.is_object() {
            reject("/payload", "object_required", None);
        }
        if self.kind == "recipe.v1"
            && self.payload.pointer("/body/kind").and_then(Value::as_str)
                != Some("authoring_script")
        {
            reject(
                "/payload/body/kind",
                "portable_recipe_body_required",
                Some(json!("authoring_script")),
            );
        }
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_ASSET_BYTES) {
            reject(
                "",
                "asset_too_large",
                Some(json!({"maxBytes":MAX_ASSET_BYTES})),
            );
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(InvalidEnvelope {
                error: "invalid_knowledge_envelope",
                contract: SCHEMA,
                violations,
            })
        }
    }
}

/// The same mapping used by validation, with no handwritten MCP schema copy.
/// Payload decoding, provenance review and execution are separate checks.
pub fn asset_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(Asset)).expect("schema serializes")
}
fn constrain_envelope(schema: &mut schemars::Schema) {
    let schema = schema.as_object_mut().expect("object schema");
    let props = schema
        .get_mut("properties")
        .unwrap()
        .as_object_mut()
        .expect("object properties");
    props.insert("schema".into(), json!({"type":"string","enum":[SCHEMA]}));
    props.insert(
        "kind".into(),
        json!({"type":"string","enum":KINDS.map(|(kind, _)| kind)}),
    );
    props.insert(
        "id".into(),
        json!({"type":"string","pattern":"^[A-Za-z0-9._/-]{1,160}$"}),
    );
    props.insert(
        "contains_private_context".into(),
        json!({"type":"boolean","const":false}),
    );
    props.insert("payload".into(), json!({"type":"object","description":"Runtime-owned payload. Discover get_knowledge_payload_schema(kind), then check_knowledge_candidate. Payload validation does not grant submission, review or release authority."}));
    for field in [
        "title",
        "description",
        "applicability",
        "redistribution_rights",
    ] {
        props.insert(field.into(), json!({"type":"string","pattern":"\\S"}));
    }
    props.insert(
        "evidence".into(),
        json!({"type":"array","minItems":1,"items":{"type":"string","pattern":"\\S"}}),
    );
    for field in ["element_classes", "required_tool_ids"] {
        props.get_mut(field).unwrap()["maxItems"] = json!(64);
    }
    let mut conditions: Vec<Value> = KINDS
        .iter()
        .map(|(kind, path)| {
            json!({
                "if":{"properties":{"kind":{"const":kind}},"required":["kind"]},
                "then":{"properties":{"path_kind":{"const":path}}}
            })
        })
        .collect();
    conditions.push(json!({"if":{"properties":{"kind":{"const":"recipe.v1"}}},
        "then":{"properties":{"payload":{"properties":{"body":{"type":"object","properties":{"kind":{"const":"authoring_script"}},"required":["kind"]}},"required":["body"]}}}}));
    schema.insert("allOf".into(), json!(conditions));
    schema.insert("description".into(), json!("Portable knowledge envelope. Maximum serialized UTF-8 size is 1048576 bytes. Validity is not provenance approval or a release."));
}

/// Content identity lets a client reject incompatible installed protocol surfaces
/// before research or candidate generation. This does not identify domain data.
pub fn contract_manifest() -> Value {
    use sha2::{Digest, Sha256};
    use std::sync::OnceLock;
    static MANIFEST: OnceLock<Value> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        fn sorted(value: Value) -> Value {
            match value {
                Value::Object(fields) => Value::Object(fields.into_iter().collect::<std::collections::BTreeMap<_,_>>()
                    .into_iter().map(|(k,v)| (k,sorted(v))).collect()),
                Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
                other => other,
            }
        }
        let bytes = serde_json::to_vec(&sorted(asset_schema())).expect("schema serializes");
        json!({"schema":SCHEMA,"envelope_schema_sha256":format!("{:x}", Sha256::digest(bytes)),
            "maximum_asset_bytes":MAX_ASSET_BYTES,"kind_paths":KINDS.iter().copied().collect::<std::collections::BTreeMap<_,_>>(),
            "validation_scope":"envelope_only","release_authority":false})
    }).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(kind: &str) -> Value {
        json!({"schema":SCHEMA,"id":"synthetic.test","kind":kind,"title":"Test",
            "description":"Synthetic protocol fixture","element_classes":["test"],
            "path_kind":portable_path_kind(kind).unwrap(),"required_tool_ids":[],
            "payload":if kind=="recipe.v1" { json!({"body":{"kind":"authoring_script","script":{}}}) } else { json!({}) },
            "applicability":"Protocol test only","limitations":[],"evidence":["Synthetic"],
            "redistribution_rights":"Original synthetic fixture","contains_private_context":false})
    }
    fn accepts(value: &Value) -> bool {
        serde_json::from_value::<Asset>(value.clone())
            .is_ok_and(|asset| asset.validate_envelope().is_ok())
    }
    #[test]
    fn generated_schema_and_decoder_agree_on_admission_boundaries() {
        let schema = asset_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for (kind, _) in KINDS {
            let valid = sample(kind);
            assert!(accepts(&valid));
            assert!(validator.is_valid(&valid), "{}", kind);
            for (field, invalid) in [
                ("schema", json!("other")),
                ("kind", json!("native_fn_ref")),
                ("path_kind", json!("recipe_or_executable_asset")),
                ("path_kind", json!("parametric_type")),
                ("id", json!("")),
                ("id", json!("a".repeat(161))),
                ("id", json!("private:identifier")),
                ("id", json!("å")),
                ("title", json!(" \n\t")),
                ("description", json!("")),
                ("applicability", json!("")),
                ("redistribution_rights", json!("")),
                ("evidence", json!([])),
                ("evidence", json!([" "])),
                ("contains_private_context", json!(true)),
                ("payload", json!([])),
                ("element_classes", json!(vec!["x"; 65])),
                ("required_tool_ids", json!(vec!["x"; 65])),
            ] {
                let mut candidate = valid.clone();
                candidate[field] = invalid;
                assert!(!accepts(&candidate), "decoder accepted {kind}/{field}");
                assert!(
                    !validator.is_valid(&candidate),
                    "schema accepted {kind}/{field}"
                );
            }
            let mut extra = valid.clone();
            extra["unknown"] = json!(true);
            assert!(!accepts(&extra));
            assert!(!validator.is_valid(&extra));
            for field in valid.as_object().unwrap().keys() {
                let mut missing = valid.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(!accepts(&missing), "missing {field}");
                assert!(!validator.is_valid(&missing), "schema missing {field}");
            }
        }
    }
    #[test]
    fn registry_kinds_are_portable_and_unknown_kinds_stay_rejected() {
        for (kind, path) in [
            ("element_class.v1", "element_class"),
            ("generation_prior.v1", "prior"),
            ("assembly_type.v1", "assembly_type"),
            ("relation_type.v1", "relation_type"),
            ("assembly_pattern.v1", "assembly_pattern"),
            ("concept_graph.v1", "concept_graph"),
        ] {
            assert_eq!(portable_path_kind(kind), Some(path));
        }
        for kind in ["prior.v1", "native_fn_ref", "constraint.v1", "recipe"] {
            assert_eq!(portable_path_kind(kind), None, "{kind}");
        }
    }
    #[test]
    fn violation_identifies_hidden_mapping_without_echoing_candidate_content() {
        let mut value = sample("parametric_type.v1");
        value["path_kind"] = json!("recipe_or_executable_asset");
        value["description"] = json!("PRIVATE_SENTINEL");
        let error = serde_json::from_value::<Asset>(value)
            .unwrap()
            .validate_envelope()
            .unwrap_err();
        assert_eq!(
            error.violations,
            vec![Violation {
                pointer: "/path_kind".into(),
                code: "kind_path_mismatch",
                expected: Some(json!("parametric"))
            }]
        );
        assert!(!error.to_string().contains("PRIVATE_SENTINEL"));
    }
    #[test]
    fn nested_tool_schemas_also_use_the_envelope_contract() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Request {
            asset: Asset,
        }
        let schema = serde_json::to_value(schemars::schema_for!(Request)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let mut value = sample("parametric_type.v1");
        assert!(validator.is_valid(&json!({"asset":value.clone()})));
        value["path_kind"] = json!("parametric_type");
        assert!(!validator.is_valid(&json!({"asset":value})));
    }
    #[test]
    fn portable_body_and_serialized_size_are_enforced_without_execution() {
        let mut value = sample("recipe.v1");
        value["payload"]["body"]["kind"] = json!("native_fn_ref");
        assert!(!accepts(&value));
        assert!(!jsonschema::validator_for(&asset_schema())
            .unwrap()
            .is_valid(&value));
        let mut asset: Asset = serde_json::from_value(sample("catalog.v1")).unwrap();
        asset.payload = json!({"synthetic": "x".repeat(MAX_ASSET_BYTES)});
        assert!(asset
            .validate_envelope()
            .unwrap_err()
            .violations
            .iter()
            .any(|v| v.code == "asset_too_large"));
    }
}
