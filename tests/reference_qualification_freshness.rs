//! Gate: the installed Reference qualification must describe the current tree.
//!
//! The Reference runtime closure fingerprint binds the exact bytes of
//! `REFERENCE_COMMON_SOURCE_PATHS`. Change one and the installed qualification
//! stops binding the running build, so Reference refuses every DSD source at
//! runtime with "Reference production promotion is inactive".
//!
//! Nothing in the ordinary gate exercises the real-tool Reference path, so that
//! drift used to surface only when a user converted a DSD album. On 2026-10-01
//! it had gone unnoticed across eight commits (R4 through R11). This test closes
//! that window: it is pure file hashing, needs no audio tools, and names the
//! exact files that drifted.
//!
//! Regenerate the sidecar by requalifying — see the failure message.

#[path = "../reference_source_lock.rs"]
#[allow(dead_code)]
mod reference_source_lock;

use reference_source_lock::{reference_source_bytes_for_hash, REFERENCE_COMMON_SOURCE_PATHS};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

const SIDECAR: &str = "tonepoet-pipeline/qualification/dsd_reference_common_v18_source_lock.json";

const REQUALIFY: &str = "\
    unset TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT \\\n\
    \x20     TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH\n\
    export TONEPOET_REQUIRE_TOOLS=1\n\
    cargo test --release --test dsd_reference_qualification \\\n\
    \x20 complete_p0_reference_qualification_report -- --nocapture\n\
    # then copy target/dsd_reference_common_v18_{report,certification,evidence,source_lock}.json\n\
    # into tonepoet-pipeline/qualification/";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Per-locked-file digest of the working tree, using the same canonicalization
/// the build and the fingerprint use (root package version is identity-only).
pub fn working_tree_locked_digests() -> BTreeMap<String, String> {
    let root = repo_root();
    REFERENCE_COMMON_SOURCE_PATHS
        .iter()
        .map(|path| {
            let raw = std::fs::read(root.join(path))
                .unwrap_or_else(|error| panic!("read locked Reference source {path}: {error}"));
            let bytes = reference_source_bytes_for_hash(path, &raw)
                .unwrap_or_else(|error| panic!("canonicalize locked Reference source {path}: {error}"));
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            (path.to_string(), format!("{:x}", hasher.finalize()))
        })
        .collect()
}

#[test]
fn installed_reference_qualification_binds_the_current_locked_sources() {
    let sidecar_path = repo_root().join(SIDECAR);
    let raw = std::fs::read(&sidecar_path).unwrap_or_else(|error| {
        panic!(
            "cannot read the Reference source-lock sidecar at {}: {error}\n\n\
             It records which locked sources the installed qualification was produced from.\n\
             Requalify to regenerate it:\n\n{REQUALIFY}\n",
            sidecar_path.display()
        )
    });
    let recorded: BTreeMap<String, String> = serde_json::from_slice::<serde_json::Value>(&raw)
        .expect("Reference source-lock sidecar is valid JSON")
        .get("locked_source_sha256")
        .expect("sidecar records locked_source_sha256")
        .as_object()
        .expect("locked_source_sha256 is an object")
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.as_str().expect("digest is a string").to_string(),
            )
        })
        .collect();

    let current = working_tree_locked_digests();

    let mut drifted: Vec<&str> = Vec::new();
    let mut untracked: Vec<&str> = Vec::new();
    for (path, digest) in &current {
        match recorded.get(path) {
            Some(known) if known == digest => {}
            Some(_) => drifted.push(path),
            None => untracked.push(path),
        }
    }
    let removed: Vec<&str> = recorded
        .keys()
        .filter(|path| !current.contains_key(*path))
        .map(String::as_str)
        .collect();

    if drifted.is_empty() && untracked.is_empty() && removed.is_empty() {
        return;
    }

    let mut message = String::from(
        "The installed Reference qualification no longer describes this tree, so Reference \
         will refuse every DSD source at runtime with \"Reference production promotion is \
         inactive\".\n\n",
    );
    if !drifted.is_empty() {
        message.push_str("Locked Reference sources changed since the qualification was run:\n");
        for path in &drifted {
            message.push_str(&format!("  {path}\n"));
        }
    }
    if !untracked.is_empty() {
        message.push_str(
            "\nLocked paths absent from the sidecar (REFERENCE_COMMON_SOURCE_PATHS grew):\n",
        );
        for path in &untracked {
            message.push_str(&format!("  {path}\n"));
        }
    }
    if !removed.is_empty() {
        message.push_str(
            "\nSidecar records paths no longer locked (REFERENCE_COMMON_SOURCE_PATHS shrank):\n",
        );
        for path in &removed {
            message.push_str(&format!("  {path}\n"));
        }
    }
    message.push_str(&format!("\nRequalify on a host with the real tools:\n\n{REQUALIFY}\n"));
    panic!("{message}");
}

