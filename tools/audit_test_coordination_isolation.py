#!/usr/bin/env python3
"""Audit tests that enter the shared coordination registry.

The 2026-08-18 corrective requires each coordination-touching test to own one
serialized, per-test registry root. This static audit covers direct registry,
queue, and recovery entrypoints plus reviewed higher-level mutation boundaries
that acquire coordination internally. It recognizes ordinary and Tokio unit
tests and also scans top-level integration tests. The approved fixture helpers
all install the same serialized isolation discipline. This supplements Rust
tests; it does not try to infer arbitrary call graphs.
"""
from __future__ import annotations

from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]

DIRECT_ENTRYPOINTS = {
    "mutation claim": re.compile(r"\bMutationClaimGuard::acquire(?:_ephemeral|_grouped)?\s*\("),
    "persistent lease": re.compile(
        r"\bPersistentLease::(?:create|acquire_existing|acquire_existing_recovery)\s*\("
    ),
    "qualified mutation claim": re.compile(r"crate::concurrency::MutationClaimGuard::acquire"),
    "qualified persistent lease": re.compile(
        r"crate::concurrency::PersistentLease::(?:create|acquire_existing|acquire_existing_recovery)"
    ),
    "queue sync": re.compile(r"\.(?:sync_queue|sync_queue_snapshot)\s*\("),
    "legacy queue import": re.compile(r"\.publish_legacy_queue_import\s*\("),
    "queue load": re.compile(r"\.load_queue_items\s*\("),
    "queue scope": re.compile(r"\.ensure_queue_scope\s*\("),
    "queue execution": re.compile(r"\.queue_execution_coordinator\s*\("),
    "metadata recovery": re.compile(r"\.recover_stale_metadata_writes\s*\("),
    "lifecycle scan": re.compile(
        r"\b(?:find_family_descriptor|lifecycle_descriptor_hints|descriptor_availability|"
        r"retire_setup_orphan_by_path_identity)\s*\("
    ),
    "coordination root handoff": re.compile(r"crate::concurrency::coordination_root\s*\("),
    "file-operation journal": re.compile(r"\bFileTaskJournalHandle::create\s*\("),
    # Reviewed production mutation boundaries from audit_concurrent_mutation_entrypoints.py.
    # Including their call sites catches tests that enter coordination indirectly.
    "metadata admission": re.compile(r"\badmit_metadata_mutation_paths\s*\("),
    "metadata save": re.compile(r"\bmetadata_editor_save\s*\("),
    "tag maintenance": re.compile(r"\bstart_tag_maintenance\s*\("),
    "artwork write": re.compile(r"\bwrite_artwork_to_files_with_cancel\s*\("),
    "artwork remove": re.compile(r"\bremove_artwork_from_files_with_cancel\s*\("),
    "tag transfer": re.compile(r"\bexecute_tag_transfer_from_entries_to_carrier\s*\("),
    "invalid APE repair": re.compile(r"\brun_invalid_ape_repair_batch\s*\("),
    "ReplayGain scan": re.compile(r"\bmetadata_editor_start_replaygain_scan\s*\("),
    "inline metadata write": re.compile(r"\bwrite_metadata_field_transactional_with_control_at_verification\s*\("),
    "DSF write authority": re.compile(r"\bacquire_dsf_write_lock\s*\("),
    "APE write authority": re.compile(r"\bacquire_ape_metadata_write_lock\s*\("),
    "conversion action admission": re.compile(r"\badmit_conversion_action_phase_claims\s*\("),
    "archive repackage admission": re.compile(r"\bacquire_browse_archive_mutation_claim\s*\("),
    "bulk rename execution": re.compile(r"\bexecute_plan_with_proofs(?:_internal|_and_expected_sources_at_verification)?\s*\("),
    "CUE write": re.compile(r"\bwrite_cue_file_with_claim\s*\("),
    "CUE sidecar create/replace/rewrite": re.compile(
        r"\b(?:rewrite_cue_sidecar_metadata_from_cuesheet|"
        r"rewrite_cue_sidecar_metadata_authoritative_from_cuesheet|"
        r"create_cue_sidecar_from_cuesheet|"
        r"replace_invalid_cue_sidecar_from_cuesheet_if_unchanged|"
        r"rewrite_cue_sidecar_metadata_from_cuesheet_validated)\s*\("
    ),
    "disc sidecar publication": re.compile(
        r"\b(?:save_dvdv_metadata_sidecar|save_bluray_metadata_sidecar|"
        r"save_dvda_metabase|save_sacd_sidecar)\s*\("
    ),
    "AccurateRip correction/repair": re.compile(
        r"\b(?:apply_offset_correction|acquire_repair_mutation_claims)\s*\("
    ),
    "Browse create": re.compile(r"\bcommit_browse_create\s*\("),
    "Browse duplicate": re.compile(r"\bduplicate_files_with_shared_admission\s*\("),
    "file-operation replay": re.compile(r"\bexecute_file_operation_replay_worker\s*\("),
    "copy-undo recovery": re.compile(r"\brecover_interrupted_copy_undo_with_claims\s*\("),
    "file-task admission": re.compile(r"\bfile_task_path_admission\s*\("),
    "permanent delete": re.compile(r"\bpermanently_delete_paths\s*\("),
    "AccurateRip report": re.compile(r"\bpublish_batch_report_with_claim\s*\("),
}

