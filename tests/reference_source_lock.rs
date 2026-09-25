#[path = "../reference_source_lock.rs"]
mod reference_source_lock;

use reference_source_lock::{
    hash_reference_source_files_with_reader, REFERENCE_COMMON_SOURCE_PATHS,
};

fn root_manifest_with_package_version(bytes: &[u8], replacement: &str) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).expect("Cargo.toml UTF-8");
    let mut in_package = false;
    let mut replaced = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = trimmed == "[package]";
        }
        if in_package && !replaced {
            if let Some(equal) = line.find('=') {
                if line[..equal].trim() == "version" {
                    let newline = if line.ends_with('\n') { "\n" } else { "" };
                    out.push_str(&line[..=equal]);
                    out.push(' ');
                    out.push('"');
                    out.push_str(replacement);
                    out.push('"');
                    out.push_str(newline);
                    replaced = true;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    assert!(replaced, "root manifest package version must exist");
    out.into_bytes()
}

fn root_lock_with_package_version(bytes: &[u8], replacement: &str) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).expect("Cargo.lock UTF-8");
    let mut blocks = text.split("[[package]]");
    let prefix = blocks.next().expect("Cargo.lock prefix");
    let mut out = prefix.to_string();
    let mut replaced = 0usize;

    for block in blocks {
        out.push_str("[[package]]");
        if block.lines().any(|line| line.trim() == "name = \"tonepoet\"")
            && !block
                .lines()
                .any(|line| line.trim_start().starts_with("source ="))
        {
            let mut version_replaced = false;
            for line in block.split_inclusive('\n') {
                if !version_replaced && line.trim_start().starts_with("version =") {
                    let indent_len = line.len() - line.trim_start().len();
                    out.push_str(&line[..indent_len]);
                    out.push_str("version = \"");
                    out.push_str(replacement);
                    out.push_str("\"\n");
                    version_replaced = true;
                } else {
                    out.push_str(line);
                }
            }
            assert!(version_replaced, "root lock package version must exist");
            replaced += 1;
        } else {
            out.push_str(block);
        }
    }

    assert_eq!(replaced, 1, "exactly one root tonepoet lock package");
    out.into_bytes()
}

#[test]
fn reference_common_source_lock_ignores_only_root_package_version_value() {
    let original_manifest = std::fs::read("Cargo.toml").expect("read root Cargo.toml");
    let bumped_manifest = root_manifest_with_package_version(&original_manifest, "987.654.321");
    let original_lock = std::fs::read("Cargo.lock").expect("read Cargo.lock");
    let bumped_lock = root_lock_with_package_version(&original_lock, "987.654.321");

    let hash_with_versions = |manifest: &[u8], lock: &[u8]| {
        hash_reference_source_files_with_reader(REFERENCE_COMMON_SOURCE_PATHS, |path| {
            if path == "Cargo.toml" {
                Ok(manifest.to_vec())
            } else if path == "Cargo.lock" {
                Ok(lock.to_vec())
            } else {
                std::fs::read(path)
            }
        })
        .expect("recompute Reference common source closure")
    };

    assert_eq!(
        hash_with_versions(&original_manifest, &original_lock),
        hash_with_versions(&bumped_manifest, &bumped_lock),
        "changing the root package version in Cargo.toml and Cargo.lock must not invalidate Reference qualification",
    );

    let mut changed_lock = original_lock.clone();
    changed_lock.push(b'\n');
    let original = hash_reference_source_files_with_reader(
        REFERENCE_COMMON_SOURCE_PATHS,
        |path| std::fs::read(path),
    )
    .expect("hash original source lock");
    let changed = hash_reference_source_files_with_reader(REFERENCE_COMMON_SOURCE_PATHS, |path| {
        if path == "Cargo.lock" {
            Ok(changed_lock.clone())
        } else {
            std::fs::read(path)
        }
    })
    .expect("hash source lock with changed Cargo.lock");
    assert_ne!(original, changed, "Cargo.lock must remain qualification-bound");
}

#[test]
fn reference_common_source_lock_includes_reference_metadata_command_builder() {
    assert!(
        REFERENCE_COMMON_SOURCE_PATHS.contains(&"tonepoet-pipeline/src/plugins.rs"),
        "Reference metadata transfer commands affect Reference behavior and must remain qualification-bound",
    );
}
