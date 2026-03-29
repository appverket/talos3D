use std::path::PathBuf;

use cadio_dwg::read_short_object_stubs;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: dump_dwg_short_records <path-to-dwg> [signature-fragment]");
    let filter = args.next().and_then(|value| value.into_string().ok());

    for record in read_short_object_stubs(&path).expect("failed to decode short object stubs") {
        if filter
            .as_deref()
            .is_some_and(|filter| !record.header_signature.contains(filter))
        {
            continue;
        }
        println!(
            "{}\t{}\t{}\t{}\t{}",
            record.handle,
            record.header_signature,
            record.payload_len,
            record.payload_matches_size_hint,
            record.payload_prefix_hex
        );
    }
}
