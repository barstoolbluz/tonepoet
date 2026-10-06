#!/usr/bin/env python3
"""Static acceptance checks for R35 runtime-written executable elimination.

R35 removes the write->chmod(+x)->exec test-fixture class without weakening
production execution or lease semantics. Runtime-variable script bodies may be
written as non-executable data, but every inode executed by these fixtures must
exist before the test harness starts. The two observed residual lease failures
are addressed only in test coordination/cleanup machinery.
"""
from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def section(text: str, start: str, end: str) -> str:
    begin = text.index(start)
    finish = text.index(end, begin)
    return text[begin:finish]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)
    print(f"PASS: {message}")


def executable_fixture(relative: str, required: tuple[str, ...]) -> None:
    path = ROOT / relative
    require(path.is_file(), f"static fixture exists: {relative}")
    mode = path.stat().st_mode
    require(mode & 0o111 != 0, f"static fixture is executable before the harness starts: {relative}")
    require(mode & 0o022 == 0, f"static fixture is not group/world writable: {relative}")
    text = path.read_text(encoding="utf-8")
    require(text.startswith("#!/bin/sh\n"), f"static fixture has a deterministic /bin/sh shebang: {relative}")
    for needle in required:
        require(needle in text, f"static fixture {relative} preserves {needle!r}")


