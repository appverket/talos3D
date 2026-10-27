//! Lossless storage envelope for native projects. Authored schemas stay unchanged.
//!
//! Legacy files are JSON objects. New files are a JSON tuple containing a versioned,
//! compressed MessagePack payload. Keeping the envelope UTF-8 preserves the existing
//! browser/shell transport. Numeric buffers are interned only on disk; decoding
//! reconstructs independent values, never aliases or merges authored entities.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{bufread::GzDecoder, write::GzEncoder, Compression};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

const MAGIC: &str = "TALOS3D";
const VERSION: u32 = 1;
const CODEC: &str = "msgpack-gzip-buffers";
// An explicit limit, checked before allocation, rather than trusting an advertised
// decompressed length. The writer uses the same bound and never truncates a model.
const MAX_PACKED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MIN_BUFFER_ITEMS: usize = 64;

#[derive(Serialize, Deserialize)]
struct PackedProject {
    root: Value,
    buffers: Vec<Value>,
    references: Vec<BufferReference>,
}

#[derive(Serialize, Deserialize)]
struct BufferReference {
    pointer: String,
    buffer: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct StorageStatistics {
    pub buffer_references: usize,
    pub unique_buffers: usize,
    pub packed_bytes: usize,
    pub file_bytes: usize,
}

/// Encode a complete authored document, including opaque extension values.
pub fn encode<T: Serialize>(project: &T) -> Result<Vec<u8>, String> {
    encode_value(serde_json::to_value(project).map_err(|e| e.to_string())?).map(|v| v.0)
}

pub fn encode_value(mut root: Value) -> Result<(Vec<u8>, StorageStatistics), String> {
    if !root.is_object() {
        return Err("A native project must be a JSON object".into());
    }
    root.sort_all_objects();
    let mut packed = PackedProject {
        root,
        buffers: Vec::new(),
        references: Vec::new(),
    };
    intern_buffers(
        &mut packed.root,
        &mut String::new(),
        &mut packed.buffers,
        &mut packed.references,
        &mut HashMap::new(),
    )?;
    let payload = rmp_serde::to_vec_named(&packed).map_err(|e| e.to_string())?;
    if payload.len() as u64 > MAX_PACKED_BYTES {
        return Err("Project exceeds the compact format's 8 GiB unpacked limit".into());
    }
    buffer_uses(&packed, payload.len())?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&payload).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&(
        MAGIC,
        VERSION,
        CODEC,
        payload.len(),
        STANDARD.encode(compressed),
    ))
    .map_err(|e| e.to_string())?;
    let statistics = StorageStatistics {
        buffer_references: packed.references.len(),
        unique_buffers: packed.buffers.len(),
        packed_bytes: payload.len(),
        file_bytes: bytes.len(),
    };
    Ok((bytes, statistics))
}

fn is_numeric_buffer(value: &Value) -> bool {
    let Value::Array(items) = value else {
        return false;
    };
    items.len() >= MIN_BUFFER_ITEMS
        && items.iter().all(|item| match item {
            Value::Number(_) => true,
            Value::Array(row) => {
                !row.is_empty() && row.len() <= 16 && row.iter().all(Value::is_number)
            }
            _ => false,
        })
}

