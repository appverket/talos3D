use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!(
            "usage: cargo run -p cadio-dwg --example probe_dwg -- <path-to-file.dwg> [--json] [--write-json]"
        );
        std::process::exit(2);
    };
    let flags = args
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let emit_json = flags.iter().any(|flag| flag == "--json");
    let write_json = flags.iter().any(|flag| flag == "--write-json");

    match cadio_dwg::summarize_file(&path) {
        Ok(summary) => {
            if emit_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary)
                        .expect("summary should serialize to JSON")
                );
            } else {
                println!("path: {}", path.display());
                println!("version: {:?}", summary.probe.version);
                println!("sentinel: {}", summary.probe.sentinel);
                println!("size_bytes: {}", summary.probe.file_size_bytes);
                println!("candidate_layers:");
                for fragment in summary.candidate_layers.iter().take(32) {
                    println!("  {fragment}");
                }
                println!("keyword_hints:");
                for (keyword, count) in &summary.keyword_hint_counts {
                    println!("  {keyword}: {count}");
                }
                println!("text_fragments:");
                for fragment in summary.summary_fragments.iter().take(64) {
                    println!("  {fragment}");
                }
            }
            if write_json {
                let output_path = path.with_extension("dwg.summary.json");
                cadio_dwg::write_summary_json(&path, &output_path)
                    .expect("summary json should be written");
                eprintln!("wrote {}", output_path.display());
            }
        }
        Err(error) => {
            eprintln!("probe failed: {error}");
            std::process::exit(1);
        }
    }
}
