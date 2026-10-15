//! Exports the target triple so `civetd version` can report it (the client
//! bootstrap picks the binary to upload by triple).

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=CIVETD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
