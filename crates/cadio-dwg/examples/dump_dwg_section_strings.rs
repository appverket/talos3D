use std::{collections::BTreeSet, path::PathBuf};

use cadio_dwg::read_section_data;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: dump_dwg_section_strings <path-to-dwg> <section-name>");
    let section_name = args
        .next()
        .and_then(|value| value.into_string().ok())
        .expect("usage: dump_dwg_section_strings <path-to-dwg> <section-name>");

    let bytes = read_section_data(&path, &section_name).expect("failed to read DWG section");
    for string in extract_ascii_strings(&bytes)
        .into_iter()
        .chain(extract_utf16le_strings(&bytes))
        .collect::<BTreeSet<_>>()
    {
        println!("{string}");
    }
}

fn extract_ascii_strings(bytes: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current = String::new();
    for byte in bytes {
        if matches!(byte, 32..=126) {
            current.push(*byte as char);
        } else if current.len() >= 4 {
            strings.push(current.trim().to_string());
            current.clear();
        } else {
            current.clear();
        }
    }
    if current.len() >= 4 {
        strings.push(current.trim().to_string());
    }
    strings
}

fn extract_utf16le_strings(bytes: &[u8]) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut index = 0;
    while index + 1 < bytes.len() {
        let mut end = index;
        while end + 1 < bytes.len() && matches!(bytes[end], 32..=126) && bytes[end + 1] == 0 {
            end += 2;
        }
        if end > index {
            let fragment = bytes[index..end]
                .chunks_exact(2)
                .map(|chunk| chunk[0] as char)
                .collect::<String>();
            if fragment.len() >= 4 {
                fragments.push(fragment);
            }
            index = end;
        } else {
            index += 1;
        }
    }
    fragments
}