fn intern_buffers(
    value: &mut Value,
    pointer: &mut String,
    buffers: &mut Vec<Value>,
    references: &mut Vec<BufferReference>,
    hashes: &mut HashMap<blake3::Hash, Vec<usize>>,
) -> Result<(), String> {
    if is_numeric_buffer(value) {
        let bytes = rmp_serde::to_vec(value).map_err(|e| e.to_string())?;
        let hash = blake3::hash(&bytes);
        let candidates = hashes.entry(hash).or_default();
        // Verify exact encoded bytes after hashing. This also distinguishes -0.0
        // from +0.0 and integers from floats; no rounding or quantization occurs.
        let mut matching = None;
        for &index in candidates.iter() {
            if rmp_serde::to_vec(&buffers[index]).map_err(|e| e.to_string())? == bytes {
                matching = Some(index);
                break;
            }
        }
        let buffer = matching.unwrap_or_else(|| {
            let index = buffers.len();
            buffers.push(value.take());
            candidates.push(index);
            index
        });
        *value = Value::Null;
        references.push(BufferReference {
            pointer: pointer.clone(),
            buffer,
        });
        return Ok(());
    }
    let prefix = pointer.len();
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                pointer.push('/');
                pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                intern_buffers(child, pointer, buffers, references, hashes)?;
                pointer.truncate(prefix);
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter_mut().enumerate() {
                pointer.push('/');
                pointer.push_str(&index.to_string());
                intern_buffers(child, pointer, buffers, references, hashes)?;
                pointer.truncate(prefix);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Accept both existing object-form JSON and compact envelopes. Decode and validate
/// completely before a caller mutates its world, including compressed checksums,
/// trailing bytes, reference locations and format versions.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) != Some(b'[') {
        return serde_json::from_slice(bytes).map_err(|e| e.to_string());
    }
    let (magic, version, codec, length, encoded): (&str, u32, &str, u64, &str) =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid project envelope: {e}"))?;
    if magic != MAGIC || version != VERSION || codec != CODEC {
        return Err(format!(
            "Unsupported project container: {magic} v{version} ({codec})"
        ));
    }
    if length > MAX_PACKED_BYTES.min(usize::MAX as u64) {
        return Err("Project exceeds the compact format's 8 GiB unpacked limit".into());
    }
    let compressed = STANDARD
        .decode(encoded)
        .map_err(|e| format!("Invalid project base64: {e}"))?;
    let mut decoder = GzDecoder::new(Cursor::new(&compressed));
    let mut payload = Vec::new();
    // Grow from actual input, not from an untrusted advertised length.
    decoder
        .by_ref()
        .take(length + 1)
        .read_to_end(&mut payload)
        .map_err(|e| format!("Invalid compressed project: {e}"))?;
    if payload.len() as u64 != length {
        return Err("Project unpacked length does not match its header".into());
    }
    if decoder.get_ref().position() != compressed.len() as u64 {
        return Err("Trailing data after compressed project".into());
    }
    let mut deserializer = rmp_serde::Deserializer::new(Cursor::new(&payload));
    let mut packed = PackedProject::deserialize(&mut deserializer)
        .map_err(|e| format!("Invalid project payload: {e}"))?;
    if deserializer.get_ref().position() != length {
        return Err("Trailing data after project payload".into());
    }
    drop(payload);
    let mut uses = buffer_uses(&packed, length as usize)?;
    for reference in packed.references {
        let target = packed
            .root
            .pointer_mut(&reference.pointer)
            .ok_or("Project buffer reference points to a missing location")?;
        if !target.is_null() {
            return Err("Project buffer reference would overwrite authored data".into());
        }
        uses[reference.buffer] -= 1;
        *target = if uses[reference.buffer] == 0 {
            packed.buffers[reference.buffer].take()
        } else {
            packed.buffers[reference.buffer].clone()
        };
    }
    if !packed.root.is_object() {
        return Err("A native project must be a JSON object".into());
    }
    serde_json::from_value(packed.root).map_err(|e| e.to_string())
}

fn buffer_uses(packed: &PackedProject, payload_bytes: usize) -> Result<Vec<usize>, String> {
    let mut uses = vec![0usize; packed.buffers.len()];
    for reference in &packed.references {
        let count = uses
            .get_mut(reference.buffer)
            .ok_or("Project contains an invalid buffer reference")?;
        *count += 1;
    }
    let mut expanded = payload_bytes as u64;
    for (buffer, count) in packed.buffers.iter().zip(&uses) {
        if *count == 0 || !is_numeric_buffer(buffer) {
            return Err("Project contains an unused or invalid numeric buffer".into());
        }
        // Bound reference expansion as well as gzip expansion. Nine bytes per
        // number and five per array header conservatively bound MessagePack size.
        let items = buffer.as_array().unwrap();
        let size = 5 + items
            .iter()
            .map(|item| item.as_array().map_or(9, |row| 5 + 9 * row.len() as u64))
            .sum::<u64>();
        expanded = expanded.saturating_add(size.saturating_mul((count - 1) as u64));
        if expanded > MAX_PACKED_BYTES.min(usize::MAX as u64) {
            return Err("Project buffer expansion exceeds this platform's format limit".into());
        }
    }
    Ok(uses)
}

