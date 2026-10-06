#!/usr/bin/env python3
"""Static acceptance checks for R31 test-registry isolation.

R31 fixes the R30 suite deadlock without changing production coordination
semantics. Ordinary coordination-touching tests must use a nonblocking
thread-local registry, while the older process-visible fixture remains available
only where worker/process participants really must share one registry.
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


def main() -> int:
    concurrency = read("src/concurrency.rs")
    audit = read("tools/audit_test_coordination_isolation.py")
    probe = read("src/tui/probe.rs")
    app = read("src/tui/app.rs")

    local_state = section(
        concurrency,
        "static ISOLATED_TEST_COORDINATION_ROOT_STACK",
        "/// Thread-owned root override",
    )
    require("thread_local!" in concurrency[: concurrency.index("/// Thread-owned root override")],
            "R31 local registry state is thread-local")
    require("RefCell<Vec<PathBuf>>" in local_state,
            "R31 local registry keeps a per-thread stack")
    require("PhantomData<std::rc::Rc<()>>" in concurrency,
            "R31 thread-local guard is non-Send and cannot retire on another OS thread")

    local_fixture = section(
        concurrency,
        "pub(crate) fn isolated_test_coordination_root()",
        "#[cfg(test)]\nimpl Drop for IsolatedTestCoordinationRootGuard",
    )
    require("test_coordination_serial" not in local_fixture,
            "R31 ordinary isolation must never take the legacy global serial mutex")
    require("std::env::" not in local_fixture and "TONEPOET_CONCURRENCY_DIR" not in local_fixture,
            "R31 ordinary isolation must not mutate process-global environment")
    require("current_owned_scoped_test_coordination_root()" in local_fixture,
            "nested local scope inside its owning process fixture reuses that registry")
    require('.join("isolated")' in local_fixture and "Uuid::new_v4().to_string()" in local_fixture,
            "ordinary test roots are unique beneath the process-private cargo-test root")
    require("create_private_dir(&root)" in local_fixture,
            "ordinary isolated registry is created before use")

    owned_scope = section(
        concurrency,
        "fn current_owned_scoped_test_coordination_root()",
        "/// Nonblocking, thread-local registry isolation",
    )
    require("current.owner == owner" in owned_scope,
            "nested reuse recognizes only the process-scope owner thread")

    root_selector = section(concurrency, "pub fn coordination_root()", "fn create_private_dir")
    isolated_index = root_selector.index("current_isolated_test_coordination_root()")
    process_index = root_selector.index("current_scoped_test_coordination_root()")
    narrow_index = root_selector.index("current_test_coordination_root_override()")
    require(isolated_index < process_index < narrow_index,
            "thread-local R31 isolation outranks an unrelated process-visible root")

    regression = section(
        concurrency,
        "fn isolated_test_coordination_root_outranks_unrelated_process_scope_without_waiting()",
        "fn nested_isolated_test_coordination_root_reuses_one_thread_registry()",
    )
    require("scoped_test_coordination_root()" in regression,
            "R31 regression holds the legacy process-visible fixture")
    require("std::thread::spawn" in regression and "isolated_test_coordination_root()" in regression,
            "R31 regression exercises a concurrent ordinary test thread")
    require("recv_timeout(Duration::from_secs(2))" in regression,
            "R31 regression is bounded and cannot turn a failure into a suite hang")
    require("drop(process_scope);" in regression and "worker.join()" in regression,
            "R31 regression releases the legacy mutex before joining on timeout")

    nested = section(
        concurrency,
        "fn nested_isolated_test_coordination_root_reuses_one_thread_registry()",
        "fn scoped_test_coordination_root_retirement_keeps_captured_family_path_alive()",
    )
    require(nested.count("isolated_test_coordination_root()") == 2,
            "R31 nested-scope regression exercises re-entrant local isolation")
    require("assert_eq!(inner.path(), expected.as_path())" in nested,
            "nested local scopes reuse one registry rather than partitioning a test")

    startup = section(
        app,
        "fn quiescent_v23_startup_activates_v24_protocol_then_current_schema_without_fallback()",
        "}\n}\n\n#[cfg(test)]\nmod queue_persistence_boundary_tests",
    )
    require("isolated_test_coordination_root()" in startup,
            "R31 restores the startup test's pre-R30 coordination isolation without global serialization")
    require("scoped_test_coordination_root()" not in startup,
            "startup isolation must not reintroduce the R30 process-visible serial fixture")
    require(startup.index("isolated_test_coordination_root()") < startup.index("test_environment_lock()"),
            "startup test installs its nonblocking coordination root before process-global test environment state")

    special = section(
        probe,
        "fn flac_overflow_rewrites_are_serialized_across_parallel_workers()",
        "fn flac_overflow_rewrite_preserves_mode_and_timestamps()",
    )
    require("scoped_test_coordination_root()" in special,
            "the real parallel metadata-writer regression retains process-visible coordination")
    require(special.count("std::thread::spawn") >= 2,
            "parallel metadata-writer regression still exercises two worker threads")

    require('"isolated_test_coordination_root()"' in audit,
            "coordination audit recognizes the R31 nonblocking fixture")
    require("thread-local coordination scope cannot cover" in audit,
            "audit rejects the local fixture when coordination crosses a worker/task boundary")
    require("PROCESS_VISIBLE_ENTRYPOINTS" in audit
            and "save_through_(?:production_path|explicit_embedded_cue_path)" in audit,
            "audit recognizes the background metadata-save helper boundary")
    require("COORDINATION_FIXTURE_SELF_TEST_EXEMPTIONS" in audit,
            "audit narrowly exempts the fixture interaction self-tests")

    local_test_lines = sum(
        path.read_text(encoding="utf-8").count(
            "let _coordination = crate::concurrency::isolated_test_coordination_root();"
        )
        for path in (ROOT / "src").rglob("*.rs")
    )
    require(local_test_lines == 151,
            f"R31 must retain exactly 151 ordinary thread-local test scopes, found {local_test_lines}")

    keybindings = read("src/tui/keybindings.rs")
    background_save_tests = [
        "multifile_sidecar_untagged_carriers_save_album_performer_and_comment",
        "sidecar_albumartist_multivalue_projection_warns_without_failing_or_staying_dirty",
        "nine_file_dff_headerless_sidecar_edits_apply_persist_and_reopen_clean",
        "single_image_multitrack_dff_edits_persist_through_production_save",
        "multifile_shn_sidecar_exercises_edit_and_save_without_format_special_cases",
        "extra_unreferenced_dff_stays_separate_while_sidecar_album_saves_cleanly",
    ]
    for index, name in enumerate(background_save_tests):
        start = f"async fn {name}()"
        begin = keybindings.index(start)
        finish = (
            keybindings.index("#[", begin + len(start))
            if "#[" in keybindings[begin + len(start):]
            else len(keybindings)
        )
        body = keybindings[begin:finish]
        require("scoped_test_coordination_root()" in body,
                f"background-save regression {name} must use process-visible coordination")
        require("isolated_test_coordination_root()" not in body,
                f"background-save regression {name} must not use a thread-local coordination root")

    # Guard against reintroducing the R30 remedy wholesale. Count both the
    # ordinary scoped helper and explicit scoped-root installation: both are
    # process-visible coordination, and ignoring the explicit form undercounted
    # the actual R31 population. R35 migrates six TempDir-backed explicit roots
    # to the stable helper without changing that true population.
    audit_prefix = audit.split("\nfailures: list[str] = []", 1)[0]
    namespace: dict[str, object] = {"__file__": str((ROOT / "tools/audit_test_coordination_isolation.py").resolve())}
    exec(audit_prefix, namespace)
    test_functions = namespace["test_functions"]
    process_scoped_tests = 0
    for path in (ROOT / "src").rglob("*.rs"):
        for _name, body, _line in test_functions(path):
            if (
                "scoped_test_coordination_root()" in body
                or "install_scoped_test_coordination_root(" in body
            ):
                process_scoped_tests += 1
    require(process_scoped_tests == 233,
            f"R31 process-visible test-scope population changed unexpectedly: {process_scoped_tests}")

    print("[ok] R31 nonblocking test coordination isolation static verification passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, ValueError) as error:
        print(f"[FAIL] {error}")
        raise SystemExit(1)
