# tools — sanity assessment

71 of 71 read · 4 surprising

Each entry below is one **reading**, of a function or of a whole file. An
agent was given its name, signature, neighboring names and comments — never
its body — and wrote down what it expected to find. Then it opened the file.
The gap between the two is the finding. A file's own entry is titled `the file
itself` and asks whether the header at the top describes what is actually in
there.

`read at` is a hash of the body as it was when the reading was made. When it
stops matching the code, the reading is marked STALE and goes back in the
queue.

What this is and how to add to it: [README.md](README.md)

## tools/audit_concurrent_mutation_entrypoints.py

### the file itself
- spec 3 · read at `1439c385fc98` · commit `a6d7e33` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-21T03:05:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A CI-run static/text-based audit script that scans the Rust workspace's production source files (excluding cfg(test) modules) using simple brace-matching rather than a real parser, finds every site that constructs an external process/command or mutates user-library/output paths, classifies each into the four buckets from the docs (supervised mutation-capable, internal durable-authority helper, read-only/UI probe, scratch/workspace producer) against a maintained registry/allowlist, and fails via main() if any command-construction or mutation site is found that isn't accounted for — a drift detector for concurrency-unsafe or unreviewed mutation entrypoints.
- found: A Python CI audit with two halves: (1) regex/brace-aware scanning of workspace Rust files for Command::new and libc exec calls, diffed against three hardcoded inventories of (file, function, expected_count, category) tuples with 8 categories, not the 4 I guessed, plus negative self-tests; (2) a curated checklist of ~40 named mutation-touching functions asserted to contain required guard/claim substrings, not a full scan for every mutation site as I predicted.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no

### `text`
- spec 3 · read at `3fba39620a2b` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:26:22Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Joins rel with the repo root and reads/returns the file's full contents as a string via read_text(), a simple file-reading helper used throughout the audit script to load source files for pattern inspection.
- found: Joins rel onto ROOT and returns read_text(encoding="utf-8") — exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `function_body`
- spec 3 · read at `cadd8f5e1cfd` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:45:12Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads the Rust source file at rel, locates the definition of `fn name` (or similar signature) by string search, and uses the _matching_brace helper to find the matching closing brace, returning the full text of the function's body/signature as a string for later pattern-matching by the audit (e.g. via require/contains_all).
- found: Reads file text, finds 'fn {name}' marker, finds the following '{', then does manual brace-depth counting to find the matching close brace, returning the substring from the fn marker through that closing brace; raises AssertionError on any of the three failure cases (missing function, missing body, unterminated body).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: The file-level doc describes the audit's purpose/classification scheme but says nothing about this text-extraction utility function specifically.

### `require`
- spec 3 · read at `ae67ab9ed64c` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:32:56Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A small assertion helper used throughout this audit script: if `condition` is false, it prints/raises a failure referencing `label` and `detail` (likely via SystemExit or raising an exception) to halt the audit; if true, it may just pass silently or record a success line. Used as the uniform way each individual check in the audit reports pass/fail.
- found: Raises AssertionError with label:detail if condition is false; otherwise prints "[ok] {label}".
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `contains_all`
- spec 3 · read at `862df868bc0c` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:37:58Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Trivial utility that checks whether all strings in needles appear as substrings of haystack, implemented as something like all(n in haystack for n in needles).
- found: Returns True iff every string in needles is a substring of haystack, via all(needle in haystack for needle in needles).
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `_matching_brace` — TRAP
- spec 3 · read at `8e840abf09b8` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:55:39Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Walks through `source` starting at index `opening` (which points at an opening `{`), maintaining a depth counter incremented/decremented on `{`/`}`, while skipping over the contents of line comments, block comments, and string/char literals (respecting escape sequences) so braces inside them aren't miscounted, and returns the index of the matching closing brace once depth returns to zero.
- found: Scans character by character tracking brace depth, correctly skipping line comments, nested block comments, quoted strings (with escapes), Rust raw strings (r#"..."# with arbitrary hash counts), and distinguishing char literals ('x', '\\n') from lifetime syntax ('a) so their quote/backslash characters don't confuse the string-skipping logic; raises if the source ends before depth returns to zero.
- predicted: most · documented: none · derivable: yes · legible: most · trap: yes
- note: Nested block comments are tracked via a depth counter, and lifetimes vs char literals are disambiguated by look-ahead — someone extending this for other Rust literal forms (byte strings b"...", byte chars b'x') would silently miscount since those aren't handled.

### `strip_cfg_test_modules`
- spec 3 · read at `8273eabbaf89` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:30:44Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Scans the Rust source text for #[cfg(test)] mod ... { ... } blocks (likely using _matching_brace to find each module's matching closing brace), then replaces the interior contents with whitespace/blank characters (not deleting them) so byte/line offsets and newline counts stay identical, letting the rest of the audit tooling ignore test code without breaking line-based reporting.
- found: Regex-matches #[cfg(test)] mod NAME { headers, finds each match's matching closing brace via _matching_brace, and overwrites every non-newline character in that span with a space, preserving all offsets/line numbers.
- predicted: full · documented: full · derivable: no · legible: full · trap: no

### `external_constructor_count`
- spec 3 · read at `8c1a5d689b3a` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:42:59Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Counts occurrences of external process constructors (e.g. "Command::new(") in the given Rust source string, likely via source.count(...) or a regex findall, returning the integer count used elsewhere to audit how many external launch sites exist.
- found: Counts regex matches of PROCESS_COMMAND_RE in the source string.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `low_level_exec_count`
- spec 3 · read at `7e99faae9b08` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:48:04Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A one-line-body helper that counts occurrences of raw/low-level process-exec calls (e.g. Command::new( or libc exec/posix_spawn) in the given Rust source string, via a simple source.count(...) -- used by the audit script to flag external command construction sites that bypass the reviewed wrapper.
- found: Counts matches of a module-level LOW_LEVEL_EXEC_RE regex in the source string and returns the count -- confirms the general purpose (counting low-level exec call sites) but via regex findall rather than a plain substring count.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `impl_function_body`
- spec 3 · read at `790841b0cbc0` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:50:16Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Reads the Rust source file at rel, locates the `impl owner { ... }` block (possibly the specific inherent impl, not a trait impl), then searches within it for `fn name` and extracts that function's body text using brace matching (_matching_brace), returning the body as a string. Likely raises/asserts (via `require`) if either the impl block or the method isn't found, since this feeds an audit that needs guaranteed matches.
- found: Regex-locates `impl owner {`, brace-matches to get that block's text, regex-locates `fn name` within it, brace-matches the method body, and returns the exact substring including signature through closing brace, raising AssertionError at each step if not found.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `reviewed_function_body`
- spec 3 · read at `e0e8856dc6ff` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:53:06Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A dispatch helper used by this audit script: if `owner` is None it calls function_body(rel, name) to get a free function's source text, otherwise it calls impl_function_body(rel, owner, name) for a method; it then calls require(...) to assert the lookup actually found something (raising/erroring if not), and returns the resulting source text string for the caller to pattern-match against (e.g. checking for expected mutation-audit markers).
- found: Dispatches to function_body(rel, name) when owner is None, else impl_function_body(rel, owner, name); no additional assertion.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `workspace_production_source_roots`
- spec 3 · read at `c4c6ec64aeb1` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:04Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Reads the root Cargo.toml (and possibly member Cargo.toml files) to find workspace members, then returns a list of Path objects pointing at each member's `src` directory plus the root crate's `src` directory, so callers like production_rust_files can enumerate only production source code and skip tests/tools/target directories.
- found: Regex-parses Cargo.toml's [workspace] section to extract the members list (no TOML library), then builds a list starting with the root src dir plus each member's src dir, raising AssertionError if the workspace section, members list, or any member's src directory is missing.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `production_rust_files`
- spec 3 · read at `85821f948edd` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:11:26Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the paths returned by workspace_production_source_roots(), globs for "*.rs" files recursively in each, and returns a combined list of Path objects — likely excluding test files or files under a "tests" directory since it's specifically "production" rust files.
- found: Collects all .rs files recursively under each workspace_production_source_roots() path into a set (dedup), then returns them sorted. No test-file filtering happens here — that's presumably handled by the "production" nature of the source roots themselves.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `audit_external_launch_inventory` — QUIRKY — TANGLED
- spec 3 · read at `6da758827b79` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:00:47Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Scans all production Rust source files for external-command construction sites (e.g. Command::new/low-level exec calls), classifies each into one of the four buckets (supervised mutation-capable, internal-authority helper, read-only/UI probe, scratch/workspace producer) using helpers like external_constructor_count and low_level_exec_count, and compares the found set/counts against an expected hardcoded inventory in this function; prints diagnostics for any mismatch (new/missing/reclassified site) and returns True only if everything matches the expected inventory exactly.
- found: Validates a hardcoded inventory of external-launch/low-level-exec sites against actual scanned counts per function (raising AssertionError, not returning False, on any drift), scans all production Rust files to ensure every discovered external-constructor/exec site is accounted for in that inventory (also banning a Command builder from being returned across function boundaries), checks that several legacy backend execution APIs remain unreachable from the root app and that the tool-availability probe still matches its reviewed read-only shape, then runs synthetic negative self-tests injecting fake unclassified spawns/execve/fexecve to confirm the audit itself would catch them, finally returning True unconditionally if nothing raised.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: Missed the negative self-tests (synthetic unclassified spawn/exec injections) and the inactive-backend-API reachability guard entirely; also the function always raises on failure rather than returning False, so the bool return only ever means success.

### `main` — TANGLED
- spec 3 · read at `434e38a38257` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:58:10Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A long sequential script-style function that loads production Rust source files, then runs many require()/contains_all() style assertions checking invariants about external command construction, mutation entrypoints, and lease/lock usage, printing progress or errors as it goes, and returning 0 if all checks pass or 1 if any fail (likely via a try/except catching an assertion-style error and printing it).
- found: A very long sequence of require() calls checking dozens of named invariants about mutation admission/claim ordering across the whole codebase (metadata, CUE, archive, rename, presets, artwork picker, recovery, etc.), each with a detailed English rationale string; ends by printing a pass message and returning 0. Far larger in scope and detail than I predicted — I got the general shape right but massively underestimated the number/specificity of individually named subsystems audited.
- predicted: most · documented: most · derivable: no · legible: some · trap: no

## tools/audit_prepared_track_sample_rate.py

### the file itself
- spec 3 · read at `dd6d706eb286` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:23:56Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A standalone Python static-analysis script (no Rust compiler needed) that walks all .rs files in the repo, finds occurrences of `.sample_rate` direct field access outside PreparedTrack's own impl block/compatibility helpers (scalar_sample_rate/require_scalar_sample_rate), and flags them as migration hazards now that sample_rate became Option<u32>. It also scans for PreparedTrack struct literal construction sites and checks that the sample_rate field expression is Option-valued (e.g. wrapped in Some(...)) rather than a bare integer, to catch places not updated for the new type. main() runs both audits and prints/reports the violations (likely with file:line), probably exiting nonzero on any findings for CI use.
- found: Matches prediction exactly: lexical (regex/brace-matching, not AST) scan of .rs files for two hazards — direct .sample_rate reads outside PreparedTrack's impl/known-safe contexts, and PreparedTrack struct literals missing sample_rate/source_audio or with a non-Option-looking sample_rate initializer. Reports violations (text or JSON) and exits nonzero for CI use. Docs (module docstring) explained the two hazard classes but not the lexical/lint-not-typechecker implementation detail, which the code + trailing docstring paragraph clarifies.
- predicted: full · documented: most · derivable: no · legible: not judged · trap: no

### `iter_rust_files`
- spec 3 · read at `178711b12fdd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:48:07Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks the given root Path and yields all files ending in .rs, likely skipping directories such as target/ or .git/ to avoid scanning build artifacts.
- found: Recursively globs *.rs files under root, skipping any path with target or .git in its parts.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `line_number_at`
- spec 3 · read at `45bbe1293bf8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:56:02Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Returns the 1-indexed line number corresponding to a given character offset in text, computed as text.count('\n', 0, offset) + 1.
- found: Exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `line_at`
- spec 3 · read at `b955a7bb1916` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:02:11Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Splits `text` into lines and returns the 1-indexed `line_no`th line (stripped or raw), returning an empty string if line_no is out of range — a small helper for reporting source context in audit messages.
- found: Exactly as predicted: splitlines, bounds-check, return stripped line or empty string.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `extract_braced_block`
- spec 3 · read at `bf35d6439c7d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:43:09Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Given text and a start index (pointing at or near an opening brace), scans forward counting brace depth to find the matching closing brace, skipping over braces inside string/char literals and comments so it doesn't miscount, and returns (block_text, end_index) — or None if no opening brace is found or the braces are unbalanced/text ends first.
- found: Finds first '{' at/after start, then scans char-by-char tracking brace depth while correctly skipping over line comments, block comments, string literals, and char literals (with escape handling), returning the matched block text and end index once depth returns to 0, or None if unbalanced.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `in_prepared_track_impl` — TRAP
- spec 3 · read at `4fcb4bdaa25a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:16:17Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Scans backward/forward from `offset` in `text` to find the enclosing `impl` block and checks whether it's an `impl PreparedTrack` (or similar) block, returning True if so. This lets the audit script exempt direct `.sample_rate` field accesses that occur inside PreparedTrack's own impl (e.g. inside scalar_sample_rate()) from being flagged as violations, since those are the sanctioned compatibility helpers.
- found: Only applies when path.name == "types.rs" (restricting the exemption to one file); finds the nearest preceding "impl PreparedTrack" via rfind, then finds the next impl/struct/derive boundary after offset, and returns True if offset falls between them — a purely textual heuristic, not real Rust parsing.
- predicted: most · documented: none · derivable: yes · legible: full · trap: yes
- note: The impl-boundary detection is a naive substring search (rfind/find on literal tokens) with no brace-depth or nested-impl awareness, so a differently-formatted or nested impl block elsewhere in types.rs could produce a false positive/negative silently — worth flagging since this gates whether the audit script correctly exempts sanctioned accessors.

### `audit_direct_reads`
- spec 3 · read at `3f82d8e0364a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:17:33Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Scans the given Rust file's text for direct `.sample_rate` field accesses (via regex), skipping ones that are inside the PreparedTrack impl block's own compatibility helpers (scalar_sample_rate/require_scalar_sample_rate) or in the struct definition itself, and skipping any that are already guarded/matched appropriately. For each remaining direct access it computes the line number and appends a Violation instructing the caller to use scalar_sample_rate()/require_scalar_sample_rate() instead.
- found: Finds regex matches for direct .sample_rate reads, builds a small surrounding-line text context for each, then skips if inside the PreparedTrack impl, if the context contains an allowlisted non-PreparedTrack read pattern, or if it already references the scalar_sample_rate helpers. Fixture test files get a distinct violation message about asserting scalar_sample_rate()/source_audio instead of the raw field; everything else gets the generic "use scalar_sample_rate()/require_scalar_sample_rate()" violation.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `field_present`
- spec 3 · read at `220093166857` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:07:34Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Checks whether a struct-literal field assignment for `field` (e.g. "field:" or "field :") appears in the given block of source text, likely via a regex search, used to detect whether a PreparedTrack literal explicitly sets the sample_rate field.
- found: Regex search using a shared FIELD_RE_TEMPLATE with the field name escaped and substituted in, returns whether it matched in the block.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `first_field_expr`
- spec 3 · read at `0204b3ac5a9a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:21:16Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Given a braced struct-literal block of Rust source text and a field name, finds the first occurrence of "field: <expr>" (e.g. "sample_rate: ...") via string/regex search and returns the expression text up to the next comma or closing brace, or None if the field isn't present in the block.
- found: Uses a regex built from a FIELD_RE_TEMPLATE plus a trailing pattern matching up to a comma (allowing trailing line comments) or end of string, to extract and strip the expression following "field:" in the block; returns None if no match.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `sample_rate_expr_is_option_valued` — QUIRKY
- spec 3 · read at `a5c36c368c8b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:24:04Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A string-based heuristic that inspects the matched `.sample_rate` expression text and returns True if it still looks like a raw Option value — i.e. it does not already end with `.unwrap()`, `.unwrap_or(...)`, `?`, or a call into the compatibility helpers (`scalar_sample_rate`/`require_scalar_sample_rate`) — so the audit only flags genuinely-unsafe direct reads and not already-unwrapped/compat-helper usages.
- found: Normalizes whitespace then checks if any token from a module-level OPTION_VALUED_SAMPLE_RATE_EXPRESSIONS allowlist appears in the expression — a positive match against known Option-valued patterns, not my guessed negative check for already-safe unwrap/helper calls.
- predicted: some · documented: none · derivable: no · legible: most · trap: no
- note: Depends on the OPTION_VALUED_SAMPLE_RATE_EXPRESSIONS constant defined elsewhere in the file, which the handout didn't include, so the actual matching semantics aren't fully visible from this function alone.

### `audit_prepared_track_literals`
- spec 3 · read at `483546a1b82e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:44:30Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Scans the given Rust file's text for PreparedTrack struct-literal construction sites, extracts the braced block for each, and checks whether a `sample_rate:` field is present and whether its value expression is Option-valued (e.g., wrapped in Some(...)) rather than a bare u32. Collects and returns a list of Violation objects for any struct literals that set sample_rate incorrectly (missing or non-Option value), using line_number_at/line_at helpers to report positions.
- found: Finds PreparedTrack struct-literal sites (filtering out struct/impl defs and string matches), extracts the braced block, and flags violations for missing sample_rate field, a sample_rate initializer that isn't visibly Option-valued, and also missing source_audio field.
- predicted: most · documented: most · derivable: no · legible: most · trap: no

### `main`
- spec 3 · read at `d86403a03f94` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:06:45Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Parses a repo-root argument (default "."), walks Rust files via iter_rust_files, and runs both audit_direct_reads and audit_prepared_track_literals over each file's contents to collect violations. Prints the violations with file/line info, and returns 0 if none were found or 1 if any migration hazards were detected (for CI use).
- found: Parses root arg and --json flag, validates root exists, walks Rust files running both audit functions, then prints results (JSON or plain text) and returns 1 if violations found, 0 if clean, 2 if root missing.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tools/audit_test_coordination_isolation.py

### the file itself
- spec 3 · read at `b8f1dbe9f83b` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:45:17Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Static-analysis CI script scanning Rust test files: uses test_functions to locate #[test]/#[tokio::test] function bodies (with matching_brace scanning source text for each function's closing brace), searches each body for calls into the coordination registry/queue/recovery entrypoints, and flags any such test that doesn't also call one of the approved serialized-isolation fixture helpers — printing violations and exiting non-zero to fail CI when an unguarded test is found.
- found: Scans all .rs files under src/ and tests/ for #[test]/#[tokio::test] functions (extracting bodies via a hand-rolled Rust-aware brace matcher that respects strings, raw strings, char literals, and comments), checks each body against a large table of regex "direct entrypoint" patterns (mutation claims, persistent leases, queue sync/load, various production mutation boundary functions), and for any test that touches one, requires it also contain one of several approved scoped-isolation fixture call markers (with two hardcoded cross-process child exemptions); prints FAIL lines and exits 1 if any coordination-touching test lacks a scope marker, otherwise prints an ok summary count.
- predicted: full · documented: most · derivable: no · legible: not judged · trap: no

### `matching_brace`
- spec 3 · read at `d8a1ace75814` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:13Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Scans Rust source text starting at an opening `{` index and returns the index of its matching `}`, tracking nesting depth via a counter. Because it's long for a brace matcher, it likely skips over string literals, char literals, and line/block comments so that braces inside those don't affect the depth count, handling escape sequences within strings too.
- found: A hand-rolled Rust lexer scanning for the matching close brace: tracks nested block comments, line comments, string literals with escapes, raw strings (r#"..."#, arbitrary hash count), and char literals (including escaped chars like '\n'), only counting { and } depth outside of those; raises AssertionError if it runs off the end unterminated.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_functions`
- spec 3 · read at `2d5e56f068f7` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:30:43Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads the Rust source file at path, scans for #[test]/#[tokio::test] attributes followed by fn declarations (via regex), and for each match uses the matching_brace helper to find the function body's closing brace, yielding tuples like (name, start_line, end_line, body_text) for each discovered test function — feeding the audit's scan for coordination-registry entrypoints inside test bodies.
- found: Regex-scans the file for #[test]/#[tokio::test(...)] attributes, then finds the following fn signature and its opening brace, uses matching_brace to locate the closing brace, and yields (name, body_text_including_braces, start_line) for each test function found.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tools/verify_concurrency_corrective_round1.py

### the file itself
- spec 3 · read at `af06157b5bf9` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:52:14Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A small standalone Python script (no Rust toolchain needed) that greps specific source files as plain text and asserts, via a require(condition, message) helper, that certain safety-critical markers from the 2026-08-18 concurrency corrective (e.g. specific guard/lock usage, claim-acquisition ordering, or seam comments) are still present -- failing loudly with a clear message if someone strips them out. text(path) is likely a small helper to read a file's contents for these substring/regex checks.
- found: A Python script with text()/require() helpers that loads several concurrency-critical Rust source files (concurrency.rs, tool.rs, processor.rs, stages.rs, script_supervisor.rs, db.rs, main.rs, plus a few TUI/pipeline files and Cargo.toml/an integration test) and runs dozens of precise substring/negative-substring/ordering assertions tied to a numbered list of specific fixes (#0 lease publication ordering and self-healing, #1a PATH resolution, #2 future-boxing for stack size, #1b test supervision, #3-5 hermetic per-test coordination roots, plus a batch of "operator" compile/integration fixes) -- failing with a labeled message and exit(1) if any protocol/safety seam is missing or reordered.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no
- note: The header describes the script's purpose/role but gives no hint of the extremely specific, itemized, numbered-fix structure and index-ordering checks inside -- that structure could only be seen by reading the body.

### `text`
- spec 3 · read at `8cc5fb74dd9c` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:41Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Tiny helper that reads the file at path and returns its contents as a string — likely just open(path).read() with UTF-8 handling, used by require()/main() to fetch source text for the string-presence checks.
- found: Reads the file at ROOT / path as UTF-8 text and returns it.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `62ab1576d4c4` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:31:03Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A simple assertion helper: if condition is false, prints a failure message including label (likely to stderr) and exits the script with non-zero status (or raises SystemExit); if true, it's a no-op, possibly printing an "ok" confirmation.
- found: If condition is false, prints "[FAIL] {label}" to stderr and raises SystemExit(1); otherwise prints "[ok] {label}" to stdout.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## tools/verify_concurrency_corrective_round4.py

### the file itself
- spec 3 · read at `86ee7b1204dc` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:27:24Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A standalone Python script (no test framework) that greps/reads specific Rust source files tied to a "round-4 concurrency corrective" patch and asserts textual invariants still hold — e.g. a mutex acquired before a channel send, a lock guard not dropped early, specific ordering/patterns in the code. It's organized into section-grouped require/require_order/ok checks called from main, printing pass/fail per check and exiting nonzero if any fail, acting as a lightweight source-level regression gate alongside the real Rust test suite.
- found: A ~250-line standalone script with helper functions (read/ok/require/require_order/section) used entirely inline in main() to assert dozens of very specific source-text invariants (substring presence, ordering, counts) across ~11 Rust files in a TUI/audio-conversion codebase (concurrency, db, tui, convert pipeline). It encodes seven numbered regression concerns (R1-R7) from a "round-4 concurrency corrective" patch — v24 activation gating, recovery lease strictness, PATH-vs-canonical executable resolution, pipeline SIGPIPE arbitration, lossless non-UTF8 path serde, writer-claim reservation ordering, boxing large futures, and error-contract/recovery-token semantics — each with a plain-English label printed on success.
- predicted: most · documented: most · derivable: no · legible: most · trap: no
- note: The header names only "the round-4 concurrency corrective" generically; the actual scope (7 distinct concerns spanning locking, PATH resolution, SIGPIPE handling, serde, and future boxing) is far broader than "concurrency" suggests and only discoverable by reading all the R1-R7 comments in main().

### `read`
- spec 3 · read at `46685565c9e3` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:49Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads a file relative to the repo root and returns its contents as a string (UTF-8), used to feed source text into require()/section() checks — same pattern as the analogous helper in round1's script.
- found: Reads the file at ROOT / relative as UTF-8 text and returns it — identical in form to the round1 script's text() helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ok`
- spec 3 · read at `5690c955211e` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:50:57Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Tiny helper that prints a formatted passing-check line to stdout, e.g. print(f" [ok] {label}") — used by require/require_order after a check succeeds, no return value or other side effects.
- found: Prints "[ok] {label}" to stdout, exactly as predicted.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `require`
- spec 3 · read at `55ea177dc603` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:58:59Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A simple assertion helper: check `condition`, and if false, report failure using `label` (likely printing an error message and either raising SystemExit/AssertionError or marking a global failure flag) so the script can continue checking other assertions and report an overall pass/fail summary at the end.
- found: Raises AssertionError(label) if condition is false; otherwise calls the peer ok(label) to record/print success.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `require_order`
- spec 3 · read at `d2a3121e9d9b` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:12:21Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Finds the string indices of `earlier` and `later` within `text` and asserts that `earlier`'s index is less than `later`'s index, reporting success/failure via the ok()/require() helpers with `label` as the descriptive message. Likely also asserts both substrings are actually found (not -1) before comparing.
- found: Finds indices of earlier/later substrings in text and calls require() to assert both are found and earlier precedes later, using label as the failure message.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `section`
- spec 3 · read at `df0d8e2124f3` · commit `a6d7e33` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-21T02:04:08Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Extracts and returns the substring of `text` between the first occurrence of `start` and the following occurrence of `end` (searched after `start`'s position), used to isolate a specific struct/impl/function block of source for targeted assertions, similar to the round-7 script's `function` helper but for arbitrary start/end markers rather than brace matching.
- found: Returns the substring of text between the first `start` marker and the following `end` marker found after it.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `main`
- spec 3 · read at `f16724131552` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:33:28Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads one or more relevant Rust source files via read, then runs a long, linear sequence of require/require_order/section calls that grep for specific safety-critical code patterns (e.g. lock acquisition order, particular guard/claim usage) introduced by the round-4 concurrency corrective patch, printing ok() for each passing check. Accumulates failures and returns a nonzero exit code if any assertion fails, 0 otherwise.
- found: Reads a fixed set of Rust source files plus the whole src/crates tree, then runs ~25 require/require_order checks (grouped R1-R7) grepping for specific textual invariants that lock down safety-critical properties from a concurrency corrective patch: v24 activation gating without env-var dependency, strict vs local-handoff lease recovery, PATH-vs-canonical executable resolution, pipeline producer/consumer polling order and SIGPIPE arbitration, lossless non-UTF8 path serde alongside UTF-8 wire compat, native-writer reservation-before-shared-claim ordering with rollback, boxing of large recursive pipeline futures without a stack-size env mask, and source-guard recovery deferral semantics. Prints a success message and returns 0 if all pass.
- predicted: most · documented: some · derivable: no · legible: most · trap: no

## tools/verify_concurrency_corrective_round5.py

### the file itself
- spec 3 · read at `7f4e168f704a` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:33:56Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Similar to the round6-r1 verifier: a standalone Python script that reads specific Rust source files relevant to a "round 5" concurrent-sessions bugfix, extracts sections/functions via `section`/`test_section`, and uses `require`/`require_order` to assert source-level ordering and authority invariants (e.g. that certain checks happen before certain mutations) plus that a fixed list of named regression tests exist. `main` runs all checks, prints a pass message, and exits non-zero on failure — a lightweight structural gate, not a substitute for compiling/running the Rust test suite.
- found: A large source-structural verifier covering many independent concurrency-fix invariants (labeled D1a-D4 plus flaky-cluster isolation) across ~10 Rust files: FLAC write-claim teardown ordering, native-lock admission ordering, script-supervisor timeout measurement points, pipeline reaping/timeout test tuning, journal recovery process-identity scoping, streaming progress/archive error ordering, and test-isolation lock ordering — plus a final check that the shell gate script runs the right focused tests and stress-run counts.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no

### `read`
- spec 3 · read at `42b8b9abc750` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:50:52Z · by ross@rossturk.com · warm reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Same as the round6 sibling: joins `relative` onto a module-level ROOT path and returns the file's contents as UTF-8 text.
- found: Joins relative onto ROOT and reads the file as UTF-8 text, identical to the round6 script's helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: Duplicate helper across multiple verify_concurrency_corrective_roundN.py scripts — same code copy-pasted per round rather than shared.

### `ok`
- spec 3 · read at `d7a294e34735` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:58:57Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Prints a formatted success line to stdout, e.g. "OK: {label}", used by the verifier script to report that a given check/assertion passed as it runs through its list of static invariants.
- found: Prints "[ok] {label}" to stdout — a trivial success-line helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `9d0c9732e46e` · commit `a6d7e33` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-21T02:04:07Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: If condition is falsy, prints/raises a failure referencing `label` and exits the script with a nonzero status (this is a structural verification script, so failures should halt it immediately); if truthy, it likely calls the `ok` peer helper to record/print success for that label.
- found: Raises AssertionError(label) if condition is false; otherwise calls ok(label) to record/print success.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require_order`
- spec 3 · read at `fa5440d580bc` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:26:59Z · by ross@rossturk.com · warm reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Identical to the round6 version: text.find for `earlier` and `later`, then require(left >= 0 and right >= 0 and left < right, label).
- found: Identical to the round6 sibling: text.find for both substrings, then require(left >= 0 and right >= 0 and left < right, label).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: Duplicated verbatim across round5/round6 verifier scripts (and likely other rounds) — a shared module would remove the duplication.

### `section`
- spec 3 · read at `49659b2d6360` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:12:18Z · by ross@rossturk.com · warm reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Same as the round6 sibling: returns text[begin:finish] where begin is text.index(start) and finish is text.index(end, begin), so the slice includes the start marker text and excludes the end marker, relying on str.index raising ValueError if either isn't found.
- found: Identical to the round6 sibling: text[begin:finish] with begin=index of start, finish=index of end after begin.
- predicted: full · documented: none · derivable: no · legible: full · trap: no
- note: Duplicated verbatim helper across round5/round6 verify scripts — this repo seems to have many near-identical one-off verify_*_roundN.py scripts rather than a shared module.

### `test_section`
- spec 3 · read at `cda374f07b97` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:33:12Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Given the full text of a Rust test file and a test function name, locates that specific test function (e.g. by finding "fn test_name" and matching to the next fn/test boundary) and returns just that function's source slice as a string, so callers like require/require_order can assert properties about statements confined to that one test.
- found: Finds "fn {test_name}" in text and slices from there to the next "\n #[" attribute line (or end of file), returning that substring as the isolated test-function section.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `main`
- spec 3 · read at `2707dd6d4a7b` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:28Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Orchestrates the whole verification run: calls section()/test_section() to group related checks, reads relevant source/test files via read(), and issues many require()/require_order() calls asserting specific ordering, authority-check, and regression-test invariants for the round-5 concurrent-sessions fix. Tracks pass/fail state (via ok()), prints a summary, and returns 0 if everything passed or a nonzero exit code if any invariant failed.
- found: Reads ~10 source/script files and runs dozens of require()/require_order() checks (each with a labeled invariant string), each checking for exact substring presence/absence or ordering of specific code snippets within named sections (functions/tests) — verifying FLAC claim teardown ordering, timeout measurement points, cancellation gates, reaping regression synchronization, journal recovery scoping, and the validate_concurrency_round5.sh gate script contents — then prints a pass message and returns 0 (would presumably raise/exit nonzero on failure inside require/require_order, not shown here).
- predicted: most · documented: most · derivable: no · legible: most · trap: no
- note: No visible error/exit-code path here since require/require_order presumably raise on failure — the never-fails 'return 0' at the end only reflects success framing, not the full contract.

## tools/verify_concurrency_corrective_round6.py

### the file itself
- spec 3 · read at `9f49bc1dc807` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:33:54Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A standalone regex/text-based static verification script (no AST/compiler) with helper functions read/require/require_order/section/test_section/ok, that scans specific Rust source files for the presence and correct ordering of code patterns that were fixed in a 'round 6' concurrency corrective, plus a check that some executable/dynamic gate still enforces the original bug's bar. main() orchestrates these checks and reports pass/fail for CI, explicitly not replacing compiling or running the real Rust test suite.
- found: A substring/index-based (not regex) static verifier with require/require_order/section/test_section helpers that reads several specific Rust source files plus a shell gate script, asserts source-level fixes for three labeled defects (F1 artwork picker policy, F2 repackage cancellation ordering, F3 scoped test coordination root lifetime/race) remain in place, and checks that the executable acceptance-gate script still contains the required tokens (compile, prior round scripts, specific test names, stress-run counts, and forbidden-env-mask absence).
- predicted: full · documented: none · derivable: yes · legible: not judged · trap: no
- note: I guessed 'regex' but it's plain str.find/index/in substring matching, and I didn't anticipate the specific three-defect (F1/F2/F3) plus guardrail/acceptance-gate structure, though the overall purpose and mechanism class were right.

### `read`
- spec 3 · read at `e105f4539337` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:50:44Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A tiny helper that joins `relative` onto a repo-root base path (a module-level constant) and returns the file's text contents as a string, used by the other checks in this script to read source files for pattern matching.
- found: Joins relative onto ROOT and reads the file as UTF-8 text, returning its contents as a string.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ok`
- spec 3 · read at `2cc065768612` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:56:02Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Tiny helper that prints a formatted "pass" line for a given check label, e.g. print(f" ok: {label}"), used alongside require/require_order to log each verification step as it succeeds in this static-check script.
- found: Prints "[ok] {label}" — exactly the predicted pass-line logger.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `b981e3685f97` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:02:25Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A small assertion helper: if condition is False, it prints/reports a failure message including label (e.g. to stderr) and exits the script with a nonzero status (sys.exit(1) or raises), acting as a hard gate for this verification script's checks. If condition is True, it likely does nothing or calls the peer `ok()` to record/print a pass.
- found: Raises AssertionError(label) if condition is false; otherwise calls ok(label) to record/print the pass, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require_order`
- spec 3 · read at `2fb0e5858a07` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:26:51Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Finds the index of `earlier` and `later` in `text` (e.g. via str.index or str.find), and raises/reports a failure with `label` if `earlier` does not appear before `later` (including cases where either substring is missing). Likely calls a shared `require`/`ok` helper to report success/failure rather than raising directly.
- found: Exactly as predicted: uses text.find for both substrings and calls require() with the label, checking both were found and earlier precedes later.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `section`
- spec 3 · read at `ab578efe9747` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:11:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Finds the index of `start` in `text`, then finds `end` after that point, and returns the substring between them (not including the markers themselves). Likely raises an assertion or exception if either marker isn't found, since this is used by a verification script that pins source-level contracts.
- found: Returns text[begin:finish] where begin is the index of start marker and finish is index of end marker found after begin; the start marker text is included in the returned slice, end marker excluded, relying on str.index's natural ValueError if not found.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: The returned slice includes the `start` marker text itself (slice begins at index of start, not after it), unlike a typical "between markers" helper that strips both.

### `test_section` — TRAP
- spec 3 · read at `260a6b5df168` · commit `a6d7e33` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-21T01:31:11Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Similar to the "function" extractor in the round6_r1 verifier: locates a Rust test function by name (e.g. `fn {test_name}(`) in the source text and extracts its full body via brace-depth counting, returning the substring so the caller can check its contents. Likely raises an error if the test name isn't found or the braces don't balance.
- found: Finds `fn {test_name}` in the text and slices out everything up to the next `\n #[` attribute marker (or end of text if none), returning that as the "section" — a much cruder heuristic than brace-counting, relying on 4-space-indented attribute lines marking the next test.
- predicted: most · documented: none · derivable: yes · legible: full · trap: yes
- note: This slice-to-next-attribute heuristic silently breaks if the next item isn't a 4-space-indented #[...] attribute (e.g. a plain fn, different indentation, or the last test in the file followed by non-attribute code) — it would over- or under-capture without any error.

### `main`
- spec 3 · read at `f8fcf8565b70` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:34Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Reads the relevant source files (likely src/concurrency.rs and its tests), then runs a sequence of section/require/require_order checks confirming specific round-6 fixes are present in the source text (e.g., that certain functions, comments, or code patterns exist and appear in the right order), prints pass/fail output via ok/test_section, and returns 0 if everything passes or 1 if any check fails — a static grep-style regression gate, not a real test runner.
- found: Reads six source/tool files and runs a long series of require()/require_order() string-presence and ordering checks against extracted sections, pinning three specific round-6 fixes (F1: artwork picker keeps full file-manager policy; F2: repackage-archive cancellation ordering vs progress/format/staging checks; F3: scoped test coordination root is no longer RAII-deleted, avoiding a use-after-retirement race) plus a guardrail that lease staging semantics weren't changed to fake-fix F3, then checks that the executable acceptance-gate shell script still contains a long list of required tokens (specific test names, cargo commands, forbidden env var removal) before printing success and returning 0.
- predicted: most · documented: most · derivable: no · legible: most · trap: no

## tools/verify_concurrency_corrective_round6_r1.py

### the file itself
- spec 3 · read at `402afc2710a9` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:33:44Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A standalone Python script (not part of the Rust build) that performs static, text-based verification of a specific concurrency-correctness patch ("round 6, r1") related to the artwork picker: it reads specific Rust source files, extracts named functions/sections via helpers like `section`/`function`, and uses `require`/`contains_all` to assert that picker mutations go through an explicit request boundary and are dispatched via existing coordination paths. `main` runs a list of such checks (each via `test`) and reports pass/fail, exiting non-zero on any failure — a grep-based structural gate meant to run in CI alongside (not instead of) actual compilation/tests.
- found: Source-structural (grep/AST-lite, not compiled) verifier that reads several specific Rust files (file-picker state/input/source_guard, keybindings, event_loop, app) plus two other tool scripts, and runs ~25 `require()` checks confirming that artwork-picker mutations (create/rename/duplicate/paste/delete/case-rename) go through a host-managed request boundary (`FilePickerHostMutationRequest`) before touching the filesystem directly, that only the artwork surface opts into host-managed mode, that dispatch reuses Tonepoet's shared claim/admission/retry primitives, and that a specific list of focused regression tests exists and is wired into the shell gate script.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no

### `read`
- spec 3 · read at `ed6951b27667` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:50:48Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Reads and returns the full UTF-8 text of the file at `relative`, resolved against a fixed ROOT base path, same pattern as the identical helper in the round7 verifier script.
- found: Identical one-liner to the round7 script's `read`: joins relative onto ROOT and reads UTF-8 text.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: Duplicated verbatim across multiple tools/verify_*.py scripts rather than shared — a repo-wide pattern, not specific to this function.

### `section`
- spec 3 · read at `bf127f8a8c37` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:58:40Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Finds the index of `start` in `text`, then finds `end` after that point, and returns the substring between them (used to slice out a specific function/impl block from a Rust source file for the structural checks that follow). Likely raises an error (or lets a later `require` call fail) if either marker isn't found.
- found: Slices text between the first occurrence of start and the first occurrence of end after it, via str.index (which raises ValueError if either marker is missing).
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `function`
- spec 3 · read at `5ee03b21791e` · commit `a6d7e33` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-21T01:30:57Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A helper used by this source-structural verifier to extract a single Rust function's body out of a larger source file's text, given its name — likely finding `fn {name}` (or similar signature pattern) and then scanning forward with brace-depth counting to capture the full function body as a substring, for later structural checks like contains_all. Probably raises/asserts if the function name can't be found.
- found: Finds `fn {name}(` in the text, then brace-depth-counts from the following `{` to find the matching closing brace, returning the full function source as a substring; raises AssertionError if the braces never balance out (never terminates).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `test` — OBSCURE
- spec 3 · read at `ff7320d09265` · commit `a6d7e33` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-21T02:03:47Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A small helper that looks up a named regex pattern (keyed by `name` from some PATTERNS table) and searches it against `text`, returning the matched substring. If no match is found it raises an error (AssertionError or similar) that includes `name` so failures in this structural verifier are easy to trace back to which invariant failed.
- found: A one-line wrapper that just calls and returns `function(text, name)` — presumably extracting a named function's source/body from text as a convenience alias.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: Name `test` is misleading in a file full of structural assertions — it's just an alias for `function`, not an assertion itself; the real checking presumably happens in `require`/`contains_all` calls elsewhere.

### `require`
- spec 3 · read at `8811c330b54c` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:11:58Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A simple assertion helper: if condition is False, it reports/prints failure with the given label and causes the script to exit non-zero (or raises), while success is either silently accepted or logged as a passed check.
- found: Raises AssertionError(label) if condition is false; otherwise prints "[ok] {label}".
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `contains_all`
- spec 3 · read at `d5aae0f7d406` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T02:26:15Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns True if every string in `needles` is a substring of `text`, implemented as a simple `all(n in text for n in needles)` one-liner.
- found: Exactly as predicted: `all(needle in text for needle in needles)`.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `main`
- spec 3 · read at `ed70245ebadd` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:27Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Orchestrates a series of static, source-text checks (using the read/section/function/test/require/contains_all helpers) against specific Rust source files to verify the round-6-r1 invariants — that artwork-picker mutations go through an explicit request boundary and that Tonepoet dispatches those requests via existing coordination paths. Accumulates any failures, prints a pass/fail report, and returns an exit code (0 for all checks passing, 1 if any failed).
- found: Reads several specific Rust source files (picker state/input/source_guard, keybindings, event_loop, app, plus two audit/gate scripts) and runs dozens of very granular `require()` structural checks (substring/ordering checks on function bodies and sections) verifying the artwork picker's host-managed mutation request boundary, shared claim reuse, focused regression test presence, and that the audit/gate scripts enforce this round's invariants; prints a success message and returns 0 (require presumably aborts/exits on failure rather than accumulating).
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

## tools/verify_concurrency_corrective_round7.py

### the file itself
- spec 3 · read at `a4fd4027b88b` · commit `a6d7e33` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-21T02:02:49Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A standalone verification script (not part of the main package) that statically checks the source of a specific fix ("round-7 fork-safe ephemeral retirement corrective") against a set of hard-coded invariants — e.g. that MutationClaimGuard teardown retires its own ephemeral descriptor, that exported/detached authority stays visible, and that the classification scanner treats retired pathnames as absent only for valid ephemeral descriptors while remaining fail-closed for other cases. It likely has a `read` helper to load target source file(s), a `function` helper to extract a named function's body via regex/AST, a `require` helper that asserts a condition and records/prints pass-fail, and a `main` that runs all the checks in sequence and exits nonzero on any failure, acting as a regression guard for this specific patch round.
- found: A standalone Python script with read/function/require helpers that greps and slices src/concurrency.rs (and scripts/validate_concurrency_round7.sh) to statically assert dozens of very specific structural invariants about the round-7 fix: that lifetime export is recorded only after descriptor clone, that eager ephemeral retirement is restricted to unexported EphemeralMutation authority and proves path identity before unlinking, that PersistentLease Drop stays close-only, that the classifier's unpublished-ephemeral skip is structural/NotFound-specific/lock-free, that specific named regression tests exist in both the source and the gate script, and that the gate script still runs all prior round verifiers plus the stress-test loops.
- predicted: most · documented: most · derivable: no · legible: most · trap: no
- note: My prediction correctly guessed the overall shape (assertion helpers + read/function/require + main) but underestimated the sheer number and specificity of individual invariants checked (nearly 20 separate require() calls covering ordering, lock-freedom, test existence, and gate-script composition).

### `read`
- spec 3 · read at `fd2812ee9f54` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:50:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Reads and returns the full text contents of the file at `relative`, joined against some fixed repo-root base path, likely using pathlib's read_text().
- found: One-liner: joins relative path onto ROOT and reads it as UTF-8 text.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `function`
- spec 3 · read at `5f841e761078` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:30:54Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Extracts and returns the source text of a single named Rust function (fn name) out of text — locating the fn name declaration, then scanning forward with brace counting to find the matching closing brace, returning the substring from the function signature through its closing brace. Likely used with require to assert particular code patterns inside that extracted body.
- found: Finds "fn name(" marker, then brace-counts from the first opening brace to find the matching closing brace, returning the substring from the fn declaration through the closing brace; raises AssertionError if unterminated.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `d69761c9aabd` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:55:56Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A small assertion helper: if condition is false, prints an error message including label (and likely exits the script with a nonzero status, since this is a standalone verification tool meant to fail CI on violation); otherwise it does nothing and returns.
- found: Raises AssertionError(label) if condition is false; otherwise prints "[ok] {label}" to indicate the check passed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: None.

### `main`
- spec 3 · read at `0123f4453ab6` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:40:38Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads the relevant Rust source (concurrency/claim-guard module) and runs a series of require() structural checks via function()/read() helpers, verifying that MutationClaimGuard teardown retires its own ephemeral descriptor, that exported/detached descriptors remain externally visible, and that descriptor classification treats retired-but-structurally-valid ephemeral paths as absent while staying fail-closed for durable/rebound/malformed/live descriptors. Prints a success message and returns 0.
- found: Reads src/concurrency.rs and the round-7 gate script, then runs many granular require() checks on specific struct/function bodies (PersistentLease export flag ordering, retire_ephemeral_descriptor_on_guard_drop restrictions, into_lease not retiring, classify_descriptor delegation without same-PID bypass, etc.), confirms a list of named regression tests exist both in source and in the gate script, checks the gate still runs all prior rounds' verifiers plus the stress-test bar, and prints success/returns 0.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

## tools/verify_dvda_phase2_workspace.sh

### the file itself — QUIRKY
- spec 3 · read at `197dbdb206cb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:23:49Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This is a small shell script (18 lines) with no header docs, one function `usage`, likely a manual verification tool for DVD-Audio "Phase 2" workspace output — probably takes a workspace path argument and checks that expected extracted/converted files exist, printing usage if invoked wrong. It's likely a developer utility script, not part of the main build/test pipeline, given the `tools/` location and narrow scope.
- found: It's an acceptance-gate script for the DVD-Audio Phase 2 bundle: it validates repo root, checks required tools (python3, cargo, an audit script), then runs a sample-rate migration audit script, cargo fmt --check (or fmt --all if an env var is set), cargo check --workspace, and cargo test --workspace. It's a CI/certification gate, not a per-file output-existence checker as I guessed.
- predicted: some · documented: none · derivable: no · legible: not judged · trap: no
- note: No file-level doc comment exists; the only documentation is embedded in the usage() heredoc, which sanity_next's `docs` field did not surface (it showed empty).

### `usage`
- spec 3 · read at `00f84d8b4c29` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:23:38Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A shell function that prints a usage/help message describing how to invoke this DVD-Audio phase-2 workspace verification script — its required arguments (likely a workspace/output directory path) and options.
- found: Heredoc usage text explaining this is an acceptance gate script running 4 checks (an audit python script, cargo fmt check, cargo check, cargo test) against the workspace, plus two env var toggles (DVDA_ALLOW_FORMAT_WRITE and DVDA_REQUIRE_UDF_ISO_FIXTURES) for CI behavior.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
