#!/usr/bin/env python3
"""Static invariants for round-6-r1 artwork-picker host admission.

This verifier pins the narrow F1 integration correction on top of round 6.  It
is deliberately source-structural: it proves that every picker mutation crosses
an explicit request boundary and that Tonepoet dispatches those requests through
its existing coordination paths.  It does not substitute for compiling or for
the repeated Rust acceptance gate.
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


def function(text: str, name: str) -> str:
    marker = f"fn {name}("
    begin = text.index(marker)
    brace = text.index("{", begin)
    depth = 0
    i = brace
    while i < len(text):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[begin : i + 1]
        i += 1
    raise AssertionError(f"unterminated function {name}")


def test(text: str, name: str) -> str:
    return function(text, name)


def require(condition: bool, label: str) -> None:
    if not condition:
        raise AssertionError(label)
    print(f"[ok] {label}")


def contains_all(text: str, needles: tuple[str, ...]) -> bool:
    return all(needle in text for needle in needles)


def main() -> int:
    picker = read("crates/tui-file-picker/src/state.rs")
    picker_input = read("crates/tui-file-picker/src/input.rs")
    picker_guard = read("crates/tui-file-picker/src/source_guard.rs")
    keybindings = read("src/tui/keybindings.rs")
    event_loop = read("src/tui/event_loop.rs")
    app = read("src/tui/app.rs")
    audit = read("tools/audit_concurrent_mutation_entrypoints.py")
    gate = read("scripts/validate_concurrency_round6.sh")

    # Public compatibility: the opt-in is additive rather than another required
    # FilePickerConfig field.  Direct remains the legacy constructor; the
    # host-managed constructor installs its mode before initial refresh.
    config = section(picker, "pub struct FilePickerConfig {", "impl PartialEq for FilePickerConfig")
    require(
        "mutation_execution" not in config,
        "picker public FilePickerConfig surface is not source-broken by the host mode",
    )
    picker_state_impl = section(
        picker,
        "impl FilePickerState {",
        "    fn swap_tab_shared_state",
    )
    direct_ctor = section(
        picker_state_impl,
        "pub fn new(config: FilePickerConfig)",
        "    /// Construct a picker whose embedding host owns every filesystem mutation.",
    )
    host_ctor = section(
        picker_state_impl,
        "pub fn new_host_managed(config: FilePickerConfig)",
        "    fn new_with_mutation_execution",
    )
    internal_begin = picker_state_impl.index("fn new_with_mutation_execution")
    internal_ctor = picker_state_impl[internal_begin:]
    require(
        "FilePickerMutationExecution::Direct" in direct_ctor
        and "new_with_mutation_execution" in direct_ctor,
        "standalone picker construction keeps direct execution as the default",
    )
    require(
        "FilePickerMutationExecution::HostManaged" in host_ctor
        and "new_with_mutation_execution" in host_ctor,
        "host-managed picker construction is an explicit opt-in before first refresh",
    )
    require(
        "mutation_execution," in internal_ctor
        and internal_ctor.find("mutation_execution,") < internal_ctor.find("state.refresh();"),
        "host execution mode is installed before FilePickerState initial refresh",
    )

    refresh = function(picker, "refresh")
    require(
        contains_all(
            refresh,
            (
                "FilePickerMutationExecution::Direct",
                "permits_filesystem_mutation",
                "recover_interrupted_verified_removals_once",
            ),
        ),
        "host-managed refresh cannot execute picker-owned interrupted-removal recovery",
    )

    request_enum = section(
        picker,
        "pub enum FilePickerHostMutationRequest {",
        "impl FilePickerHostMutationRequest",
    )
    for variant in ("Create", "Rename", "Duplicate", "Paste", "Delete", "CaseRename"):
        require(f"    {variant}" in request_enum, f"host request boundary covers {variant}")

    mutation_functions = {
        "try_create_named_item": ("HostManaged", "FilePickerHostMutationRequest::Create", "queue_host_mutation"),
        "try_rename_current": ("HostManaged", "FilePickerHostMutationRequest::Rename", "queue_host_mutation"),
        "duplicate_action_paths": ("HostManaged", "FilePickerHostMutationRequest::Duplicate", "queue_host_mutation"),
        "try_duplicate_current": ("HostManaged", "FilePickerHostMutationRequest::Duplicate", "queue_host_mutation"),
        "try_paste_clipboard_to": ("HostManaged", "FilePickerHostMutationRequest::Paste", "queue_host_mutation"),
        "try_confirm_delete": ("HostManaged", "FilePickerHostMutationRequest::Delete", "queue_host_mutation"),
        "apply_path_case_transform": (
            "HostManaged",
            "FilePickerHostMutationRequest::CaseRename",
            "queue_host_mutation",
        ),
    }
    for name, needles in mutation_functions.items():
        body = function(picker, name)
        require(contains_all(body, needles), f"{name} yields a host request before direct mutation")

    create = function(picker, "try_create_named_item")
    duplicate_many = function(picker, "duplicate_action_paths")
    duplicate_named = function(picker, "try_duplicate_current")
    duplicate_begin = function(picker, "begin_duplicate_path")
    paste_intent = function(picker, "try_paste_clipboard_to")
    case_plan = function(picker, "plan_picker_case_rename_transaction")
    require(
        create.find("FilePickerMutationExecution::HostManaged") < create.find("path.exists()"),
        "host-managed create yields intent before destination existence probing",
    )
    require(
        duplicate_many.find("FilePickerMutationExecution::HostManaged")
        < duplicate_many.find("duplicate_files_in_place"),
        "host-managed bulk duplicate yields sources before picker filesystem planning",
    )
    require(
        duplicate_named.find("FilePickerMutationExecution::HostManaged")
        < duplicate_named.find("source.is_file()")
        and duplicate_named.find("FilePickerMutationExecution::HostManaged")
        < duplicate_named.find("destination.exists()"),
        "host-managed named duplicate defers source/destination filesystem checks to Tonepoet",
    )
    require(
        contains_all(
            duplicate_begin,
            (
                "cached_is_directory",
                "unique_path_from_cached_entries",
                "FilePickerMutationExecution::Direct && !path.is_file()",
            ),
        ),
        "opening the host-managed duplicate editor uses cached picker state rather than filesystem probes",
    )
    require(
        paste_intent.find("FilePickerMutationExecution::HostManaged")
        < paste_intent.find("target_dir.is_dir()")
        and paste_intent.find("FilePickerMutationExecution::HostManaged")
        < paste_intent.find("plan_filesystem_paste_with_retry"),
        "host-managed paste yields clipboard intent before target validation or suffix planning",
    )
    require(
        ".exists()" not in case_plan,
        "host-managed case-rename request planning is lexical and performs no destination probe",
    )

    for name in ("handle_key", "handle_mouse"):
        body = function(picker_input, name)
        require(
            "host_mutation_in_flight()" in body
            and "host-managed filesystem operation is still running" in body,
            f"picker {name} blocks competing UI actions while host mutation is in flight",
        )

    artwork_open = function(keybindings, "metadata_editor_open_artwork_picker")
    artwork_policy = function(keybindings, "artwork_file_picker_policy")
    require(
        "FilePickerState::new_host_managed" in artwork_open
        and "FileOperationPolicy::default()" in artwork_policy
        and "selection_only" not in artwork_policy,
        "only the artwork surface keeps full file-manager authority behind host execution",
    )
    require(
        keybindings.count("FilePickerState::new_host_managed(") == 1,
        "Tonepoet production keybindings opt only the artwork picker into host-managed mode",
    )

    post_input = function(keybindings, "finish_metadata_file_picker_input")
    key_input = function(keybindings, "handle_metadata_file_picker_key")
    mouse_input = function(keybindings, "handle_metadata_file_picker_mouse")
    require(
        contains_all(
            post_input,
            (
                "take_host_mutation_request",
                "FilePickerPurpose::SelectArtwork",
                "dispatch_artwork_picker_host_mutation",
                "complete_host_mutation_failure",
            ),
        )
        and "finish_metadata_file_picker_input" in key_input
        and "finish_metadata_file_picker_input" in mouse_input,
        "keyboard and mouse artwork mutations share the same fail-closed host dispatch boundary",
    )

    sync = function(keybindings, "execute_artwork_picker_host_mutation_sync")
    rename = function(keybindings, "execute_artwork_picker_rename_request")
    paste = function(keybindings, "start_artwork_picker_host_paste")
    require(
        contains_all(
            sync,
            (
                "PathResolutionSemantics::NamespaceObject",
                "MutationClaimGuard::acquire_ephemeral",
                "plan_duplicate_files_in_place",
                "file_task_path_admission(false, &mappings)",
                "execute_duplicate_plan",
                "delete_path_with_policy",
            ),
        ),
        "create/duplicate/delete requests reuse Tonepoet shared claim and admission primitives",
    )
    require(
        contains_all(rename, ("RenamePlan::new", "validate_plan", "execute_plan_with_proofs_at_verification")),
        "rename and case-rename reuse the existing claimed RenamePlan executor",
    )
    require(
        contains_all(
            paste,
            (
                "plan_filesystem_paste_for_dispatch",
                "BrowsePasteRetryPlan::from_plan",
                "start_file_op",
                "artwork_picker_file_tasks.insert",
                "Some(destinations)",
            ),
        ),
        "paste/cut-move reuse the hosted file-task worker with frozen destinations and retry authority",
    )

    app_task = section(app, "pub struct ArtworkPickerFileTask", "pub struct FileTransferQueueState")
    require(
        "picker_session_id" in app_task
        and "FilePickerHostMutationRequest" in app_task
        and "PastePlan" in app_task,
        "hosted paste retains picker/session correlation until terminal worker reconciliation",
    )
    reconcile = function(event_loop, "reconcile_artwork_picker_file_task")
    require(
        contains_all(
            reconcile,
            (
                "root.source == mapping.source",
                "artwork_picker_paste_retries",
                "complete_host_paste",
            ),
        ),
        "hosted paste reconciles reports by stable lexical source identity and retains exact retry authority",
    )

    recovery_test = test(
        picker_guard,
        "host_managed_picker_refresh_never_restores_interrupted_removal_without_host_admission",
    )
    require(
        "FilePickerState::new_host_managed" in recovery_test
        and "must not perform unclaimed recovery" in recovery_test
        and "quarantine.exists()" in recovery_test
        and "journal.exists()" in recovery_test,
        "focused recovery regression proves host-managed construction/refresh leaves detached recovery untouched",
    )

    focused_picker = (
        "host_managed_create_defers_destination_existence_to_host",
        "host_managed_named_duplicate_defers_type_and_destination_checks_to_host",
        "host_managed_paste_defers_target_validation_and_suffix_planning_to_host",
        "host_managed_picker_refresh_never_restores_interrupted_removal_without_host_admission",
    )
    focused_tonepoet = (
        "artwork_host_create_rejects_conflicting_tonepoet_claim_then_succeeds",
        "artwork_host_rename_rejects_conflicting_tonepoet_claim_then_succeeds",
        "artwork_host_duplicate_rejects_conflicting_tonepoet_claim_then_succeeds",
        "artwork_host_delete_rejects_conflicting_tonepoet_claim_then_succeeds_with_picker_policy",
        "artwork_host_paste_routes_exact_plan_to_hosted_file_task_worker",
        "artwork_host_delete_preserves_non_recursive_picker_delete_policy",
    )
    for name in focused_picker:
        require(f"fn {name}" in picker or f"fn {name}" in picker_guard, f"focused F1 regression exists: {name}")
    for name in focused_tonepoet:
        require(f"fn {name}" in keybindings, f"focused F1 regression exists: {name}")

    require(
        "artwork picker host-managed mutation boundary" in audit
        and "artwork picker mutations join Tonepoet shared coordination" in audit
        and "handle_metadata_file_picker_mouse" in audit
        and "FilePickerState::new_host_managed" in audit,
        "mutation-entrypoint audit enforces authority plus host admission rather than an F1 exemption",
    )

    require(
        "tools/verify_concurrency_corrective_round6_r1.py" in gate,
        "dynamic gate executes the round-6-r1 static verifier",
    )
    for name in focused_picker + focused_tonepoet:
        require(name in gate, f"dynamic gate executes focused F1 test: {name}")
    require(
        "for run in $(seq 1 5)" in gate
        and "cargo test --workspace --no-fail-fast" in gate
        and "for run in $(seq 1 50)" in gate
        and "pipeline_reports_consumer_nonzero_and_closes_producer" in gate,
        "round-6-r1 gate preserves the five-run workspace and 50x SIGPIPE acceptance bar",
    )

    print("round-6-r1 F1 host-admission static verification passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
