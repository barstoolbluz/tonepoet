#!/usr/bin/env python3
"""Static invariants for the round-6 concurrent-sessions corrective.

This verifier pins the narrow source-level contracts repaired in round 6 and
also checks that the executable gate retains the brief's repeated dynamic bar.
It intentionally does not substitute for compiling or running the Rust suite.
"""

from __future__ import annotations

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def ok(label: str) -> None:
    print(f"[ok] {label}")


def require(condition: bool, label: str) -> None:
    if not condition:
        raise AssertionError(label)
    ok(label)


def require_order(text: str, earlier: str, later: str, label: str) -> None:
    left = text.find(earlier)
    right = text.find(later)
    require(left >= 0 and right >= 0 and left < right, label)


def section(text: str, start: str, end: str) -> str:
    begin = text.index(start)
    finish = text.index(end, begin)
    return text[begin:finish]


def test_section(text: str, test_name: str) -> str:
    begin = text.index(f"fn {test_name}")
    next_test = text.find("\n    #[", begin + 3)
    if next_test < 0:
        next_test = len(text)
    return text[begin:next_test]


def main() -> int:
    concurrency = read("src/concurrency.rs")
    keybindings = read("src/tui/keybindings.rs")
    picker_state = read("crates/tui-file-picker/src/state.rs")
    archive = read("src/convert/pipeline/materializer_archive.rs")
    mutation_audit = read("tools/audit_concurrent_mutation_entrypoints.py")
    gate = read("scripts/validate_concurrency_round6.sh")

    # F1: artwork selection remains a full file-manager surface. Round 5's
    # selection-only policy contradicted both the established picker behavior
    # and the production-facing test contract.
    artwork_policy = section(
        keybindings,
        "fn artwork_file_picker_policy(",
        "pub(crate) fn file_picker_theme_from_theme",
    )
    require(
        "FileOperationPolicy::default()" in artwork_policy
        and "policy.verbose_degrade_notices = verbose_degrade_notices" in artwork_policy
        and "selection_only" not in artwork_policy,
        "F1 artwork picker restores default file-manager authority while preserving presentation preference",
    )
    picker_default = section(
        picker_state,
        "impl Default for FileOperationPolicy",
        "impl FileOperationPolicy",
    )
    for flag in (
        "allow_new_file",
        "allow_new_folder",
        "allow_cut",
        "allow_copy",
        "allow_paste",
        "allow_delete",
        "allow_rename",
        "allow_duplicate",
    ):
        require(
            f"{flag}: true" in picker_default,
            f"F1 picker default keeps {flag} enabled",
        )
    artwork_contract = section(
        keybindings,
        "fn assert_artwork_plus_opens_crate_picker_with_images_and_file_manager_policy()",
        "#[test]\n    fn artwork_plus_opens_crate_picker_with_images_and_file_manager_policy()",
    )
    artwork_canonical_test = test_section(
        keybindings,
        "artwork_plus_opens_crate_picker_with_images_and_file_manager_policy",
    )
    artwork_legacy_test = test_section(
        keybindings,
        "artwork_plus_opens_crate_picker_with_images_and_explicit_non_mutating_policy",
    )
    for flag in (
        "allow_new_file",
        "allow_new_folder",
        "allow_cut",
        "allow_copy",
        "allow_paste",
        "allow_delete",
        "allow_rename",
        "allow_duplicate",
    ):
        require(
            f"assert!(policy.{flag});" in artwork_contract,
            f"F1 artwork regression retains {flag} contract",
        )
    require(
        "FilePickerFilter::Images" in artwork_contract
        and "FilePickerSelectionMode::Files" in artwork_contract
        and "app.file_task_verbose_degrade_notices = true" in artwork_contract
        and "assert!(policy.verbose_degrade_notices);" in artwork_contract
        and "test_environment_lock()" in artwork_canonical_test
        and "assert_artwork_plus_opens_crate_picker_with_images_and_file_manager_policy()"
        in artwork_canonical_test
        and "test_environment_lock()" in artwork_legacy_test
        and "Compatibility alias for the round-5 regression filter" in artwork_legacy_test,
        "F1 canonical test states file-manager intent while the round-5 filter remains executable",
    )
    require(
        "artwork picker retains established file-manager operation authority" in mutation_audit,
        "mutation-entrypoint audit agrees with the restored artwork picker contract",
    )

    # F2: entering a repackage attempt emits Validating even when cancellation
    # is already set. Cancellation still wins before format/tool/staging work,
    # and creator capability is still checked before staging traversal.
    direct_repackage = section(
        archive,
        "pub async fn repackage_archive_with_progress_and_cancel",
        "fn validate_repackage_staging_tree",
    )
    require_order(
        direct_repackage,
        "progress(ArchiveRepackageProgressSnapshot::new(",
        "check_repackage_cancelled(cancel)?;",
        "F2 initial Validating snapshot precedes pre-cancel return",
    )
    require_order(
        direct_repackage,
        "check_repackage_cancelled(cancel)?;",
        "let format = repackage_archive_format(original_archive)?;",
        "F2 cancellation still precedes format/tool capability work",
    )
    require_order(
        direct_repackage,
        "require_repackage_format_tool_available(format, tool_paths)?;",
        "validate_repackage_staging_tree(staging_dir, cancel)?;",
        "F2 missing creator still fails before staging traversal",
    )
    cancel_test = test_section(
        archive,
        "repackage_archive_cancelled_before_create_preserves_original_and_reports_cancel",
    )
    require(
        "assert_eq!(err, ARCHIVE_REPACKAGE_CANCELLED);" in cancel_test
        and "cancel must preserve the original archive" in cancel_test
        and "cancel before create must not leave temp repack artifacts" in cancel_test
        and "cancel should still emit the initial validating snapshot" in cancel_test,
        "F2 cancellation regression covers error, preservation, cleanup, and initial progress",
    )
    rar_preflight = test_section(
        archive,
        "preflight_reports_missing_rar_creator_before_extraction_work",
    )
    rar_repackage = test_section(
        archive,
        "repackage_archive_reports_missing_rar_creator_without_replacing_original",
    )
    require(
        "RAR archive creation requires the `rar` executable" in rar_preflight
        and "RAR archive creation requires the `rar` executable" in rar_repackage
        and "failed rar creation must not replace the original archive" in rar_repackage,
        "F2 neighboring missing-RAR actionable-error contracts remain pinned",
    )

    # F3: the scoped test root is process-visible, so an unrelated libtest
    # worker can capture it before that worker enters any serialized fixture.
    # Auto-deleting the root on guard drop therefore creates a deterministic
    # use-after-retirement race. Generated scoped roots now live beneath the
    # already process-private cargo-test root and are never reused or deleted
    # while the executable can still have a borrower.
    guard_struct = section(
        concurrency,
        "pub(crate) struct ScopedTestCoordinationRootGuard",
        "impl ScopedTestCoordinationRootGuard",
    )
    guard_impl = section(
        concurrency,
        "impl ScopedTestCoordinationRootGuard",
        "pub(crate) fn scoped_test_coordination_root()",
    )
    require(
        "TempDir" not in guard_struct
        and "TempDir" not in guard_impl
        and "fn install(path: PathBuf)" in guard_impl,
        "F3 scoped-root guard cannot RAII-delete a process-visible directory",
    )
    scoped_root = section(
        concurrency,
        "pub(crate) fn scoped_test_coordination_root()",
        "pub(crate) fn install_scoped_test_coordination_root",
    )
    require(
        "cargo_test_coordination_root()" in scoped_root
        and '.join("scoped")' in scoped_root
        and "Uuid::new_v4().to_string()" in scoped_root
        and "create_private_dir(&root)" in scoped_root
        and "ScopedTestCoordinationRootGuard::install(root)" in scoped_root
        and ".tempdir()" not in scoped_root
        and "Some(temp_dir)" not in scoped_root,
        "F3 generated scoped roots are unique, process-private, and non-RAII-retired",
    )
    retirement_test = test_section(
        concurrency,
        "scoped_test_coordination_root_retirement_keeps_captured_family_path_alive",
    )
    worker_half = section(
        retirement_test,
        "let worker = std::thread::spawn(move || {",
        "captured_rx",
    )
    require_order(
        worker_half,
        "let root = coordination_root();",
        "captured_tx",
        "F3 regression worker captures the process-visible root before synchronization",
    )
    require_order(
        worker_half,
        "retired_rx",
        ".open(&staging)",
        "F3 regression defers lease-staging create until retirement signal",
    )
    main_half = retirement_test[retirement_test.index("captured_rx") :]
    require_order(
        main_half,
        "captured_rx",
        "drop(scope);",
        "F3 regression owner waits until the worker captured the scoped root",
    )
    require_order(
        main_half,
        "drop(scope);",
        "retired_tx\n            .send(())",
        "F3 regression releases the worker only after scoped-root retirement",
    )
    require(
        "lease.tmp-" in retirement_test
        and "captured coordination family must survive scoped-root retirement" in retirement_test
        and "worker.join()" in retirement_test,
        "F3 regression exercises the exact staging-path lifetime and joins the borrower",
    )

    # Guardrail: round 6 must not solve F3 by changing production lease
    # publication/retry semantics or broadening same-process handoff.
    lease_create = section(
        concurrency,
        "fn create_while_registry_locked(",
        "/// Acquire durable recovery authority using the global lock order.",
    )
    require(
        lease_create.count("temp_options.open(&temp_path)") == 1
        and "ErrorKind::NotFound" not in lease_create
        and "sleep(" not in lease_create,
        "F3 production lease staging remains a single no-clobber create without retry masking",
    )
    local_handoff = section(
        concurrency,
        "fn current_process_coheld_descriptor(",
        "/// Removes a coordination pathname owned by this creation attempt",
    )
    require(
        "LeaseFamily::JournalOperation" in local_handoff
        and "descriptor.owner != OwnerProcessIdentity::current()" in local_handoff,
        "round-6 same-process recovery authorization remains journal-only and identity-bound",
    )

    # Acceptance gate: compile/format, all prior static invariants, focused
    # regressions, five consecutive default-parallel workspace runs, and 50x
    # SIGPIPE arbitration stress. No test-thread or stack-size masks are allowed.
    required_gate_tokens = (
        "cargo fmt --all -- --check",
        "verify_concurrency_corrective_round1.py",
        "verify_concurrency_corrective_round4.py",
        "verify_concurrency_corrective_round5.py",
        "verify_concurrency_corrective_round6.py",
        "audit_concurrent_mutation_entrypoints.py",
        "audit_test_coordination_isolation.py",
        "artwork_plus_opens_crate_picker_with_images_and_file_manager_policy",
        "artwork_plus_opens_crate_picker_with_images_and_explicit_non_mutating_policy",
        "repackage_archive_cancelled_before_create_preserves_original_and_reports_cancel",
        "preflight_reports_missing_rar_creator_before_extraction_work",
        "repackage_archive_reports_missing_rar_creator_without_replacing_original",
        "scoped_test_coordination_root_retirement_keeps_captured_family_path_alive",
        "same_process_recovery_coholds_exact_descriptor_without_weakening_strict_acquire",
        "common_write_cleanup_keeps_process_reservation_until_shared_claim_retires",
        "common_write_drop_cleanup_keeps_process_reservation_until_shared_claim_retires",
        "cargo test --workspace --no-fail-fast",
        "seq 1 5",
        "seq 1 50",
        "pipeline_reports_consumer_nonzero_and_closes_producer",
        "create persistent lease staging file",
        "No such file or directory",
    )
    for token in required_gate_tokens:
        require(token in gate, f"round-6 executable gate retains {token!r}")
    require(
        "--test-threads" not in gate
        and "RUST_MIN_STACK" in gate
        and "TONEPOET_*" in gate,
        "round-6 gate uses default libtest parallelism and explicitly removes forbidden environment masks",
    )

    print("round-6 static corrective assertions passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}", file=sys.stderr)
        raise SystemExit(1)
