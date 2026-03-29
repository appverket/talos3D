//! `compare_oracle` compares native DWG parse output against a stored oracle summary.
//!
//! Usage:
//!   cargo run -p cadio-dwg --example compare_oracle -- <dwg-path> [oracle.json]
//!
//! If `oracle.json` is omitted it defaults to
//! `tests/fixtures/cadio/survey_oracle.json`
//! relative to the workspace root.
//!
//! Exit code 0 = native output matches oracle.
//! Exit code 1 = divergence detected; a diff is printed.
//! Exit code 2 = oracle file not found; native summary written to that path.

use std::path::PathBuf;

use cadio_dwg::read_document;
use cadio_ir::ImportSummary;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let dwg_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: compare_oracle <dwg-path> [oracle.json]");

    let default_oracle = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/cadio/survey_oracle.json");
    let oracle_path = args.next().map(PathBuf::from).unwrap_or(default_oracle);

    eprintln!("reading native DWG: {}", dwg_path.display());
    let document = match read_document(&dwg_path) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("native read failed: {e}");
            std::process::exit(1);
        }
    };
    let native_summary = ImportSummary::from_document(&document);

    eprintln!(
        "native: {} layers, {} blocks, {} model entities",
        native_summary.layers.len(),
        native_summary.blocks.len(),
        native_summary.model_entity_counts.total(),
    );
    if let Some(bounds) = &native_summary.model_bounds {
        eprintln!(
            "native bounds: x=[{:.1}, {:.1}] y=[{:.1}, {:.1}] z=[{:.4}, {:.4}]",
            bounds.min.x, bounds.max.x, bounds.min.y, bounds.max.y, bounds.min.z, bounds.max.z,
        );
    }

    if !oracle_path.exists() {
        let json = serde_json::to_string_pretty(&native_summary).expect("serialisation failed");
        std::fs::create_dir_all(oracle_path.parent().unwrap()).ok();
        std::fs::write(&oracle_path, &json).expect("failed to write oracle");
        eprintln!(
            "oracle not found — wrote native summary as new oracle to {}",
            oracle_path.display()
        );
        eprintln!("Re-run after generating a proper ODA oracle to compare.");
        std::process::exit(2);
    }

    let oracle_json = std::fs::read_to_string(&oracle_path).expect("failed to read oracle file");
    let oracle: ImportSummary =
        serde_json::from_str(&oracle_json).expect("failed to parse oracle JSON");

    eprintln!(
        "oracle: {} layers, {} blocks, {} model entities",
        oracle.layers.len(),
        oracle.blocks.len(),
        oracle.model_entity_counts.total(),
    );

    let diff = oracle.diff_report(&native_summary);
    if diff.is_empty() {
        println!("PASS — native output matches oracle thresholds.");
        std::process::exit(0);
    } else {
        println!("FAIL — divergence detected:\n{diff}");
        std::process::exit(1);
    }
}
