//! Shared build script for Talos3D application packages (not the core library).
use std::{
    env,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn main() {
    println!("cargo:rerun-if-env-changed=TALOS3D_BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let build_number = env::var("TALOS3D_BUILD_NUMBER")
        .or_else(|_| env::var("SOURCE_DATE_EPOCH"))
        .unwrap_or_else(|_| {
            // A dependency-only rebuild must not reuse the previous app stamp.
            // Keep this path absent so Cargo reruns this small leaf build script.
            let marker = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"))
                .join("talos3d-build-invocation");
            println!("cargo:rerun-if-changed={}", marker.display());
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("build clock must be after the Unix epoch")
                .as_millis()
                .to_string()
        });
    assert!(
        !build_number.is_empty() && build_number.bytes().all(|b| b.is_ascii_digit()),
        "TALOS3D_BUILD_NUMBER / SOURCE_DATE_EPOCH must be a nonempty numeric build number"
    );
    println!("cargo:rustc-env=TALOS3D_BUILD_NUMBER={build_number}");
}
