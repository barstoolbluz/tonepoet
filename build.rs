mod build_native_mlp_decoder;

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn hash_source_files(paths: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for path in paths {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|error| panic!("could not read Reference closure source {path}: {error}"));
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
        println!("cargo:rerun-if-changed={path}");
    }
    format!("{:x}", hasher.finalize())
}

fn collect_tree_files(root: &str) -> Vec<PathBuf> {
    fn visit(path: &Path, files: &mut Vec<PathBuf>) {
        let metadata = std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("could not stat Reference closure source {}: {error}", path.display()));
        if metadata.is_file() {
            files.push(path.to_path_buf());
            return;
        }
        let mut children = std::fs::read_dir(path)
            .unwrap_or_else(|error| panic!("could not read Reference closure source directory {}: {error}", path.display()))
            .map(|entry| entry.expect("Reference closure source directory entry").path())
            .collect::<Vec<_>>();
        children.sort();
        for child in children {
            visit(&child, files);
        }
    }

    let mut files = Vec::new();
    visit(Path::new(root), &mut files);
    files.sort();
    files
}

fn hash_source_tree(root: &str, extra_paths: &[&str]) -> String {
    let mut paths = collect_tree_files(root);
    paths.extend(extra_paths.iter().map(PathBuf::from));
    paths.sort();
    paths.dedup();
    let mut hasher = Sha256::new();
    for path in paths {
        let relative = path.to_string_lossy();
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("could not read Reference closure source {}: {error}", path.display()));
        hasher.update((relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
        println!("cargo:rerun-if-changed={}", path.display());
    }
    format!("{:x}", hasher.finalize())
}

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

    // Reference release evidence binds the actual compiler/build closure rather
    // than a user-selectable policy version. Keep the value textual so the
    // qualification report can record and independently hash the exact same
    // compiled identity.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let command_text = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().replace('\n', ";"))
            .unwrap_or_else(|| "unavailable".to_string())
    };
    let compiler_build_closure = format!(
        "rustc={};cargo={};host={};target={};profile={};opt_level={};debug={}",
        command_text(&rustc, &["-vV"]),
        command_text(&cargo, &["-V"]),
        std::env::var("HOST").unwrap_or_else(|_| "unknown".to_string()),
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string()),
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string()),
        std::env::var("OPT_LEVEL").unwrap_or_else(|_| "unknown".to_string()),
        std::env::var("DEBUG").unwrap_or_else(|_| "unknown".to_string()),
    );
    println!("cargo:rustc-env=TONEPOET_COMPILER_BUILD_CLOSURE={compiler_build_closure}");

    // Q02 binds the characterized closure to the actual source compiled into
    // the shipping Reference path. Keep this set intentionally narrow: it is
    // the common planner/executor/readers/package/metadata/check ownership
    // surface, not an unrelated whole-workspace cache key.
    let reference_common_source_sha256 = hash_source_files(&[
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "tonepoet-pipeline/Cargo.toml",
        "tonepoet-pipeline/src/semantic_plan.rs",
        "tonepoet-pipeline/src/dsd_reference.rs",
        "tonepoet-pipeline/src/dsd_album_gain.rs",
        "tonepoet-pipeline/src/plan.rs",
        "tonepoet-pipeline/src/settings.rs",
        "tonepoet-pipeline/src/enums.rs",
        "tonepoet-pipeline/src/source.rs",
        "tonepoet-pipeline/src/tools.rs",
        "tonepoet-pipeline/src/fingerprint.rs",
        "tonepoet-pipeline/src/qualification_schema.rs",
        "tonepoet-pipeline/src/w64.rs",
        "src/convert/pipeline/plan_bridge.rs",
        "src/convert/pipeline/track_executor.rs",
        "src/convert/pipeline/stages.rs",
        "src/convert/pipeline/manifest_builder.rs",
        "src/convert/replaygain.rs",
    ]);
    println!(
        "cargo:rustc-env=TONEPOET_REFERENCE_COMMON_SOURCE_SHA256={reference_common_source_sha256}"
    );

    // The certified observer and its deliberate fused arithmetic are a
    // separate evidence-bearing numerical authority. Hash the complete crate
    // source plus its manifest so qualification cannot characterize one
    // numerical implementation and ship another under the same version.
    let true_peak_source_sha256 = hash_source_tree(
        "crates/tonepoet-true-peak/src",
        &["crates/tonepoet-true-peak/Cargo.toml"],
    );
    println!(
        "cargo:rustc-env=TONEPOET_TRUE_PEAK_SOURCE_SHA256={true_peak_source_sha256}"
    );
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
