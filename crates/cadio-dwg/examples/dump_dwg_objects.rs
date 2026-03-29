use std::path::PathBuf;

use cadio_dwg::{read_object_index, read_section_data};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: dump_dwg_objects <path-to-dwg> [handle-decimal]");
    let filter = args
        .next()
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<u64>().ok());

    let objects = read_section_data(&path, "Objects").expect("failed to read Objects section");
    for record in read_object_index(&path).expect("failed to read DWG object index") {
        if filter.is_some_and(|filter| record.handle != filter) {
            continue;
        }
        let hex = if filter.is_some() {
            let start = usize::try_from(record.offset).expect("offset should fit usize");
            hex_preview(
                objects.get(start..).unwrap_or_default(),
                record.span_bytes.min(64),
            )
        } else {
            record.prefix_hex.clone()
        };
        println!(
            "{}\t{:?}\t{:?}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}\t{:?}\t{:?}\t{:?}\t{:?}\t{}",
            record.handle,
            record.declared_handle_code,
            record.declared_handle,
            record.declared_handle_matches_index,
            record.handle_match_search_delta_bits,
            record.handle_match_search_encoding,
            record.header_signature,
            record.span_bytes,
            record.offset_bits,
            record.offset,
            record.handle_stream_bits,
            record.object_type,
            record.object_type_name,
            record.object_data_bits,
            hex
        );
    }
}

fn hex_preview(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .take(len)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
