#!/usr/bin/env python3
"""Static regression assertions for the 2026-08-18 concurrency corrective.

This intentionally supplements, and never substitutes for, Rust compilation and
tests.  It catches accidental removal of the corrective's protocol/safety seams
when the tree is reviewed or patched in an environment without a Rust toolchain.
"""
from __future__ import annotations

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]


def text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def require(label: str, condition: bool) -> None:
    if not condition:
        print(f"[FAIL] {label}", file=sys.stderr)
        raise SystemExit(1)
    print(f"[ok] {label}")


concurrency = text("src/concurrency.rs")
tool = text("src/convert/pipeline/tool.rs")
processor = text("src/convert/processor.rs")
stages = text("src/convert/pipeline/stages.rs")
supervisor = text("src/convert/script_supervisor.rs")
db = text("src/db.rs")
main = text("src/main.rs")
all_rust = "\n".join(path.read_text(encoding="utf-8") for path in (ROOT / "src").rglob("*.rs"))

# #0: fully formed inode publication + legacy/self-healing classification.
require("lease staging remains create_new", "temp_options.read(true).write(true).create_new(true);" in concurrency)
require("lease staging keeps nofollow/cloexec", "libc::O_NOFOLLOW | libc::O_CLOEXEC" in concurrency)
require("lease staging is private at create", ".mode(0o600);" in concurrency)
require("final lease publication is atomic no-clobber", "std::fs::hard_link(&temp_path, &path)" in concurrency)
require("published lease binding is reverified", 'verify_coordination_path_binding(&file, &path, "published persistent lease")' in concurrency)
publish_pos = concurrency.index("std::fs::hard_link(&temp_path, &path)")
first_dir_sync_after_publish = concurrency.index("sync_coordination_directory(&family_dir)?;", publish_pos)
temp_unlink_after_publish = concurrency.index("temp_cleanup.remove_owned_path_now()", publish_pos)
final_binding_after_publish = concurrency.index(
    'verify_coordination_path_binding(&file, &path, "published persistent lease")?;',
    temp_unlink_after_publish,
)
require("durable descriptors fsync file and directory", "file.sync_all()" in concurrency)
require(
    "durable final link is persisted before staging unlink",
    publish_pos < first_dir_sync_after_publish < temp_unlink_after_publish < final_binding_after_publish,
)
require(
    "staging unlink avoids redundant durable directory fsync",
    "sync_coordination_directory(&family_dir)?;"
    not in concurrency[temp_unlink_after_publish:final_binding_after_publish],
)
require(
    "ephemeral descriptors avoid unconditional durable fsync",
    "let crash_durable_descriptor = !matches!(" in concurrency
    and "LeaseFamily::EphemeralMutation { .. }" in concurrency,
)
require("unlocked zero-length descriptors self-heal", "reclaim_empty_descriptor_from_locked_file(&file, path)?" in concurrency)
require("unlocked malformed ephemeral descriptors self-heal", "reclaim_invalid_ephemeral_descriptor_from_locked_file(&file, path)?" in concurrency)
require("nonempty durable malformed descriptors remain fail-closed", "malformed coordination descriptor requires lifecycle repair" in concurrency)
require("zero-length crash regression exists", "fn zero_length_crash_orphan_is_reclaimed_by_next_admission()" in concurrency)
require("durable malformed regression exists", "fn truncated_durable_descriptor_routes_by_path_to_lifecycle_cleanup_only()" in concurrency)
require("abandoned staging regression exists", "fn singular_lifecycle_create_cleans_abandoned_atomic_staging_file()" in concurrency)

# #1a: a bare program is PATH-bound before identity/canonicalization regardless of env policy.
require("bare command resolution is environment-policy independent", "_environment_policy: CommandEnvironmentPolicy" in tool and "candidate.components().count() == 1 && !candidate.is_absolute()" in tool)
require("bare command resolver is fallible", ") -> std::io::Result<PathBuf>" in tool)
require("bare PATH miss cannot fall back to cwd pathname", "unwrap_or(candidate)" not in tool and "std::io::ErrorKind::NotFound" in tool)
require("inherited PATH regression exists", "fn inherited_environment_bare_program_resolves_from_path_before_supervision()" in tool)
require("bare PATH miss regression exists", "fn bare_program_missing_from_path_is_not_reinterpreted_relative_to_cwd()" in tool)
require("normal run propagates PATH miss", "fn normal_run_propagates_bare_path_miss_as_not_found()" in tool)
require("pipeline propagates PATH miss", "fn pipeline_propagates_bare_path_miss_as_not_found()" in tool)
require("explicit relative-path regression exists", "fn explicit_relative_program_path_is_preserved_for_filesystem_resolution()" in tool)

