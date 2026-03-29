use std::path::PathBuf;

use cadio_dwg::probe_record;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: probe_dwg_record <path-to-dwg> <handle-decimal>");
    let handle = args
        .next()
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("expected decimal handle");
    let probe = probe_record(&path, handle).expect("failed to probe record");
    println!("{probe:#?}");
}
