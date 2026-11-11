//! Lossless offline conversion and comparison; never overwrites an existing file.
//! cargo run --release -p talos3d-core --example project_storage -- compact IN OUT
//! cargo run --release -p talos3d-core --example project_storage -- legacy IN OUT
//! cargo run --release -p talos3d-core --example project_storage -- verify A B

use serde_json::Value;
use std::{fs, io::Write, time::Instant};
use talos3d_core::plugins::project_codec;

fn digest(value: &Value) -> Result<blake3::Hash, Box<dyn std::error::Error>> {
    struct HashWriter(blake3::Hasher);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(blake3::Hasher::new());
    rmp_serde::encode::write_named(&mut writer, value)?;
    Ok(writer.0.finalize())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 || !matches!(args[1].as_str(), "compact" | "legacy" | "verify") {
        return Err("Usage: project_storage compact|legacy|verify INPUT OUTPUT".into());
    }
    let started = Instant::now();
    let bytes = fs::read(&args[2])?;
    let input_bytes = bytes.len();
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let decode_started = Instant::now();
    let mut value: Value = project_codec::decode(&bytes)?;
    drop(bytes);
    value.sort_all_objects();
    let decode_ms = decode_started.elapsed().as_secs_f64() * 1000.0;
    let expected = digest(&value)?;
    if args[1] == "verify" {
        drop(value);
        let other_started = Instant::now();
        let other_bytes = fs::read(&args[3])?;
        let output_bytes = other_bytes.len();
        let other_read_ms = other_started.elapsed().as_secs_f64() * 1000.0;
        let decoded_started = Instant::now();
        let mut other: Value = project_codec::decode(&other_bytes)?;
        drop(other_bytes);
        other.sort_all_objects();
        let other_decode_ms = decoded_started.elapsed().as_secs_f64() * 1000.0;
        if digest(&other)? != expected {
            return Err("Logical documents differ".into());
        }
        println!(
            "{}",
            serde_json::json!({"verified":true,"input_bytes":input_bytes,"output_bytes":output_bytes,"read_ms":read_ms,"decode_ms":decode_ms,"other_read_ms":other_read_ms,"other_decode_ms":other_decode_ms})
        );
        return Ok(());
    }
    let encoding_started = Instant::now();
    let (output, statistics) = if args[1] == "compact" {
        let (bytes, stats) = project_codec::encode_value(value)?;
        (bytes, Some(stats))
    } else {
        (serde_json::to_vec(&value)?, None)
    };
    let encode_ms = encoding_started.elapsed().as_secs_f64() * 1000.0;
    let mut verified: Value = project_codec::decode(&output)?;
    verified.sort_all_objects();
    if digest(&verified)? != expected {
        return Err("Conversion failed lossless verification".into());
    }
    // A create-new write ensures that neither source nor an existing result can
    // be destroyed by an accidental path choice. Verify before publishing bytes.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[3])?;
    file.write_all(&output)?;
    file.sync_all()?;
    println!(
        "{}",
        serde_json::json!({"verified":true,"input_bytes":input_bytes,"output_bytes":output.len(),"read_ms":read_ms,"decode_ms":decode_ms,"encode_ms":encode_ms,"statistics":statistics})
    );
    Ok(())
}