# #2: cut the album-postprocess generator at pointer-sized suspension boundaries.
require("album postprocess future is heap-indirected", "type AlbumPostprocessFuture = Pin<Box<dyn Future<Output = QueueWorkOutput> + Send + 'static>>;" in processor)
require("scoped album postprocess is no longer async fn", "fn run_album_postprocess_work_scoped(" in processor and "async fn run_album_postprocess_work_scoped(" not in processor)
require("large scheduler child future is boxed", "Box::pin(finish_pipeline_album_for_scheduler_with_tool_limits(" in processor)
require("large finish-stage children are boxed", "Box::pin(merge_tracks_with_tool_limits(" in stages and "Box::pin(finalize_report_with_binding(" in stages)
require("stack-size mask is absent from Rust source", "RUST_MIN_STACK" not in all_rust)

# #1b: real supervision is exercised under libtest without an env requirement.
require("cargo harness helper resolver exists", "fn cargo_test_helper_candidate(" in supervisor and "fn resolve_supervisor_helper_executable(" in supervisor)
require("production helper remains current executable", "Production behavior remains re-exec of the running tonepoet binary." in supervisor)
require("unit tests no longer bypass item supervision", 'cfg!(test) && std::env::var_os("TONEPOET_SCRIPT_SUPERVISOR_HELPER")' not in concurrency)
require("helper env is only an optional resolver override", supervisor.count('var_os("TONEPOET_SCRIPT_SUPERVISOR_HELPER")') == 1)

# Round-2 compile guard: the binary crate must not call library-private test coordination helpers.
require("binary CLI unit tests do not call crate::concurrency", "crate::concurrency::scoped_test_coordination_root()" not in main)

# #3/#4/#5: per-test hermetic roots and activation behavior without production exemptions.
require("cargo-test automatic fallback root exists", "fn cargo_test_coordination_root()" in concurrency)
require("narrow same-thread fixture root remains available", "pub(crate) fn install_test_coordination_root(path: &Path)" in concurrency)
require("one shared test coordination serial mutex exists", "fn test_coordination_serial()" in concurrency)
require("process-visible per-test root scope exists", "pub(crate) fn scoped_test_coordination_root()" in concurrency and "ScopedTestCoordinationRootGuard" in concurrency)
require("explicit-path process-visible root scope exists", "pub(crate) fn install_scoped_test_coordination_root(" in concurrency)
require("scoped root overrides and restores the coordination environment", 'std::env::set_var("TONEPOET_CONCURRENCY_DIR", &path);' in concurrency and 'std::env::remove_var("TONEPOET_CONCURRENCY_DIR")' in concurrency)
require(
    "worker-thread root propagation regression exists",
    "fn scoped_test_coordination_root_is_visible_to_spawned_worker_thread()" in concurrency
    and "std::thread::spawn(||" in concurrency
    and 'std::env::var_os("TONEPOET_CONCURRENCY_DIR")' in concurrency,
)
require("cross-root durable-state isolation regression exists", "fn scoped_test_coordination_roots_isolate_durable_state_between_tests()" in concurrency)
require("cross-process DB proofs explicitly inherit one root", db.count('.env("TONEPOET_TEST_CONCURRENCY_DIR_INHERIT", "1")') == 2)
require("activation bypass is restricted to detected Cargo harness", "if crate::concurrency::running_under_cargo_test_harness()" in db)
require("production peer detection remains present", 'name == "tonepoet" || name.starts_with("tonepoet-")' in db)

# Settled operator compile/integration fixes from the supplied baseline must survive.
cargo_toml = text("Cargo.toml")
probe = text("src/tui/probe.rs")
file_task = text("src/tui/file_task_runtime.rs")
command = text("src/tui/command.rs")
external_editor = text("src/tui/external_editor.rs")
streaming = text("src/convert/pipeline/progress/streaming.rs")
containment = text("tests/conversion_action_runscript_containment.rs")
require("operator uuid serde feature retained", 'features = ["v4", "serde"]' in cargo_toml)
require("operator probe concurrency imports retained", "ClaimMode, ClaimScope, MutationClaimGuard, PathClaim, PathResolutionSemantics" in probe)
require("operator descriptor-path accessor retained", "guard.lease().descriptor_path()" in file_task)
require("operator recovery Debug arms retained", 'f.debug_tuple("FileRecoveryResume")' in command and 'f.debug_tuple("FileRecoveryDefer")' in command)
require("operator sidecar visibility retained", "pub(super) fn sidecar_for_playlist" in command)
require("operator retained Arc fix retained", "retained_lifetime_files: vec![retained]" in external_editor)
require("operator unsafe annotations retained", "#[allow(unsafe_code)]" in streaming and "#[allow(unsafe_code)]" in tool)
require(
    "operator containment fd fields retained",
    all(
        field in containment
        for field in (
            "retained_lifetime_files:",
            "stdin_file:",
            "stdout_file:",
            "stderr_file:",
        )
    ),
)

print("static concurrency corrective assertions passed")
