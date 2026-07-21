mod build_native_mlp_decoder;
fn main() {
    build_native_mlp_decoder::build();
    // Read the pipeline crate version from its Cargo.toml directly.
    let toml = std::fs::read_to_string("tonepoet-pipeline/Cargo.toml")
        .unwrap_or_else(|_| String::new());
    let version = toml
        .lines()
        .find(|line| line.starts_with("version"))
        .and_then(|line| line.split('"').nth(1))
        .unwrap_or("unknown");
    println!("cargo:rustc-env=TONEPOET_PIPELINE_VERSION={version}");
    for variable in [
        "TONEPOET_REFERENCE_SOX_STORE_PATH",
        "TONEPOET_REFERENCE_FFMPEG_STORE_PATH",
        "TONEPOET_REFERENCE_METAFLAC_STORE_PATH",
        "TONEPOET_REFERENCE_WVTAG_STORE_PATH",
        "TONEPOET_REFERENCE_ATOMIC_PARSLEY_STORE_PATH",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        if let Ok(value) = std::env::var(variable) {
            println!("cargo:rustc-env={variable}={value}");
        }
    }
}