SCOPE_MARKERS = (
    "scoped_test_coordination_root()",
    "install_scoped_test_coordination_root(",
    # The legacy narrow fixture remains valid for tests proven to remain on one
    # thread; it shares the same global serial mutex with the process-wide scope.
    "install_test_coordination_root(",
    "with_root(",
    "JournalDirGuard::install(",
    "FileTaskTestEnvironment::install(",
    "TestFileTaskJournalEnvironment::install(",
)

# These are deliberately re-executed libtest child entrypoints. Their parents
# pass TONEPOET_CONCURRENCY_DIR plus TONEPOET_TEST_CONCURRENCY_DIR_INHERIT=1 so
# parent and child coordinate through exactly one selected registry.
CROSS_PROCESS_CHILD_EXEMPTIONS = {
    ("src/db.rs", "cross_process_database_open_child"),
    ("src/db.rs", "concurrent_queue_scope_process_child"),
}


def matching_brace(source: str, opening: int) -> int:
    depth = 0
    index = opening
    block_comment_depth = 0
    in_line_comment = False
    in_string = False
    string_escape = False
    raw_hashes: int | None = None
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
    raise AssertionError("unterminated Rust block while scanning tests")


def test_functions(path: Path):
    source = path.read_text(encoding="utf-8")
    for test_attr in re.finditer(
        r"#\s*\[\s*(?:test|tokio::test(?:\s*\([^]]*\))?)\s*\]",
        source,
    ):
        function = re.search(
            r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+"
            r"([A-Za-z0-9_]+)\b[^\{]*\{",
            source[test_attr.end() :],
        )
        if function is None:
            continue
        start = test_attr.end() + function.start()
        name = function.group(1)
        opening = source.find("{", start, test_attr.end() + function.end())
        closing = matching_brace(source, opening)
        line = source.count("\n", 0, start) + 1
        yield name, source[start : closing + 1], line


failures: list[str] = []
reviewed = 0
paths = list((ROOT / "src").rglob("*.rs"))
if (ROOT / "tests").is_dir():
    paths.extend((ROOT / "tests").rglob("*.rs"))
for path in sorted(paths):
    rel = path.relative_to(ROOT).as_posix()
    for name, body, line in test_functions(path):
        hits = [label for label, pattern in DIRECT_ENTRYPOINTS.items() if pattern.search(body)]
        if not hits:
            continue
        reviewed += 1
        scoped = any(marker in body for marker in SCOPE_MARKERS)
        if (rel, name) in CROSS_PROCESS_CHILD_EXEMPTIONS:
            scoped = True
        if not scoped:
            failures.append(f"{rel}:{line} {name}: {', '.join(hits)}")

if failures:
    for failure in failures:
        print(f"[FAIL] unscoped coordination-touching test: {failure}")
    raise SystemExit(1)

print(f"[ok] {reviewed} reviewed coordination-touching tests are isolated or explicit child handoffs")
