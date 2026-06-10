#!/usr/bin/env python3
"""Audit PreparedTrack sample-rate migration hazards.

This script is intentionally repository-scoped. Run it from the tonepoet root
*after* applying the DVD-Audio Phase 2 overlay:

    python3 tools/audit_prepared_track_sample_rate.py .

It checks two classes of migration risk introduced by
PreparedTrack::sample_rate: Option<u32>:

1. Direct reads of `.sample_rate` outside the type's compatibility helpers.
   Callers that need one scalar rate should use `scalar_sample_rate()` or
   `require_scalar_sample_rate(...)`; DVD-Aware callers should inspect
   `source_audio` / channel groups.
2. PreparedTrack struct literals that look like pre-migration constructors:
   missing `source_audio`, missing `sample_rate`, or initializing `sample_rate`
   with a bare scalar expression instead of an Option-valued expression.

The audit is lexical rather than a Rust typechecker, so it is a guardrail, not a
substitute for `cargo check --workspace` and `cargo test --workspace`.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

DIRECT_SAMPLE_RATE_RE = re.compile(r"\.[ \t]*sample_rate\b(?!\s*=)")
PREPARED_TRACK_LITERAL_RE = re.compile(r"\bPreparedTrack\s*\{")
FIELD_RE_TEMPLATE = r"(?m)^\s*{field}(?:\s*:|\s*,)"
SAMPLE_RATE_FIELD_RE = re.compile(r"(?m)^\s*sample_rate\s*:\s*(?P<expr>.+?)(?:,\s*(?://.*)?$|$)")

# Contexts where `.sample_rate` does not mean "read PreparedTrack.sample_rate".
# Keep this list narrow. New full-repo exceptions should be reviewed rather than
# silently added, because a too-permissive audit was the original weakness.
NON_PREPARED_TRACK_DOT_READS = (
    "audio_facts.sample_rate",
    "facts.sample_rate",
    "expected.sample_rate",
    "format.sample_rate",
    "format.group1_sample_rate",
    "format.group2_sample_rate",
    "group.sample_rate",
    "probe.sample_rate",
    "descriptor.channel_groups",
    "ChannelGroupDescriptor",
)

# PreparedTrack constructors should make Option-ness visible. These expressions
# are known Option-valued sources in this overlay.
OPTION_VALUED_SAMPLE_RATE_EXPRESSIONS = (
    "None",
    "Some(",
    "audio_facts.sample_rate",
    "facts.sample_rate",
    "track.scalar_sample_rate()",
    "prepared.scalar_sample_rate()",
    "prepared_track.scalar_sample_rate()",
)


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    message: str
    snippet: str


def iter_rust_files(root: Path) -> Iterable[Path]:
    for path in root.rglob("*.rs"):
        if "target" in path.parts or ".git" in path.parts:
            continue
        yield path


def line_number_at(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def line_at(text: str, line_no: int) -> str:
    lines = text.splitlines()
    if 1 <= line_no <= len(lines):
        return lines[line_no - 1].strip()
    return ""


def extract_braced_block(text: str, start: int) -> tuple[str, int] | None:
    brace_start = text.find("{", start)
    if brace_start == -1:
        return None

    depth = 0
    in_line_comment = False
    in_block_comment = False
    in_string = False
    in_char = False
    escaped = False

    for idx in range(brace_start, len(text)):
        ch = text[idx]
        nxt = text[idx + 1] if idx + 1 < len(text) else ""

        if in_line_comment:
            if ch == "\n":
                in_line_comment = False
            continue
        if in_block_comment:
            if ch == "*" and nxt == "/":
                in_block_comment = False
            continue
        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            continue
        if in_char:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == "'":
                in_char = False
            continue

        if ch == "/" and nxt == "/":
            in_line_comment = True
            continue
        if ch == "/" and nxt == "*":
            in_block_comment = True
            continue
        if ch == '"':
            in_string = True
            continue
        if ch == "'":
            in_char = True
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return text[brace_start : idx + 1], idx + 1
    return None


def in_prepared_track_impl(path: Path, text: str, offset: int) -> bool:
    if path.name != "types.rs":
        return False
    impl_pos = text.rfind("impl PreparedTrack", 0, offset)
    if impl_pos == -1:
        return False
    next_impl_or_struct = min(
        [pos for pos in (text.find("\nimpl ", offset), text.find("\npub struct ", offset), text.find("\n#[derive", offset)) if pos != -1]
        or [len(text)]
    )
    return impl_pos < offset < next_impl_or_struct


def audit_direct_reads(root: Path, path: Path, text: str) -> list[Violation]:
    violations: list[Violation] = []
    rel = path.relative_to(root)
    lines = text.splitlines()
    for match in DIRECT_SAMPLE_RATE_RE.finditer(text):
        line_no = line_number_at(text, match.start())
        line = lines[line_no - 1].strip()
        context_start = max(0, line_no - 3)
        context_end = min(len(lines), line_no + 2)
        context = " ".join(line.strip() for line in lines[context_start:context_end])

        if in_prepared_track_impl(path, text, match.start()):
            continue
        if any(allowed in context for allowed in NON_PREPARED_TRACK_DOT_READS):
            continue
        if "scalar_sample_rate" in context or "require_scalar_sample_rate" in context:
            continue
        if path.name.endswith("_fixture_tests.rs") and "track.sample_rate" in context:
            violations.append(Violation(rel, line_no, "fixture tests should assert scalar_sample_rate() or source_audio facts, not PreparedTrack.sample_rate directly", line))
            continue

        violations.append(Violation(rel, line_no, "direct .sample_rate read; use scalar_sample_rate(), require_scalar_sample_rate(...), or source_audio facts", line))
    return violations


def field_present(block: str, field: str) -> bool:
    return re.search(FIELD_RE_TEMPLATE.format(field=re.escape(field)), block) is not None


def first_field_expr(block: str, field: str) -> str | None:
    regex = re.compile(FIELD_RE_TEMPLATE.format(field=re.escape(field)) + r"\s*(?P<expr>.+?)(?:,\s*(?://.*)?$|$)")
    match = regex.search(block)
    if not match:
        return None
    return match.group("expr").strip()


def sample_rate_expr_is_option_valued(expr: str) -> bool:
    normalized = " ".join(expr.split())
    return any(token in normalized for token in OPTION_VALUED_SAMPLE_RATE_EXPRESSIONS)


def audit_prepared_track_literals(root: Path, path: Path, text: str) -> list[Violation]:
    violations: list[Violation] = []
    rel = path.relative_to(root)
    search_pos = 0
    while True:
        match = PREPARED_TRACK_LITERAL_RE.search(text, search_pos)
        if not match:
            break
        line_no = line_number_at(text, match.start())
        line = line_at(text, line_no)
        before = text[max(0, match.start() - 24):match.start()]
        if "struct " in before or "impl " in before or "\"" in line:
            search_pos = match.end()
            continue
        extracted = extract_braced_block(text, match.start())
        if extracted is None:
            line_no = line_number_at(text, match.start())
            violations.append(Violation(rel, line_no, "could not parse PreparedTrack literal for sample-rate audit", line_at(text, line_no)))
            search_pos = match.end()
            continue

        block, end = extracted
        line_no = line_number_at(text, match.start())
        search_pos = end

        if not field_present(block, "sample_rate"):
            violations.append(Violation(rel, line_no, "PreparedTrack literal missing sample_rate field", line_at(text, line_no)))
        else:
            expr = first_field_expr(block, "sample_rate")
            if expr is not None and not sample_rate_expr_is_option_valued(expr):
                violations.append(Violation(
                    rel,
                    line_number_at(text, text.find(expr, match.start(), end)),
                    "PreparedTrack.sample_rate initializer should be visibly Option-valued (Some(...), None, or a known Option source)",
                    f"sample_rate: {expr}",
                ))

        if not field_present(block, "source_audio"):
            violations.append(Violation(rel, line_no, "PreparedTrack literal missing source_audio descriptor", line_at(text, line_no)))

    return violations


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", nargs="?", default=".", help="repository root")
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = parser.parse_args()

    root = Path(args.root).resolve()
    if not root.exists():
        print(f"audit root does not exist: {root}", file=sys.stderr)
        return 2

    violations: list[Violation] = []
    for path in iter_rust_files(root):
        text = path.read_text(errors="ignore")
        violations.extend(audit_direct_reads(root, path, text))
        violations.extend(audit_prepared_track_literals(root, path, text))

    if violations:
        if args.json:
            import json
            print(json.dumps([v.__dict__ | {"path": str(v.path)} for v in violations], indent=2))
        else:
            print("PreparedTrack sample-rate migration hazards found:")
            for violation in violations:
                print(f"{violation.path}:{violation.line}: {violation.message}")
                print(f"    {violation.snippet}")
        return 1

    print("No PreparedTrack sample-rate migration hazards found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