#[test]
fn sidecar_covers_exactly_the_locked_source_set() {
    let sidecar_path = repo_root().join(SIDECAR);
    let raw = std::fs::read(&sidecar_path).expect("read Reference source-lock sidecar");
    let value: serde_json::Value = serde_json::from_slice(&raw).expect("sidecar is valid JSON");
    let recorded = value
        .get("locked_source_sha256")
        .and_then(|v| v.as_object())
        .expect("sidecar records locked_source_sha256");

    assert_eq!(
        recorded.len(),
        REFERENCE_COMMON_SOURCE_PATHS.len(),
        "sidecar must record every locked Reference source, no more and no less"
    );
    for path in REFERENCE_COMMON_SOURCE_PATHS {
        assert!(
            recorded.contains_key(*path),
            "sidecar is missing locked Reference source {path}"
        );
    }
}

/// Gate: the embedded in-process attestation must still describe this tree.
///
/// `sacd-rs` is deliberately NOT in `REFERENCE_COMMON_SOURCE_PATHS`, so the
/// source-lock tests above cannot see it. But `sacd_rs::REFERENCE_BUILD_ID` is a
/// SHA-256 of that crate's own source, computed in its `build.rs`, and
/// `realize_track` compares it against the `sacd_rs_build_identity` recorded in
/// the qualification manifest it embeds at compile time. The DST fixture, its
/// manifest, its provenance, the commission attestation and the standards-literal
/// oracle are compared the same way.
///
/// So editing any `sacd-rs` source file, or any of those assets, invalidates the
/// installed qualification exactly as touching a locked file does — every
/// compressed-DSD source then refuses at runtime with "Reference production
/// promotion is inactive" — and nothing else in the gate notices. This closes
/// that second door.
///
/// Mirrors the production comparison in `realize_track`; it is string and digest
/// equality only, needs no audio tools, and names what drifted.
#[test]
fn embedded_in_process_attestation_binds_the_current_sacd_rs_build() {
    use sha2::{Digest, Sha256};

    const MANIFEST: &str = "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v18.json";

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(root.join(MANIFEST))
        .unwrap_or_else(|error| panic!("read {MANIFEST}: {error}"));
    let manifest: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|error| panic!("parse {MANIFEST}: {error}"));
    let in_process = manifest
        .get("in_process")
        .expect("qualification manifest must carry an in_process attestation");
    let field = |key: &str| -> String {
        in_process
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| panic!("in_process.{key} must be a string"))
            .to_owned()
    };
    let sha256_of = |relative: &str| -> String {
        let path = root.join(relative);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        format!("{:x}", Sha256::digest(&bytes))
    };

    // (what the manifest recorded, what this tree actually is, what drifted)
    let checks: Vec<(String, String, &str)> = vec![
        (
            field("sacd_rs_build_identity"),
            sacd_rs::REFERENCE_BUILD_ID.to_owned(),
            "crates/sacd-rs/ source (its build identity is a hash of its own source)",
        ),
        (
            format!("sha256:{}", field("dst_fixture_digest")),
            sacd_rs::DST_REFERENCE_FIXTURE_CORPUS_ID.to_owned(),
            "the DST reference fixture corpus",
        ),
        (
            format!("sha256:{}", field("dst_fixture_manifest_digest")),
            sacd_rs::DST_REFERENCE_FIXTURE_MANIFEST_ID.to_owned(),
            "the DST reference fixture manifest",
        ),
        (
            format!("sha256:{}", field("dst_fixture_provenance_digest")),
            sacd_rs::DST_REFERENCE_FIXTURE_PROVENANCE_ID.to_owned(),
            "the DST reference fixture provenance",
        ),
        (
            field("commission_attestation_digest"),
            sha256_of("assets/dsd_reference/brief_dsd_reference_p0_scope_and_commission.md"),
            "assets/dsd_reference/brief_dsd_reference_p0_scope_and_commission.md",
        ),
        (
            field("standards_literal_oracle_sha256"),
            sha256_of("crates/sacd-rs/src/dst/fixtures/verify_p0_raw_oracle.py"),
            "crates/sacd-rs/src/dst/fixtures/verify_p0_raw_oracle.py",
        ),
    ];

    let drifted: Vec<&str> = checks
        .iter()
        .filter(|(recorded, actual, _)| recorded != actual)
        .map(|(_, _, what)| *what)
        .collect();

    assert!(
        drifted.is_empty(),
        "The embedded in-process attestation no longer describes this tree, so Reference \
         will refuse every compressed-DSD source at runtime with \"Reference production \
         promotion is inactive\".\n\n\
         Drifted:\n{}\n\n\
         Note that `sacd-rs` is not in REFERENCE_COMMON_SOURCE_PATHS, so the source-lock \
         tests in this file cannot catch this.\n\n\
         Requalify and reinstall the sidecars:\n{REQUALIFY}",
        drifted
            .iter()
            .map(|what| format!("  - {what}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
