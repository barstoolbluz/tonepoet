#!/usr/bin/env python3
"""Static invariants for the round-7 fork-safe ephemeral retirement corrective.

These assertions pin the production-safety shape of the fix: ordinary
MutationClaimGuard teardown actively retires its own ephemeral descriptor so a
transient fork-time CLOEXEC duplicate cannot extend lexical authority, while
explicitly exported/detached authority remains externally visible. The scanner
also treats a descriptor pathname retired during classification as absent only
for structurally valid ephemeral descriptors, while retaining fail-closed
handling for durable, rebound, malformed, and genuinely live publications.

This is intentionally structural and does not replace compiling or the repeated
workspace gate in scripts/validate_concurrency_round7.sh.
"""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def function(text: str, name: str) -> str:
    marker = f"fn {name}("
    begin = text.index(marker)
    brace = text.index("{", begin)
    depth = 0
    for i in range(brace, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[begin : i + 1]
    raise AssertionError(f"unterminated function {name}")


def require(condition: bool, label: str) -> None:
    if not condition:
        raise AssertionError(label)
    print(f"[ok] {label}")


def main() -> int:
    concurrency = read("src/concurrency.rs")
    gate = read("scripts/validate_concurrency_round7.sh")

    persistent = concurrency[
        concurrency.index("pub struct PersistentLease {") : concurrency.index(
            "/// Process-local view of descriptor handles", concurrency.index("pub struct PersistentLease {")
        )
    ]
    require(
        "lifetime_file_exported: std::sync::atomic::AtomicBool" in persistent,
        "persistent lease records whether close-only authority was exported",
    )

    duplicate = function(concurrency, "duplicate_lifetime_file")
    clone_at = duplicate.index("self.file.try_clone()")
    export_at = duplicate.index("lifetime_file_exported")
    require(
        clone_at < export_at
        and ".store(true, std::sync::atomic::Ordering::Release)" in duplicate,
        "lifetime export is recorded only after the descriptor clone succeeds",
    )

    inherited_fd = function(concurrency, "inherited_fd")
    require(
        "lifetime_file_exported" in inherited_fd
        and ".store(true, std::sync::atomic::Ordering::Release)" in inherited_fd,
        "raw inherited-fd exposure is also recorded as intentional authority export",
    )

    retire = function(concurrency, "retire_ephemeral_descriptor_on_guard_drop")
    require(
        "LeaseFamily::EphemeralMutation" in retire
        and "lifetime_file_exported" in retire
        and ".load(std::sync::atomic::Ordering::Acquire)" in retire,
        "eager retirement is restricted to unexported EphemeralMutation authority",
    )
    require(
        "verify_coordination_path_binding" in retire
        and "same_file::Handle::from_file" in retire
        and "same_file::Handle::from_path" in retire
        and "std::fs::remove_file(&self.descriptor_path)" in retire,
        "retirement proves descriptor pathname identity before best-effort unlink",
    )
    require(
        "RegistryLock::acquire" not in retire and "sync_coordination_directory" not in retire,
        "ephemeral Drop retirement is nonblocking and does not add crash-durability I/O",
    )

    guard_struct = concurrency[
        concurrency.index("pub struct MutationClaimGuard {") : concurrency.index(
            "impl MutationClaimGuard", concurrency.index("pub struct MutationClaimGuard {")
        )
    ]
    require(
        "lease: Option<PersistentLease>" in guard_struct,
        "guard represents explicit transfer by taking its lease out before Drop",
    )
    into_lease = function(concurrency, "into_lease")
    require(
        "self.lease" in into_lease and ".take()" in into_lease,
        "into_lease transfers authority without running eager descriptor retirement",
    )

    guard_drop_begin = concurrency.index("impl Drop for MutationClaimGuard")
    guard_drop_end = concurrency.index("/// Enumerate lifecycle ids", guard_drop_begin)
    guard_drop = concurrency[guard_drop_begin:guard_drop_end]
    require(
        "retire_ephemeral_descriptor_on_guard_drop" in guard_drop,
        "ordinary guard Drop explicitly retires eligible ephemeral descriptors",
    )

    persistent_drop_begin = concurrency.index("impl Drop for PersistentLease")
    persistent_drop_end = concurrency.index("#[derive(Debug)]\npub struct MutationClaimGuard", persistent_drop_begin)
    persistent_drop = concurrency[persistent_drop_begin:persistent_drop_end]
    require(
        "remove_file" not in persistent_drop
        and "unregister_local_persistent_lease" in persistent_drop,
        "raw/detached PersistentLease keeps the existing close-only Drop contract",
    )

    admission = function(concurrency, "acquire_grouped_internal")
    require(
        "OwnerProcessIdentity::current()" not in admission
        and "ClaimAvailability::Live => \"live owner\"" in admission
        and "first_conflict(&claims, &existing_claims)" in admission,
        "generic admission has no same-PID bypass and still rejects live overlap",
    )

    classifier = function(concurrency, "classify_descriptor")
    opened_classifier = function(concurrency, "classify_opened_descriptor")
    unpublished = function(concurrency, "ephemeral_descriptor_unpublished_during_classification")
    require(
        "classify_opened_descriptor(file, path, lock_state)" in classifier
        and "OwnerProcessIdentity::current()" not in classifier,
        "descriptor scanner delegates post-open classification without any same-PID bypass",
    )
    require(
        "ephemeral_descriptor_unpublished_during_classification(path)?" in opened_classifier
        and "classify_availability(&descriptor.family, lock_state)" in opened_classifier
        and "OwnerProcessIdentity::current()" not in opened_classifier,
        "post-open scanner skips only the explicit unpublished-ephemeral transition and preserves ordinary live classification",
    )
    require(
        "structurally_ephemeral_descriptor_path(path)" in unpublished
        and "std::io::ErrorKind::NotFound" in unpublished
        and "std::fs::symlink_metadata(path)" in unpublished
        and "RegistryLock::acquire" not in unpublished,
        "unpublish recognition is classifier-local, structural, NotFound-specific, and lock-free",
    )

    tests = (
        "scanner_treats_opened_unpublished_ephemeral_descriptor_as_absent",
        "scanner_keeps_published_ephemeral_rebind_fail_closed",
        "scanner_keeps_disappeared_durable_descriptor_fail_closed",
        "ordinary_ephemeral_guard_drop_retires_descriptor_before_reacquire",
        "fork_inherited_cloexec_ephemeral_fd_cannot_block_next_guard",
        "exported_ephemeral_lifetime_keeps_descriptor_visible_until_final_holder_closes",
        "raw_inherited_fd_export_keeps_descriptor_visible_until_duplicate_closes",
        "into_lease_preserves_detached_ephemeral_authority",
        "ephemeral_guard_retirement_never_unlinks_a_rebound_descriptor_path",
    )
    for name in tests:
        require(f"fn {name}" in concurrency, f"round-7 regression exists: {name}")
        require(name in gate, f"round-7 gate executes focused regression: {name}")

    require(
        "fn holder_drop_does_not_unlink_descriptor()" in concurrency,
        "raw PersistentLease close-only regression remains in place",
    )

    require(
        "tools/verify_concurrency_corrective_round1.py" in gate
        and "tools/verify_concurrency_corrective_round4.py" in gate
        and "tools/verify_concurrency_corrective_round5.py" in gate
        and "tools/verify_concurrency_corrective_round6.py" in gate
        and "tools/verify_concurrency_corrective_round6_r1.py" in gate
        and "tools/verify_concurrency_corrective_round7.py" in gate,
        "round-7 gate preserves every prior static concurrency proof",
    )
    require(
        "unset RUST_MIN_STACK" in gate
        and "TONEPOET_*) unset" in gate
        and "for run in $(seq 1 5)" in gate
        and "cargo test --workspace --no-fail-fast" in gate
        and "for run in $(seq 1 50)" in gate
        and "pipeline_reports_consumer_nonzero_and_closes_producer" in gate,
        "round-7 gate preserves the unmasked five-run workspace and 50x SIGPIPE bar",
    )
    require(
        "self-overlap" in gate
        and "filesystem mutation conflicts with live owner" in gate,
        "workspace log scan explicitly rejects same-path live-owner self-overlap",
    )

    require(
        "CLOEXEC does not prevent fork-time descriptor inheritance" in concurrency
        and "libc::F_GETFD" in concurrency
        and "libc::FD_CLOEXEC" in concurrency
        and "libc::fork()" in concurrency,
        "regression models the fork-before-exec inherited-flock mechanism",
    )

    print("round-7 fork-safe ephemeral mutation retirement static verification passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