def main() -> int:
    tool = read("src/convert/pipeline/tool.rs")
    bluray = read("src/disc/bluray_mapper.rs")
    archive = read("src/tui/archive_listing.rs")
    command = read("src/tui/command.rs")
    keybindings = read("src/tui/keybindings.rs")
    editor = read("src/tui/external_editor.rs")
    supervisor = read("src/convert/script_supervisor.rs")
    stages = read("src/convert/pipeline/stages.rs")
    containment = read("tests/conversion_action_runscript_containment.rs")
    concurrency = read("src/concurrency.rs")
    memory_budget = read("src/convert/pipeline/memory_budget.rs")
    file_tasks = read("src/tui/file_task_runtime.rs")

    executable_fixture(
        "fixtures/test-bin/runtime-script-launcher.sh",
        (
            "TONEPOET_TEST_SCRIPT_BODY",
            "TONEPOET_TEST_SCRIPT_PATH",
            'script_path=${TONEPOET_TEST_SCRIPT_PATH:-"$0"}',
            'exec /bin/sh "$body" "$@"',
        ),
    )
    executable_fixture(
        "fixtures/test-bin/action-publication-lock-probe.sh",
        ("flock -n 9", "TONEPOET_ALBUM_DIR", "script_marker", "environment_marker"),
    )
    executable_fixture(
        "fixtures/test-bin/action-phase-marker.sh",
        ("TONEPOET_ACTION_PHASE", "prefix", "marker"),
    )
    executable_fixture("fixtures/test-bin/reviewed-code.sh", ("reviewed-code",))
    executable_fixture("fixtures/test-bin/replacement-code.sh", ("replacement-code",))
    executable_fixture(
        "fixtures/test-bin/foreground-editor-signal.sh",
        ("self_pgid", "parent_pgid", 'while [ ! -e "$folder/release" ]', "kill -TERM $$"),
    )
    executable_fixture("fixtures/test-bin/exit-7.sh", ("exit 7",))
    executable_fixture("fixtures/test-bin/bound-exact.sh", ("bound-exact",))
    executable_fixture("fixtures/test-bin/bound-exit-0-a.sh", ("exit 0",))
    executable_fixture("fixtures/test-bin/bound-exit-0-b.sh", ("exit 0",))
    executable_fixture(
        "fixtures/test-bin/bound-pump-exact.sh",
        ("cat >/dev/null", "bound-pump-exact"),
    )
    executable_fixture("fixtures/test-bin/bound-pump-noop-a.sh", ("cat >/dev/null",))
    executable_fixture("fixtures/test-bin/bound-pump-noop-b.sh", ("cat >/dev/null",))

    install = section(
        tool,
        "pub(crate) fn install_executable_test_script(path: &Path, body: &str)",
        "pub(crate) fn write_executable_test_script(name: &str, body: &str)",
    )
    require(
        "runtime-script-launcher.sh" in install and "symlink(&launcher, path)" in install,
        "runtime-variable fixture scripts execute through the pre-existing static launcher",
    )
    require(
        "fs::write(&body_path, body)" in install
        and "permissions().mode()" in install
        and "& 0o111" in install,
        "runtime-variable fixture bodies are written only as non-executable data and asserted non-executable",
    )
    require(
        "set_mode(" not in install and "set_permissions(" not in install and "Command::new" not in install,
        "fixture installer never publishes or self-executes a runtime-written executable inode",
    )
    writer = section(
        tool,
        "pub(crate) fn write_executable_test_script(name: &str, body: &str)",
        "// ===========================================================================\n// Real runner — PR 2",
    )
    require(
        "install_executable_test_script(&path, body);" in writer
        and "set_mode(" not in writer
        and "set_permissions(" not in writer
        and "ETXTBSY" not in writer
        and "Command::new" not in writer,
        "shared test-script writer removes the retry workaround and delegates to static-executable publication",
    )

    supervised_runner = section(
        tool,
        "pub(crate) async fn run_supervised_with_stdio(",
        "pub(crate) async fn run_with_binary_path(",
    )
    require(
        "#[cfg(test)]" in supervised_runner
        and "runtime_test_script_body_path(&launch_path)" in supervised_runner
        and '"TONEPOET_TEST_SCRIPT_BODY"' in supervised_runner
        and '"TONEPOET_TEST_SCRIPT_PATH"' in supervised_runner,
        "retained-FD tool execution carries the runtime body binding only in test builds",
    )

    bound_tests = section(
        tool,
        "async fn bound_execution_spawns_the_exact_attested_executable()",
        "fn real_runner_caches_detected_version()",
    )
    for fixture in (
        "bound-exact.sh",
        "bound-exit-0-a.sh",
        "bound-exit-0-b.sh",
        "bound-pump-exact.sh",
        "bound-pump-noop-a.sh",
        "bound-pump-noop-b.sh",
    ):
        require(
            f'static_bound_executable_fixture("{fixture}")' in bound_tests,
            f"identity-sensitive bound test uses immutable fixture: {fixture}",
        )
    require(
        "write_executable_script(" not in bound_tests
        and "runtime-script-launcher.sh" not in bound_tests,
        "identity-sensitive bound tests never canonicalize through the generic launcher indirection",
    )

    require(
        bluray.count("install_executable_test_script(") >= 2
        and "retrying fixture ffprobe spawn" not in bluray,
        "both executable ffprobe fixtures use the static launcher and the former ETXTBSY retry is gone",
    )
    for text, label in [
        (archive, "archive listing"),
        (command, "archive bulk-auth command"),
        (keybindings, "file-task helper"),
    ]:
        require(
            "install_executable_test_script" in text,
            f"{label} runtime-variable executable fixtures route through the static launcher",
        )

    require(
        "foreground-editor-signal.sh" in editor
        and "exit-7.sh" in editor
        and "install_executable_test_script" not in editor,
        "external-editor regressions use static regular executables so canonicalization cannot lose a runtime sidecar binding",
    )

    retained = section(
        supervisor,
        "fn retained_descriptor_executes_reviewed_script_after_path_replacement()",
        "fn post_album_environment_is_rebound_to_retained_cwd_after_parent_replacement()",
    )
    require(
        "reviewed-code.sh" in retained
        and "replacement-code.sh" in retained
        and "symlink(" in retained
        and "fs::write(&script" not in retained
        and "set_permissions" not in retained,
        "retained-descriptor replacement regression executes only pre-existing static script inodes",
    )

    post_action = section(
        stages,
        "async fn post_script_runs_after_publication_lock_release_under_action_authority()",
        "fn action_enabled_identity_persistence_remains_capability_bound()",
    )
    require(
        "action-publication-lock-probe.sh" in post_action
        and "std::fs::write(&script" not in post_action
        and "set_permissions" not in post_action,
        "publication-lock runscript regression uses a static regular executable",
    )
    prepost = section(
        stages,
        "impl PrePostLifecycleE2eFixture {",
        "async fn run_pre_post_lifecycle_once(",
    )
    require(
        "action-phase-marker.sh" in prepost
        and "std::fs::write(\n                &pre_script" not in prepost
        and "std::fs::write(\n                &post_script" not in prepost
        and "set_mode(0o700)" not in prepost,
        "PRE/POST lifecycle runscript fixture executes a static regular executable",
    )

    fixture_new = section(
        containment,
        "impl Fixture {",
        "fn next_token() -> String",
    )
    require(
        "runtime-script-launcher.sh" in fixture_new
        and "fixture-script.body" in fixture_new
        and "& 0o111" in fixture_new
        and "script_body" in fixture_new,
        "runscript-containment integration tests retain static executable identity and non-executable runtime bodies",
    )
    require(
        '"TONEPOET_TEST_SCRIPT_BODY"' in containment
        and '"TONEPOET_CRASH_SCRIPT_BODY"' in containment,
        "runscript-containment crash-driver path preserves the non-executable body binding across re-exec",
    )

    # The second observed R35 residual was a process-visible root under a
    # TempDir. Explicit scoped-root installation is now constrained to the
    # stable process-private test root, while affected fixtures use the
    # existing stable scoped helper directly.
    scoped_installer = section(
        concurrency,
        "pub(crate) fn install_scoped_test_coordination_root(",
        "impl Drop for ScopedTestCoordinationRootGuard",
    )
    require(
        "cargo_test_coordination_root()" in scoped_installer
        and "path.starts_with(&process_root)" in scoped_installer
        and "create_private_dir(path)" in scoped_installer,
        "explicit process-visible test roots are constrained beneath the stable per-process coordination root",
    )
    require(
        "stable_scoped_test_coordination_root_path" not in concurrency,
        "R35 reuses the existing scoped-root abstraction instead of adding a parallel test-root API",
    )
    for text, expected_count, label in [
        (memory_budget, 6, "memory-budget"),
        (file_tasks, 1, "file-task-runtime"),
        (keybindings, 2, "keybindings file-task fixtures"),
    ]:
        require(
            text.count("scoped_test_coordination_root()") >= expected_count
            and '.join("claims")' not in text,
            f"{label} process-visible roots no longer live under expiring TempDirs",
        )

    wait = section(
        concurrency,
        "pub(crate) fn wait_for_close_driven_untyped_descriptor_release(path: &Path, context: &str)",
        "/// Classify a descriptor after the owning subsystem has durably established",
    )
    require(
        "Duration::from_secs(5)" in wait
        and "try_lock_exclusive" in wait
        and "FileExt::unlock(&file)" in wait,
        "transient descriptor-probe drain is bounded and explicitly unlocks its successful probe OFD",
    )
    permanent_delete = section(
        keybindings,
        "fn permanent_delete_is_blocked_by_recovery_reserved_claim()",
        "fn permanent_delete_of_disjoint_paths_still_proceeds()",
    )
    probe_wait = "wait_for_close_driven_untyped_descriptor_release("
    require(
        probe_wait in permanent_delete,
        "permanent-delete regression retains the bounded post-observation probe drain",
    )
    require(
        permanent_delete.index("assert!(summary.busy")
        < permanent_delete.index(probe_wait)
        < permanent_delete.index("retire_descriptor_after_lifecycle_release("),
        "permanent-delete regression drains only its post-observation probe inheritance before fixture retirement",
    )
    require(
        "wait_for_close_driven_untyped_descriptor_release" not in section(
            concurrency,
            "fn acquire_existing_for_lifecycle_retirement(",
            "fn acquire_existing_with_transient_contention(",
        ),
        "production lifecycle retirement remains free of the R35 test-only probe wait",
    )

    # No static fixture used to satisfy user-runscript validation may be a symlink.
    for relative in (
        "fixtures/test-bin/action-publication-lock-probe.sh",
        "fixtures/test-bin/action-phase-marker.sh",
    ):
        require(not (ROOT / relative).is_symlink(), f"user-runscript fixture is a regular file, not a symlink: {relative}")

    print("R35 runtime-written executable and residual-race static verification passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}")
        raise SystemExit(1)
