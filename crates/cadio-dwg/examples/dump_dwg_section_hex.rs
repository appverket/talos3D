use std::path::PathBuf;

use cadio_dwg::read_section_data;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: dump_dwg_section_hex <path-to-dwg> <section-name> <offset> [len]");
    let section_name = args
        .next()
        .and_then(|value| value.into_string().ok())
        .expect("usage: dump_dwg_section_hex <path-to-dwg> <section-name> <offset> [len]");
    let offset = args
        .next()
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .expect("usage: dump_dwg_section_hex <path-to-dwg> <section-name> <offset> [len]");
    let len = args
        .next()
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(128);

    let bytes = read_section_data(&path, &section_name).expect("failed to read DWG section");
    let slice = bytes
        .get(offset..offset.saturating_add(len))
        .expect("requested range should exist in DWG section");
    println!("{}", hex_preview(slice));
}

fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