/// Explicit compatibility export for older applications and JSON inspection tools.
/// This operates on the generic document so unknown plugin fields survive too.
pub fn to_legacy_json(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut value: Value = decode(bytes)?;
    value.sort_all_objects();
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        let vertices: Vec<_> = (0..256)
            .map(|i| json!([i, 0.12345678901234567, -0.0]))
            .collect();
        json!({"version":3,"definitions":{"mesh":vertices},"entities":[{"mesh":vertices}],
            "plugin~/future":{"matrix":vertices,"text":"å漢字","null":null},
            "bounds":[u64::MAX, i64::MIN, 1.0, -0.0], "empty":[],"boolean":true})
    }

    #[test]
    fn exact_round_trip_interns_buffers_and_preserves_opaque_values() {
        let value = fixture();
        let (bytes, stats) = encode_value(value.clone()).unwrap();
        assert_eq!(stats.buffer_references, 3);
        assert_eq!(stats.unique_buffers, 1);
        let decoded: Value = decode(&bytes).unwrap();
        // Encoded equality proves numeric types and signed zero, beyond Value's ==.
        assert_eq!(
            rmp_serde::to_vec(&decoded).unwrap(),
            rmp_serde::to_vec(&value).unwrap()
        );
        assert_eq!(encode(&decoded).unwrap(), bytes);
        assert!(bytes.len() < serde_json::to_vec(&value).unwrap().len() / 2);
        assert_eq!(
            decode::<Value>(&to_legacy_json(&bytes).unwrap()).unwrap(),
            value
        );
    }

    #[test]
    fn legacy_json_objects_and_reserved_looking_extensions_remain_legacy() {
        let value = json!({"TALOS3D":1,"talos3d_container":42,"version":3,"entities":[]});
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        assert_eq!(decode::<Value>(&bytes).unwrap(), value);
    }

    #[test]
    fn independent_buffer_instances_are_restored_without_aliases() {
        let mut decoded: Value = decode(&encode(&fixture()).unwrap()).unwrap();
        decoded["entities"][0]["mesh"][0][0] = json!(999);
        assert_eq!(decoded["definitions"]["mesh"][0][0], json!(0));
    }

    fn envelope(packed: &PackedProject, trailing: &[u8]) -> Vec<u8> {
        let mut payload = rmp_serde::to_vec_named(packed).unwrap();
        payload.extend_from_slice(trailing);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&payload).unwrap();
        serde_json::to_vec(&(
            MAGIC,
            VERSION,
            CODEC,
            payload.len(),
            STANDARD.encode(encoder.finish().unwrap()),
        ))
        .unwrap()
    }

    #[test]
    fn rejects_invalid_references_overwrites_and_trailing_payloads() {
        let buffer = json!(vec![1.25; 64]);
        let mut packed = PackedProject {
            root: json!({"mesh":null}),
            buffers: vec![buffer],
            references: vec![BufferReference {
                pointer: "/mesh".into(),
                buffer: 0,
            }],
        };
        assert!(decode::<Value>(&envelope(&packed, &[0]))
            .unwrap_err()
            .contains("Trailing"));
        packed.references[0].buffer = 1;
        assert!(decode::<Value>(&envelope(&packed, &[]))
            .unwrap_err()
            .contains("invalid buffer reference"));
        packed.references[0].buffer = 0;
        packed.references[0].pointer = "/missing".into();
        assert!(decode::<Value>(&envelope(&packed, &[]))
            .unwrap_err()
            .contains("missing"));
        packed.references[0].pointer = "/mesh".into();
        packed.root["mesh"] = json!("authored value");
        assert!(decode::<Value>(&envelope(&packed, &[]))
            .unwrap_err()
            .contains("overwrite"));
        packed.root["mesh"] = Value::Null;
        packed.references.push(BufferReference {
            pointer: "/mesh".into(),
            buffer: 0,
        });
        assert!(decode::<Value>(&envelope(&packed, &[]))
            .unwrap_err()
            .contains("overwrite"));
        assert!(buffer_uses(&packed, MAX_PACKED_BYTES as usize).is_err());
    }

    #[test]
    fn rejects_corruption_truncation_versions_and_false_lengths() {
        let bytes = encode(&fixture()).unwrap();
        for end in [0, 1, bytes.len() / 2, bytes.len() - 1] {
            assert!(decode::<Value>(&bytes[..end]).is_err());
        }
        let envelope: Value = serde_json::from_slice(&bytes).unwrap();
        for (field, value) in [
            (1, json!(99)),
            (2, json!("unknown")),
            (3, json!(1)),
            (3, json!(MAX_PACKED_BYTES + 1)),
            (4, json!("bad!")),
        ] {
            let mut changed = envelope.clone();
            changed[field] = value;
            assert!(decode::<Value>(&serde_json::to_vec(&changed).unwrap()).is_err());
        }
        let mut corrupted = envelope.clone();
        let mut compressed = STANDARD.decode(envelope[4].as_str().unwrap()).unwrap();
        let last = compressed.len() - 5;
        compressed[last] ^= 1;
        corrupted[4] = json!(STANDARD.encode(compressed));
        assert!(decode::<Value>(&serde_json::to_vec(&corrupted).unwrap()).is_err());
    }
}
