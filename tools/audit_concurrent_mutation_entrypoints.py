#!/usr/bin/env python3
"""Focused concurrent-session mutation-entrypoint audit.

This audits user-library/output mutation boundaries and every production
external-command construction site. External launches are explicitly classified
as supervised mutation-capable commands, internal helpers carrying durable
authority, read-only/UI probes, or scratch/workspace producers. Process-private
files and configuration writes remain outside the user-library path registry.
"""
from pathlib import Path
from collections import Counter
import re
import sys

ROOT = Path(__file__).resolve().parents[1]


def text(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def function_body(rel: str, name: str) -> str:
    source = text(rel)
    marker = f"fn {name}"
    start = source.find(marker)
    if start < 0:
        raise AssertionError(f"{rel}: missing function {name}")
    brace = source.find("{", start)
    if brace < 0:
        raise AssertionError(f"{rel}: missing body for {name}")
    depth = 0
    for index in range(brace, len(source)):
        ch = source[index]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return source[start:index + 1]
    raise AssertionError(f"{rel}: unterminated body for {name}")


def require(label: str, condition: bool, detail: str) -> None:
    if not condition:
        raise AssertionError(f"{label}: {detail}")
    print(f"[ok] {label}")


def contains_all(haystack: str, needles: list[str]) -> bool:
    return all(needle in haystack for needle in needles)


PROCESS_COMMAND_RE = re.compile(
    r"(?:std::process::Command|tokio::process::Command|"
    r"(?<![A-Za-z0-9_])ProcessCommand|(?<![A-Za-z0-9_])TokioCommand|"
    r"(?<![A-Za-z0-9_])Command)::new\s*\("
)
PROCESS_TERMINAL_RE = re.compile(r"\.(?:spawn|status|output)\s*\(")
PROCESS_COMMAND_RETURN_RE = re.compile(
    r"->\s*(?:std::process::Command|tokio::process::Command)\b"
)
LOW_LEVEL_EXEC_RE = re.compile(r"\blibc::(?:fexecve|execve)\s*\(")


def _matching_brace(source: str, opening: int) -> int:
    """Find a Rust block end while ignoring comments and string literals."""
    depth = 0
    index = opening
    block_comment_depth = 0
    in_line_comment = False
    in_string = False
    string_escape = False
    raw_hashes = None
    while index < len(source):
        ch = source[index]
        nxt = source[index + 1] if index + 1 < len(source) else ""

        if in_line_comment:
            if ch == "\n":
                in_line_comment = False
            index += 1
            continue
        if block_comment_depth:
            if ch == "/" and nxt == "*":
                block_comment_depth += 1
                index += 2
                continue
            if ch == "*" and nxt == "/":
                block_comment_depth -= 1
                index += 2
                continue
            index += 1
            continue
        if raw_hashes is not None:
            if ch == '"' and source.startswith("#" * raw_hashes, index + 1):
                index += 1 + raw_hashes
                raw_hashes = None
            index += 1
            continue
        if in_string:
            if string_escape:
                string_escape = False
            elif ch == "\\":
                string_escape = True
            elif ch == '"':
                in_string = False
            index += 1
            continue

        if ch == "'":
            # Skip ordinary/escaped Rust character literals without treating
            # lifetime syntax such as `'a` as a character.
            if index + 2 < len(source) and source[index + 2] == "'":
                index += 3
                continue
            if (
                index + 3 < len(source)
                and source[index + 1] == "\\"
                and source[index + 3] == "'"
            ):
                index += 4
                continue
        if ch == "/" and nxt == "/":
            in_line_comment = True
            index += 2
            continue
        if ch == "/" and nxt == "*":
            block_comment_depth = 1
            index += 2
            continue
        if ch == "r":
            probe = index + 1
            hashes = 0
            while probe < len(source) and source[probe] == "#":
                hashes += 1
                probe += 1
            if probe < len(source) and source[probe] == '"':
                raw_hashes = hashes
                index = probe + 1
                continue
        if ch == '"':
            in_string = True
            index += 1
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return index
        index += 1
    raise AssertionError("unterminated Rust block while scanning subprocess inventory")


def strip_cfg_test_modules(source: str) -> str:
    """Blank conventional cfg(test) modules while preserving offsets/newlines."""
    chars = list(source)
    pattern = re.compile(
        r"#\s*\[\s*cfg\s*\([^\]]*\btest\b[^\]]*\)\s*\]\s*"
        r"(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z0-9_]+\s*\{",
        re.S,
    )
    for match in list(pattern.finditer(source)):
        opening = source.find("{", match.start(), match.end() + 1)
        closing = _matching_brace(source, opening)
        for index in range(match.start(), closing + 1):
            if chars[index] != "\n":
                chars[index] = " "
    return "".join(chars)


def external_constructor_count(source: str) -> int:
    return len(PROCESS_COMMAND_RE.findall(source))


def low_level_exec_count(source: str) -> int:
    return len(LOW_LEVEL_EXEC_RE.findall(source))


def impl_function_body(rel: str, owner: str, name: str) -> str:
    """Return a named method from a specific inherent impl block."""
    source = text(rel)
    impl_match = re.search(rf"\bimpl\s+{re.escape(owner)}\s*\{{", source)
    if impl_match is None:
        raise AssertionError(f"{rel}: missing impl {owner}")
    impl_open = source.find("{", impl_match.start(), impl_match.end())
    impl_close = _matching_brace(source, impl_open)
    impl_source = source[impl_open + 1:impl_close]
    method_match = re.search(rf"\bfn\s+{re.escape(name)}\b", impl_source)
    if method_match is None:
        raise AssertionError(f"{rel}: missing {owner}::{name}")
    method_start = impl_open + 1 + method_match.start()
    method_open = source.find("{", method_start, impl_close)
    if method_open < 0:
        raise AssertionError(f"{rel}: missing body for {owner}::{name}")
    method_close = _matching_brace(source, method_open)
    return source[method_start:method_close + 1]


def reviewed_function_body(rel: str, owner, name: str) -> str:
    if owner is None:
        return function_body(rel, name)
    return impl_function_body(rel, owner, name)


def workspace_production_source_roots() -> list[Path]:
    """Root package plus every Cargo workspace member's production src tree."""
    cargo = text("Cargo.toml")
    section = re.search(r"(?ms)^\[workspace\]\s*(.*?)(?=^\[|\Z)", cargo)
    if section is None:
        raise AssertionError("Cargo.toml: missing [workspace] section")
    members_match = re.search(r"members\s*=\s*\[(.*?)\]", section.group(1), re.S)
    if members_match is None:
        raise AssertionError("Cargo.toml: [workspace] is missing members")
    members = re.findall(r'"([^"]+)"', members_match.group(1))
    if not members:
        raise AssertionError("Cargo.toml: workspace member inventory is empty")

    roots = [ROOT / "src"]
    for member in members:
        source_root = ROOT / member / "src"
        if not source_root.is_dir():
            raise AssertionError(f"workspace member has no production src tree: {member}")
        roots.append(source_root)
    return roots


def production_rust_files() -> list[Path]:
    files: set[Path] = set()
    for source_root in workspace_production_source_roots():
        files.update(source_root.rglob("*.rs"))
    return sorted(files)


# Reviewed root-application subprocess inventory. The count is per named function, so
# adding another direct construction even inside an already-classified function
# forces a new review decision. cfg(test) modules are excluded from the global
# production count below.
EXTERNAL_LAUNCH_INVENTORY = [
    ("src/convert/pipeline/tool.rs", "detect_tool_version", 1, "read_only_probe"),
    ("src/convert/script_supervisor.rs", "run_supervised", 1, "supervisor_boundary"),
    ("src/convert/script_supervisor.rs", "start", 1, "supervisor_boundary"),
    ("src/convert/script_supervisor.rs", "spawn_launcher", 1, "supervisor_internal"),
    ("src/disc/bluray_mapper.rs", "ffprobe_bluray_playlist_audio_streams", 1, "read_only_probe"),
    ("src/lib.rs", "detect_7z_binary", 2, "read_only_probe"),
    ("src/main.rs", "run_check_tools", 1, "read_only_probe"),
    ("src/tui/accuraterip.rs", "probe_sample_count_wvunpack", 1, "read_only_probe"),
    ("src/tui/accuraterip.rs", "decode_to_raw_i16_wvunpack", 1, "read_only_probe"),
    ("src/tui/accuraterip.rs", "tool_exists", 1, "read_only_probe"),
    ("src/tui/accuraterip.rs", "encode_corrected_track", 1, "scratch_workspace"),
    ("src/tui/accuraterip.rs", "encode_via_ffmpeg", 1, "scratch_workspace"),
    ("src/tui/accuraterip.rs", "copy_metadata_metaflac", 4, "scratch_workspace"),
    ("src/tui/analyze.rs", "measure_loudness", 1, "read_only_probe"),
    ("src/tui/analyze.rs", "detect_hdcd", 1, "read_only_probe"),
    ("src/tui/archive_listing.rs", "list_archive_with_options", 1, "read_only_probe"),
    ("src/tui/bit_compare.rs", "spawn_decoder", 1, "read_only_probe"),
    ("src/tui/browse.rs", "extract_archive_entry_to_temp", 1, "scratch_workspace"),
    ("src/tui/browse.rs", "extract_archive_entry_to_temp_blocking", 1, "scratch_workspace"),
    ("src/tui/browse.rs", "run_7z_extract_to_dir", 1, "scratch_workspace"),
    ("src/tui/cue_parser.rs", "extract_single_image_tracks", 2, "scratch_workspace"),
    ("src/tui/cue_parser.rs", "can_ffmpeg_read", 1, "read_only_probe"),
    ("src/tui/external_editor.rs", "run_read_only_command", 1, "read_only_probe"),
    ("src/tui/external_editor.rs", "which", 1, "read_only_probe"),
    ("src/tui/host_clipboard.rs", "run_clipboard_write", 1, "ui_helper"),
    ("src/tui/host_clipboard.rs", "run_clipboard_read", 1, "ui_helper"),
    ("src/tui/keybindings.rs", "supervise_file_task_process", 1, "internal_helper_durable_lease"),
    ("src/tui/keychain.rs", "test_password", 1, "ui_helper"),
    ("src/tui/probe.rs", "capture_acl_text", 1, "read_only_probe"),
    ("src/tui/probe.rs", "apply_acl_text", 1, "scratch_workspace"),
    ("src/tui/probe.rs", "command_exists_for_metadata", 1, "read_only_probe"),
    ("src/tui/tmux_clipboard.rs", "detected_tmux_version", 1, "ui_helper"),
    ("src/tui/tmux_clipboard.rs", "apply_if_enabled", 1, "ui_helper"),
    ("src/tui/verify.rs", "verify_flac", 1, "read_only_probe"),
    ("src/tui/verify.rs", "verify_wavpack", 1, "read_only_probe"),
    ("src/tui/verify.rs", "verify_ffmpeg", 1, "read_only_probe"),
]


# Workspace-member process constructions that are outside the root package.
# Mutation-capable tonepoet-backend execution APIs are intentionally inventoried
# but classified inactive: the protocol-aware application must not call them.
WORKSPACE_EXTERNAL_LAUNCH_INVENTORY = [
    ("crates/tonepoet-backend/src/lib.rs", "CommandBuilder", "is_available", 2, "read_only_probe"),
    ("crates/tonepoet-backend/src/integration_api.rs", "ConversionBackend", "check_tool_available", 2, "read_only_probe"),
    ("crates/tonepoet-backend/src/types.rs", "ConversionCommand", "execute", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/types.rs", "ConversionCommand", "execute_with_timeout", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/types.rs", "ConversionCommand", "execute_ffmpeg_with_progress", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/types.rs", "ConversionCommand", "execute_sox_with_progress", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/types.rs", "ConversionCommand", "execute_with_proportion_progress", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/types.rs", "ConversionCommand", "execute_with_estimated_progress", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/types.rs", None, "get_audio_metadata", 1, "read_only_probe"),
    ("crates/tonepoet-backend/src/metadata.rs", "FlacMetadataExtractor", "extract", 1, "read_only_probe"),
    ("crates/tonepoet-backend/src/metadata.rs", "FlacMetadataApplier", "apply", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/metadata.rs", "WavPackMetadataExtractor", "extract", 1, "read_only_probe"),
    ("crates/tonepoet-backend/src/metadata.rs", "WavPackMetadataApplier", "apply", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/metadata.rs", "OpusMetadataExtractor", "extract", 1, "read_only_probe"),
    ("crates/tonepoet-backend/src/metadata.rs", "OpusMetadataApplier", "apply", 1, "inactive_library_api"),
    ("crates/tonepoet-backend/src/metadata.rs", "AacMetadataExtractor", "find_atomicparsley", 1, "read_only_probe"),
    ("crates/tonepoet-backend/src/metadata.rs", "AacMetadataExtractor", "extract", 1, "read_only_probe"),
    ("crates/tonepoet-backend/src/metadata.rs", "AacMetadataApplier", "apply", 1, "inactive_library_api"),
]

# Low-level external transitions are inventoried separately from Command::new.
# Both platform-specific calls are the final reviewed-script exec inside the
# established retained-descriptor supervisor launcher.
LOW_LEVEL_EXEC_INVENTORY = [
    ("src/convert/script_supervisor.rs", None, "exec_retained_script", 2, "supervisor_contained_exec"),
]


def audit_external_launch_inventory() -> bool:
    allowed_categories = {
        "supervisor_boundary",
        "supervisor_internal",
        "supervisor_contained_exec",
        "internal_helper_durable_lease",
        "inactive_library_api",
        "read_only_probe",
        "scratch_workspace",
        "ui_helper",
    }
    expected_by_file = Counter()
    for rel, function, count, category in EXTERNAL_LAUNCH_INVENTORY:
        if category not in allowed_categories:
            raise AssertionError(f"unreviewed external launch category {category}: {rel}::{function}")
        body = function_body(rel, function)
        actual = external_constructor_count(body)
        if actual != count:
            raise AssertionError(
                f"external launch inventory drift in {rel}::{function}: expected {count}, found {actual}"
            )
        terminal_count = len(PROCESS_TERMINAL_RE.findall(body))
        if terminal_count < count:
            raise AssertionError(
                f"external launch inventory has {count} constructors but only {terminal_count} spawn/status/output terminals in {rel}::{function}"
            )
        expected_by_file[rel] += count

    for rel, owner, function, count, category in WORKSPACE_EXTERNAL_LAUNCH_INVENTORY:
        if category not in allowed_categories:
            raise AssertionError(
                f"unreviewed workspace external launch category {category}: {rel}::{owner or ''}{function}"
            )
        body = reviewed_function_body(rel, owner, function)
        actual = external_constructor_count(body)
        if actual != count:
            qualifier = f"{owner}::" if owner else ""
            raise AssertionError(
                f"workspace external launch inventory drift in {rel}::{qualifier}{function}: expected {count}, found {actual}"
            )
        terminal_count = len(PROCESS_TERMINAL_RE.findall(body))
        if terminal_count < count:
            qualifier = f"{owner}::" if owner else ""
            raise AssertionError(
                f"workspace launch inventory has {count} constructors but only {terminal_count} spawn/status/output terminals in {rel}::{qualifier}{function}"
            )
        expected_by_file[rel] += count

    expected_exec_by_file = Counter()
    for rel, owner, function, count, category in LOW_LEVEL_EXEC_INVENTORY:
        if category != "supervisor_contained_exec":
            raise AssertionError(
                f"low-level external exec requires supervisor_contained_exec classification: {rel}::{function}"
            )
        if rel != "src/convert/script_supervisor.rs" or function != "exec_retained_script":
            raise AssertionError(
                f"reviewed-script low-level exec moved outside the established supervisor launcher: {rel}::{function}"
            )
        body = reviewed_function_body(rel, owner, function)
        actual = low_level_exec_count(body)
        if actual != count or not contains_all(body, ["script_fd", "libc::fexecve", "libc::execve"]):
            raise AssertionError(
                f"supervisor-contained external exec inventory drift in {rel}::{function}: expected {count}, found {actual}"
            )
        expected_exec_by_file[rel] += count

    discovered_by_file = Counter()
    discovered_exec_by_file = Counter()
    for path in production_rust_files():
        rel = str(path.relative_to(ROOT))
        production_source = strip_cfg_test_modules(path.read_text(encoding="utf-8"))
        count = external_constructor_count(production_source)
        if count:
            discovered_by_file[rel] = count
        exec_count = low_level_exec_count(production_source)
        if exec_count:
            discovered_exec_by_file[rel] = exec_count
        # Production code must not split a raw process Command builder from its
        # launch across functions; that would evade per-function classification.
        if PROCESS_COMMAND_RETURN_RE.search(production_source):
            raise AssertionError(
                f"production raw process Command builder return requires explicit audit design: {rel}"
            )

    def assert_inventory_match(
        discovered: Counter, expected: Counter, description: str
    ) -> None:
        if discovered != expected:
            extra = discovered - expected
            missing = expected - discovered
            raise AssertionError(
                f"{description} inventory is incomplete; unclassified={dict(extra)}, missing={dict(missing)}"
            )

    assert_inventory_match(
        discovered_by_file, expected_by_file, "production external launch"
    )
    assert_inventory_match(
        discovered_exec_by_file, expected_exec_by_file, "production low-level external exec"
    )

    # The main concurrent-session application uses tonepoet-backend for
    # settings/types plus the read-only tool-availability check. Legacy backend
    # conversion/metadata execution APIs remain library code and must not become
    # reachable without a fresh supervision/capability review.
    root_production = "\n".join(
        strip_cfg_test_modules(path.read_text(encoding="utf-8"))
        for path in (ROOT / "src").rglob("*.rs")
    )
    inactive_backend_entry_patterns = {
        "convert_with_backend": r"\bconvert_with_backend\s*\(",
        "ConversionBackend::convert_item": r"\.convert_item\s*\(",
        "legacy CommandBuilder execution": r"\bCommandBuilder\b",
        "legacy ConversionCommand execution": r"\bConversionCommand\b",
        "legacy ConversionPipeline execution": r"\bConversionPipeline\b",
        "legacy metadata pipeline": r"\bMetadataPreservingPipeline\b",
        "legacy FLAC metadata applier": r"\bFlacMetadataApplier\b",
        "legacy WavPack metadata applier": r"\bWavPackMetadataApplier\b",
        "legacy Opus metadata applier": r"\bOpusMetadataApplier\b",
        "legacy AAC metadata applier": r"\bAacMetadataApplier\b",
        "legacy ffmpeg progress executor": r"\bexecute_ffmpeg_with_progress\s*\(",
        "legacy SoX progress executor": r"\bexecute_sox_with_progress\s*\(",
        "legacy proportional progress executor": r"\bexecute_with_proportion_progress\s*\(",
        "legacy estimated progress executor": r"\bexecute_with_estimated_progress\s*\(",
    }
    for label, pattern in inactive_backend_entry_patterns.items():
        if re.search(pattern, root_production):
            raise AssertionError(
                f"inactive tonepoet-backend direct-execution API became reachable from root application: {label}"
            )
    check_tools = function_body("src/main.rs", "run_check_tools")
    if not contains_all(
        check_tools,
        ["tonepoet_backend::ConversionBackend::new", "check_tool_availability()"],
    ):
        raise AssertionError(
            "root tool-availability path no longer matches reviewed read-only backend probe classification"
        )

    # Negative self-tests exercise all three discovery boundaries that have
    # previously been easy to miss: root Command::new, workspace-member
    # Command::new, and low-level execve/fexecve.
    synthetic_command = (
        'fn accidental_mutator() { std::process::Command::new("tool").status(); }'
    )
    synthetic_count = external_constructor_count(strip_cfg_test_modules(synthetic_command))
    if synthetic_count != 1:
        raise AssertionError(
            "external launch inventory self-test failed to detect synthetic direct spawn"
        )
    for synthetic_rel in (
        "src/__audit_unclassified_spawn_fixture.rs",
        "crates/tonepoet-backend/src/__audit_unclassified_spawn_fixture.rs",
    ):
        synthetic_discovered = discovered_by_file.copy()
        synthetic_discovered[synthetic_rel] += synthetic_count
        try:
            assert_inventory_match(
                synthetic_discovered,
                expected_by_file,
                "synthetic production external launch",
            )
        except AssertionError:
            pass
        else:
            raise AssertionError(
                f"external launch inventory self-test accepted an unclassified direct spawn: {synthetic_rel}"
            )

    synthetic_execs = {
        "execve": (
            "fn accidental_exec(path: *const libc::c_char, argv: *const *const libc::c_char, "
            "envp: *const *const libc::c_char) { unsafe { libc::execve(path, argv, envp); } }"
        ),
        "fexecve": (
            "fn accidental_fexec(fd: libc::c_int, argv: *const *const libc::c_char, "
            "envp: *const *const libc::c_char) { unsafe { libc::fexecve(fd, argv, envp); } }"
        ),
    }
    for primitive, synthetic_exec in synthetic_execs.items():
        synthetic_exec_count = low_level_exec_count(strip_cfg_test_modules(synthetic_exec))
        if synthetic_exec_count != 1:
            raise AssertionError(
                f"external launch inventory self-test failed to detect synthetic {primitive}"
            )
        synthetic_exec_discovered = discovered_exec_by_file.copy()
        synthetic_exec_discovered[
            f"src/__audit_unclassified_{primitive}_fixture.rs"
        ] += synthetic_exec_count
        try:
            assert_inventory_match(
                synthetic_exec_discovered,
                expected_exec_by_file,
                "synthetic production low-level external exec",
            )
        except AssertionError:
            pass
        else:
            raise AssertionError(
                f"external launch inventory self-test accepted an unclassified {primitive}"
            )
    return True


def main() -> int:
    # Existing core participants retained from the approved v3 correction.
    file_ops = text("src/tui/file_task_runtime.rs")
    file_worker = text("src/tui/keybindings.rs")
    require(
        "normal file operations",
        contains_all(file_ops, [
            "file_task_path_admission",
            "admitted_mappings",
            "admitted_destination_root",
            "PathResolutionSemantics::NamespaceObject",
            "MutationClaimGuard::acquire(",
            "LeaseFamily::JournalOperation",
        ])
        and contains_all(file_worker, [
            "admitted_mappings_for(&requested_mappings)",
            "new_with_sink_and_admitted_paths",
            "execution_mappings",
            "execution_destination_root",
        ]),
        "durable file-operation admission must persist resolved endpoints and the helper/worker must execute those admitted endpoints",
    )

    concurrency = text("src/concurrency.rs")
    ordered_resolution = function_body("src/concurrency.rs", "resolve_follow_referent_ordered")
    dependency_discovery = function_body(
        "src/concurrency.rs", "discover_namespace_symlink_dependencies"
    )
    claim_conflicts = function_body("src/concurrency.rs", "conflicts_with")
    claim_covers = function_body("src/concurrency.rs", "covers")
    require(
        "resolved path alias and parent-component semantics",
        contains_all(concurrency, [
            "enum PathResolutionSemantics",
            "FollowReferent",
            "NamespaceObject",
            "namespace_dependencies",
            "absolute_path_preserving_component_order",
            "resolve_follow_referent_ordered",
            "write_touches_dependency",
        ])
        and contains_all(ordered_resolution, [
            "absolute.components().collect",
            'raw_prefix.push("..")',
            "std::fs::canonicalize(&raw_prefix)",
            "prospective.pop()",
            "escapes canonical existing ancestor",
        ])
        and contains_all(dependency_discovery, [
            "Component::ParentDir",
            'prefix.push("..")',
            "std::fs::symlink_metadata(&prefix)",
            "std::fs::canonicalize(parent)",
            "stabilized_parent.join(basename)",
            "std::fs::read_link",
            "discover_namespace_symlink_dependencies",
        ])
        and "lexical_normalize_absolute" not in concurrency
        and "RegistryLock" not in ordered_resolution
        and "registry.lock" not in ordered_resolution
        and "RegistryLock" not in dependency_discovery
        and "registry.lock" not in dependency_discovery
        and "namespace_dependencies" in claim_conflicts
        and "namespace_dependencies" in claim_covers,
        "shared path identity must preserve symlink/.. component order, normalize only a prospective suffix below its canonical anchor, and compare stabilized alias dependencies without filesystem traversal under the registry lock",
    )

    stages = text("src/convert/pipeline/stages.rs")
    require(
        "conversion source/output/staging mutation",
        contains_all(stages, ["fn register_execution_claims", "fn admit_initial_conversion_claims", "fn admit_planned_output_claim", "ExecutionStaging"]),
        "conversion pipeline must retain execution-scoped source/output/staging claims",
    )

    stale_staging_cleanup = function_body(
        "src/convert/pipeline/memory_budget.rs", "cleanup_stale_staging_dir"
    )
    require(
        "scratch stale-tree deletion admission",
        contains_all(stale_staging_cleanup, [
            "probe_existing_run_lock",
            "PathResolutionSemantics::NamespaceObject",
            "ClaimMode::Write",
            "ClaimScope::Subtree",
            "MutationClaimGuard::acquire_ephemeral",
            "admitted_path",
            "remove_stale_staging_tree(&admitted_path)",
        ]),
        "an unlocked/missing legacy run lock must not authorize stale staging deletion until a shared WRITE/Subtree claim admits the candidate, preserving live and RecoveryReserved ExecutionStaging ownership",
    )

    real_tool = function_body("src/convert/pipeline/tool.rs", "run_supervised_with_stdio")
    actions_script = function_body("src/convert/pipeline/actions.rs", "run")
    replaygain_scan = function_body("src/tui/keybindings.rs", "metadata_editor_start_replaygain_scan")
    external_editor = function_body("src/tui/external_editor.rs", "run_supervised_interactive_editor")
    file_helper = function_body("src/tui/keybindings.rs", "supervise_file_task_process")
    require(
        "external-command launch inventory and supervision classification",
        audit_external_launch_inventory()
        and contains_all(real_tool, ["run_supervised_via_item_supervisor", "run_supervised", "current_supervision_lifetime_files"])
        and contains_all(actions_script, ["run_supervised_via_item_supervisor", "run_supervised", "current_supervision_lifetime_files"])
        and contains_all(replaygain_scan, ["MutationClaimGuard::acquire_ephemeral", "with_additional_supervision_lifetime_files", "ToolRunner::run"])
        and contains_all(external_editor, ["mutation_claim.into_lease()", "duplicate_lifetime_file", "retained_lifetime_files", "run_supervised"])
        and contains_all(file_helper, ["journal.lease_fd()", "FD_CLOEXEC", "__file-task-worker", "command.spawn()"]),
        "every production process construction must be explicitly classified; mutation-capable conversion/action/ReplayGain/editor launches must route through the existing supervisor with retained lifetime authority, while the file helper must carry its durable lease",
    )

    # Metadata uses one named outer admission helper. The audit checks the real
    # user-action boundaries rather than counting unrelated registry calls in
    # broad TUI files.
    metadata_admission = function_body("src/tui/probe.rs", "admit_metadata_mutation_paths")
    metadata_member_batch = function_body(
        "src/tui/probe.rs",
        "apply_metadata_editor_tag_changes_with_save_blocks_progress_and_forced_deletes_at_verification",
    )
    metadata_save = function_body("src/tui/keybindings.rs", "metadata_editor_save")
    tag_maintenance = function_body("src/tui/keybindings.rs", "start_tag_maintenance")
    artwork_write = function_body("src/tui/probe.rs", "write_artwork_to_files_with_cancel")
    artwork_remove = function_body("src/tui/probe.rs", "remove_artwork_from_files_with_cancel")
    tag_transfer = function_body("src/tui/tag_interchange.rs", "execute_tag_transfer_from_entries_to_carrier")
    invalid_ape_batch = function_body("src/tui/keybindings.rs", "run_invalid_ape_repair_batch")
    replaygain_scan = function_body("src/tui/keybindings.rs", "metadata_editor_start_replaygain_scan")
    inline_metadata = function_body(
        "src/tui/probe.rs",
        "write_metadata_field_transactional_with_control_at_verification",
    )
    single_metadata_admission = function_body(
        "src/tui/probe.rs",
        "with_single_metadata_path_admission",
    )
    flac_common_write = function_body("src/tui/probe.rs", "acquire_common_write_claim")
    dsf_lock = function_body("src/dsf_tags.rs", "acquire_dsf_write_lock")
    ape_lock = function_body("src/tui/probe.rs", "acquire_ape_metadata_write_lock")
    store_lock = function_body("src/config.rs", "acquire")
    require(
        "metadata complete-set admission",
        contains_all(metadata_admission, [
            "PathResolutionSemantics::NamespaceObject",
            "ClaimMode::Write",
            "ClaimScope::Exact",
            "current_mutation_authority_covers",
            "MutationClaimGuard::acquire_ephemeral",
        ])
        and contains_all(metadata_member_batch, ["admit_metadata_mutation_paths", "admitted_paths", "admission.run"])
        and contains_all(metadata_save, ["mutation_paths", "plan.cue_path", "admit_metadata_mutation_paths", "admission.run"])
        and contains_all(artwork_write, ["admit_metadata_mutation_paths", "admission.run", "apply_artwork_batch"])
        and contains_all(artwork_remove, ["admit_metadata_mutation_paths", "admission.run", "apply_artwork_batch"])
        and contains_all(tag_maintenance, ["admit_metadata_mutation_paths", "admission.run", "run_tag_maintenance"])
        and contains_all(tag_transfer, ["metadata_mutation_paths", "admit_metadata_mutation_paths", "admission.run"])
        and contains_all(invalid_ape_batch, ["mutation_paths", "admit_metadata_mutation_paths", "admission.run"])
        and contains_all(replaygain_scan, ["worker_paths", "MutationClaimGuard::acquire_ephemeral", "admitted_paths", "ToolRunner::run"])
        and contains_all(inline_metadata, ["with_single_metadata_path_admission"])
        and contains_all(single_metadata_admission, [
            "MetadataPersistenceRoute::NativeFlacVorbis",
            "PathResolutionSemantics::NamespaceObject",
            "admit_single_metadata_path",
            "admission.run",
        ])
        and contains_all(flac_common_write, [
            "lock_set.insert(canonical_path.clone())",
            "current_mutation_authority_covers(&required_claim)",
            "MutationClaimGuard::acquire_ephemeral",
        ])
        and contains_all(dsf_lock, ["PathResolutionSemantics::NamespaceObject", "current_mutation_authority_covers", "MutationClaimGuard::acquire_ephemeral", "StoreFileLock::acquire_for_path"])
        and contains_all(ape_lock, ["PathResolutionSemantics::NamespaceObject", "current_mutation_authority_covers", "MutationClaimGuard::acquire_ephemeral", "StoreFileLock::acquire_for_path"])
        and "MutationClaimGuard" not in store_lock,
        "metadata editor/inline/artwork/maintenance/transfer/repair/ReplayGain must admit complete WRITE sets; native FLAC keeps native-lock-first shared authority, native DSF/APEv2 recovery keeps explicit shared authority, and generic StoreFileLock remains store-local",
    )

    # Automatic action phases: one complete phase admission before the action loop;
    # concrete plans are assertions only. Manual :actions-run remains separately
    # outer-admitted in conversion_actions_ui.rs.
    actions = text("src/convert/pipeline/actions.rs")
    execute_phase = function_body("src/convert/pipeline/actions.rs", "execute_phase_internal")
    phase_call = execute_phase.find("admit_conversion_action_phase_claims")
    action_loop = execute_phase.find("for action_index in 0..journal.actions.len()", phase_call)
    phase_admission = function_body("src/convert/pipeline/actions.rs", "admit_conversion_action_phase_claims")
    plan_assertion = function_body("src/convert/pipeline/actions.rs", "assert_conversion_action_plan_is_admitted")
    require(
        "automatic conversion action phase admission",
        phase_call >= 0 and action_loop >= 0 and phase_call < action_loop
        and "admit_conversion_action_plan_claims" not in actions
        and contains_all(phase_admission, [
            "shared_path_claims_for_configured_action_phase",
            "runtime_execution_claims",
            "LeaseFamily::ExecutionClaim",
            "register_runtime_supplemental_lease",
        ])
        and "MutationClaimGuard" not in plan_assertion
        and "assert_conversion_action_plan_is_admitted" in execute_phase,
        "complete configured phase claims must be admitted as one supervised ExecutionClaim before the first action mutation, without per-plan acquisitions",
    )
    manual = text("src/tui/conversion_actions_ui.rs")
    require(
        "manual :actions-run outer admission",
        contains_all(manual, ["shared_path_claims_for_action_plans", "MutationClaimGuard::acquire(", "LeaseFamily::EphemeralMutation"]),
        "manual reviewed actions must keep their existing complete outer admission",
    )

    archive = function_body("src/tui/event_loop.rs", "start_browse_archive_repackage_inner")
    archive_helper = function_body("src/tui/event_loop.rs", "acquire_browse_archive_mutation_claim")
    archive_acquire = archive.find("acquire_browse_archive_mutation_claim")
    archive_recheck = archive.find("archive_conflict()")
    archive_spawn = archive.find("tokio::spawn")
    require(
        "browse archive save/repackage",
        0 <= archive_acquire < archive_recheck < archive_spawn
        and "ClaimMode::Write" in archive
        and "ClaimScope::Exact" in archive
        and "PathResolutionSemantics::NamespaceObject" in archive
        and "MutationClaimGuard::acquire_ephemeral" in archive_helper
        and "let _archive_mutation_claim = archive_mutation_claim" in archive
        and "&admitted_archive_path" in archive,
        "archive WRITE/Exact claim must precede the final conflict recheck and live through the async installer",
    )

    rename = function_body("src/tui/rename_plan.rs", "execute_plan_with_proofs_internal")
    rename_acquire = rename.find("MutationClaimGuard::acquire_ephemeral")
    rename_manifest = rename.find("capture_manifest_with_mode")
    rename_workspace = rename.find("create_unique_rename_workspace")
    require(
        "bulk rename transactional replay boundary",
        0 <= rename_acquire < rename_manifest < rename_workspace
        and "ClaimScope::Subtree" in rename
        and "ClaimScope::Exact" in rename
        and rename.count("PathResolutionSemantics::NamespaceObject") >= 2
        and "admitted_paths" in rename
        and "create_unique_rename_workspace(&admitted_base_dir)" in rename
        and "execute_plan_with_proofs_and_expected_sources_at_verification" in text("src/tui/keybindings.rs"),
        "transactional rename/replay must atomically admit source/destination namespace objects before proof capture and execute only admitted paths",
    )

    command = text("src/tui/command.rs")
    execute_command = function_body("src/tui/command.rs", "execute_command")
    cue_write = function_body("src/tui/command.rs", "write_cue_file_with_claim")
    require(
        "direct CUE save/generation",
        contains_all(cue_write, ["ClaimMode::Write", "ClaimScope::Exact", "MutationClaimGuard::acquire_ephemeral", "resolved_io_path"])
        and execute_command.count("write_cue_file_with_claim(") >= 3
        and "std::fs::write(&cue_path, &cue_content)" not in execute_command,
        "Write/WriteQuit/GenerateCue must publish through one exact-CUE helper",
    )

    cue_parser = text("src/convert/cue_parser.rs")
    require(
        "CUE atomic create/replace/rewrite helpers",
        cue_parser.count("acquire_cue_sidecar_write_claim(cue_path)") >= 3
        and contains_all(function_body("src/convert/cue_parser.rs", "acquire_cue_sidecar_write_claim"),
                         ["ClaimMode::Write", "ClaimScope::Exact", "PathResolutionSemantics::NamespaceObject",
                          "current_mutation_authority_covers", "MutationClaimGuard::acquire_ephemeral"]),
        "CUE atomic sidecar helpers must preserve the final namespace object and reuse an already-held metadata authority",
    )

    dvdv = function_body("src/tui/command.rs", "save_dvdv_metadata_sidecar")
    bluray = function_body("src/tui/command.rs", "save_bluray_metadata_sidecar")
    bluray_batch = function_body("src/tui/command.rs", "acquire_bluray_sidecar_source_claims")
    dvda = function_body("src/tui/keybindings.rs", "save_dvda_metabase")
    sacd = function_body("src/tui/keybindings.rs", "save_sacd_sidecar")
    offset_claim = function_body("src/tui/accuraterip.rs", "acquire_offset_correction_mutation_claims")
    offset_apply = function_body("src/tui/accuraterip.rs", "apply_offset_correction")
    offset_install = function_body("src/tui/accuraterip.rs", "install_offset_corrected_tracks")
    ctdb_claim = function_body("src/tui/accuraterip.rs", "acquire_repair_mutation_claims")
    require(
        "atomic replacement namespace semantics",
        all("PathResolutionSemantics::NamespaceObject" in body for body in [dvdv, bluray, bluray_batch, dvda, sacd, archive])
        and "PathResolutionSemantics::NamespaceObject" in offset_claim
        and contains_all(offset_apply, ["acquire_offset_correction_mutation_claims", "admitted_paths"])
        and contains_all(offset_install, ["std::fs::rename(orig, &bak)", "std::fs::copy(corrected, orig)"])
        and "PathResolutionSemantics::FollowReferent" in ctdb_claim,
        "atomic rename/replacement publishers must preserve the final namespace entry; copy-based CTDB repair must keep follow-referent semantics",
    )

    browse_create = function_body("src/tui/keybindings.rs", "commit_browse_create")
    browse_delete = function_body("src/tui/keybindings.rs", "permanently_delete_paths")
    delete_claim = function_body("src/tui/keybindings.rs", "permanent_delete_claim")
    require(
        "Browse create/permanent-delete admission",
        contains_all(browse_create, [
            "PathResolutionSemantics::NamespaceObject",
            "ClaimScope::Exact",
            "ClaimScope::Subtree",
            "MutationClaimGuard::acquire_ephemeral",
            "admitted_target",
        ])
        and contains_all(browse_delete, [
            "permanent_delete_claim",
            "MutationClaimGuard::acquire_ephemeral(claims)",
            "delete_path_permanently_admitted",
        ])
        and contains_all(delete_claim, [
            "PathResolutionSemantics::NamespaceObject",
            "ClaimScope::Subtree",
            "ClaimScope::Exact",
        ]),
        "Browse create and the complete permanent-delete selection must join namespace-object admission before mutation",
    )


    browse_duplicate = function_body(
        "src/tui/context_menu.rs", "duplicate_files_with_shared_admission"
    )
    browse_context = function_body("src/tui/context_menu.rs", "execute_context_action")
    replay_worker = function_body("src/tui/keybindings.rs", "execute_file_operation_replay_worker")
    picker_refresh = function_body("crates/tui-file-picker/src/state.rs", "refresh")
    picker_mutation_policy = function_body(
        "crates/tui-file-picker/src/state.rs", "permits_filesystem_mutation"
    )
    browse_recovery = function_body(
        "src/tui/browse.rs", "recover_interrupted_copy_undo_with_claims"
    )
    require(
        "Browse Duplicate complete-set admission",
        "ContextAction::DuplicateSelection" in browse_context
        and "duplicate_files_with_shared_admission" in browse_context
        and contains_all(browse_duplicate, [
            "plan_duplicate_files_in_place",
            "file_task_path_admission(false, &plan)",
            "MutationClaimGuard::acquire_ephemeral",
            "execute_duplicate_plan",
            "admission.admitted_mappings",
        ]),
        "Browse Duplicate must plan all siblings, atomically admit source READ/destination WRITE mappings, and copy only admitted paths",
    )
    require(
        "copy/move undo-redo admission",
        contains_all(replay_worker, [
            "Copy && undo",
            "PathResolutionSemantics::NamespaceObject",
            "MutationClaimGuard::acquire_ephemeral(claims)",
            "admitted_destinations",
            "execute_transactional_rename_replay",
            "file_task_path_admission",
            "admission.admitted_mappings",
        ])
        and replay_worker.find("execute_transactional_rename_replay")
            < replay_worker.find("let logical_plan ="),
        "copy undo must claim its complete removal set; copy redo and move undo/redo must reuse file-task admission and admitted mappings, while rename replay stays on the transactional rename boundary",
    )
    require(
        "copy-undo recovery host admission",
        "permits_filesystem_mutation" in picker_refresh
        and "recover_interrupted_verified_removals_once" in picker_refresh
        and contains_all(picker_mutation_policy, [
            "allow_new_file",
            "allow_new_folder",
            "allow_paste",
            "allow_delete",
            "allow_rename",
            "allow_duplicate",
        ])
        and contains_all(browse_recovery, [
            "discover_interrupted_verified_removal_restore_targets",
            "target.original()",
            "target.quarantine()",
            "PathResolutionSemantics::NamespaceObject",
            "MutationClaimGuard::acquire_ephemeral",
            "recover_interrupted_verified_removal_restore_target",
            "admitted_original",
            "admitted_quarantine",
        ]),
        "selection-only modal refresh must perform no automatic copy-undo restore; main Browse must claim each original/quarantine pair before restoring admitted paths",
    )


    picker_selection_only = function_body(
        "crates/tui-file-picker/src/state.rs", "selection_only"
    )
    picker_begin_rename = function_body(
        "crates/tui-file-picker/src/state.rs", "begin_rename_path"
    )
    picker_begin_duplicate = function_body(
        "crates/tui-file-picker/src/state.rs", "begin_duplicate_path"
    )
    picker_case_rename = function_body(
        "crates/tui-file-picker/src/state.rs", "apply_path_case_transform"
    )
    picker_duplicate_helper = function_body(
        "crates/tui-file-picker/src/state.rs", "plan_duplicate_files_in_place"
    )
    tag_block_picker = function_body(
        "src/tui/keybindings.rs", "metadata_editor_open_tag_blocks_file_picker"
    )
    tag_transfer_picker = function_body(
        "src/tui/keybindings.rs", "metadata_editor_open_tag_transfer_picker_scoped"
    )
    artwork_picker_policy = function_body(
        "src/tui/keybindings.rs", "artwork_file_picker_policy"
    )
    browse_tag_transfer_picker = function_body(
        "src/tui/context_menu.rs", "open_browse_tag_transfer_picker"
    )
    destination_picker_policy = function_body(
        "src/tui/command.rs", "directory_destination_picker_policy"
    )
    preset_picker_policy_body = function_body(
        "src/tui/command.rs", "preset_picker_policy"
    )
    require(
        "modal file pickers are selection-only",
        contains_all(picker_selection_only, [
            "allow_new_file: false",
            "allow_new_folder: false",
            "allow_cut: false",
            "allow_copy: false",
            "allow_paste: false",
            "allow_delete: false",
            "allow_rename: false",
            "allow_duplicate: false",
        ])
        and "allow_rename" in picker_begin_rename
        and "OperationDisabled(\"rename\")" in picker_begin_rename
        and "allow_duplicate" in picker_begin_duplicate
        and "OperationDisabled(\"duplicate\")" in picker_begin_duplicate
        and "allow_rename" in picker_case_rename
        and "allow_duplicate" in picker_duplicate_helper
        and "is_delayed_repeat" in text("crates/tui-file-picker/src/input.rs")
        and "self.begin_rename_current()" in text("crates/tui-file-picker/src/input.rs")
        and "self.begin_rename_path(path)" in text("crates/tui-file-picker/src/input.rs")
        and "FileOperationPolicy::selection_only" in tag_block_picker
        and "FileOperationPolicy::selection_only" in tag_transfer_picker
        and "FileOperationPolicy::selection_only" in artwork_picker_policy
        and "FileOperationPolicy::selection_only" in browse_tag_transfer_picker
        and "FileOperationPolicy::selection_only" in destination_picker_policy
        and "FileOperationPolicy::selection_only" in preset_picker_policy_body,
        "root-reachable modal selectors must disable create/cut/paste/delete/rename/case-rename/duplicate at the picker mutation boundary",
    )

    wizard_events_source = strip_cfg_test_modules(
        text("crates/tonepoet-wizard/src/events.rs")
    )
    wizard_presets_source = strip_cfg_test_modules(
        text("crates/tonepoet-wizard/src/presets.rs")
    )
    wizard_ui_source = strip_cfg_test_modules(
        text("crates/tonepoet-wizard/src/ui.rs")
    )
    wizard_destination_validation = function_body(
        "crates/tonepoet-wizard/src/events.rs", "validate_custom_destination"
    )
    require(
        "legacy wizard destination selection is non-mutating",
        ".convert_wizard_test" not in wizard_events_source
        and "std::fs::File::create" not in wizard_destination_validation
        and "std::fs::create_dir(&new_path)" not in wizard_events_source
        and contains_all(wizard_destination_validation, [
            "path.exists()",
            "path.is_dir()",
            "parent.exists()",
            "parent.is_dir()",
        ]),
        "wizard destination validation must be structural only and New Folder must remain prospective until claimed conversion output creation",
    )

    modern_preset_save = function_body("src/tui/presets.rs", "save_preset_with_db")
    modern_preset_save_path = function_body(
        "src/tui/presets.rs", "save_preset_to_path_with_db"
    )
    modern_preset_delete = function_body("src/tui/presets.rs", "delete_preset_with_db")
    preset_overlay_keys = function_body("src/tui/keybindings.rs", "handle_preset_overlay_key")
    require(
        "preset persistence has one coordinated writer",
        "fn save_preset" not in wizard_presets_source
        and "fn delete_preset" not in wizard_presets_source
        and "fs::write(" not in wizard_presets_source
        and "fs::remove_file(" not in wizard_presets_source
        and "create_dir_all" not in impl_function_body(
            "crates/tonepoet-wizard/src/presets.rs", "PresetManager", "new"
        )
        and "mouse_areas.add(save_preset_area, ButtonId::SavePreset)" not in wizard_ui_source
        and "manager.save_preset" not in wizard_events_source
        and all(
            contains_all(body, ["StoreFileLock::acquire_for_path", "db"])
            for body in [modern_preset_save, modern_preset_save_path, modern_preset_delete]
        )
        and "delete_preset_with_db" in preset_overlay_keys
        and "FileOperationPolicy::selection_only" in preset_picker_policy_body,
        "legacy wizard and preset picker must not mutate preset TOML directly; modern save/delete must retain the per-preset lock through SQLite index mutation",
    )

    sidecar_dir_eligibility = function_body("src/tui/keybindings.rs", "is_dir_writable")
    require(
        "metadata editor open is sidecar-write-probe free",
        contains_all(sidecar_dir_eligibility, ["std::fs::metadata(dir)", "metadata.is_dir()"])
        and "File::create" not in sidecar_dir_eligibility
        and "remove_file" not in sidecar_dir_eligibility
        and ".tonepoet-write-probe-" not in strip_cfg_test_modules(text("src/tui/keybindings.rs")),
        "metadata editor eligibility must remain read-only; actual claimed sidecar save is authoritative for permission/I/O failure",
    )

    accuraterip = function_body("src/tui/accuraterip.rs", "publish_batch_report_with_claim")
    batch = function_body("src/tui/accuraterip.rs", "batch_verify")
    require(
        "AccurateRip report publication",
        contains_all(accuraterip, ["ClaimMode::Write", "ClaimScope::Exact", "MutationClaimGuard::acquire_ephemeral", "std::fs::write(&admitted_path"])
        and "publish_batch_report_with_claim" in batch
        and "Err(error)" in batch
        and "None" in batch
        and "ArBatchResult" in batch,
        "report publication must use exact shared admission while remaining nonfatal to verification",
    )

    db_recovery = function_body("src/db.rs", "recover_stale_metadata_writes")
    db_recovery_admit = function_body("src/db.rs", "admit_stale_metadata_restore")
    db_restore = function_body("src/db.rs", "copy_backup_over_admitted")
    flac_recovery_admit = function_body(
        "src/tui/probe.rs", "acquire_flac_recovery_mutation_authority"
    )
    flac_journal_recovery = function_body(
        "src/tui/probe.rs", "recover_metadata_journal_impl"
    )
    artwork_recovery = function_body(
        "src/tui/probe.rs", "recover_artwork_rollback_journal_internal"
    )
    common_lock_recovery = function_body("src/tui/probe.rs", "recover_common_write_lock")
    ordinary_recovery_before_read = function_body("src/tui/probe.rs", "recover_metadata_before_read")
    require(
        "metadata recovery mutation admission",
        db_recovery.find("admit_stale_metadata_restore")
            < db_recovery.find("copy_backup_over_admitted")
        and contains_all(db_recovery, [
            "RECOVERY DEFERRED",
            "admitted_original",
            "drop(recovery_guard)",
        ])
        and contains_all(db_recovery_admit, [
            "ClaimMode::Write",
            "ClaimScope::Exact",
            "PathResolutionSemantics::FollowReferent",
            "current_mutation_authority_covers",
            "MutationClaimGuard::acquire_ephemeral",
        ])
        and "resolve_config_save_target" not in db_restore
        and contains_all(flac_recovery_admit, [
            "ClaimMode::Write",
            "ClaimScope::Exact",
            "PathResolutionSemantics::FollowReferent",
            "current_mutation_authority_covers",
            "MutationClaimGuard::acquire_ephemeral",
        ])
        and flac_journal_recovery.find("let recovery_authority =")
            < flac_journal_recovery.find("|| overwrite_metadata_region(")
        and contains_all(artwork_recovery, [
            "acquire_flac_recovery_mutation_authority",
            "restore_metadata_snapshot_from_audio_start",
            "recovering_stale_common_lock",
        ])
        and "recover_artwork_rollback_journal_for_common_lock_recovery" in common_lock_recovery
        and "MutationClaimGuard" not in ordinary_recovery_before_read
        and "acquire_ephemeral" not in ordinary_recovery_before_read,
        "DB PREPARED rollback and native FLAC byte-restoring recovery must acquire/reuse Exact WRITE authority only before mutation, carry admitted paths, defer cleanly on Busy, avoid stale-common-lock recursion, and leave ordinary no-artifact reads claim-free",
    )

    # Existing disc/sidecar publication paths are deliberately enumerated here
    # so future edits do not silently bypass the registry.
    disc_sidecars = text("src/tui/command.rs") + text("src/tui/keybindings.rs")
    require(
        "disc/CUE sidecar publication and repair",
        contains_all(disc_sidecars, ["DVD-Video sidecar admission produced no path claim", "Blu-ray sidecar admission produced no path claim", "SACD sidecar admission produced no path claim"])
        and "acquire_cue_sidecar_write_claim" in cue_parser,
        "existing DVD/Blu-ray/SACD/CUE sidecar admissions must remain wired",
    )

    print("focused concurrent-session mutation audit passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AssertionError as error:
        print(f"[FAIL] {error}", file=sys.stderr)
        raise SystemExit(1)
