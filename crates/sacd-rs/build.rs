use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_files_with_extension(root: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", root.display()))
        .map(|entry| entry.unwrap_or_else(|error| panic!("cannot read directory entry: {error}")))
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("cannot inspect {}: {error}", path.display()));
        if file_type.is_dir() {
            collect_files_with_extension(&path, extension, out);
        } else if file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some(extension)
        {
            out.push(path);
        }
    }
}

fn hash_file_set(domain: &[u8], base: &Path, files: &[PathBuf]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for path in files {
        let relative = path
            .strip_prefix(base)
            .unwrap_or_else(|_| panic!("{} is outside {}", path.display(), base.display()));
        let relative = relative.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        hasher.update((relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    hex_digest(hasher.finalize())
}

fn hash_file(path: &Path) -> String {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    hex_digest(Sha256::digest(bytes))
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("write to String");
    }
    hex
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("src");
    let fixture_dir = src.join("dst/fixtures");
    println!("cargo:rerun-if-changed={}", src.display());

    let mut source_files = Vec::new();
    collect_files_with_extension(&src, "rs", &mut source_files);
    source_files.sort();
    let source_digest = hash_file_set(b"sacd-rs-reference-source/v1\0", &manifest_dir, &source_files);

    let fixture_manifest = fixture_dir.join("P0_SHA256SUMS");
    let fixture_provenance = fixture_dir.join("P0_PROVENANCE.json");
    let fixture_payloads = [
        "frame_001.dst.bin",
        "frame_001.dsd.bin",
        "frame_001_6ch.dst.bin",
        "frame_001_6ch.dsd.bin",
        "frame_002.dst.bin",
        "frame_002.dsd.bin",
        "frame_002_6ch.dst.bin",
        "frame_002_6ch.dsd.bin",
        "frame_003.dst.bin",
        "frame_003.dsd.bin",
        "frame_003_6ch.dst.bin",
        "frame_003_6ch.dsd.bin",
        "raw_dsd64_mono.dst.bin",
        "raw_dsd64_mono.dsd.bin",
        "raw_dsd64_6ch.dst.bin",
        "raw_dsd64_6ch.dsd.bin",
        "raw_dsd128_mono.dst.bin",
        "raw_dsd128_mono.dsd.bin",
        "raw_dsd128_stereo.dst.bin",
        "raw_dsd128_stereo.dsd.bin",
        "raw_dsd256_mono.dst.bin",
        "raw_dsd256_mono.dsd.bin",
        "raw_dsd256_stereo.dst.bin",
        "raw_dsd256_stereo.dsd.bin",
        "generate_p0_raw_fixtures.py",
        "verify_p0_raw_oracle.py",
    ];
    let mut fixture_files = vec![fixture_manifest.clone(), fixture_provenance.clone()];
    fixture_files.extend(fixture_payloads.iter().map(|name| fixture_dir.join(name)));
    fixture_files.sort();
    for path in &fixture_files {
        if !path.is_file() {
            panic!("required P0 DST fixture authority is absent: {}", path.display());
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let fixture_digest = hash_file_set(
        b"sacd-rs-dst-reference-fixtures/v2\0",
        &fixture_dir,
        &fixture_files,
    );
    let fixture_manifest_digest = hash_file(&fixture_manifest);
    let fixture_provenance_digest = hash_file(&fixture_provenance);

    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    println!(
        "cargo:rustc-env=SACD_RS_REFERENCE_BUILD_ID=sacd-rs-{version}-src-sha256:{source_digest}"
    );
    println!(
        "cargo:rustc-env=SACD_RS_DST_FIXTURE_CORPUS_ID=sha256:{fixture_digest}"
    );
    println!(
        "cargo:rustc-env=SACD_RS_DST_FIXTURE_MANIFEST_ID=sha256:{fixture_manifest_digest}"
    );
    println!(
        "cargo:rustc-env=SACD_RS_DST_FIXTURE_PROVENANCE_ID=sha256:{fixture_provenance_digest}"
    );
}
