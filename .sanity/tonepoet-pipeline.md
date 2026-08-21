# tonepoet-pipeline — sanity assessment

767 of 767 read · 209 surprising

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

## tonepoet-pipeline/qualification/derive_dsd_reference_v10_production_metadata.py

### the file itself
- spec 3 · read at `95a47a3665e3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:35Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Sibling script to the v6 terminal-bounds one, but for policy-v10, checking "exact production metadata" rather than deriving mathematical peak bounds — no Decimal-precision derivation function here, consistent with peers lacking a derive_* function. sha256 hashes files for provenance checks, require is a small assertion helper (raises with a message if a condition fails), verify loads a checked-in JSON qualification manifest and checks its metadata fields (policy identity, schema version, tool/version strings, provenance doc hashes) against expected/recomputed values, and main is a CLI entrypoint enforcing the same append-only historical-checker lineage contract as the v6 script.
- found: Verifies the v10 qualification artifact: checks v9 artifacts are byte-unchanged (pinned hashes), v10 current/candidate manifests match, all inherited fields (outside an allowed changed-set) equal v9, and the sample-identity metadata_mutation block matches an exact expected dict (case counts, environment policy, w64 rejection, etc.) hardcoded in the script. It then greps for dozens of exact string markers across several Rust source files (dsd_reference.rs, stages.rs, tool.rs, track_executor.rs), the qualification test file, flake.nix, and a findings doc, asserting they contain specific implementation identifiers, magic numbers (420/160/180/80/20/60), and one negative check that a stale v9 claim string is gone.
- predicted: most · documented: some · derivable: no · legible: not judged · trap: no
- note: This is less a "verifier" than a frozen snapshot/lockfile of dozens of source-code strings and magic numbers across many files — any refactor of those files (even semantics-preserving renames) will break it, so it's a trap for future editors even though the file itself calls this out as intentional (append-only lineage contract).

### `sha256`
- spec 3 · read at `b127d414a996` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:32Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads the file at path (likely in binary chunks) and returns its SHA-256 hex digest as a string, used to verify pinned artifact hashes against expected values.
- found: Reads the whole file into memory with read_bytes() (not chunked) and returns hashlib.sha256(...).hexdigest().
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `26e77ae462f9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:57:35Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Checks whether `marker` is a substring of `text`; if not found, raises a SystemExit or custom exception with an error message that includes `label` to identify what check failed. Used as a lightweight assertion utility in the qualification verification script.
- found: Substring check that raises AssertionError with label+marker message if marker missing from text.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify` — QUIRKY — TANGLED
- spec 3 · read at `67ccc732157e` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T22:16:00Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Reads one or more metadata/artifact files under root related to DSD reference derivation, computes sha256 hashes of pinned artifacts and compares against hardcoded expected hash values, and uses require() to assert specific immutable fields (e.g. policy identity, version string) match expected pinned values — while explicitly not touching/asserting the mutable "current policy" pointer per the historical-checker contract.
- found: Far more extensive than predicted: checks byte-identical v9 hashes, byte-identical v10 current/candidate, deep field-by-field JSON diffing of inherited vs changed keys, an enormous hardcoded expected_mutation dict compared exactly, then greps dozens of hardcoded string markers across seven different source/doc files (Rust pipeline code, tests, flake.nix, findings doc) via a require() helper to assert specific implementation details are still present verbatim.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: This checker encodes an enormous amount of project-specific historical/qualification context (specific case counts, tool paths, markers across many files) that isn't derivable from the function alone without the surrounding project history.

### `main` — QUIRKY
- spec 3 · read at `f808715d7452` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:50:26Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: main() orchestrates the qualification check: it calls verify() to run the actual policy-v10 metadata checks (likely using require() internally to assert conditions and sha256 to hash/pin artifacts), then reports success/failure, probably printing a message and exiting with a non-zero status code via sys.exit() if verification fails.
- found: Parses CLI args (--repository-root, --check, the latter unused in the body shown), calls verify() with the resolved repository root, and prints a fixed success message if verify() didn't raise/exit.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The --check flag is parsed but not referenced in main's body, so its effect (if any) must live inside verify() or elsewhere.

## tonepoet-pipeline/qualification/derive_dsd_reference_v11_runtime_mutator_binding.py

### the file itself
- spec 3 · read at `f3a66f18dff7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:58Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Same lineage-checker pattern as v9/v16: sha256() hashes files, require() asserts a marker is present in text with a label, verify() checks pinned hashes of frozen prior-version (v10) artifacts, confirms v11 current/candidate byte-identity, asserts manifest identity/report/certification descriptor fields and an exact runtime-bound metadata-mutator authority policy dict, then scans compiled Rust source files (dsd_reference.rs, track_executor.rs, etc.) and docs for markers proving the runtime binding is wired into production code; main() provides a --check CLI entry point.
- found: Same helper pattern (sha256/require) as siblings, but verify() is more principled: it pins v10 artifact hashes, checks v11 current/candidate byte-identity, then generically diffs every top-level manifest key against v10 (only allowing a fixed changed-set) rather than hardcoding all fields, and constructs the expected sample_identity by deep-copying v10's and merging in specific new runtime-binding keys rather than hardcoding the whole dict. It then scans nine compiled/doc files (dsd_reference.rs, fingerprint.rs, build.rs, flake.nix, tool.rs, track_executor.rs, stages.rs, qualification test, findings doc, and the v11 report itself) for dozens of markers proving a runtime-bound metadata-mutator attestation chain (store-path env vars, bound-executable execution, fingerprinting, attestation/verification calls) is wired end to end.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no

### `sha256`
- spec 3 · read at `485e824a1552` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:19Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Reads the file at `path` (likely in chunks to handle large files) and computes its SHA-256 hash, returning the hex digest as a string, for use by the `verify`/`require` functions to pin/check immutable artifact hashes against expected policy values.
- found: Reads the whole file into memory and returns its SHA-256 hex digest.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `28d7805ee80d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:00:00Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Identical to the sibling checker's require: if marker not in text, raise AssertionError(f"{label}: missing {marker!r}").
- found: Raises AssertionError with label and marker if marker is not found in text; identical to the sibling v14 checker's require function.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: This prediction was informed by seeing the identical function in a sibling file moments earlier, not purely from this handout.

### `verify` — QUIRKY — TANGLED
- spec 3 · read at `dfdaffc08a57` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:57:15Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Reads specific source/artifact files under root (e.g. the metadata-mutator authority list or runtime binding source for policy v11), computes sha256 hashes of them via the sha256 helper, and calls require() to assert those hashes match hardcoded/pinned values from when the checker was authored. It deliberately avoids reading or asserting against any "current policy" pointer/file that could be mutated by later policy revisions, only checking immutable, generation-pinned identities and artifacts so the checker stays valid across future policy versions.
- found: Checks append-only v10 artifact hashes are unchanged, verifies the v11 candidate JSON is byte-identical to the "current" file and remains an unpromoted qualification_candidate, checks that all fields inherited from v10 are unchanged except an allowed changed-set, validates a nested sample_identity/metadata_mutation structure with specific new keys, checks report/certification descriptor structure and digests, then scans nine different Rust/build/doc source files for dozens of specific string markers (struct names, error messages, env var names) proving the runtime-bound metadata-mutator attestation machinery actually exists in code.
- predicted: some · documented: most · derivable: no · legible: some · trap: no

### `main` — QUIRKY
- spec 3 · read at `97e973487c00` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:34:10Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Calls verify() to run the actual policy-v11 runtime-mutator-binding check (which likely uses require() and sha256() internally to assert pinned artifact hashes), then prints a pass/fail message and calls sys.exit with a nonzero code on failure so this can be used as a CI/qualification gate.
- found: Parses CLI args (--repository-root, --check, the latter unused in the body shown), calls verify() with the resolved repo root, and prints a success message — no explicit sys.exit, presumably verify() raises/asserts on failure rather than main() handling exit codes; I correctly guessed verify() was the workhorse but wrong about argparse and about explicit exit-code handling.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/qualification/derive_dsd_reference_v12_streamed_wav_capacity.py

### the file itself
- spec 3 · read at `6d43bd99a3db` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:47Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Same family and shape as the v15 hardening checker: pins prior historical qualification artifacts by sha256, then verifies policy-v12's manifest/config fields against canonical expected values (likely capacity/size limits for streamed WAV output — e.g. max stream duration or byte-size the "bounded" authority permits) via a verify() function, and greps compiled Rust source for required markers (require) while forbidding markers that indicate obsolete/bypassed logic (forbid), all orchestrated from main(). Unlike the v15 file, this one factors more logic into named helpers (sha256, verify) rather than inlining everything in main.
- found: verify() pins v11 artifacts by sha256, diffs the v12 manifest against v11 field-by-field (only an allowed set may differ), checks a specific streamed-WAV byte-capacity contract (RIFF 32-bit size overflow math) plus its arithmetic self-consistency, checks report/certification descriptors are canonical, then requires dozens of literal markers across nine different Rust/markdown source files (planner, manifest, qualification schema, metadata rewrite contract, stages, report, findings doc) and forbids three stale markers in three of them. main() just parses --root/--check and calls verify().
- predicted: most · documented: some · derivable: no · legible: not judged · trap: no
- note: The scope (9 distinct source files checked, not just the compiled policy file) is far beyond what 'streamed-WAV capacity' in the header suggests — it also silently re-verifies unrelated metadata-rewrite (chown/xattr/rename) contract markers that seem to belong to a different feature bundled into this policy revision.

### `sha256`
- spec 3 · read at `01415bc77a0d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:01Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Computes the SHA-256 hex digest of the file at path by reading its bytes (likely in chunks to handle large files) and returning hexdigest().
- found: Reads the whole file into memory via path.read_bytes() and returns hashlib.sha256(...).hexdigest().
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `9f003fe8b342` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:59:54Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Checks whether `marker` (a substring) is present in `text`, and if not, raises/prints an error referencing `label` and exits or asserts failure — a small assertion helper, the inverse of the sibling `forbid`, used to validate expected content markers in this qualification checker.
- found: Raises AssertionError with the label and marker if marker is not found in text; otherwise no-op.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `forbid`
- spec 3 · read at `26d5eb9aada7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:07Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Checks that `marker` does not appear as a substring in `text`; if it does, raises an assertion error or exits with a message referencing `label`, enforcing the "must never assert the mutable current-policy embed pointer" rule described in the file doc.
- found: Raises AssertionError with label and marker if marker substring is found in text.
- predicted: full · documented: some · derivable: no · legible: full · trap: no
- note: The file_doc explains the policy rationale but not this specific helper's mechanics; documented reflects the enclosing file's contract, not this function.

### `verify` — QUIRKY — TANGLED
- spec 3 · read at `f545253dbcc3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:21:09Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Loads generated qualification artifacts under `root` (likely a JSON report and/or manifest produced by running the v12 policy against a DSD reference case), uses `require`/`forbid` helper assertions to check pinned, immutable values from the v12 generation — such as fixed hashes (via `sha256`) of specific output files and the streamed-WAV capacity bound/policy identity — and explicitly forbids checking against any current/mutable policy embed pointer so the checker stays valid across future policy versions. Raises/exits with an error if any expectation fails.
- found: Checks v11 artifact hashes are unchanged (append-only), that the v12 current/candidate JSON files are byte-identical and remain an unpromoted qualification candidate with correct schema/policy identity, that fields inherited from v11 are unchanged except an allowed changed-set, validates streamed-WAV capacity arithmetic, checks a certification stub is a canonical 'not_run' placeholder, then does a large marker-string audit across many Rust source files (planner, manifest, qualification schema, metadata rewrite, stages, report, findings doc) requiring specific identifiers/strings to be present and forbidding a few legacy strings (streaming_size_sentinel_floor etc.) from reappearing.
- predicted: some · documented: some · derivable: no · legible: some · trap: no

### `main` — QUIRKY
- spec 3 · read at `c49e29ab6730` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:59:59Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: main() calls verify() (which performs the actual policy-v12 checks using require/forbid/sha256 helpers), wraps the call in a try/except to catch assertion-style failures, prints a pass/fail message, and calls sys.exit(0) or sys.exit(1) accordingly so the script works as a standalone CLI qualification check.
- found: main() sets up an argparse CLI with --repository-root/--root (defaulting to the repo root inferred from file location) and a --check flag (unused in the body), then simply calls verify(args.root.resolve()). No try/except or explicit exit code handling.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The --check flag is parsed but never referenced in the body, so its purpose isn't visible from main() alone.

## tonepoet-pipeline/qualification/derive_dsd_reference_v13_streamed_wav_header.py

### the file itself — TANGLED
- spec 3 · read at `c5d06456edb3` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:16:09Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A "qualification" regression checker script for a versioned policy-lineage system, checking DSD-to-WAV streaming header handling for policy v13. It computes sha256 hashes of pinned reference artifacts (e.g. a known-good streamed Float64 WAV header) and uses generic require/forbid assertion helpers inside verify() to check that specific immutable, policy-v13-era facts still hold (fixed byte layout/header fields/hash values), while deliberately never asserting the mutable "current policy" pointer, so later policy generations don't break this old checker. main() is a CLI entrypoint invoked from CI, exiting nonzero on failure.
- found: A qualification checker verifying that pinned v12 artifact hashes are unchanged, that the v13 candidate JSON is byte-identical to the "current" file and remains an unpromoted candidate with correct inherited/changed fields, that a precise streamed-WAV byte-capacity contract (58-byte header, max payload, frame-alignment/overflow-boundary arithmetic) is canonical and internally consistent, and that specific literal markers (exact Rust const names/values, JSON schema strings, report prose) are present in six real source files (dsd_reference.rs, qualification_schema.rs, manifest.rs, manifest_builder.rs, the qualification test, and a findings doc) while stale v12-era markers are forbidden — i.e. it's not just hash-pinning but a dense web of cross-file textual/structural assertions enforcing the append-only policy lineage contract.
- predicted: most · documented: most · derivable: no · legible: some · trap: no
- note: The doc explains the *why* (append-only lineage contract, never assert the mutable current-policy pointer) but gives no hint of the sheer density of hardcoded arithmetic constants and cross-file literal-string markers this actually checks.

### `sha256`
- spec 3 · read at `62a2583f3c55` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:08Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Reads the file at path and returns its SHA-256 hex digest, likely via hashlib.sha256(path.read_bytes()).hexdigest().
- found: hashlib.sha256(path.read_bytes()).hexdigest(), exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `790ddaff104a` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:57:20Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Checks whether `marker` is a substring of `text`; if not present, raises an exception or prints an error and exits, using `label` in the message to identify which required marker was missing. Likely mirrors a `forbid` function that checks the marker is absent instead.
- found: Raises an AssertionError with a message combining label and the missing marker if marker is not a substring of text; exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `forbid`
- spec 3 · read at `87e14643d24f` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:02:23Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Asserts that `marker` does NOT appear in `text`; if it does, raises/prints an error referencing `label` explaining that this forbidden marker (e.g. a mutable current-policy pointer) must not appear in the pinned historical checker output, then likely exits with failure.
- found: Raises AssertionError with a message including label and the forbidden marker if marker is found in text — exactly as predicted, just via raise rather than print+exit.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify` — QUIRKY
- spec 3 · read at `bbb85fb0819e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:55:28Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Given a repo root, this loads the pinned v13 WAV-header artifact(s) and policy identifiers from this file's own generation, computes sha256 over the relevant files/bytes, and calls require() to assert those hashes/identities still match the pinned constants. It also calls forbid() to assert that this checker's own source does NOT reference the current/mutable policy embed pointer (e.g. a "current_policy" symbol or similar), enforcing the lineage contract that it stays valid against future policy changes. main() likely just calls verify(Path(...)) with the repo root and reports pass/fail.
- found: Verifies the append-only v12 artifacts are byte-unchanged (via sha256), that v13's current and candidate JSON are byte-identical, checks v13's schema/policy identity and unpromoted-candidate status, checks that only an allowlisted set of fields differ between v12 and v13 (all others must be inherited unchanged), validates a large set of hardcoded streamed-WAV capacity arithmetic constants (header bytes, max payload, frame alignment, wraparound transition count), checks the qualification report and release-certification stub match expected canonical structures, and finally requires/forbids specific literal string markers across several Rust source files and the report/findings docs to ensure v13 symbols are present in planner/schema/manifest code and that stale v12-era constants/paths are no longer active.
- predicted: some · documented: most · derivable: no · legible: most · trap: no
- note: Predicted the general shape (hash pinning + require/forbid) but not the sheer volume of hardcoded arithmetic invariants and exact literal string markers this checks — very brittle to any refactor/reformat of the target files even when semantically equivalent.

### `main` — QUIRKY
- spec 3 · read at `8eb9cc452110` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:54:30Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A thin CLI entrypoint that calls verify() (the actual policy-v13 header-authority check, likely using require/forbid/sha256 helpers), prints a pass/fail message, and exits with a nonzero status code on failure so this can run as a qualification/CI check.
- found: Parses CLI args (--repository-root/--root defaulting to two parents up from this file, and a --check flag that is accepted but never read/used) then calls verify(args.root.resolve()) — no explicit print/exit-code handling here, that must live inside verify.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

## tonepoet-pipeline/qualification/derive_dsd_reference_v14_true_peak_analyzer.py

### the file itself — QUIRKY — TANGLED
- spec 3 · read at `a9f2f595b349` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:41Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A "historical checker" script that pins the policy-v14 oversampled true-peak analyzer as an immutable artifact: sha256 hashes a specific pinned file (the v14 true-peak analyzer source/binary), require is an assert-with-message helper, verify computes/compares that hash plus any persistent (non-mutable) policy-identity strings against hardcoded v14-era values, deliberately never touching whichever policy is currently "active" since that pointer is allowed to change in later generations, and main runs verify and exits nonzero with a clear error if the pinned v14 artifact or identity has drifted.
- found: Directionally right (pins v14 as an immutable, unpromoted candidate, never asserts the "current" mutable pointer) but far more elaborate than predicted: it hashes v13 artifacts, byte-compares the v14 current/candidate JSON files, diffs every JSON field against inherited v13 values except an allowlist, re-derives the qualification-matrix case counts and the analytic oversampling grid-bound constant from first-principles math, checks an enormous literal "carrier contract" dict field-by-field, and greps specific string markers out of dsd_reference.rs, the v14 report markdown, and a findings doc to confirm the Rust implementation and docs still contain the exact pinned constants/identifiers.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: This is less a "hash pin" checker and more a full structural+numeric+cross-file re-derivation of the v14 qualification contract — anyone editing dsd_reference.rs, the analyzer JSON, or the v14 report markdown should expect this script to catch even minor wording/formatting drift, not just semantic changes.

### `sha256`
- spec 3 · read at `eb130d2196f8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:53:59Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Computes the SHA-256 hex digest of a file at path, likely reading it in chunks and returning hashlib.sha256(...).hexdigest().
- found: Reads the whole file into memory and returns its SHA-256 hex digest.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `57e828d9b76f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:59:52Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Checks if marker is a substring of text; if not, raises an error (or prints and exits) including label so the caller (verify) can report which specific expected marker/policy string was missing from the checked artifact.
- found: Raises AssertionError with label and marker if marker is not found in text.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify` — QUIRKY
- spec 3 · read at `d3d99c7ab340` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:14:36Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This function checks that the repo still honors the v14 true-peak analyzer policy by reading some manifest/config under `root`, computing sha256 hashes of pinned artifact files (using the `sha256` helper) and comparing them against hardcoded/expected hash values baked in at v14's creation, calling `require()` to fail loudly on mismatch. Per the historical-checker contract, it deliberately avoids referencing the "current policy" embed pointer (which could have moved on to v15+), instead only pinning immutable, versioned identifiers/artifacts from its own generation.
- found: It exhaustively re-verifies the entire v14 DSD-reference qualification policy: checks pinned v13 artifact hashes are unchanged, that v14's live file is byte-identical to a preserved candidate copy, that v14 only differs from v13 in an allowlisted set of fields (all others must match exactly), that numerous analyzer/carrier constants and computed case-count arithmetic match exact expected values, that the certification stub is still an unpromoted "not_run" placeholder, and finally that specific string markers exist in the Rust planner source, the qualification report, and a findings doc.
- predicted: some · documented: some · derivable: no · legible: most · trap: no

### `main` — QUIRKY
- spec 3 · read at `6deca966ac35` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:54:38Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A thin CLI entrypoint that calls the verify() function (which does the actual sha256/require-based policy checks) and prints a success message, letting any require() failure raise/propagate to produce a non-zero exit for CI use.
- found: Parses CLI args (--repository-root/--root defaulting to the repo root inferred from this file's location, and an unused-here --check flag) then calls verify(root) to run the actual policy checks.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The --check flag is parsed but not referenced in this body, so its effect (if any) must live inside verify() or it's currently a no-op — worth checking.

## tonepoet-pipeline/qualification/derive_dsd_reference_v15_hardening.py

### the file itself — QUIRKY
- spec 3 · read at `d056ffd83038` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:29Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A sibling checker script to the v5 terminal-bounds one, but simpler: since it's about verifying "hardening" of policy v15 rather than deriving numeric bounds, it likely just pins immutable artifacts by content hash (digest — probably SHA-256 of qualification JSON files) and asserts (require — an assertion helper that raises with a message) that specific v15 policy identity markers/constants appear in the compiled Rust policy source, similar to v5's --check mode. main() is the sole entrypoint (no derive/print mode), reading the qualification directory and compiled source, exiting non-zero on any failed check.
- found: Verifies policy v15 by: (1) SHA-256 pinning five frozen v14 artifacts, (2) exhaustively asserting dozens of exact canonical fields/values in the v15 JSON manifest (schema version, analyzer config, residual-authority math, deadline-model arithmetic, carrier config, certification stub state), and (3) grepping five Rust source files for ~30 required literal markers, including structural checks like counting exactly 3 production pipeline call sites and confirming each is preceded by a specific permit-acquisition call within a byte window, and asserting an obsolete helper is unreachable.
- predicted: some · documented: some · derivable: no · legible: not judged · trap: no
- note: Massively more scope than the two-function peer list (digest, require, main) suggests — most of the ~150 lines of assertions are inlined directly in main() rather than factored into named helpers, so the peer list undersells how much this file actually checks.

### `digest`
- spec 3 · read at `e7db008bf12d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:23Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads the file at path and returns its SHA-256 hexdigest, used to pin/verify immutable artifacts against the shipped checker's expected hashes.
- found: SHA-256 hexdigest of the file's bytes, one line.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `39517e9a0578` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:57:30Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Identical to the require() in the v13 script: if marker is not a substring of text, raise an AssertionError with a message combining label and the missing marker.
- found: Raises AssertionError with slightly different message wording than the v13 sibling, but same logic: marker must be a substring of text or it fails.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: This require() is duplicated near-verbatim across multiple qualification scripts (v13, v15) with only cosmetic message wording differences — worth factoring into a shared helper if more get added.

### `main` — QUIRKY — TANGLED
- spec 3 · read at `4a44dd70a042` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:57:46Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Runs a sequence of `require(...)` assertions (using `digest` to hash/verify pinned artifacts) that check immutable, historically-pinned properties of the v15 DSD Reference policy — e.g. fixed hashes of certain outputs or persistent policy identities — without touching the current mutable policy pointer. On any failure it likely prints a message and exits non-zero; on success it prints a confirmation.
- found: A long sequence of hardcoded assertions verifying: frozen v14 artifact digests unchanged, v15 current/candidate manifest byte-identity and specific canonical field values (schema version, policy id, analyzer config, residual authority math, deadline model math, carrier config), certification stub still 'not_run', and presence of dozens of exact source-code marker strings across five Rust files (dsd_reference.rs, track_executor.rs, manifest.rs, settings.rs, and two test files) plus a check that exactly 3 production pipeline call sites are guarded by composite permit acquisition and the obsolete single-family permit helper is unreachable.
- predicted: some · documented: some · derivable: no · legible: some · trap: no
- note: The file_doc's 'historical-checker lineage' framing describes the intent well, but the body is a giant flat script of manual marker/digest checks rather than any generalized mechanism — future changes to any of the five source files will need corresponding marker updates here or this silently goes stale/fails.

## tonepoet-pipeline/qualification/derive_dsd_reference_v16_w64_integrity.py

### the file itself
- spec 3 · read at `42b4d03c99c6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:43Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Same lineage-checker pattern as v8/v9: digest() hashes files, require() is a shared assert-with-message helper, and main() verifies pinned SHA256 hashes of the preserved v16 qualification artifacts, checks current/candidate byte-identity, asserts specific policy identity and W64-integrity admission rule fields in the JSON, and scans compiled Rust source files for historical markers proving the v16 W64 integrity contract (e.g. checksum/CRC validation on W64 containers) is still honored in production code.
- found: digest() hashes files, require() asserts a marker substring is present with a labeled error, main() verifies frozen v15 artifact hashes, checks v16 current/candidate byte-identity and manifest identity/report/certification descriptors (with exact SHA256-bound evidence fields), asserts an exact w64_integrity policy dict (required invariants, rates, boundary-region fraction, trigger claim) and packaging fields, checks the uncommissioned v16 certification fails closed on cell coverage/malformed publication/same-path evidence, then scans six files (dsd_reference.rs, w64.rs exact parser, track_executor.rs, dsd_reference_qualification.rs, settings.rs, a handoff doc) for dozens of markers proving the exact-W64-structure integrity contract is implemented and wired, and finally asserts a disproven trigger-claim phrase is absent from the report.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no

### `digest`
- spec 3 · read at `a48190a53a43` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:53:59Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Computes a cryptographic hash (likely SHA-256) of the file at `path` by reading it (possibly in chunks for large files) and returns the hex digest as a string, used elsewhere to verify/derive integrity values for reference W64 files.
- found: Reads the whole file into memory with read_bytes() and returns its SHA-256 hex digest — no chunking.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `require`
- spec 3 · read at `1e13995649ca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:59:55Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A small assertion helper — checks whether marker appears in text, and if not, raises an error (likely SystemExit or AssertionError) with a message that includes label and marker, used to validate expected content is present in some derived/rendered text.
- found: Raises AssertionError with a message naming the label and missing marker if marker is not found in text.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `main` — OBSCURE — TANGLED
- spec 3 · read at `a24a3db80f77` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:42:15Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: CLI script that reads a W64 reference/ledger file for "DSD Reference policy v16", computes cryptographic digests of entries via the digest helper, and walks through the ledger verifying it's append-only (previously recorded digests still match, new entries only extend the log), failing loudly via require() on any mismatch, then reports success/failure and exit code.
- found: Not a ledger/append-only-log verifier at all: it's a static consistency checker for the v16 DSD reference qualification artifacts. It verifies frozen v15 artifact hashes haven't changed, that the v16 candidate/current manifests are byte-identical, checks exact-equality of large nested JSON descriptor blocks (report descriptor, release certification, W64 integrity policy, packaging) against hardcoded canonical values, checks the certification file is still in an uncommissioned/fail-closed state, and finally greps several Rust source files (dsd_reference.rs, w64.rs, track_executor.rs, tests, settings.rs, a handoff doc) for required string markers via require(), raising AssertionError on any mismatch.
- predicted: none · documented: none · derivable: yes · legible: some · trap: no
- note: The file_doc ('Deterministically verify append-only DSD Reference policy v16 W64 integrity') is misleading about what 'integrity' means here — it's not a checksum-chain/ledger check but a hardcoded-value and source-marker consistency gate across manifest JSON and multiple Rust files.

## tonepoet-pipeline/qualification/derive_dsd_reference_v5_terminal_bounds.py

### the file itself
- spec 3 · read at `1cce9d77bbbc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:13Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A standalone stdlib-only Python script using Decimal at 120-digit precision to derive conservative true-peak dB bounds (in nanodecibel units) for each qualified terminal realization under "policy-v5", per the formula A=10^((C-R)/20), S=20*log10(A-epsilon) rounded toward -infinity. derive_safe_dbnano computes one bound, render_dbnano formats it as a string/nanodecibel integer, derived_cells builds the full table across qualified realizations, verify_artifact/verify_compiled_policy check a previously-generated artifact or compiled policy file still matches what re-derivation produces (to satisfy the "must remain valid against every successor policy" lineage contract), and main is a CLI that either regenerates the artifact or runs verification, exiting non-zero on mismatch.
- found: CLI with two modes: default prints the derived per-bit-depth (int24_tpdf/float32/float64) safe terminal ceiling cells as JSON; --check reads two checked-in JSON qualification artifacts and verifies their terminal_bounds fields match fresh derivation, then greps the compiled Rust policy source (dsd_reference.rs) via regex for matching DbNano constants and per-variant q63/safe-ceiling tuples, plus checks two literal identity markers are present.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no
- note: Header's lineage contract (must stay valid against successor policies, never assert the mutable current-policy pointer) isn't visibly enforced in this file's code — it reads as a constraint on future edits, not something this script checks itself.

### `derive_safe_dbnano`
- spec 3 · read at `428dd176182f` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:14:36Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Implements the documented formula using Python's Decimal at 120-digit precision: converts q63_ceil (the Q1.63 additive peak bound) into a Decimal epsilon, computes A = 10**((C-R)/20) from module-level constants C (public post-final ceiling) and R (analyzer reporting quantum), then computes S = 20*log10(A - epsilon), rounds S toward negative infinity to the nearest nanodecibel, and returns it as an int.
- found: Uses 120-digit Decimal precision to compute admitted_peak = 10**((C-R)/20) via ln/exp, converts q63_ceil to epsilon, validates epsilon is within (0, admitted_peak) or raises ValueError, computes safe_db = 20*log10(admitted_peak - epsilon), and floors to an integer nanodecibel value.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `render_dbnano`
- spec 3 · read at `5e6a5aca4eb5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:16Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Formats an integer count of nanodecibels as a fixed-point decibel string with 9 decimal places (dividing by 1_000_000_000), preserving sign and zero-padding, so it can be embedded in generated policy/reference text without floating-point rounding surprises.
- found: Exactly as predicted: splits sign/magnitude, divides by 1e9 for the integer part and mods for a 9-digit zero-padded fractional part.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `derived_cells` — QUIRKY
- spec 3 · read at `c363f69eeec9` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:13:09Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Returns a hardcoded lookup table mapping each qualified terminal-realization identifier (e.g. a format/sample-rate label) to a small dict of its parameters — the ceiling C and reporting quantum R from the module docstring, as int or str values — the fixed input data consumed by derive_safe_dbnano and the verify functions.
- found: Builds a dict keyed by cell name from a module-level CELLS table, computing for each the safe pre-terminal ceiling in dB via derive_safe_dbnano/render_dbnano alongside the raw Q1.63 epsilon value, rather than storing a hardcoded C/R pair as I guessed.
- predicted: some · documented: some · derivable: no · legible: full · trap: no

### `verify_artifact`
- spec 3 · read at `68bdefd13114` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:22:32Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Reads the compiled policy artifact at `path` (likely JSON), recomputes the expected terminal true-peak bounds using derive_safe_dbnano/derived_cells, and compares them against the values stored in the artifact, raising an assertion/exception if any mismatch is found — essentially a self-check that the shipped artifact matches what this script would derive now.
- found: Loads the JSON artifact, checks two specific hardcoded invariants (analyzer uncertainty must equal a fixed constant, and reserve must equal the uncertainty), then recomputes expected_cells via derived_cells() and compares each field of each named cell against the artifact's terminal_bounds, raising AssertionError with details on any mismatch.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `verify_compiled_policy` — QUIRKY — TRAP
- spec 3 · read at `f3a26a3ade6a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:02:46Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Loads a compiled policy artifact from disk at `path` (likely JSON), recomputes the expected terminal bound cells using derive_safe_dbnano/derived_cells for each qualified realization, and compares the recomputed values against what's stored in the artifact, raising an assertion or exception if any mismatch is found. Returns None on success (no return value, just validation side-effect).
- found: Reads the compiled Rust source file as text and uses regex to extract two DbNano constants and three PcmBitDepth terminal-cell tuples, comparing the extracted integers against hardcoded expected constants and freshly derived_cells() values, raising AssertionError with a descriptive message on any mismatch or if a pattern isn't found.
- predicted: some · documented: some · derivable: no · legible: full · trap: yes
- note: The signature/name suggest verifying a structured artifact (JSON/config), but it actually regex-parses generated Rust source text against exact literal patterns (e.g. `pub const NAME: Self = Self(...)`) — brittle to any reformatting of the compiled Rust output, and that fragility isn't hinted at by the docstring.

### `main` — QUIRKY
- spec 3 · read at `f4170e923d6f` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:17:06Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Orchestrates the script: computes derived_cells for the terminal bounds, renders them via render_dbnano, then runs verify_artifact and verify_compiled_policy against the shipped policy files, printing a report and returning 0 on success or a nonzero exit code if any verification fails or mismatches.
- found: Argparse with a single --check flag: without it, just prints derived_cells() as JSON; with it, verifies two checked-in JSON artifacts via verify_artifact and inline-checks that the compiled Rust policy source file contains specific identity string markers, raising AssertionError if missing.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/qualification/derive_dsd_reference_v6_terminal_bounds.py

### the file itself
- spec 3 · read at `d1d700b2c373` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:13Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Standalone derivation/verification script (not a test file) that recomputes "policy-v6" terminal true-peak safety bounds from first principles using high-precision Decimal arithmetic per the documented formula (A = 10^((C-R)/20), S = log-based bound rounded toward -infinity to one nanodecibel). derive_safe_dbnano computes one bound, render_dbnano formats it, derived_cells generates the full table across qualified terminal realizations, sha256_file/verify_artifact/verify_compiled_policy check that a shipped/compiled policy artifact's hash and values match the freshly-derived ones (guarding against drift), and main is a CLI entrypoint, likely run in CI to enforce the "once shipped, must remain valid for every successor policy" lineage contract mentioned in the docs.
- found: Standalone CLI/verification script for the policy-v6 DSD-reference true-peak terminal bounds. derive_safe_dbnano/render_dbnano/derived_cells recompute the safe pre-terminal ceiling per bit-depth cell (int24_tpdf/float32/float64) from first principles via high-precision Decimal math; verify_artifact checks a checked-in JSON qualification manifest's schema/policy identity, provenance doc sha256 hashes, and derived terminal-bound cells against fresh computation; verify_compiled_policy regex-extracts constants and per-variant (q63_ceil, safe_bound) pairs from the compiled Rust source (dsd_reference.rs) and checks them against the same derivation; main() either prints the derived cells as JSON or, with --check, verifies the current and candidate manifests are byte-identical and match the compiled policy, enforcing the append-only historical-checker lineage contract.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no

### `derive_safe_dbnano`
- spec 3 · read at `e3d27859a738` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:14:30Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Using module-level constants for C (post-final ceiling) and R (analyzer reporting quantum), it computes A = 10**((C-R)/20) with Decimal at 120-digit precision, converts q63_ceil (a Q1.63 fixed-point integer) into a decimal epsilon, computes S = 20*log10(A - epsilon), rounds toward negative infinity to one nanodecibel, and returns that as an integer number of nanodecibels.
- found: At 120-digit Decimal precision, computes admitted_peak = 10**((PUBLIC_CEILING_DB - REPORTING_QUANTUM_DB)/20) via ln/exp, converts q63_ceil into epsilon by dividing by Q63_DENOMINATOR, validates epsilon is strictly within (0, admitted_peak) or raises ValueError, computes safe_db = 20*ln(admitted_peak-epsilon)/ln(10), and returns it floored to nanodecibel integer resolution.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `render_dbnano`
- spec 3 · read at `80889639ae20` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:16Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: value is an integer count of nanodecibels (1e-9 dB units). render_dbnano formats it as a fixed-point decimal string by dividing by 1_000_000_000, producing something like \"-1.234567890\" with exactly 9 fractional digits and an explicit sign, likely via string manipulation of the integer's digits rather than float division to avoid precision loss.
- found: Splits the integer nanodecibel value into sign, integer part (floor div by 1e9), and 9-digit zero-padded fractional remainder, concatenated as a decimal string.
- predicted: full · documented: some · derivable: no · legible: full · trap: no

### `derived_cells` — QUIRKY
- spec 3 · read at `6a93792b7ba8` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:13:08Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds and returns a dict mapping named terminal realizations to a dict of their derived values: for each qualified realization it takes the ceiling C and reporting quantum R, computes epsilon (the Q1.63 additive peak bound), then A = 10**((C-R)/20) and S = 20*log10(A - epsilon) using Decimal at 120-digit precision, rounds S down (toward -inf) to one nanodecibel, and stores these fields (ceiling, quantum, epsilon, resulting bound) keyed by cell/realization name.
- found: Iterates a module-level CELLS dict of name -> q63 value, and for each builds a small dict with the raw q63 ceiling and a rendered safe dB bound computed by delegating to derive_safe_dbnano/render_dbnano rather than doing the Decimal math inline.
- predicted: some · documented: most · derivable: no · legible: full · trap: no
- note: The actual A/S Decimal formula from the file doc lives in derive_safe_dbnano, not here — this function is just a thin dict-comprehension wrapper over CELLS.

### `sha256_file`
- spec 3 · read at `79e25c5b6f9d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:57:20Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Opens the file at path in binary mode, reads its contents (possibly in chunks for large files), computes a SHA256 hash over the bytes, and returns the hex digest string. Used elsewhere to verify checksum integrity of compiled policy/artifact files.
- found: Reads whole file into memory and returns hex sha256 digest, one-liner using read_bytes.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify_artifact`
- spec 3 · read at `951c0b9c34ea` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:46:01Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Loads the compiled artifact at `path` (likely JSON), recomputes the expected derived cells/values using derive_safe_dbnano and derived_cells against the current source in repository_root, and asserts/compares that the artifact's stored values match the freshly recomputed ones (including a sha256_file check of the deriving script itself for lineage integrity). Raises an exception (e.g. AssertionError) on mismatch, returns None on success.
- found: Validates a compiled qualification-candidate artifact JSON against a large hardcoded contract: schema_version, policy id, status, analyzer carrier fields (schema/parser/routing_rule/disk_intermediate), sha256 hashes of several referenced doc/brief files, fixed analyzer reporting uncertainty and reserve equality, and finally that every field of every derived_cells() entry matches the artifact's terminal_bounds. Raises AssertionError with a descriptive message on any mismatch.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: The function encodes a long list of expected constants (schema version, policy id, doc paths for sha256) that live only in this function body, not in any doc or type — a future policy revision must hunt through this single function to know what changed.

### `verify_compiled_policy` — QUIRKY
- spec 3 · read at `ed932e9b8e67` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:02:43Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Loads a compiled policy artifact from `path` (likely JSON) containing previously-derived terminal true-peak bounds, then recomputes the same bounds using derived_cells/derive_safe_dbnano for each qualified realization and compares them against what's stored in the file. If any recomputed value disagrees with the stored value it raises an error (assertion or SystemExit), enforcing the "once shipped, must remain valid" lineage contract; otherwise it just returns/prints success.
- found: Reads a Rust source file as text and uses regex to extract hardcoded compiled constants (DbNano::REFERENCE_CEILING, POST_FINAL_ACCEPTANCE_RESERVE, and per-PcmBitDepth-variant terminal cells), comparing each against freshly recomputed expected values (via derived_cells()), raising AssertionError on any mismatch.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `main` — QUIRKY
- spec 3 · read at `624d7e1f9d5c` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:34:41Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Computes the derived terminal true-peak bound cells via derived_cells(), then verifies them against an on-disk reference artifact (verify_artifact) and the compiled policy (verify_compiled_policy), printing results/mismatches, and returns 0 if everything matches or a nonzero exit code if any verification fails, likely also handling a --write/regenerate flag via sha256_file for checksums.
- found: With --check, verifies the checked-in current and candidate v6 JSON manifests via verify_artifact, asserts they're byte-identical, and asserts the compiled Rust policy source contains the expected v6 key/variant markers, printing a success message; without --check, just prints derived_cells() as JSON. Always returns 0 (relies on AssertionError to signal failure).
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

## tonepoet-pipeline/qualification/derive_dsd_reference_v7_terminal_bounds.py

### the file itself — TANGLED
- spec 3 · read at `512105c96211` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:16:56Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Another qualification-lineage checker script, deriving the policy-v7 "terminal true-peak" safety bounds using Python's Decimal at high precision per the documented formula. derive_safe_dbnano computes the safe dB-nano bound per input row, render_dbnano formats it for output/comparison, derived_cells builds the full table of derived values (likely per sample-rate/format/ceiling combination), sha256_file hashes pinned reference artifacts, verify_artifact checks those hashes match, verify_compiled_policy checks the derived values are embedded correctly in compiled Rust policy code, verify_compiled_v7_routes checks specific v7 code paths/routes reference the correct bounds, and main wires these together as a CLI entrypoint for CI, following the same append-only lineage contract as the v13 checker (never assert the mutable current-policy pointer).
- found: derive_safe_dbnano/render_dbnano/derived_cells do compute the documented high-precision Decimal true-peak formula per PCM bit-depth as I expected, but verify_artifact/verify_compiled_policy/verify_compiled_v7_routes turn out to be a much larger surface: verify_artifact checks a huge tree of exact JSON fields (analyzer carrier schema, packaging producer/consumer argv templates, sample-identity route table, sha256-bound doc paths) not just the derived bounds; verify_compiled_v7_routes greps five different Rust source files for dozens of exact route-table tuples, struct/function signatures (regex-matched), and forbidden-pattern regressions to ensure the compiled decode-route authority, executor, qualification tests, and manifest all still match the v7 contract exactly.
- predicted: most · documented: some · derivable: no · legible: some · trap: no
- note: The module doc only describes the dB math; it gives zero indication that most of the file is a sprawling exact-string/regex contract check across five other Rust source files enforcing route-table and API-signature invariants.

### `derive_safe_dbnano`
- spec 3 · read at `5d9f2f18a609` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:18:35Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Converts q63_ceil (a Q1.63 fixed-point integer epsilon bound) to a high-precision Decimal, computes A = 10**((C-R)/20) using module-level constants C (post-final ceiling) and R (analyzer reporting quantum) at 120-digit Decimal precision, then S = 20*log10(A - epsilon), rounds S toward negative infinity to one nanodecibel, and returns the result as an integer count of nanodecibels.
- found: Computes admitted_peak = 10**((C-R)/20) and epsilon = q63_ceil/Q63_DENOMINATOR at 120-digit precision, validates epsilon is in (0, admitted_peak) raising ValueError otherwise, then computes safe_db = 20*log10(admitted_peak - epsilon) and floors to integer nanodecibels.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `render_dbnano`
- spec 3 · read at `bdbfb95ab24f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:06Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Formats an integer count of nanodecibels (value) into a human-readable decimal string in dB, e.g. dividing by 1_000_000_000 and formatting with 9 decimal places and an explicit sign, matching the nanodecibel precision used throughout this policy-bounds derivation.
- found: Formats an integer nanodecibel value as a signed decimal string with sign extracted, integer part from division by 1e9, and 9-digit zero-padded fractional part from the modulus.
- predicted: full · documented: some · derivable: no · legible: full · trap: no

### `derived_cells`
- spec 3 · read at `1a10a16a07d7` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:13:16Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Iterates over a fixed set of qualified terminal realizations (C/R combinations), computing S via derive_safe_dbnano for each and formatting with render_dbnano, returning a dict mapping cell/realization identifiers to dicts containing the numeric nanodecibel value and its rendered string form.
- found: Builds a dict from module-level CELLS mapping name->q63 into name->{raw q63 value, rendered safe dbTP ceiling via derive_safe_dbnano+render_dbnano}.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `sha256_file`
- spec 3 · read at `8a34a8b1e650` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:59:59Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A tiny 2-line utility that reads the file at path and returns its SHA-256 hex digest, used to verify artifact integrity elsewhere in this policy-checker script.
- found: Reads the whole file into bytes and returns its SHA-256 hex digest, one line, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify_artifact` — QUIRKY
- spec 3 · read at `fa1dc37474e1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:22:53Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Loads the artifact file at `path` (probably JSON containing previously derived terminal bounds and metadata like a source hash), recomputes the same values fresh (via derived_cells/derive_safe_dbnano/render_dbnano) and asserts they match exactly, then also calls verify_compiled_policy and verify_compiled_v7_routes against files under repository_root to make sure the compiled policy shipped in the repo is consistent with this artifact. Raises an AssertionError or exits with an error message on any mismatch since it returns None on success.
- found: Loads the JSON artifact and does an exhaustive field-by-field check against dozens of hardcoded canonical constants (schema versions, analyzer carrier/parser/routing rule, packaging producer/consumer argv templates, sample-identity route table, environment policy, etc.), then verifies sha256 hashes of several bound doc files under repository_root, and finally compares the terminal_bounds cells against freshly recomputed derived_cells(), raising AssertionError with a specific message on any mismatch.
- predicted: some · documented: some · derivable: no · legible: most · trap: no
- note: The bulk of the function is pinning down a large literal contract (packaging/identity dicts) that isn't mentioned in the module doc at all — only the derived_cells/hash-verification tail matches the doc's framing.

### `verify_compiled_policy`
- spec 3 · read at `4c65db46d12c` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:02:48Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Loads a compiled policy file from `path` (likely JSON), recomputes the expected derived cells/bounds via derive_safe_dbnano and derived_cells, and compares them against what's stored in the compiled file, raising an assertion or error (and printing diagnostics) if there's a mismatch, to ensure the shipped policy artifact matches what this script would currently derive.
- found: Reads a Rust source file as text, regex-extracts specific compiled constants (DbNano::REFERENCE_CEILING, POST_FINAL_ACCEPTANCE_RESERVE) and per-PcmBitDepth-variant terminal cell tuples, and asserts each matches the freshly recomputed expected values from derived_cells(), raising AssertionError with a diagnostic message on any mismatch.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `verify_compiled_v7_routes` — QUIRKY — TANGLED
- spec 3 · read at `da7e76dbe661` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:43:58Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Locates the compiled/shipped policy-v7 source file(s) under repository_root, extracts the hardcoded terminal bound constants (routes) from that compiled code, and cross-checks each against freshly derived values (via derive_safe_dbnano/derived_cells) to confirm the shipped constants still match what the current derivation logic produces — raising an error/assertion on any mismatch.
- found: Reads several Rust source files (dsd_reference.rs, track_executor.rs, dsd_reference_qualification.rs, manifest.rs, manifest_builder.rs) as raw text and regex/substring-checks them against a large hardcoded set of expected decode route tuples plus dozens of required string/regex markers (struct names, function signatures, forbidden regressions), raising AssertionError on any mismatch or missing marker — it's a textual structural-invariant check on the compiled source, not a numeric bounds check.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: Despite living in a file about deriving numeric true-peak bounds, this function does none of that arithmetic — it's a pure text/regex audit of five other source files for API-shape and route-table drift.

### `main` — QUIRKY
- spec 3 · read at `ad4bbee6abf5` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:50:34Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: main() orchestrates the script: calls derived_cells() to compute the terminal true-peak bounds table, renders each value via render_dbnano, writes/prints the result, then calls verify_artifact, verify_compiled_policy, and verify_compiled_v7_routes to cross-check the derived values against the shipped compiled policy, returning 0 on success or 1 if any verification fails, with mismatches printed to stderr.
- found: With --check: verifies the current and candidate checked-in v7 JSON artifacts, asserts they're byte-identical, greps the compiled dsd_reference.rs source for required policy-identity markers, and verifies compiled v7 routes, printing a success message. Without --check: just prints derived_cells() as pretty JSON. Always returns 0; failures raise AssertionError instead of returning nonzero.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/qualification/derive_dsd_reference_v8_terminal_bounds.py

### the file itself — QUIRKY
- spec 3 · read at `0d79a6cac009` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:15Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Standalone stdlib-only Python script using Decimal at 120-digit precision to derive per-terminal-realization true-peak dB bounds via the documented formula (A = 10^((C-R)/20), S = 20*log10(A-epsilon), rounded toward -inf to one nanodecibel). derive_safe_dbnano computes a single bound, render_dbnano formats it, derived_cells builds the full table of cells across qualified realizations, sha256_file hashes generated/compiled artifacts, verify_artifact/verify_compiled_policy/verify_compiled_v8_routes check a compiled policy file's values and routing match the derived reference exactly, and main is a CLI entry point that ties generation and verification together (e.g. --derive vs --verify modes) to guarantee this checker stays valid against future policy successors.
- found: derive_safe_dbnano/render_dbnano/derived_cells implement the documented Decimal formula as predicted. But verify_artifact is a massive, extremely specific regression-lock: it checks dozens of exact JSON fields (schema/policy/status, inherited-from-v7 fields, supported target/depth cell lists, rejected int16 cells, activation text, release-certification stub, analyzer/packaging/sample-identity contracts down to exact argv templates, SHA256-bound doc paths, derived terminal cells) plus a not-run certification stub. verify_compiled_policy and verify_compiled_v8_routes then regex/substring-scan several Rust source files (dsd_reference.rs, track_executor.rs, manifest.rs, manifest_builder.rs, a qualification test) for dozens of exact marker strings/signatures/route tuples and a separate proof markdown file, to lock the compiled implementation against the derived policy. main() is --check (verify everything) vs. print derived_cells JSON.
- predicted: some · documented: some · derivable: no · legible: not judged · trap: no

### `derive_safe_dbnano`
- spec 3 · read at `8b203e49462a` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:18:26Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Using module-level constants for C (the public post-final ceiling) and R (one analyzer reporting quantum), this converts q63_ceil (a Q1.63 fixed-point integer) into its epsilon value, computes A = 10**((C-R)/20) with Decimal at 120-digit precision, then S = 20*log10(A - epsilon), rounds S toward negative infinity to one nanodecibel, and returns it as an integer.
- found: Computes admitted_peak = 10**((C-R)/20) via Decimal exp/ln at 120-digit precision, converts q63_ceil to epsilon by dividing by Q63_DENOMINATOR, raises ValueError if epsilon is out of the valid (0, admitted_peak) domain, then computes S = 20*log10(admitted_peak - epsilon) and floors it to an integer nanodecibel value.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `render_dbnano`
- spec 3 · read at `31649da48b42` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:24Z · by ross@rossturk.com · warm reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Identical to the v5 sibling: splits sign from magnitude, then formats as "{sign}{magnitude // 1_000_000_000}.{magnitude % 1_000_000_000:09d}" to render an integer nanodecibel count as a fixed-point decibel string.
- found: Byte-for-byte identical to the v5 file's render_dbnano, confirming this is a duplicated helper across policy-version scripts.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: This function is duplicated verbatim across at least the v5 and v8 qualification scripts rather than shared — worth knowing before editing one copy and assuming the other updates too.

### `derived_cells`
- spec 3 · read at `f751de26e9f6` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:13:08Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Computes the actual derivation table described in the file docstring: for each qualified terminal realization it likely calls derive_safe_dbnano (implementing the A/S formula at 120-digit Decimal precision) and render_dbnano to format the result, returning a dict keyed by realization name mapping to a dict of computed fields (e.g. value + rendered string) used elsewhere for verification against compiled policy artifacts.
- found: Builds a dict comprehension over a module-level CELLS mapping (name -> q63 value), producing per-name dict with the raw q63 ceiling and a rendered safe pre-terminal ceiling in dB via derive_safe_dbnano/render_dbnano.
- predicted: most · documented: some · derivable: no · legible: full · trap: no
- note: Correctly guessed the two helper calls and shape, but didn't anticipate the module-level CELLS dict as the iteration source, nor that the raw q63 value is passed through unrendered alongside the rendered one.

### `sha256_file`
- spec 3 · read at `20824bfc9473` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:57:27Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Identical to the v6 version: reads the file's bytes and returns hashlib.sha256(...).hexdigest() as a one-liner.
- found: Identical duplicate of the v6 sha256_file helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify_artifact` — OBSCURE — TANGLED
- spec 3 · read at `879733515482` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:22:54Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Loads the artifact file at `path` (likely JSON) containing previously-derived terminal true-peak bounds, and recomputes the expected values from scratch using the documented formula (A = 10**((C-R)/20), S = 20*log10(A-epsilon) via Decimal at 120-digit precision, rounded toward -inf to one nanodecibel), calling helpers like derive_safe_dbnano/render_dbnano. It also likely verifies the compiled policy/routes against repository_root (via verify_compiled_policy/verify_compiled_v8_routes and sha256_file for integrity), and raises an exception (e.g. AssertionError) if any derived value or hash doesn't match what's stored in the artifact, enforcing the "once shipped, stays valid" contract.
- found: It's a giant literal-contract validator for a v8 qualification artifact JSON: checks schema_version/policy/status, diffs a whitelist of fields against the historical v7 artifact to ensure they're unchanged, checks an exact expected list of supported target/depth cells and that int16 cells are uniformly rejected, checks canonical strings for runtime_activation/release_certification/analyzer carrier/packaging/sample_identity dicts against hardcoded expected values, verifies sha256 hashes of several bound docs against repository_root, cross-checks analyzer uncertainty/reserve equality, compares terminal bound cells against derived_cells(), checks realization strings and basis text markers, and finally validates a linked certification stub file. It barely touches the A/S formula described in the file_doc (only via derived_cells()) — it's mostly a frozen-contract/regression guard, not a computation.
- predicted: none · documented: none · derivable: no · legible: some · trap: no
- note: The file_doc describes the module's derivation formula, but this specific function is almost entirely a hardcoded contract/regression validator (dozens of literal expected-value checks) with only a thin touchpoint (derived_cells()) into the actual math — a reader expecting formula-verification logic here will be surprised by how much is schema/string pinning instead.

### `verify_compiled_policy`
- spec 3 · read at `bc7d1a62cca6` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:56:53Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Reads the compiled policy source file at `path`, extracts the embedded derived constants (dbnano cell values, likely via render_dbnano formatting) that were written into it, recomputes the expected values via derived_cells/derive_safe_dbnano, and compares them, raising an error (e.g. AssertionError or SystemExit) if any embedded value no longer matches what the derivation produces — enforcing the historical-checker lineage contract.
- found: Checks the compiled Rust source contains required v8 marker strings, then regex-extracts DbNano::REFERENCE_CEILING/POST_FINAL_ACCEPTANCE_RESERVE constants and per-bit-depth-variant (Int24/Float32/Float64) terminal cell tuples (q63, safe ceiling), comparing each against freshly derived expected values from derived_cells(), raising AssertionError on any mismatch.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `verify_compiled_v8_routes` — QUIRKY
- spec 3 · read at `edeb8a732339` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:59:09Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the qualified terminal realizations (various DSD rates/channel/ceiling combos), computes the expected S value via derive_safe_dbnano/the decimal formula, then parses/greps the actual compiled Rust source (policy-v8 routing table) for the corresponding hard-coded true-peak bound values and asserts they exactly match, raising an assertion error or exiting non-zero on any mismatch since this is a locked historical-checker contract.
- found: Reads five compiled Rust source files (dsd_reference.rs, track_executor.rs, dsd_reference_qualification.rs, manifest.rs, manifest_builder.rs) plus a source-proof markdown, and asserts via regex/substring checks that specific route tuples, function signatures, required string markers, and validator logic are present exactly as expected — a large battery of structural/textual assertions guarding that the v8 policy implementation hasn't silently drifted from its locked contract. It does not compute or compare numeric dB bounds at all, despite the file_doc describing a numeric derivation.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `main` — QUIRKY
- spec 3 · read at `3872feabab75` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:14:38Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A CLI entry point that orchestrates the whole derivation/verification pipeline: computes the terminal bounds via derive_safe_dbnano/derived_cells, hashes and verifies the compiled policy artifact with sha256_file/verify_artifact/verify_compiled_policy/verify_compiled_v8_routes, prints a report, and returns 0 on success or a nonzero exit code if any verification step fails.
- found: CLI with two modes: default prints derived_cells() as JSON; --check verifies checked-in current/candidate artifacts are byte-identical and calls verify_artifact/verify_compiled_policy, then greps compiled sources plus two other repo files for specific string markers to enforce an append-only policy-lineage contract, and verifies route contracts, printing a success message.
- predicted: some · documented: some · derivable: no · legible: most · trap: no
- note: The module doc describes the math/contract but not the two CLI modes or the marker-grepping lineage check, which is the bulk of this function's actual logic.

## tonepoet-pipeline/qualification/derive_dsd_reference_v9_metadata_admission.py

### the file itself
- spec 3 · read at `7256bbd369da` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:28Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A small companion script to the v8 one: sha256 hashes a file, verify() checks the checked-in policy-v9 W64 metadata-admission artifact(s) against pinned immutable values (schema/policy identity, admission rules, bound doc hashes) without asserting the mutable current-policy pointer, and main() provides a --check CLI entry that calls verify() and reports success/failure.
- found: sha256() hashes files; verify() checks four preserved v9 qualification artifacts against pinned SHA256 hashes, confirms current/candidate byte-identity, checks schema/policy/status identity and an exact metadata_mutation admission-rule dict (W64 metadata-write rejection vs other formats' qualification), then scans compiled Rust source (dsd_reference.rs, stages.rs) and a findings doc for specific historical markers to ensure the append-only v9 policy contract is still honored; main() is a --check CLI entry.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no

### `sha256`
- spec 3 · read at `0bd090b1e583` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:32Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads the file at `path` as bytes and returns hashlib.sha256(...).hexdigest() in a single expression/return.
- found: Exactly as predicted: hashlib.sha256(path.read_bytes()).hexdigest().
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify` — QUIRKY
- spec 3 · read at `849693433c91` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:54:03Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Locates specific pinned admission artifact files under root, computes their sha256 hashes (using the sha256 helper), and asserts each matches a hardcoded expected hash from when this checker was written — raising an AssertionError with a descriptive message if any artifact has changed. It also likely checks a persistent policy identity string/value rather than any mutable "current policy" pointer, per the docstring's contract.
- found: Checks sha256 hashes of pinned artifact files against EXPECTED, verifies the current/candidate v9 JSON bytes are identical and have the canonical schema/policy/status identity and metadata_mutation dict, then additionally greps three source files (Rust dsd_reference.rs, stages.rs, and a findings markdown doc) for specific string markers to confirm production code still honors the historical v9 policy.
- predicted: some · documented: some · derivable: no · legible: full · trap: no

### `main` — QUIRKY
- spec 3 · read at `6dd28c3f982d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:49:22Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Calls verify() to check the pinned policy-v9 W64 admission artifacts, printing a pass/fail message, and exits the process with a nonzero status code if verification fails (e.g. via sys.exit).
- found: Parses --repository-root and --check CLI args, calls verify(repository_root) which presumably raises on failure, and prints a success message on completion.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

## tonepoet-pipeline/qualification/verify_handoff_symlinks.py

### the file itself — QUIRKY
- spec 3 · read at `4d565d2ccd6d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:16:17Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A small standalone Python qualification script (not a pytest-style test) that checks a "handoff" directory's symlinks: it has a hardcoded list of expected symlink names and their expected literal target strings (not resolved paths), walks/checks each one exists as a symlink and that os.readlink() matches exactly, and exits nonzero with an error message listing mismatches/missing links if verification fails — run as a release/qualification gate via main().
- found: Reads expected symlink->target pairs from an external tab-separated ledger file (docs/handoff_symlinks.txt) rather than a hardcoded dict, walks the entire repo root (not just a "handoff" subdirectory) for any symlinks, diffs actual vs expected exactly, then does extra safety checks I didn't predict: rejecting absolute/traversal/duplicate ledger paths, verifying each symlink's resolved target stays within the repo root (no escapes), and confirming each target actually exists on disk.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The ledger-driven design plus root-escape check means this guards against symlinks quietly pointing outside the shipped tree, not just "did the expected links exist" — a much stronger qualification gate than the name alone suggests.

### `main` — QUIRKY
- spec 3 · read at `0e1ecff39d44` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:53:04Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Defines a hardcoded list/dict of expected symlink paths and their expected literal targets (the "handoff" set), then for each one checks the path exists, is a symlink, and its target matches exactly — printing a report and calling sys.exit(1) (or raising) if any are missing or mismatched, otherwise exiting cleanly.
- found: Parses an expected symlink set from a tab-delimited ledger file (docs/handoff_symlinks.txt), validating each entry is a safe relative non-duplicate path; walks the whole repo tree (without following links) to discover actual symlinks and their raw readlink targets; requires the actual dict to exactly equal the expected dict; then for each entry resolves the target and asserts it stays within the repo root and actually exists; prints a summary count on success.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

## tonepoet-pipeline/src/dsd_reference.rs

### the file itself
- spec 3 · read at `ba69d8de3294` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T17:22:50Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A pure, I/O-free planning module defining the "P0 Reference" DSD-to-PCM conversion policy: types for decibel fixed-point values (DbNano), content hashing (Sha256Digest), resolved output targets/profiles/depths/gain policies, and a decode-route authority table mapping source/role combinations to decode mechanisms and hash encodings. It provides pure functions to resolve/validate reference conversion parameters, build ffmpeg/sox command argument lists (render, measurement, packaging) and normalize/hash a plan into a semantic digest for qualification manifests, plus an extensive test suite pinning policy invariants (route table completeness, frozen error text, capacity/deadline bounds) — leaving actual filesystem/process execution to the orchestrator crate.
- found: Confirms the pure planning/no-I/O shape, the decode route/authority table, DbNano/Sha256Digest types, resolve/validate functions, command-argument builders, and semantic_plan_hash normalization for qualification-manifest digests — plus a large (~36% of file) test suite pinning policy invariants across 16 versioned policy generations, which I anticipated existed but underestimated the versioning depth (16 policy versions, 3 distinct hash-normalization eras) and the sheer enum/type surface (17 structs, 28 enums, 40 output-target variants).
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no
- note: The module doc header covers the I/O-free/pure-planning framing well but says nothing about the append-only versioned-policy hashing scheme, which is the file's real complexity core.

### `checked_add`
- spec 3 · read at `8a08fb779dd7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:42:37Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: DbNano is a fixed-point newtype wrapping an integer (nanodecibels). checked_add extracts the inner integer from self and other, performs a checked integer addition, and returns Some(DbNano(result)) on success or None on overflow.
- found: Unwraps the inner value from self and other, calls checked_add on the primitive, and maps the result back into Self, returning None on overflow.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `checked_sub`
- spec 3 · read at `9379e035dde4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:47:50Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: DbNano is a newtype wrapping an integer (nanodecibels), and checked_sub subtracts the inner values via the primitive's checked_sub, mapping the Option result back into Self, returning None on underflow/overflow.
- found: Exactly as predicted: self.0.checked_sub(other.0).map(Self).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `render`
- spec 3 · read at `c3bb7b8a24f0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:50:00Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: DbNano stores a decibel-like value as an integer count of nanounits. render splits self into integer and fractional (9-digit) parts, formats the fractional part zero-padded to 9 digits, and prepends a '+' sign when mandatory_sign is true and the value is non-negative (negative values already get '-' from the integer formatting), producing something like "+1.234567890".
- found: Splits the nanounit integer into whole/fractional parts by magnitude, applies '-' for negative, optional mandatory '+' otherwise, and formats fractional zero-padded to 9 digits.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `fmt`
- spec 3 · read at `a72b1e03e267` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:56:11Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Display impl for DbNano that delegates to self.render(f), writing the nanodecibel fixed-point value as a formatted dB string.
- found: Display impl writes self.render(false) — render takes a bool parameter (likely a sign/plus-prefix or verbosity flag) I didn't anticipate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `from_str`
- spec 3 · read at `e00e68c041be` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:27:20Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: DbNano is a fixed-point representation of a decibel value stored as nanodecibels (integer). This from_str parses a decimal string like "-3.5" (possibly with a "dB" suffix) into that integer nano-unit representation, splitting on the decimal point, validating digits, and returning a parse error (Self::Err) for malformed input or overflow when scaling to nano precision.
- found: Parses a plain decimal string (no exponent/comma, optional sign, up to 9 fractional digits) into nanodecibel i64 units via i128 intermediate math with checked_mul/checked_add/checked_neg to guard overflow, returning descriptive string errors for each malformed case.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `serialize`
- spec 3 · read at `2f4d691524bc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:04:13Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Calls self.render() to get the human-readable dB string representation, then serializer.serialize_str(&rendered), so the value round-trips symmetrically with the from_str/deserialize implementations.
- found: Serializes as a string via serializer.serialize_str(&self.render(false)), delegating formatting to render with a boolean flag (likely a sign/verbosity toggle) whose meaning isn't visible from this function alone.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `deserialize`
- spec 3 · read at `53f5aba80dcb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:32:25Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Deserializes a string (via String::deserialize or similar) and parses it with DbNano::from_str, converting any parse error into a serde D::Error via serde::de::Error::custom — mirroring the paired serialize which presumably calls render/fmt.
- found: Deserializes a String then parses it via raw.parse() (FromStr) mapping errors with serde::de::Error::custom, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `of_bytes`
- spec 3 · read at `60580b905912` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:26:26Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Computes the SHA-256 hash of the given byte slice using a Sha256 hasher, then wraps the resulting digest bytes into a Sha256Digest struct and returns it.
- found: Computes SHA-256 digest of bytes, copies into a fixed 32-byte array, wraps in Self (tuple struct newtype).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `from_hex`
- spec 3 · read at `0c6efaef1262` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:57:07Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Validates the input is exactly 64 hex characters (case-insensitive), decodes it into 32 bytes, and constructs a Sha256Digest; returns an Err(String) with a descriptive message if length is wrong or characters aren't valid hex.
- found: Validates length is 64 and all chars are ascii hex digits, then decodes byte-pairs via chunks_exact(2) into a 32-byte array wrapped in Self. Matches prediction closely.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `to_hex`
- spec 3 · read at `7b53467c438a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:46:13Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Converts the Sha256Digest's internal byte array into a lowercase hex string, likely by iterating bytes and formatting each with {:02x} into a String (e.g. via fold or a format! + iterator chain).
- found: Builds a 64-char capacity String and writes each byte formatted as {:02x} via std::fmt::Write, matching prediction closely except it uses write! with a preallocated buffer rather than fold/iterator chain.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `fmt` #2
- spec 3 · read at `1b5326e566a3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:39:00Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Display impl for Sha256Digest that writes the hex-encoded digest to the formatter, likely delegating to to_hex().
- found: Exactly as predicted: f.write_str(&self.to_hex()).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `serialize` #2
- spec 3 · read at `a4ebb75a141e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:47:17Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Serializes the Sha256Digest as a hex-encoded string (calling self.to_hex()) via serializer.serialize_str, rather than serializing the raw byte array, so the digest round-trips as readable hex in formats like JSON.
- found: serialize_str(&self.to_hex()) — serializes the digest as its hex string form.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `deserialize` #2
- spec 3 · read at `cd83ed394dea` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:12:14Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Custom serde Deserialize impl for Sha256Digest: deserializes a hex string (via deserializer.deserialize_str or String deserialization) and then parses it with Sha256Digest::from_hex, converting any parse failure into a D::Error via serde::de::Error::custom, mirroring the Serialize impl that presumably emits the hex string.
- found: Deserializes a String via serde, then parses it with Sha256Digest::from_hex, mapping any error to D::Error via serde::de::Error::custom.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `key`
- spec 3 · read at `5f7f7ec628d2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:40:55Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const fn matching on self (a DsdReferencePolicyVersion enum) and returning a static string literal per variant — a stable identifier key, likely something like "p0-v1", used for serialization.
- found: Matches self against 16 SoxNg14801V1..V16 variants, each mapped to its own named static string constant (DSD_REFERENCE_POLICY_V{n}_KEY).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` — OBSCURE
- spec 3 · read at `b36bf00d1d3e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:49:26Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Returns a DsdSourceSettings struct populated with sensible default values for describing a DSD source — likely a standard DSD rate (e.g. DSD64), default channel count (stereo), and default bit/byte ordering — used as a baseline before being overridden by actual source detection or user config.
- found: Returns DsdSourceSettings defaulted to the "Reference" pathway/profile/gain-mode, a specific named reference policy version (SoxNg14801V16), no fixed gain override, and a default normalize peak target — this is about conversion policy selection, not sample-rate/channel description as I guessed.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `is_p0_reference_lossless`
- spec 3 · read at `7bff3b150149` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:55:46Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A const fn on ResolvedOutputTarget that matches self against exactly seven specific enum variants (the P0 Reference lossless targets, likely lossless codec/format combos like FLAC or WAV variants) using a matches! macro or match expression, returning true for those and false for everything else.
- found: A const fn using matches! to check if self is one of seven lossless ResolvedOutputTarget variants: FlacNative, WavRiff, WavRf64, WavW64, AiffNative, WavPackNative, AlacM4a.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `is_lossy`
- spec 3 · read at `b591302fdd7c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:44:50Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A const match over the ResolvedOutputTarget enum variants, returning true only for the variant(s) representing lossy delivery targets (per the doc, a reserved-for-future-frontend-use target) and false for all lossless/PCM/reference variants. Likely a simple one-arm match or two-arm boolean match with no other logic.
- found: Returns true via a matches! macro checking self against an explicit list of ~21 lossy delivery target variants spanning Mp3, Aac, Opus, Dts, and Ac3 codec families (in various container wrappers like native/mka/mkv/mp4/m4a). Everything else (presumably PCM/lossless targets) returns false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The doc string ("True for a lossy delivery target reserved for future Reference-front-end use") reads like it describes a single specific variant, not this broad multi-codec-family matches! list — it appears misattributed or copied from elsewhere rather than describing this function.

### `key` #2 — QUIRKY
- spec 3 · read at `863bdc0d2ef8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:06:40Z · by ross@rossturk.com · warm reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: ResolvedOutputTarget is a small Copy enum of resolved output formats (various PCM bit-depths and/or DSD rates); this const fn is a match over self returning a fixed lowercase static string identifier per variant (like "pcm_s16le", "pcm_s24le", "dsd64", etc.) used as the canonical key for presets/fingerprints.
- found: Exhaustive match over ResolvedOutputTarget variants (codec × container combos: FLAC/WAV/AIFF/WavPack/MP3/AAC/Opus/ALAC/DSF/DFF/DTS/AC3/LPCM, each in one or more container flavors) returning a fixed lowercase snake_case string per variant.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The enum is codec+container pairs (e.g. flac_mka, opus_webm), not bit-depth/DSD-rate variants as I guessed — much larger surface (42 arms) than expected from a DSD-reference-focused file.

### `resolve` — OBSCURE
- spec 3 · read at `80c81e094605` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:16:08Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Matches on the AudioFormat variant to pick a base target codec/container, validates or cross-checks the given extension against that format (erroring on mismatch), and inspects the flags slice for a "trusted catalog" marker to adjust the resolved target's policy (e.g. whether it counts as P0-reference-lossless), building and returning a ResolvedOutputTarget or an error for unsupported combinations.
- found: Normalizes the extension, checks flags against exact known sets (empty, ["-rf64","auto"], ["-f","webm"]), then does an exhaustive match over (AudioFormat, extension, flag-shape) tuples to pick one of many enum variants representing a specific codec+container combination, erroring with a canonical-target error if no combination matches. No "trusted catalog" concept present despite the docstring wording.
- predicted: none · documented: some · derivable: no · legible: most · trap: no
- note: Docstring says "trusted catalog flags" but the flags param is actually raw CLI-style flag strings (e.g. -rf64 auto) used to disambiguate container variants, not a trust/catalog concept — misleading doc wording for the next reader.

### `default` #2 — QUIRKY
- spec 3 · read at `1984054fcdbc` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T06:41:14Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: ReferenceProgrammeScope is an enum (or similar) selecting which subset of the reference qualification programme runs; default() returns the full/broadest scope variant so nothing is skipped unless explicitly restricted.
- found: Returns Self::Singleton as the default scope variant — confirms it's an enum with a default variant, but the variant name "Singleton" (rather than something like "Full") suggests the default scope is a single-item/single-track programme rather than a broad one.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Would need the enum definition to know what Singleton means vs other variants (e.g. Album, Batch) — the name alone doesn't disambiguate "one track" from "one qualification run."

### `key` #3
- spec 3 · read at `5b80df5957c4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T15:17:53Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: ResolvedDsdProfile is an enum for DSD quality/rate profiles (e.g. DSD64/128/256/512), paired with sinc/passband_hz/stopband_hz filter methods. key() matches on self and returns a stable static &str identifier per variant, used for things like cache keys or serialization tags.
- found: A simple match over the ResolvedDsdProfile enum variants (B1RateOnly, B2RateOnly, B3{..}, B4{..}, B4W{..}, B5{..}, B6{..}), returning a short stable static string key per variant (b1, b2, b3, b4, b4w, b5, b6).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sinc`
- spec 3 · read at `4053d1a318bc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T17:09:21Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A const fn on the ResolvedDsdProfile enum that matches self against its variants, returning Some((transition_width, center_frequency)) for profiles that specify explicit sinc filter parameters and None for those that use some default/implicit filter design instead.
- found: Matches ResolvedDsdProfile variants: B1RateOnly/B2RateOnly return None; B3/B4/B4W/B5/B6 (which carry transition_hz/center_hz fields) return Some((transition_hz, center_hz)). Field names confirm doc's parameter meaning.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `passband_hz`
- spec 3 · read at `778070118fe3` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T16:05:04Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A const fn that matches on the ResolvedDsdProfile enum variants, returning Some(fixed_frequency_in_hz) for the explicit-sinc profile variant(s) with a frozen/hardcoded passband edge, and None for other profile variants that don't use an explicit-sinc filter (e.g. those relying on a different resampler).
- found: Matches on ResolvedDsdProfile: B1RateOnly/B2RateOnly return None; B3/B4/B4W/B5/B6 variants each carry a passband_hz field which is extracted and returned as Some(value) — not a hardcoded constant in this function, but a field read from the enum's own data.
- predicted: most · documented: some · derivable: no · legible: full · trap: no
- note: The doc comment 'Frozen flat-passband edge' suggests the value is fixed, but this function just reads whatever passband_hz field the variant was constructed with — the freezing (if any) happens at construction time elsewhere, not here.

### `stopband_hz` — QUIRKY
- spec 3 · read at `8f3dc99c9c4b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:59:06Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This is a const fn on ResolvedDsdProfile that matches over the profile's variants, returning Some(frequency) for the explicit-sinc filter profiles (a fixed stopband edge value in Hz) and None for other profile kinds where the concept doesn't apply — mirroring the sibling passband_hz accessor.
- found: Matches over ResolvedDsdProfile variants: rate-only profiles (B1/B2) have no stopband and return None; the B3-B6 explicit-sinc variants each carry passband_hz and transition_hz fields, and the stopband edge is computed as their checked sum (None on overflow).
- predicted: some · documented: some · derivable: no · legible: full · trap: no
- note: Doc says 'Frozen stopband edge' but the value is actually derived at call time from passband_hz + transition_hz, not a stored/fixed constant — the wording is misleading.

### `typed_b6_profile`
- spec 3 · read at `c6dfcd080b04` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:18:49Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const fn that constructs and returns a ResolvedDsdProfile struct literal for the "B6" profile variant, with a distinct key string and specific sinc/passband/stopband filter parameters, but likely marked or intended as unused/disabled in actual conversion selection logic — existing only so diagnostics and qualification artifacts can reference a consistent B6 entry.
- found: Constructs the B6 enum variant of ResolvedDsdProfile with specific passband/transition/center frequency constants (88200/51800/114100 Hz), not a struct with sinc/stopband fields as I guessed — ResolvedDsdProfile is an enum with per-variant fields, and B6 uses a transition/center band shape rather than sinc/passband/stopband.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `key` #4
- spec 3 · read at `fbf32689d960` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:06:55Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A const fn key() on the ReferenceDecodedCarrierSelector enum that matches over its variants and returns a short static string identifier for each, mirroring the same key() pattern on sibling types (ResolvedDsdProfile::key, ReferenceDecodeRoleClass::key) — used as a stable diagnostic/logging key.
- found: Matches over the four ReferenceDecodedCarrierSelector variants and returns a static snake_case string for each, matching the pattern of sibling key() methods exactly.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `key` #5
- spec 3 · read at `c227f74bb912` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:05:53Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A simple const match over the ReferenceDecodeRoleClass enum variants, returning a fixed lowercase &'static str identifier for each variant (e.g. "primary", "secondary", "control") to use as a stable key in evidence/reporting records.
- found: Const match over 6 role-class variants (reconstruction, terminal qpcm, packaged w64/non-w64, post-metadata w64/non-w64) returning their fixed string keys.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `key` #6
- spec 3 · read at `b303a50cc947` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:37:24Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A const match over ReferenceDecodeMechanism's variants, each returning a fixed static string slug (like a tool/mechanism name) used as a stable identifier in evidence records.
- found: Const match over ReferenceDecodeMechanism's two variants (DirectFfmpeg, SoxFloat64W64RawStream), returning fixed static string slugs "ffmpeg_direct" and "sox_f64le_raw_stream" respectively.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_codec`
- spec 3 · read at `7b204a64b029` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:52:00Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A const match over the ReferenceSampleHashEncoding enum variants, returning the corresponding ffmpeg PCM codec name string literal (e.g. "pcm_s16le", "pcm_s24le", "pcm_f32le") for each variant so the caller can pass it as ffmpeg's -acodec/-c:a argument to produce output matching that hash encoding.
- found: A const match over three variants (SignedInt24Le, Float32Le, Float64Le) returning the matching ffmpeg PCM codec name literal for each.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Guessed pcm_s16le as a variant but the actual enum has no 16-bit variant, only 24-bit int and 32/64-bit float — minor miss on exact variant set.

### `key` #7
- spec 3 · read at `ff1e153461f3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:51:13Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A const match over the ReferenceSampleHashEncoding enum's variants, returning a stable, short static string identifier for each (e.g. "s16le", "s24le", "f32le") to be used as a machine-stable evidence/report key rather than a Debug-derived name that could drift with refactors.
- found: Const match over the three hash-encoding variants returning stable static string keys ("int24_le", "float32_le", "float64_le"), exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `new`
- spec 3 · read at `93a6b785a820` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:31:28Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A trivial const constructor that takes role_class, bit_depth, mechanism, and hash_encoding and returns a ReferenceDecodeRouteRule struct literal with those four fields set directly, with no validation or derived logic (consistent with the accessor peers like role_class/bit_depth/mechanism/hash_encoding that just read the fields back).
- found: Trivial const struct-literal constructor setting the four fields directly, no validation.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `role_class` — QUIRKY
- spec 3 · read at `86d47d90bd71` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:51:26Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A const fn on ReferenceDecodeRouteRule (likely an enum) that matches self and maps each specific route/carrier variant to a coarser, normalized ReferenceDecodeRoleClass category — a pure classification/grouping function with no I/O.
- found: ReferenceDecodeRouteRule is a struct (Copy) with a role_class field, and this is a trivial accessor returning it by value — not a match/classification over variants as I guessed.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The doc string 'Normalized carrier role.' appears to belong to the role_class field itself rather than this accessor method.

### `bit_depth` — QUIRKY
- spec 3 · read at `0c00eb17a8c9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:58:08Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns the fixed PCM bit depth (terminal depth whose decoded bytes get inspected/hashed) associated with this reference decode route rule — likely a single constant value (e.g. 24-bit) since this is a qualified P0 reference policy, rather than varying per rule variant.
- found: Plain accessor returning the stored bit_depth field of the rule struct — not a hardcoded constant, just a per-instance field value.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `mechanism`
- spec 3 · read at `2cb7782882a2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:06:32Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Trivial const accessor returning self.mechanism, the stored ReferenceDecodeMechanism field, mirroring sibling accessors like role_class, bit_depth, hash_encoding.
- found: Trivial const accessor returning self.mechanism.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `hash_encoding` — QUIRKY
- spec 3 · read at `8fb2f8f19a20` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:13:41Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const fn on ReferenceDecodeRouteRule that derives the appropriate ReferenceSampleHashEncoding (the exact depth-native byte layout to hash) based on the rule's bit_depth field, likely matching on bit depth to pick between e.g. 16-bit, 24-bit, or 32-bit sample encodings.
- found: Plain field accessor returning the already-stored hash_encoding field, no computation or matching.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: I expected it to compute the encoding from bit_depth via a match; it's actually just a stored field set elsewhere (probably in ::new).

### `new` #2
- spec 3 · read at `0989476009e2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:36:49Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A trivial constructor for the ReferenceDecodeAuthorityError type: takes anything convertible into a String and stores it in a single `message` (or similarly named) field, returning Self. No validation or side effects.
- found: Trivial constructor storing the into-String message in a `message` field.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `fmt` #3
- spec 3 · read at `999ad98c5c92` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:22:21Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Display impl for ReferenceDecodeAuthorityError that writes out a stored message/context string field, likely via write!(f, "{}", self.message) since the type has a ::new constructor presumably taking a message.
- found: f.write_str(&self.message) — writes the stored message field directly, essentially as predicted (write! vs write_str is a trivial difference).
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `role` — OBSCURE
- spec 3 · read at `e0edd7d3797a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:29:40Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Const function that returns a fixed ReferenceDecodedSampleRole::Original variant, representing that this authority always produces the "original" semantic carrier role regardless of self's specific variant, similar to how role_class likely maps to a different classification.
- found: Trivial getter returning the stored role field; I incorrectly guessed it was a hardcoded constant rather than a per-instance stored value.
- predicted: none · documented: full · derivable: no · legible: full · trap: no
- note: The one-line doc "Original semantic carrier role." actually documents the field's meaning/value for this particular authority instance, not the function's general behavior — it's genuinely informative context that isn't derivable from the getter body alone.

### `role_class` #2 — QUIRKY
- spec 3 · read at `574559c39c7f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:35:11Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A const fn on ReferenceDecodeAuthority (a small Copy enum) that does a plain match over self, returning the corresponding ReferenceDecodeRoleClass variant for each authority — a static lookup table with no other logic.
- found: It's a trivial field accessor: ReferenceDecodeAuthority is a struct (not an enum as I guessed) with a role_class field, and this just returns self.role_class directly.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `contract` — QUIRKY
- spec 3 · read at `ddede66ab786` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:40:24Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const match over the ReferenceDecodeAuthority enum variants, returning each variant's fixed/associated FinalPcmContract (the exact PCM format/bit-depth guarantee bound to that decode authority).
- found: It's a plain field accessor: ReferenceDecodeAuthority is a struct with a `contract` field, and this just returns a copy of it. Not a match over enum variants as I guessed — the value was already computed/stored elsewhere (likely at construction).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: ReferenceDecodeAuthority is a struct, not an enum, despite the peer list (role, role_class, mechanism, hash_encoding accessors) reading like enum-variant dispatch — those are probably also plain field getters.

### `mechanism` #2 — QUIRKY
- spec 3 · read at `2cb7782882a2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:45:31Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: ReferenceDecodeAuthority is likely an enum of decode authority variants, and this const fn matches self to return the corresponding ReferenceDecodeMechanism variant authorized for that authority — a simple match-based lookup.
- found: Trivial const field accessor returning self.mechanism — ReferenceDecodeAuthority is a struct holding a mechanism field directly, not an enum requiring a match, as I'd guessed.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `hash_encoding` #2 — QUIRKY
- spec 3 · read at `8fb2f8f19a20` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:50:32Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A const match over the ReferenceDecodeAuthority enum's variants (or a delegation to its associated route rule's hash_encoding), returning the fixed ReferenceSampleHashEncoding constant — depth-native bytes — associated with that authority, mirroring the sibling ReferenceDecodeRouteRule::hash_encoding.
- found: Plain field accessor: returns the stored hash_encoding field of the ReferenceDecodeAuthority struct directly, no matching or delegation.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: I assumed ReferenceDecodeAuthority was an enum needing a match; it's actually a struct with a hash_encoding field, so this is a trivial getter.

### `hash_format`
- spec 3 · read at `c33febeec835` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:56:29Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const fn matching on the ReferenceDecodeAuthority enum variant (or just returning a fixed value) that yields a canonical hash algorithm identifier string such as "sha256", used alongside hash_encoding to describe how reference decodes are verified.
- found: Simply returns a module-level constant REFERENCE_SAMPLE_HASH_FORMAT regardless of self/variant — not a match on enum variants as I guessed, just a fixed constant reference.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `path`
- spec 3 · read at `3018089338d7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:12:16Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A trivial getter that returns &self.path, exposing the stored PathBuf field of ReferenceDecodedCarrier as a &Path reference. No computation or validation.
- found: Trivial getter returning &self.path.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `authority`
- spec 3 · read at `8000f9112fac` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:59:26Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A trivial const accessor returning self.authority (a Copy ReferenceDecodeAuthority field) — the opaque route authority value bound to this carrier's specific path, set at construction time.
- found: Exactly matched: trivial const accessor returning self.authority.
- predicted: full · documented: some · derivable: no · legible: full · trap: no
- note: The one-line doc ("Opaque route authority bound to this exact path") explains the semantic meaning of the field, which the code alone (just `self.authority`) doesn't convey.

### `reference_decode_role_class`
- spec 3 · read at `21858effd773` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:17:28Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Pattern-matches on `role` combined with properties of `contract` (e.g. bit depth/format) to classify the decode into a `ReferenceDecodeRoleClass` variant, returning a `ReferenceDecodeAuthorityError` if the role/contract combination is invalid or unsupported.
- found: Matches on ReferenceDecodedSampleRole variant: for ReconstructionR64W64 validates the contract is undithered Float64 PCM and errors otherwise; for TerminalQpcmW64 unconditionally classifies; for PackagedOutput/PostMetadataOutput it validates the target/bit-depth combo via validate_reference_target_depth and classifies further into W64 vs NonW64 subvariants based on whether target equals WavW64.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `reference_decode_authority`
- spec 3 · read at `d611bd4f0e4a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T19:24:39Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Given a carrier role and a PCM contract, looks up/matches the single allowed decoder mechanism, hash encoding, and hash format for that combination and constructs a ReferenceDecodeAuthority. Returns a ReferenceDecodeAuthorityError if the role/contract pairing isn't one of the recognized, admitted combinations (e.g. wrong role class or unsupported contract for that role).
- found: Validates the PCM contract (nonzero rate/channels, sample_kind matches bit_depth, dither matches the depth-appropriate expected dither — TPDF for Int24, none for float, error for other int depths), derives a role_class, then looks up the unique matching rule in REFERENCE_DECODE_ROUTE_RULES by role_class+bit_depth, erroring on zero or multiple (ambiguous) matches, and builds the ReferenceDecodeAuthority from the rule's mechanism/hash_encoding.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `validate_reference_decode_mechanism`
- spec 3 · read at `bd36a43e90a4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:45:17Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Looks up the canonical ReferenceDecodeAuthority for the given role and contract (via reference_decode_authority), and compares its mechanism against `proposed`. If they match exactly, returns Ok(authority); otherwise returns Err(ReferenceDecodeAuthorityError) describing the mismatch, so an externally supplied mechanism can never diverge from the immutable rule table.
- found: Looks up the canonical authority via reference_decode_authority (propagating its error with ?), compares its mechanism field to `proposed`, and returns Err with a formatted mismatch message (naming the rejected mechanism, role class, bit depth, and required mechanism) if they differ, else Ok(authority).
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `carrier_path` — OBSCURE
- spec 3 · read at `f6f227f92771` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:06:01Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Simple accessor on PlannedMeasurement that returns the filesystem path of its decoded carrier, if one has been bound/set — likely delegating to an inner Option<ReferenceDecodedCarrier>'s path() method, returning None if no carrier has been bound yet.
- found: Returns the path from self.input_stage's input if that optional stage is present and has a path, otherwise falls back to self.command.input's path.
- predicted: none · documented: some · derivable: no · legible: full · trap: no
- note: The doc calls it "the durable path-backed carrier" but the field is actually a fallback between input_stage and command.input — no field named carrier exists, so the name/doc don't map cleanly onto the struct's actual fields.

### `decoded_carrier_spec` — OBSCURE
- spec 3 · read at `23a6d8690b46` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:43:54Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This method looks up the decoded carrier(s) bound to this plan summary matching the given selector (likely picking among multiple decoded carriers, e.g. by role), then returns a tuple of that carrier's file path, its sample role, and its final PCM contract. It probably panics or unwraps if no matching carrier was bound yet, since the return type isn't an Option and by this stage in the pipeline the carrier should already be bound via bind_decoded_carrier.
- found: A pure match over the four ReferenceDecodedCarrierSelector variants, each returning a fixed (path, role, contract) triple built from the struct's own fields (r64_path/qpcm_path/packaged_path/delivered_path, final_pcm, target); no lookup, no fallible binding, no panic path — this is a static dispatch table, not a query into bound state.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `decoded_carrier`
- spec 3 · read at `b608361479b0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:48:57Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Looks up the decoded carrier spec matching the given selector within this plan summary, validates that it resolves to a single concrete (closed/non-ambiguous) path binding, and returns a ReferenceDecodedCarrier with that path and its decode authority — erroring if the selector doesn't match or isn't fully resolved.
- found: Delegates to decoded_carrier_spec(selector) to get (path, role, contract), resolves the decode authority via reference_decode_authority(role, contract) (which can error), and wraps the result into a ReferenceDecodedCarrier struct with path and authority.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `bind_decoded_carrier`
- spec 3 · read at `25caf757217c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T12:01:01Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Looks up the planner-owned carrier path/spec for the given selector on self (DsdReferencePlanSummary), compares it against candidate_path, and if they match exactly, constructs and returns Ok(ReferenceDecodedCarrier) wrapping the path and authority; if they don't match (or the selector doesn't resolve), returns Err(ReferenceDecodeAuthorityError) describing the mismatch, enforcing the fail-closed guarantee described in the docs.
- found: Calls self.decoded_carrier(selector) to get the planner-owned carrier (propagating error via ?), compares its path to candidate_path with != , returns a formatted mismatch error if unequal (naming selector.key(), expected and got paths), otherwise returns Ok(carrier). Matches prediction closely.
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `reference_error_text`
- spec 3 · read at `32ca4299b238` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:25:25Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A match statement over ReferenceErrorCode variants, returning a fixed &'static str for each — a stable, exact human-readable error message per code, used so failure text stays consistent for tests/reporting rather than being generated dynamically.
- found: A large match over ReferenceErrorCode variants (~30) returning a fixed, detailed &'static str per code — each is a "DSD-REF-P0-NNN:" prefixed message explaining exactly why a given Reference DSD policy constraint failed and what the user can do instead, referencing the specific pinned policy name sox_ng_14_8_0_1_v16.
- predicted: full · documented: some · derivable: no · legible: most · trap: no

### `reference_metadata_mutation_rejection`
- spec 3 · read at `1191d3fe1c2f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:21:30Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Checks the resolved output target and returns Some(static rejection string) if that target type (likely DSD) has no qualified route for metadata mutation, otherwise returns None to indicate the mutation is permitted.
- found: Matches on the resolved output target; for WavW64 it returns a static error string via reference_error_text(W64MetadataMutationUnqualified), and for every other target variant returns None (mutation allowed).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The doc's "no qualified route" phrasing doesn't hint which specific target(s) are unqualified — I guessed DSD by module theme, but it's WAV64 specifically.

### `invalid_reference`
- spec 3 · read at `b22bd577273a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:20:25Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A small constructor helper that builds a PlanningError value tagging the given `field` name with the given `ReferenceErrorCode`, likely producing a generic "invalid reference" error variant, possibly formatting a message via reference_error_text.
- found: Calls PlanningError::invalid_settings with the field name and a message produced by reference_error_text(code), a thin one-line wrapper.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `source_rate_name`
- spec 3 · read at `015af160d81c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:55:17Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A simple match over the DsdRate enum variants (e.g. Dsd64, Dsd128, Dsd256, Dsd512) returning a corresponding static string like "DSD64" for use in logging/error messages/reports.
- found: Simple exhaustive match over DsdRate variants (Dsd64/128/256/512/1024) returning the corresponding static display string.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `invalid_target_profile` — QUIRKY — TRAP
- spec 3 · read at `ca7dd09df012` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:47:24Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A small error-constructor helper, one of a family (invalid_reference, invalid_target_depth, invalid_exact_gain, invalid_terminal_depth) that builds a PlanningError variant for when a requested target profile is invalid given the DSD source_rate. It likely formats a message string (possibly via reference_error_text) embedding the field name and source_rate, and wraps it with the given ReferenceErrorCode into a PlanningError.
- found: Matches on ReferenceErrorCode: for Target882/Target96 it returns a PlanningError with a specific hardcoded policy-citation message (naming the exact qualified policy version and suggesting alternate rates); for any other code it silently delegates to invalid_reference(field, code) instead.
- predicted: some · documented: none · derivable: yes · legible: full · trap: yes
- note: The fallback to invalid_reference for any ReferenceErrorCode other than Target882/Target96 means a new target-profile-related error code added later will silently get the generic message unless this match is updated too.

### `invalid_target_depth`
- spec 3 · read at `25deced1ee09` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:32:21Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A small constructor that builds a PlanningError describing that the given bit depth is not valid/allowed for the resolved output target, formatting a message that includes the field name, the target, and the depth value.
- found: Builds a PlanningError::invalid_settings with a coded message (DSD-REF-P0-011) stating that target.key() does not support the given depth under the named Reference policy version, telling the caller to choose a listed target/depth pair.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `invalid_exact_gain` — QUIRKY
- spec 3 · read at `af70dce1447d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:46:56Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A small error-constructor helper: given a field name and the resolved gain policy, it builds and returns a PlanningError with a message stating that `field` requires an exact numeric gain value but the resolved policy (formatted via Debug or a helper) is not an exact gain, mirroring the sibling invalid_reference/invalid_target_profile/invalid_target_depth constructors.
- found: For NativeLevelExact/FixedExact policies, builds a specific coded PlanningError (DSD-REF-P0-016) explaining the requested exact gain would violate the -1.000000000 dBTP true-peak ceiling; for ReferenceCompensated/NormalizePeak it instead delegates to invalid_reference with an UnsafeExactGain error code.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `invalid_terminal_depth`
- spec 3 · read at `bf9399a8aaf9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T11:50:11Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A small error-constructor helper that builds and returns a PlanningError variant indicating the given terminal PcmBitDepth is invalid, incorporating the field name and the offending depth value into the error.
- found: Maps the invalid PcmBitDepth variant to a specific ReferenceErrorCode (Int8/Int32 get dedicated codes, Int16 gets a distinct 'unqualified' code, and the remaining depths fall back to a generic TargetDepth code), then delegates to invalid_reference(field, code) to construct the PlanningError.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `resolve_reference_target_rate`
- spec 3 · read at `37281c1b5663` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:10:06Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Matches on `target`: if it's an explicit numeric rate, validates and returns it directly; if it's the DSD-source sentinel, derives the appropriate PCM sample rate from `source_rate` (e.g. via a standard DSD-to-PCM multiple/table) and returns that, returning an error if the resulting rate isn't supported/valid.
- found: Rejects Dsd512/1024 sources upfront; for RateTarget::Source maps DSD64/128/256 to fixed PCM rates (88.2k/176.4k/352.8k); for PcmHz uses given rate; for RateTarget::Dsd returns an unsupported-target error; finally validates the resolved rate against a fixed whitelist of standard PCM rates.
- predicted: most · documented: some · derivable: no · legible: most · trap: no

### `resolve_reference_profile` — QUIRKY
- spec 3 · read at `093329e4de55` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:56:32Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Validates the source_rate/target_rate_hz/selection combination against a fixed qualified P0 profile matrix, and composes a ResolvedDsdProfile by delegating to peer resolvers (resolve_reference_target_rate, resolve_reference_depth, resolve_reference_front_end, resolve_gain_policy), returning one of the specific invalid_* errors if the combination is not a qualified/supported one.
- found: Directly matches (source_rate, target_rate_hz) or source_rate alone against a hardcoded qualified-profile table, returning ResolvedDsdProfile variants with concrete filter parameters (passband/transition/center Hz) for Reference vs Wideband selections, with specific invalid_reference/invalid_target_profile errors for unqualified combinations; no delegation to the sibling resolver functions.
- predicted: some · documented: some · derivable: no · legible: most · trap: no

### `resolve_reference_depth`
- spec 3 · read at `bdfa273eac36` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:16:29Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Maps a BitDepthTarget enum (likely variants for an explicit fixed depth vs. a "terminal"/default depth) to a concrete PcmBitDepth, validating the value against supported depths and returning an Err (via something like invalid_target_depth) if unsupported. Pure function, no I/O, consistent with the file's stated no-I/O policy.
- found: Maps BitDepthTarget::Source to Int24 default, or unwraps BitDepthTarget::Pcm(depth). Then explicitly rejects Int8, Int32, and Int16 as invalid terminal/unqualified depths for this Reference policy (each with a distinct ReferenceErrorCode), allowing only Int24, Float32, and Float64 through.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `validate_reference_target_depth`
- spec 3 · read at `7be967b98a47` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:24:34Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Looks up the given ResolvedOutputTarget in a fixed/frozen matrix of allowed PcmBitDepth values for that target, and returns Ok(()) if the passed depth is one of the allowed depths, or an error describing the invalid combination otherwise.
- found: Rejects Int16 outright as unqualified for reference/terminal use, then matches target against a hardcoded set of allowed depths (WAV variants allow Int24/Float32/Float64; FLAC/AIFF/WavPack/ALAC allow only Int24; anything else unsupported), returning Ok or an invalid_target_depth error.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `resolve_reference_front_end`
- spec 3 · read at `803bc47d9035` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:26Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Pattern-matches on the DsdSourceKind enum variants and maps each to the corresponding DsdInputFrontEnd (which decoder/tool path handles that DSD source type), returning Err via a helper like invalid_reference for unsupported/invalid kinds. Pure function, no I/O, deterministic.
- found: Matches DsdSourceKind: uncompressed DSF/DSDIFF map to NativeUncompressed; DSDIFF+DST maps to a qualified DST decoder front-end; SacdTrack is explicitly rejected as unqualified integration; UnknownDsdContainer is rejected as unknown encoding. Pure lookup, no I/O.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The doc 'Resolve the front-end from immutable source facts' actually describes the whole file's DsdSourceKind concept rather than this specific function's per-variant error codes.

### `terminal_realization_bound` — QUIRKY — TANGLED
- spec 3 · read at `f919f8e42793` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:02:43Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds and returns a TerminalRealizationBound struct whose true-peak ceiling is a fixed -1.0 dBTP value regardless of target_rate_hz (rate-invariant per the doc), but which encodes target_rate_hz and depth into an identity/derivation tag so the value is locked to that specific cell. It likely also reserves an extra measurement quantum/margin as mentioned in the doc, probably via a match on depth.
- found: Matches on PcmBitDepth to pick a per-depth (q63, safe-ceiling-in-dBnano, realization-name) triple — not a single fixed -1.0dBTP value as I guessed. Then it builds a derivation string encoding policy version, rate, depth, realization name, q63, reserve, and safe value, hashes it with SHA256, and returns a TerminalRealizationBound with the q63 ceiling, safe pre-terminal ceiling (as DbNano), and the derivation digest.
- predicted: some · documented: some · derivable: no · legible: some · trap: no
- note: The doc's claim that the bound is 'rate-invariant' is true numerically (rate isn't used in computing q63/safe) but rate is folded into the derivation_digest identity, exactly as the doc says — worth flagging that the digest, not the struct's numeric fields, is what changes per rate.

### `resolve_gain_policy`
- spec 3 · read at `dfcbca3cf000` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:12:09Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads the gain-related fields out of DsdSourceSettings (profile, exact gain value, etc.), validates them against target_rate_hz and depth — rejecting invalid combinations like a non-finite/out-of-range exact gain, an unsupported target profile, or an invalid terminal depth — and returns a ResolvedGainPolicy describing the concrete gain operation to apply (fixed gain, loudness normalization, or none) along with any derived bounds needed later in the pipeline.
- found: Matches on settings.gain_mode (Reference, NativeLevel, Fixed, NormalizePeak), validating that fixed_gain_db is only set in Fixed mode and within +-24dB, and normalize_peak_target_dbfs is within -12..0 dBFS for NormalizePeak; computes a terminal_realization_bound from target_rate_hz/depth, and returns the appropriate ResolvedGainPolicy variant (ReferenceCompensated/NativeLevelExact/FixedExact/NormalizePeak) using fixed DbNano constants like HEADROOM_RESTORATION and DSD_COMPENSATION.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `extract_single_loudnorm_report`
- spec 3 · read at `0cb6af13c7c7` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:01:10Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Scans ffmpeg stderr text for JSON blocks (loudnorm filter's printed report), filters to ones that contain an "input_tp" field, and requires there to be exactly one such match — returning it as a String on success, or an Err with a descriptive message if zero or more than one are found.
- found: Hand-scans stderr byte-by-byte tracking brace depth and string/escape state to find top-level JSON object spans, collects those containing \"input_tp\", errors on truncated/unterminated JSON, and requires exactly one qualifying report (errors on zero or duplicates).
- predicted: most · documented: full · derivable: no · legible: full · trap: no
- note: The JSON extraction is a manual brace/string-depth scanner, not a JSON parser or regex — worth knowing if adding new report fields to filter on.

### `sox_stats_peak_token_is_supported` — OBSCURE
- spec 3 · read at `11656bee134e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:07:44Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given token string matches one of a small fixed set of recognized labels that `sox stats`/`sox stat` output uses for peak-level fields (e.g. "Pk lev dB", "RMS Pk dB"), returning true if it's one of the supported ones this parser knows how to handle and false otherwise.
- found: Validates that a numeric dB peak-value token (not a field label) is in a safely parseable plain-decimal form: rejects scientific notation, commas, leading '+', "inf"/"nan" variants, but allows "-inf" as a special case, then confirms it actually parses as DbNano.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `extract_single_sox_stats_peak_report`
- spec 3 · read at `cd519521cf45` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:20:23Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Searches stderr lines for the one SoX "Pk lev dB" line (erroring if zero or more than one match), splits it into whitespace tokens after the label, and validates the token count matches the expected shape: 1 token if channels==1, or channels+1 tokens (Overall plus one per channel) otherwise. Returns the Overall/first token as the peak level string, or an Err describing the mismatch.
- found: Matches prediction on the line-search, dedup, and column-count validation, but also adds a channels==0 guard and a per-token numeric-syntax support check (sox_stats_peak_token_is_supported) that I didn't anticipate.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `parse_reference_sox_stats_true_peak_measurement` — QUIRKY
- spec 3 · read at `0fc5dc367eb6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:15:13Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Parses raw_peak_db as a float, returning Err(String) on malformed input; adds reporting_uncertainty and analyzer_residual to the parsed peak to derive a conservative (worst-case) bound; constructs and returns a TruePeakMeasurement with id, scope, purpose, verified_silence and the computed conservative peak value.
- found: Handles "-inf" specially, requiring verified_silence to accept it as VerifiedSilence; otherwise rejects unsupported numeric syntax (commas, exponents, leading +, inf, nan) before parsing as DbNano, range-checks to -1000..=+100 dBTP, computes conservative_upper via checked (overflow-safe) addition of reporting_uncertainty and analyzer_residual, and returns a TruePeakMeasurement storing both the raw reported value and the conservative_upper bound plus a raw_json snippet.
- predicted: some · documented: some · derivable: no · legible: most · trap: no
- note: I predicted the conservative-bound arithmetic but missed the -inf/verified-silence branch, the strict syntax rejection, range validation, overflow checking, and that both the raw reported value and the conservative bound are kept as separate fields.

### `parse_reference_true_peak_measurement` — QUIRKY — TANGLED — TRAP
- spec 3 · read at `0b9a6340bcc0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:36:26Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Uses extract_single_loudnorm_report to parse the true-peak value out of raw_json, then applies reporting_uncertainty and analyzer_residual as a conservative safety margin added to the measured peak (so the reported value errs toward "louder"/worse rather than optimistic), and constructs a TruePeakMeasurement carrying id, scope, purpose, and verified_silence; returns Err(String) if the JSON can't be parsed or the expected field is missing/malformed.
- found: Deserializes raw_json into a locally-defined strict struct (deny_unknown_fields) matching ffmpeg loudnorm's exact grammar, then parses input_tp (not output_tp) with hand-rolled numeric-syntax rejection (no exponents/commas/+/-inf/nan), range-checks it to -1000..=100 dBTP, treats literal "-inf" as VerifiedSilence only if verified_silence was independently proven, then computes a conservative_upper by checked-adding reporting_uncertainty and analyzer_residual (erroring on overflow), and returns a TruePeakMeasurement with both the raw 'reported' and the 'conservative_upper' value.
- predicted: some · documented: some · derivable: no · legible: some · trap: yes
- note: Uses input_tp rather than output_tp — a future editor assuming this reports the normalized/output true peak rather than the pre-normalization input peak would silently measure the wrong quantity; nothing in the signature or docstring flags which field is used.

### `build_reference_silence_scan_command`
- spec 3 · read at `a06628383aeb` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:55:19Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand that runs a stats/silence-detection scan on the decoded reference audio to check for signed-zero samples. It branches on the carrier's route: if the decode route is the qualified SoX-ng raw-stream path for float64 W64, it constructs a sox invocation; otherwise it constructs an FFmpeg invocation reading from carrier's path and writing/analyzing to `output`. Returns the assembled command struct rather than executing it, consistent with the "no filesystem or process I/O" module doc.
- found: Branches on carrier's decode mechanism to build either an ffmpeg or sox command decoding to raw f64le output, both outputting to `output`; also sets a locale-clearing environment policy (LC_ALL=C) not predicted.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `validate_signed_zero_f64le`
- spec 3 · read at `74d72c0dd482` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:02:17Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Iterates the byte slice in 8-byte chunks (erroring if length isn't a multiple of 8), parses each chunk as an f64 via from_le_bytes, and checks each value is exactly zero (either +0.0 or -0.0, both allowed since it's "signed zero") — returning Err with a descriptive message (including the offending index/value) on the first NaN, infinite, or non-zero value found.
- found: Errors if the byte length is empty or not a multiple of 8; otherwise checks each 8-byte chunk's raw u64 bit pattern is exactly 0 or 1<<63 (i.e. +0.0 or -0.0), returning a specific error message tying this to loudnorm reporting -inf but the independent scan finding a non-zero/non-finite sample if any chunk fails.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `resolve_reference_deferred_command`
- spec 3 · read at `30c21766b4ee` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:59:28Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Looks up the measurement(s) referenced by the deferred command's measurement id(s) in the provided map, and if found, computes the deferred value (e.g. a gain adjustment derived from a true-peak measurement) and substitutes it into the planned command's arguments, producing a concrete PlannedCommand. Returns an error string if the required measurement is missing from the map.
- found: Iterates each planned arg: literals pass through, BoundGainDb args look up the referenced true-peak measurement, validate its scope/purpose are Plan/GainAuthority, resolve the bound gain via resolve_bound_gain, and render it as a string arg. Builds a PlannedCommand from the resolved args plus the deferred command's tool/input/output/description/environment fields.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `resolve_bound_gain` — QUIRKY — TANGLED
- spec 3 · read at `74a4b48c7630` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:31:16Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Computes the gain to apply by taking the smaller of the policy's target gain and the headroom available before `value` (true peak) would exceed the policy's ceiling, converting the result to DbNano and returning an error if the input measurement or computed gain is invalid (e.g. non-finite or out of representable range).
- found: Extracts requested gain and (ceiling, terminal_bound, may_reduce) from the policy variant, rejecting NormalizePeak outright and validating the terminal bound doesn't exceed the ceiling. For VerifiedSilence it just returns the requested gain unchanged; for a finite true peak it computes max safe gain as ceiling-minus-peak (erroring on overflow), returns the requested gain if it already fits, otherwise reduces to the max safe value only if the policy allows reduction (may_reduce), else errors.
- predicted: some · documented: some · derivable: no · legible: some · trap: no

### `validate_post_final_true_peak` — QUIRKY
- spec 3 · read at `56f709b3d919` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:56:05Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Compares `value` against a true-peak ceiling carried in `policy`, returning Ok(()) if the peak is at or below the allowed ceiling and an Err with a descriptive message if it exceeds it, applicable to qualified gain policy modes.
- found: Skips validation entirely for NormalizePeak policy; otherwise treats VerifiedSilence as fine, and for a finite peak value checks it against a fixed constant ceiling (DbNano::REFERENCE_CEILING, -1 dBTP), erroring with a PlanningError if exceeded.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `validate_reference_streamed_wav_capacity`
- spec 3 · read at `13ec8945f6ce` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:41:42Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Validates that a streamed WAV output (duration possibly unknown/Option) will not exceed capacity limits implied by FinalPcmContract (sample rate, bit depth, channel count -> bytes per second). Computes expected total byte size from duration and contract, compares against a max allowed size (likely u32::MAX or similar RIFF-adjacent limit for streamed data), returning Err if it would overflow; if duration is None, may skip the check or use a permissive/aupplied fallback since streaming doesn't require an upfront size.
- found: Requires duration to be Some (errors otherwise), computes sample frame count from nanoseconds and sample rate with ceiling rounding plus a fixed guard-frame margin, multiplies by channels and fixed bytes-per-sample to get payload size, and errors if it exceeds a max constant, using checked arithmetic throughout to avoid overflow.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `reference_true_peak_measurement_deadline`
- spec 3 · read at `b7b766927114` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:41:23Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Computes source frame count from duration and sample_rate_hz (guarding against missing/zero duration, perhaps erroring or defaulting), multiplies by channels and a fixed 16x oversampling factor to get total oversampled sample count, divides by 1,000,000 rounding up to get number of started blocks, allocates one second per block, adds a fixed process-startup reserve, and returns the total as a Duration wrapped in Result.
- found: Errors if duration is None; computes frame count from duration/sample_rate (rounded up) plus a fixed guard-frame constant, all via checked arithmetic; multiplies by channels and the oversample factor to get workload sample values, erroring if it exceeds a max-admitted-workload constant; divides by a min-throughput constant (rounded up) to get workload seconds, adds a fixed startup-seconds constant, errors if the total exceeds a max-deadline constant, and returns it as a Duration.
- predicted: most · documented: most · derivable: no · legible: most · trap: no

### `validate_reference_riff_capacity`
- spec 3 · read at `1a697e0fc9e5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:01:34Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Computes the estimated total RIFF file size for a planned reference render — audio data bytes derived from duration and the PCM contract (sample rate, channels, bit depth) plus planned_non_audio_upper_bound_bytes for headers/chunks — and returns an error if that total would exceed the classic RIFF/WAV 32-bit (~4GiB) size limit, else Ok(()). If duration is None, it likely skips the check and returns Ok(()).
- found: Requires duration and non-audio-bound to be Some (erroring otherwise), computes bytes-per-sample from bit depth (rejecting Int8/Int32 as invalid terminal depths), computes sample frames from duration*sample_rate with checked arithmetic (rounding up), multiplies by channels and bytes-per-sample for audio_bytes, adds the non-audio bound, and errors if the predicted total exceeds REFERENCE_RIFF_MAX_FILE_BYTES.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `for_source_kind` — QUIRKY
- spec 3 · read at `6ec0b9a40e20` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:29:35Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Constructs a ReferenceScratchPaths by deriving a deterministic namespace/prefix string from the DsdSourceKind (e.g. its label) and joining it under work_dir, then delegates to the same path-building logic as reference_scratch_paths to produce the full set of scratch file paths (render output, measurement, package, etc.) for that source kind.
- found: Maps the source kind to a file extension (dsf/dff/dsd), then builds a fixed struct of work_dir-joined paths (admitted_source + its .tmp variant, canonical_dsd + .tmp, sacd_extracted_source + .tmp, silence_scan) with hardcoded filenames — no delegation to reference_scratch_paths and no generic namespace prefix as I guessed.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `all`
- spec 3 · read at `af9af6b68a56` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:39:42Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Returns a fixed array [&self.field1, &self.field2, ..., &self.field7] listing all 7 path fields of ReferenceScratchPaths in a fixed, stable order, so callers can iterate over every scratch file (e.g. to delete or check existence) without needing to know the individual field names.
- found: Exactly as predicted: returns a fixed array of &Path for all 7 named scratch-path fields (admitted_source, admitted_source_temporary, canonical_dsd, canonical_dsd_temporary, sacd_extracted_source, sacd_extracted_source_temporary, silence_scan) in declared order.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `reference_scratch_paths`
- spec 3 · read at `55dba72f1db3` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:18:04Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Given the PlanRequest, this deterministically constructs a ReferenceScratchPaths struct (the full set of scratch/temp file paths needed for the DSD-to-PCM reference pipeline — e.g. intermediate WAV, measurement output, package output), delegating most of the actual path construction to ReferenceScratchPaths::for_source_kind based on the request's source kind, and returning an error if the request lacks something required to build a valid scratch directory root.
- found: Extracts intermediate_dir and source.dsd_source_kind from the request, erroring with specific codes if either is missing, then delegates to ReferenceScratchPaths::for_source_kind to build the actual path set.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `plan_reference_dsd` — QUIRKY — TANGLED
- spec 3 · read at `cd8b5fbcbd0b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:29:40Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Pure function that takes a PlanRequest and assembles a deterministic ConversionPlan for the "P0 Reference" DSD-to-PCM path: resolves scratch paths, builds the render command (DSD decode), the true-peak measurement step, gain resolution, and the packaging/terminal command(s) (e.g. wavpack), chaining them into an ordered list of plan steps. Returns Err if the request's parameters are invalid or unsupported for this reference policy, since it does no I/O itself — only planning.
- found: Extensive up-front validation (pathway, reference policy, programme scope, source rate/channels/kind, target losslessness, bit depth, flac/wavpack constraints), then builds render/measure/finalize/measure/package steps, computes a semantic plan hash and qualification manifest digest, collects and dedups scratch/cleanup paths, and wraps everything with atomic-rename finalization into a ConversionPlan summary.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `build_reference_render_transcript_fixture`
- spec 3 · read at `27984db6d0ab` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T17:14:25Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A thin wrapper that calls the same internal planning/render-command builder (e.g. build_render_command or plan_reference_dsd) with the given input/output/rate/profile/duration, returning the resulting PlannedCommand unchanged, so test fixtures exercise identical command construction to production even though B6 admission is otherwise blocked before execution.
- found: Exactly a one-line delegation to build_render_command with the same arguments, returning its PlannedCommand.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `build_render_command`
- spec 3 · read at `104961d0732a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:24:07Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand that invokes an external converter (likely sox or ffmpeg) to render the DSD `input` file to PCM at `target_rate_hz`, writing to `output`. It uses `profile` (a ResolvedDsdProfile) to set conversion parameters like filter/dither settings, and if `duration` is Some, adds a trim/length argument to limit the rendered output to that duration.
- found: Constructs a PlannedCommand invoking sox to convert a DSD input to a 64-bit floating-point w64 PCM file, applying a fixed -12dB gain, resampling to target_rate_hz, and conditionally applying a sinc filter based on profile settings; it also forces a cleared/reset environment with LC_ALL=C.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_true_peak_measurement`
- spec 3 · read at `56b4dcee602f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:11:31Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Constructs a PlannedMeasurement describing a true-peak analysis pass: assembles command/args for an oversampling true-peak measurement (input path, sample_rate_hz, channels, oversampled_rate_hz), computes a deadline via reference_true_peak_measurement_deadline based on expected_duration, and tags the result with id, purpose, and route so the orchestrator can dispatch/identify it.
- found: Builds a PlannedMeasurement for true-peak analysis using sox (with oversampled 'rate'+'stats'), routed either directly on the input path or via an ffmpeg producer piping raw f64le PCM into sox over stdin depending on AnalyzerCarrierRoute. Sets environment policy to clear-and-set LC_ALL=C, uses expected_duration as the command timeout, and tags the result with a SoxStatsPkLevDbV1 parser and MeasurementScope::Plan rather than computing any deadline itself (no separate deadline computation, despite the peer function existing).
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The peer reference_true_peak_measurement_deadline is not called here despite being a sibling — deadline logic lives elsewhere; this function only sets expected_duration as a timeout hint.

### `build_terminal_command`
- spec 3 · read at `d12ddab2b207` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:46:16Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Assembles the final conversion command that transforms the DSD-derived intermediate into the final PCM output described by `contract`, applying a gain adjustment derived from `gain_policy` (which may depend on the true-peak measurement identified by `pre_id`). Returns a `PlannedDeferredCommand` — a command whose concrete arguments (e.g. gain value) may be deferred/resolved from measurement results rather than fully known at plan time. Likely constructs args for an external tool like sox, setting input/output paths and format flags per `contract`.
- found: Builds a sox invocation as a PlannedDeferredCommand: maps PCM bit depth to sox encoding/bits flags (rejecting Int8/Int32), sets DSD input and w64 output args, appends either a 'norm' arg (for NormalizePeak policy) or a 'gain' arg with a BoundGainDb placeholder resolved later from the true-peak measurement (pre_id), appends dither flags per contract.dither, and sets a clear-and-set LC_ALL=C environment policy.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `reference_command_environment`
- spec 3 · read at `a1daa88529c9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:25:27Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns a small fixed BTreeMap of environment variables (like LC_ALL=C, TZ=UTC) that should be set when invoking reference DSD-to-PCM tools, to ensure deterministic, locale-independent output for qualification measurements.
- found: Just LC_ALL=C, not TZ=UTC as I guessed — matches the general "locale determinism" idea but with only one of the two entries I predicted.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_float64_wav_package_pipeline` — QUIRKY
- spec 3 · read at `8bb192877dfe` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:30:46Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Assembles a PlannedCommandPipeline for producing a float64 WAV reference package: calls build_render_command to decode DSD to float64 PCM at the input path, adds a true-peak measurement step via build_true_peak_measurement, then a packaging step via build_package_command/build_package_step to wrap the result into the final WAV container at output according to the ResolvedOutputTarget and FinalPcmContract, returning the ordered pipeline of commands.
- found: Validates target is WavRiff/WavRf64 (else errors), then builds a two-command pipeline directly inline: a SoX producer streaming raw float64 PCM from the DSD input to stdout, piped into an FFmpeg consumer that packages it into a WAV/RF64 container at output with no metadata/other streams and no resampling; no measurement step and no use of the build_render_command/build_package_command helper peers I guessed.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: Despite the many builder-named peers in the file, this function hand-builds the sox/ffmpeg commands inline rather than delegating to build_render_command/build_package_command, and includes a target-type validation guard not suggested by the signature.

### `build_package_command`
- spec 3 · read at `48602db0fe46` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:15:34Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Matches on the ResolvedOutputTarget variant (FLAC/WAV/WavPack/etc.) to select the packaging tool and construct its command-line arguments for encoding the rendered PCM input into the final container at output, validating that the FinalPcmContract (bit depth/sample rate/channels) is compatible with that target and returning Err if not. It likely applies settings like WavPack compression level and pulls in a reference command environment, returning a PlannedCommand ready for execution.
- found: Builds an ffmpeg PlannedCommand: derives a raw PCM codec string from contract.bit_depth (rejecting Float64/Int8/Int32), assembles a base ffmpeg arg list (strip metadata/video/subs), then matches on ResolvedOutputTarget to append per-container args (WAV/RF64, FLAC with compression level, AIFF with big-endian codec + depth validation, WavPack with an Int24 bits_per_raw_sample workaround comment and compression level, ALAC/M4A), explicitly errors for WavW64 (must not be packaged this way) and any other unmatched target, then sets a clear-and-set environment policy with reference_command_environment().
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `build_package_step`
- spec 3 · read at `ecd250657347` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:53:56Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs the packaging command (e.g. via build_package_command) that turns the rendered PCM output into the final packaged file (WavPack or similar) according to the ResolvedOutputTarget and FinalPcmContract, then wraps it into a PlannedExecutionStep with the input/output paths and the settings-derived environment/compression options — purely building a plan struct, no I/O.
- found: Branches on bit depth: for Float64 it builds a multi-step pipeline via build_float64_wav_package_pipeline wrapped as PlannedExecutionStep::Pipeline; otherwise it builds a single command via build_package_command wrapped as PlannedExecutionStep::Command.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `wavpack_compression_level_value`
- spec 3 · read at `02b4fb4a4ffb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T17:04:21Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This matches on the WavPackMode enum variants and returns a numeric compression level (u8) corresponding to each mode, e.g. Fast->1, Normal->2, High->3, VeryHigh/Extra->4-8, likely mapping to WavPack's -c/-cc/-hh compression flags. It's a straightforward lookup/dispatch function with no side effects, complementing wavpack_compression_level (which probably returns a string name or CLI flag).
- found: Matches the 4-variant WavPackMode enum (Fast, Normal, High, VeryHigh) to a 0-3 index, presumably an ordinal encoding rather than an actual WavPack CLI compression flag value.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `wavpack_compression_level` — QUIRKY
- spec 3 · read at `fa747d4ffdee` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:30:28Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A match over WavPackMode variants that returns the corresponding wavpack CLI compression-level flag as a String (e.g. "-f" for fast, "-h" for high, "-hh" for very high, "-x"/"-xx" for extra), used to build the wavpack encoder command line arguments.
- found: Just delegates to wavpack_compression_level_value(mode).to_string() — a String-owning wrapper around a presumably &'static str-returning sibling function, not the match logic itself.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `qualification_manifest_digest`
- spec 3 · read at `9d4cedc33dc3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:56:49Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Computes a SHA256 digest over a fixed, hardcoded byte string or constant that represents the canonical v16 qualification artifact schema/content, and returns it as a Sha256Digest. No dynamic input — it's a pure constant-hashing function used to detect schema drift.
- found: Hashes the raw bytes of a source-controlled JSON file (dsd_reference_sox_ng_14_8_0_1_v16.json) embedded at compile time via include_bytes!/concat!/env!, rather than hashing a literal in-code constant.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `semantic_plan_hash`
- spec 3 · read at `84321470eb3e` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T10:01:35Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs a canonical, version-aware representation of the plan by normalizing each PlannedExecutionStep (dispatching to normalize_step_for_hash_legacy/v4/v15 based on policy), plus normalized source rate, channels, target, profile, final PCM contract, gain policy, and front-end, then hashes the combined normalized bytes with SHA-256 to produce a deterministic Sha256Digest fingerprint of the plan's semantic content, independent of incidental details like absolute paths.
- found: Builds a versioned plain-text representation of the plan (header fields plus policy-selected per-step normalization function, with extra "identity" marker lines appended for newer policy versions), then hashes the text as UTF-8 bytes with SHA-256 to get a deterministic Sha256Digest.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `normalize_step_for_hash_legacy` — QUIRKY
- spec 3 · read at `359c211c4c1b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:36:13Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds a canonical string representation of a PlannedExecutionStep (command, args, environment, etc.) in the original legacy format used for the v1-v3 semantic-hash byte contract, so that old plans/evidence hash the same way they always did. It likely differs from normalize_step_for_hash_v4/v15 by omitting newer fields or using older formatting/ordering, and is kept frozen (decode-only) even as newer versions evolve.
- found: Matches on the PlannedExecutionStep variant (Command, Pipeline, Measurement, DeferredCommand) and builds a distinct delimited canonical string per variant using tool program names, normalized args/paths/environment, and for DeferredCommand a special {BOUND_GAIN:...} token for gain-bound args — the frozen v1-v3 hash serialization.
- predicted: some · documented: some · derivable: no · legible: most · trap: no

### `normalize_step_for_hash_v4`
- spec 3 · read at `418de1c164e7` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:24:34Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a canonical string representation of a PlannedExecutionStep (command, args, environment, input/output) for use in a stable hash, following the "v4" serialization scheme — one of several versioned normalization formats kept alongside normalize_step_for_hash_legacy and normalize_step_for_hash_v15 so that hashes computed under old pipeline versions remain reproducible. It likely concatenates normalized fields (via normalize_args, normalize_environment, normalize_input_source, normalize_output_sink, etc.) with fixed separators/labels particular to v4's exact format.
- found: Matches on the PlannedExecutionStep variant (Command, Pipeline, Measurement, DeferredCommand) and builds a version-4-specific canonical colon-delimited string for each, using normalize_args/normalize_input_source/normalize_output_sink/normalize_environment(_policy) helpers; DeferredCommand additionally normalizes bound-gain placeholder args and joins them with a unit separator character.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `normalize_step_for_hash_v15` — QUIRKY
- spec 3 · read at `55d977d1b056` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:58:26Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a canonical, version-tagged (v15) string representation of a PlannedExecutionStep for hashing purposes, by calling the various normalize_* helpers (args, environment, input source, output sink, expected duration, path tokens) and concatenating/joining their normalized outputs deterministically, so that semantically-equivalent execution steps produce the same hash input string regardless of incidental differences like path formatting or ordering.
- found: Thin wrapper over normalize_step_for_hash_v4: it appends a "deadline" component (derived from expected_duration, varying by step variant — Command, Pipeline's producer/consumer, Measurement's input_stage/command, or "not_applicable" for DeferredCommand) onto the v4 normalized string, versioning the hash to now be duration/deadline-sensitive.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: The v15/v4 naming implies successive hash-format versions layered by delegation rather than independent reimplementations — worth knowing before adding a v16 that also needs to wrap v15, not reinvent normalization.

### `normalize_expected_duration`
- spec 3 · read at `96d7f4db5e30` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:11:01Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Converts an Option<Duration> into a canonical string representation for use in hashing/normalization: returns something like "none" for None, and a deterministic numeric string (e.g. total nanoseconds or seconds) for Some(duration), so that plan hashing is stable regardless of Duration's internal representation.
- found: Returns "none" for None, or "{secs}.{nanos:09}" formatted string for Some(duration), giving a deterministic canonical string for hashing.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `normalize_environment_policy`
- spec 3 · read at `0c93a4d8d8a6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:49:51Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A match over the CommandEnvironmentPolicy enum variants mapping each to a fixed &'static str label (e.g. "inherit", "clean", "minimal"), used by the normalize_step_for_hash_* functions to produce a deterministic string representation of the policy for the semantic plan hash.
- found: Exactly the predicted match, mapping the two-variant CommandEnvironmentPolicy enum (InheritAndSet, ClearAndSet) to fixed snake_case string labels, presumably for deterministic hash normalization.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `normalize_input_source`
- spec 3 · read at `0e00f5271995` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:13:51Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Part of the family of normalize_* helpers used to build a stable, hashable representation of a conversion plan. Given an InputSource enum, it produces a short, canonical string describing the input (e.g. its variant tag plus a normalized path token), used as an ingredient in semantic_plan_hash so that equivalent inputs hash identically regardless of incidental differences like absolute vs relative paths.
- found: Matches InputSource: Path variant formats as "path:" + normalize_path_token(path), Stdin variant is just the literal string "stdin".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `normalize_output_sink`
- spec 3 · read at `1f1be8b40505` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:18:56Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Converts an OutputSink enum value into a canonical string used as part of hash input for plan/step hashing — matches on sink variants and normalizes any path component via normalize_path_token so the resulting hash is stable across platforms/environments.
- found: Matches OutputSink::Path/Stdout/InPlace, formatting Path and InPlace as "tag:normalized_path_token(display)" and Stdout as the literal string "stdout".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `normalize_environment`
- spec 3 · read at `cf76ac15b95c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:00:26Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Given the BTreeMap (already sorted by key), it builds a deterministic canonical string representation for hashing purposes — likely joining key=value pairs with a delimiter like \n or ; into a single String, relying on BTreeMap's sorted iteration order for determinism.
- found: Joins key=value pairs (sorted via BTreeMap) with the unit-separator control character \u{1f} instead of a printable delimiter.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Delimiter choice was the unit separator control char, not a common printable one — small but exact detail my prediction only guessed generically.

### `normalize_args`
- spec 3 · read at `dd8e8a1ef1c5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:00:55Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Joins the given command-line argument strings into a single canonical string for hashing purposes (e.g. a semantic plan hash), likely using a delimiter unlikely to appear in real args (like a control character) so the joined form round-trips unambiguously, without altering the individual argument contents themselves.
- found: Maps each arg through normalize_path_token (to canonicalize path-like content) then joins them with a U+001F unit-separator control character, for deterministic hashing.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I predicted the delimiter approach correctly but missed the per-arg normalize_path_token step.

### `normalize_path_token` — QUIRKY
- spec 3 · read at `139a8fce2ed4` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:51:56Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Normalizes a path-like string token for stable hashing across platforms/runs — likely converts backslashes to forward slashes, trims whitespace, and possibly collapses redundant separators, so equivalent paths produce identical hash inputs regardless of OS or formatting.
- found: Replaces absolute paths or paths containing the temp-file marker `.tonepoet-` with a placeholder token (`{PATH}` or `{PATH:ext}` preserving the extension), leaving other (relative) values unchanged — used to keep hashes stable across machine-specific or temp-file paths rather than normalizing path formatting.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `decode_contract`
- spec 3 · read at `e27e4f9b1611` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:31:29Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Takes a PcmBitDepth enum value and returns a FinalPcmContract struct describing the canonical decode target for that bit depth — likely a match/switch on the bit depth variant producing appropriate format fields (sample format, byte order, maybe codec/container requirements) for that depth, as a pure lookup/construction function with no I/O.
- found: Constructs a FinalPcmContract with fixed sample_rate_hz=176400 and channels=2, deriving sample_kind from the bit_depth argument, and setting dither to TPDF only when bit_depth is Int24 (otherwise None).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `v7_decode_route_table_is_complete_unique_and_depth_native` — QUIRKY
- spec 3 · read at `36beafdb95b0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T13:10:07Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A large exhaustive test that iterates over every combination of relevant input dimensions (e.g. sample format/class, container, package identity) for the v7 DSD decode route table, asserting for each combination that exactly one decode route/contract is selected (completeness and uniqueness — no missing or ambiguous combos), and that the chosen route's bit depth matches the source's native depth rather than being coerced to some fixed depth.
- found: Collects (role_class, bit_depth, mechanism, hash_encoding) tuples from the static REFERENCE_DECODE_ROUTE_RULES table into a BTreeSet and compares it against a large hand-written expected set (uniqueness/completeness via set equality plus a length check against the raw rule list), then separately asserts the ffmpeg codec string mapping for each hash encoding and the canonical hash format string constant.
- predicted: some · documented: some · derivable: no · legible: most · trap: no

### `v7_decode_authority_rejects_invalid_pcm_contracts` — QUIRKY
- spec 3 · read at `7137780a8f1c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:03:29Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs one or more invalid PCM decode-contract inputs (e.g. bad bit-depth/rate/format combinations not present in the v7 decode route table) and asserts that decode_contract (or the relevant authority function) returns an error/None for each, verifying the route table's validation rejects contracts outside its supported matrix rather than silently accepting or panicking.
- found: Builds a valid Int24 decode_contract, mutates dither to None (invalid for that depth), asserts reference_decode_authority errors; then builds a valid Float32 contract, mutates channels to 0, asserts it also errors — checking two independent invalidation rules rather than a route-table lookup.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `v7_float64_w64_direct_ffmpeg_route_is_rejected`
- spec 3 · read at `59b1da6a2f09` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:35:12Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Test asserts that requesting a DSD decode route to float64 PCM in a W64 container directly via ffmpeg is rejected by the decode route table/authority, since W64 float64 isn't a valid depth-native ffmpeg target per policy — likely checking that decode_contract or the route table returns an error/None for this combination.
- found: Iterates over several ReferenceDecodedSampleRole variants (reconstruction, terminal QPCM, packaged/post-metadata WAV W64 outputs) for a Float64 contract, asserts the authorized mechanism is SoxFloat64W64RawStream for each, then asserts that validating DirectFfmpeg as the mechanism fails with an error message referencing 'sox_f64le_raw_stream'.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `reference_silence_scan_obeys_the_decode_route_table`
- spec 3 · read at `c48612e00907` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T11:44:48Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test that iterates over every entry in the v7 decode route table (varying PCM bit depth / output target combinations) and asserts that the reference silence-scan step planned for each route is consistent with that route's decode contract — e.g. it targets the correct intermediate file/carrier and uses parameters appropriate to that route, rather than being hardcoded to one route.
- found: Iterates Int24/Float32/Float64 route table entries, plans a reference W64 decode, gets the terminal QPCM carrier, and asserts the built silence-scan command uses the expected tool (ffmpeg vs sox) with exact argv and environment policy for that mechanism; then separately checks the ReconstructionR64 carrier also produces a correct sox silence-scan command.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `v7_carrier_binding_rejects_qpcm_path_with_riff_package_identity` — QUIRKY
- spec 3 · read at `b6131fc5dbe0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:10:31Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Test constructs a v7 decode contract/route where the sample carrier is QPCM (a raw, non-RIFF path) but the package identity is set/tagged as RIFF, then invokes the carrier-binding validation function and asserts it returns an error (rejects), since a QPCM carrier paired with a RIFF package identity is an invalid/contradictory combination that the fail-closed policy must not allow through.
- found: Plans a reference DSD-to-RIFF conversion, then exercises bind_decoded_carrier/decoded_carrier across three carrier selectors (PackagedOutput, TerminalQpcm, PostMetadataOutput): asserts binding a selector to the wrong path errors with 'carrier path mismatch' (tried for both PackagedOutput given the QPCM path, and PostMetadataOutput given the packaged path), and asserts each selector's correct planner-owned path/decode-mechanism when queried properly.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: I predicted only the single QPCM/RIFF mismatch case; the test actually validates a 3-way carrier-selector binding API (correct-path lookups plus two distinct mismatch rejections) that isn't guessable from the name alone.

### `v7_role_authority_selects_independent_float64_package_routes` — QUIRKY
- spec 3 · read at `9b791adbd416` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:20:40Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test that builds decode contracts/requests for multiple roles (e.g. reference vs candidate) with float64 output for v7, resolves each role's package route, and asserts the routes are independent/distinct per role rather than sharing a single route — likely via assert_ne! or comparing route/package identities.
- found: Resolves reference_decode_authority for three roles (terminal QPCM W64, packaged RIFF, packaged RF64) at Float64 bit depth, and asserts each gets its correct specific decode mechanism (Sox raw stream for the terminal role vs DirectFfmpeg for both packaged outputs) and correct hash encoding — not a generic pairwise-inequality check but concrete expected values per role.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `db_nano_is_canonical_and_strict` — OBSCURE
- spec 3 · read at `0518de776d90` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:32:24Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Unit test verifying the DbNano fixed-point decibel type has a single canonical representation and strict comparison: e.g. that constructing/normalizing values collapses equivalent representations (like positive/negative zero) to one canonical form, and that values differing by even the smallest nano unit compare as distinct/not-equal.
- found: Unit test for DbNano's FromStr parser: verifies a parsed value renders back to fixed 9-decimal-place canonical text, that a '+' prefixed nano-precision value parses to the expected integer nano representation, and that scientific notation, over-precise (10-decimal) input, and comma-formatted input are all rejected as parse errors (strict decimal grammar only).
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: Nothing in the docs or naming hints that 'canonical and strict' refers to the string parsing grammar rather than numeric value equality/normalization.

### `db_nano_round_trips_the_complete_i64_domain`
- spec 3 · read at `57e78b513a89` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:26:08Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test verifying that a nanodecibel value stored as i64 round-trips correctly through some conversion (e.g., i64 -> f64 dB -> i64, or into/from a wrapper type), checking boundary values like i64::MIN, i64::MAX, 0, and a few others rather than literally exhaustively iterating the whole domain.
- found: Tests DbNano(i64) render/parse round-trip at MIN and MAX boundaries, asserts exact string renderings, and checks that values just beyond the representable range fail to parse.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `db_nano_serde_round_trips_the_complete_i64_domain`
- spec 3 · read at `89e0cc3687ff` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:32:00Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A #[test] that checks a DbNano-style newtype wrapping i64 round-trips correctly through serde (e.g. serialize to JSON then deserialize back) for a set of boundary/representative i64 values (i64::MIN, i64::MAX, 0, -1, 1, etc.), asserting the deserialized value equals the original for each.
- found: Test that round-trips DbNano(i64::MIN) and DbNano(i64::MAX) through serde_json to_string/from_str and asserts equality; "complete i64 domain" refers to covering both extremes, not an exhaustive/broader sample as I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `loudnorm_json`
- spec 3 · read at `32f607d82087` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:48:10Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Test helper that builds a synthetic ffmpeg loudnorm-filter JSON string, substituting input_tp and output_tp into the true-peak fields while other loudnorm measurement fields (input_i, output_i, etc.) are filled with fixed placeholder values, for use in tests exercising loudnorm JSON parsing.
- found: Builds a synthetic ffmpeg loudnorm JSON string with input_tp/output_tp interpolated and other fields fixed placeholders, for tests.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: file_doc describes the module's DSD-to-PCM policy scope, not this small JSON test fixture builder.

### `shared_true_peak_authority_is_strict_and_uses_input_tp` — QUIRKY — TANGLED
- spec 3 · read at `32392ed1ee54` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:25:40Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Unit test verifying that the shared true-peak "authority" logic used across policy variants is strict — i.e. it fails closed / rejects plans lacking valid true-peak data — and specifically that it takes the true-peak value from the input measurement rather than recomputing or defaulting it, likely asserting equality against a fixed input_tp value and rejection on malformed/missing cases.
- found: Test covering multiple facets: extract_single_loudnorm_report parsing/dedup/error cases, parse_reference_true_peak_measurement rejecting duplicate JSON keys, correctly parsing input_tp into a reported value plus a conservative_upper bound, handling -inf/silence readings (error unless a verified-silence flag is set), and validate_signed_zero_f64le byte-pattern checks.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `v14_sox_stats_authority_is_strict_and_conservative` — QUIRKY — TANGLED
- spec 3 · read at `2b8e8ed20051` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:40:47Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test that feeds a "v14" sox-stats-based authority/policy function a handful of borderline or malformed stats inputs (missing fields, out-of-range values, ambiguous cases) and asserts it rejects/fails closed on all of them, plus one clearly valid case that it accepts — demonstrating the authority errs toward rejection rather than leniency.
- found: Tests two functions: extract_single_sox_stats_peak_report (parses sox stderr for a single "Pk lev dB" line by column index, erroring on missing/duplicate/malformed lines) and parse_reference_sox_stats_true_peak_measurement (converts the extracted string into a TruePeakValue with a conservative upper bound, erroring on "-inf" unless an explicit verified-silence flag is set).
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `policy_ids_are_append_only_and_stably_serialized`
- spec 3 · read at `f53b8e380859` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T13:58:46Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test asserting that each known policy ID variant has a fixed, hardcoded serialized representation (string or number) that must never change once assigned, and that the full set of policy IDs only grows over time (no removals/renumbering) — likely iterating over an enum and comparing against a frozen expected list/mapping, failing loudly if someone alters an existing ID's serialization or removes one, to guard against breaking historical qualification records.
- found: Explicit assert_eq pairs for every DsdReferencePolicyVersion variant (V1..V16), checking serde_json serialization to a fixed string and deserialization back from that string, so any future variant renumbering/renaming breaks the test.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `corrected_profile_centers_are_frozen`
- spec 3 · read at `d1815d1471fd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:35:31Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A pinning/regression test that asserts specific numeric "corrected profile center" constants (likely dB or frequency values used in the DSD-to-PCM correction policy) equal exact hardcoded literals, so that any future change to these calibration values fails the test and must be a deliberate, reviewed change rather than an accidental drift.
- found: Pins the sinc filter transition-band (center, rolloff) frequency pairs returned by resolve_reference_profile for several combinations of DsdRate, output sample rate, and DsdReconstructionSelection (Reference/Wideband), asserting exact hardcoded frequency tuples so these calibration values can't silently drift.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `unsupported_matrix_cells_fail_closed`
- spec 3 · read at `8215c76eee3c` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:12:48Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test that iterates over the set of known-unsupported request/format combinations in the depth/format matrix and asserts each one is rejected by the planner (returns an Err/rejection variant) rather than silently succeeding, verifying a fail-closed policy for unsupported capability cells.
- found: Two hardcoded calls to resolve_reference_profile for specific unsupported DSD rate/target-rate/selection combos; first asserts the exact error string, second just asserts is_err(), rather than looping over the whole matrix.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `reference_request`
- spec 3 · read at `c6801e3c0834` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:34:40Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture helper (given the surrounding peers are all test names) that builds and returns a PlanRequest struct, populating its source_rate, target_rate_hz, target, depth, and profile fields from the parameters, and filling in other required PlanRequest fields with fixed/default values (e.g. no manual override, default policy) so tests can construct minimal requests concisely.
- found: Test fixture helper that maps the ResolvedOutputTarget to an AudioFormat/file-extension pair, builds PipelineSettings from defaults with DSD native_v2 and the given rate/depth/profile, then constructs a full PlanRequest with a synthetic DFF SourceInfo (2ch, 60s, DSD) and fixed paths, including a RIFF-specific non-audio upper bound quirk.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `deferred_binding_uses_the_planner_step_and_historical_policies_cannot_execute_as_v4`
- spec 3 · read at `0e1ecfff61ea` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:21:55Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Tests that a deferred-binding policy resolves to using the planner's chosen step reference rather than eagerly picking a concrete policy, and that older/historical policy versions cannot be run through the current v4 execution path, likely asserting an error or rejection when attempting to do so.
- found: Plans a reference DSD conversion, finds its one DeferredCommand step, resolves it against a true-peak measurement, and asserts the resolved gain arg matches the expected computed value. Then, for every historical (pre-v4-equivalent) reference policy version, it swaps the policy into the request and asserts planning fails with error code DSD-REF-P0-015, confirming old policies cannot produce a plan under the current scheme.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `planner_rejection_precedence_is_cartesian_and_manual_always_wins`
- spec 3 · read at `fbc36e195d4d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:16:15Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Iterates the cartesian product of multiple independent rejection reasons (e.g. manual override, policy rejection, programme rejection, source-admission rejection) across combinations, calling the planning function for each combination and asserting that whichever reason is "highest precedence" is the one reported — with manual rejection always winning over every other combination regardless of which other reasons are also present. Probably a nested loop building request variants and checking the returned rejection reason/enum against an expected precedence ordering.
- found: Nested loop over the cartesian product of source kind, sample rate, channel count, programme scope, policy version, output target, and bit depth (thousands of combos); for every combination it sets the pathway to Manual and asserts plan_reference_dsd always returns the exact same "ManualUnavailable" error string, proving manual pathway is unconditionally rejected regardless of any other setting — 'manual always wins' meaning it always wins as the reported rejection reason, not that it overrides other independently-triggered rejections.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The name suggests competing rejection reasons being arbitrated by precedence, but there's only ever one rejection path exercised (ManualUnavailable) — the cartesian sweep is there to prove that error is invariant across every other setting, not to test precedence among distinct rejection causes.

### `public_plan_entrypoint_preserves_manual_and_policy_precedence` — QUIRKY
- spec 3 · read at `ce0017bf36b1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:56:30Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test on the public plan entrypoint verifying precedence ordering: when a manual override is specified it always wins regardless of policy, and when no manual override exists, policy decisions take precedence over programme/source-admission defaults - likely calling the entrypoint with combinations of manual/policy/programme inputs and asserting the expected value wins in each case.
- found: Builds a reference request with a DsdiffDst source under continuous-image programme scope; with pathway=Manual asserts plan_conversion fails with a "pathway...ManualUnavailable" error, then switches pathway=Reference and asserts it instead fails with a "reference_policy...Toolchain" error — checking which specific validation error surfaces depends on the pathway setting.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `policy_precedes_programme_and_source_admission` — QUIRKY
- spec 3 · read at `f0b9c116eebe` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:43:01Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Constructs a planning request where both the policy is invalid/unsupported AND the programme content or source admission would also fail, then asserts the returned rejection is the policy error specifically, demonstrating that policy validation runs before (and short-circuits) programme and source-admission checks in the planner's precedence ordering.
- found: Sets an invalid programme scope and source kind combo (which would fail admission) alongside each of a dozen legacy/toolchain-unsupported reference policy versions, and asserts plan_reference_dsd errors with the policy/toolchain error every time, showing the policy check fires before programme/source admission errors would.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `predictive_dst_without_independent_oracle_is_rejected_outside_dsd64_stereo`
- spec 3 · read at `1b1623973b39` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:27:24Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A pure planning test that iterates over combinations of DSD rate (dsd64, dsd128, ...) and channel layout (stereo, multichannel), building a reference_request for a predictive-DST source with no independent oracle attested. It asserts that planning rejects the request with a specific policy error for every combination except dsd64+stereo, where the request is expected to be accepted, confirming that only the dsd64 stereo case is exempt from requiring an independent oracle for predictive DST.
- found: Loops over dsd64/128/256 x mono/stereo combos (all except dsd64 stereo) with a DsdiffDst source kind, asserting plan_reference_dsd errors with a specific CompressedDstRateUnqualified message for each; then separately asserts dsd64 stereo alone succeeds — matching the predicted exemption structure closely, just with the exact rejection reason/error code being new information.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sacd_front_ends_remain_unavailable_until_production_path_fixtures_exist`
- spec 3 · read at `7cfd35a88e9d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:44:29Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A pure test that constructs a reference/planning request for a SACD-sourced input (ISO or DSD from SACD) and asserts that planning it currently fails or is rejected with an explicit "unavailable"/unsupported-source error, documenting that SACD support is intentionally gated until real production-path fixtures are added, rather than silently succeeding with fake data.
- found: Exhaustively loops over SacdFrameEncoding x DsdRate x channel-count combinations, builds a reference request with dsd_source_kind = SacdTrack, and asserts plan_reference_dsd errors with the exact SacdFrontEndIntegrationUnqualified error text for every combination.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `int16_is_rejected_until_a_conservative_shibata_bound_is_derived`
- spec 3 · read at `847c8d5bc961` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T21:11:03Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a plan/reference request targeting int16 output depth with Shibata noise shaping, calls the planning function, and asserts it fails closed with an error (since no conservative bound for that combination has been derived/qualified yet), likely checking the error message names int16 and/or Shibata specifically rather than silently succeeding or falling back.
- found: Loops over every supported output container/format target and asserts that requesting Int16 depth via plan_reference_dsd always fails with the same specific "Int16TerminalUnqualified" error code, regardless of container.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: Error code is Int16TerminalUnqualified, not a Shibata-specific message — the shibata connection is only in the test's name/intent, not the error text.

### `complete_reference_rate_matrix_is_pinned`
- spec 3 · read at `9aabea9bcf26` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:15:20Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a Rust unit test that iterates over the complete matrix of supported DSD input rates (and possibly target PCM depths/profiles) and asserts that each combination maps to a specific, hardcoded/expected PCM output rate. It's a "pinning" test to catch any unintended drift in the reference rate matrix, likely failing loudly (panic/assert) if any cell is missing or a mapped value changes unexpectedly.
- found: A test iterating over DSD source rates (64/128/256/512/1024) x a list of PCM target rates, asserting resolve_reference_profile succeeds or fails per source/target combination (DSD64 always succeeds, DSD128/256 fail for 88.2k/96k, DSD512/1024 always fail), rather than pinning exact output values.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `complete_wideband_matrix_is_pinned`
- spec 3 · read at `ff9ce07d0268` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:31:12Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the full cartesian set of relevant plan-input combinations for the "wideband" measurement/filter dimension, calls the planning/policy function for each, and asserts the resulting decision matches a pinned/hardcoded expected value for every cell — a regression test to catch any unintended change in wideband handling across the matrix.
- found: Checks resolve_reference_profile for DSD128 wideband reconstruction: low target rates (44.1/48/88.2/96k) error out, high target rates (176.4k and above) succeed as B4W profile, and non-DSD128 source rates at 352.8k target error out for wideband selection.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `complete_target_depth_matrix_is_pinned`
- spec 3 · read at `14a05fff49bd` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T22:51:24Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A golden-matrix test that iterates every supported target bit depth (e.g. int16, int24, float32/64) and asserts, for each, the exact accept/reject outcome (and if accepted, the resolved policy/gain behavior) from planning a DSD-to-PCM reference request — pinning the current policy table so any future change to which depths are permitted shows up as a diff here rather than silently.
- found: Cartesian-product golden test over 7 output targets x 4 PCM bit depths, asserting validate_reference_target_depth's ok/err matches a hardcoded should_succeed rule (int24 always ok, int16 never, float32/64 only for the three WAV variants), plus separate assertions that resolve_reference_depth rejects Int8/Int32 and resolves BitDepthTarget::Source to Int24.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `v15_oversampled_measurement_routes_deadlines_and_hash_identity_are_frozen` — QUIRKY — TANGLED
- spec 3 · read at `94c0be64bfb3` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:35:48Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A pinning/regression test that iterates over the v15 oversampled DSD measurement scenarios (combinations of rate/profile), computing for each the chosen measurement route, its deadline value, and a hash used for identity, then asserts each equals a hardcoded expected value. This guards against silent changes to routing, deadlines, or hash computation when the policy code is edited, without doing any real I/O since the module is pure.
- found: Plans a DSD64/88200/WavW64/Float64 reference request and asserts exact deadline (290s), exact SoX measurement command/args/env for both measurements, then proves the hash-normalization function is sensitive to rate, parser, transport, environment, and deadline changes by mutating a clone and asserting the normalized hash differs. Then repeats for a Float32 variant checking the FFmpeg producer stage feeding into the SoX stats measurement, again pinning exact args/env/deadline.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: file_doc describes the whole module's purpose (pure planning, no I/O), not this specific test.

### `v9_float64_riff_and_rf64_use_typed_streamed_packaging` — QUIRKY — TANGLED
- spec 3 · read at `9f701cee1de9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:46:23Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A pure-planning test asserting that for the "v9" profile (float64 PCM output) targeting both RIFF and RF64 WAV containers, the planner selects a "typed streamed packaging" strategy variant rather than a generic/untyped one, checking the plan for both container types produces the expected packaging enum/field value.
- found: For float64 PCM targeting WavRiff/WavRf64, the planner builds a sox-producer|ffmpeg-consumer piped execution step (raw f64le over stdin/stdout) with specific CLI args, a ClearAndSet LC_ALL=C environment policy, an -rf64 always flag only for RF64, and confirms the hash-normalization function is sensitive to environment_policy changes.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `every_supported_profile_renders_the_corrected_frequency_argument` — QUIRKY
- spec 3 · read at `d784d4192d30` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:06:38Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Test iterates over every supported profile (likely SSRC or resampling profiles across DSD rates), builds a conversion plan/argv for each, and asserts the rendered frequency argument reflects a "corrected" value (some adjustment like rounding/filter correction) rather than the naive nominal target rate, verifying correction logic applies uniformly across the whole profile matrix.
- found: Table-driven test over (source DSD rate, target PCM rate, reconstruction selection, expected sinc filter params) tuples: plans a reference DSD conversion, extracts the first SoX render command, and asserts the rendered args either omit a "sinc" filter (None case) or include a specific "sinc -a 180 -L -t <transition> -<center>" argument tail matching the expected corrected transition/center frequency for that profile.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `dynamic_policy_errors_name_the_exact_source_target_depth_and_gain_mode`
- spec 3 · read at `d264e52262e4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:31:57Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a test that constructs one or more invalid/rejected policy combinations (e.g. mismatched source format, target bit depth, gain mode) and asserts that the resulting error's message/string explicitly names the exact source, target depth, and gain mode involved, rather than a generic error. It's part of a suite of descriptively-named tests documenting DSD-to-PCM policy invariants.
- found: A test with three assertions checking exact error-message strings for three distinct invalid policy scenarios: DSD256 at 96kHz sample rate not qualified, FLAC-native target with Float32 depth not supported, and native-level/fixed gain modes that can't satisfy the -1dBTP true-peak ceiling.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Peers are all similarly named exhaustive assertion tests documenting specific error codes (DSD-REF-P0-xxx) and exact wording; useful as a spec but brittle to wording changes.

### `streamed_wav_capacity_is_fail_closed_and_boundary_exact`
- spec 3 · read at `9486cf0c24a9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:08:48Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Test asserting a streamed WAV capacity check is boundary-exact and fail-closed: it computes the maximum representable sample count/size for streamed WAV output, verifies the check passes exactly at that boundary, and verifies it rejects (returns an error/false) one unit beyond it, likely iterating over a few depth/rate combinations.
- found: Tests validate_reference_streamed_wav_capacity at the exact largest admitted duration (passes), one second beyond it (rejects with StreamedWavCapacity error), a None duration (also rejects — fail-closed on missing duration), and an overflow contract with u32::MAX sample rate/u16::MAX channels plus Duration::MAX (rejects, no panic/overflow).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `true_peak_deadline_is_workload_derived_and_bounded_by_admission` — QUIRKY
- spec 3 · read at `8e00665967ba` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:53:15Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A unit test that computes the true-peak measurement deadline for varying workload sizes (e.g. sample counts or duration), asserting the deadline scales with the workload (larger workload → larger/no-smaller deadline) while never exceeding some fixed admission-control ceiling constant, confirming the deadline formula is both workload-derived and clamped.
- found: Verifies bound-constant arithmetic invariants, checks the deadline function returns an exact expected duration for an ordinary case, checks the deadline caps at REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS for the largest admitted workload size, and checks a None duration input errors with a specific StreamedWavCapacity error message.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `streamed_wav_capacity_applies_to_every_terminal_depth_and_delivery_container` — QUIRKY
- spec 3 · read at `8c7aa921130b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:12:09Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Iterates over every combination of terminal bit depth (16/24/32-bit int, float32/64) and delivery container (WAV/RF64/W64/etc.) in the policy matrix, and asserts that the "streamed WAV capacity" rule (a RIFF/streaming size limit or fail-closed capacity check) applies consistently across all of them — i.e., no depth/container combination silently skips the capacity check. It's a pinning/regression test asserting universality rather than checking specific numeric values.
- found: For three specific (target, depth) combos (FlacNative/Int24, WavRf64/Float32, WavW64/Float64), builds a reference DSD64 request and checks that a 5-minute source duration plans successfully (must be under cap) while a 6-minute duration is rejected with the exact StreamedWavCapacity error message — proving the streaming capacity boundary is enforced at the same duration cutoff regardless of target container/depth.
- predicted: some · documented: none · derivable: no · legible: most · trap: no

### `riff_capacity_requires_a_complete_non_audio_plan_bound`
- spec 3 · read at `4bf0c54cb797` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:54:28Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Unit test verifying fail-closed behavior: calls the RIFF capacity-checking function with a plan bound where the non-audio (metadata/header) portion is incomplete or unknown, and asserts it returns an error rather than silently computing a capacity — because an incomplete non-audio bound could hide an overflow.
- found: Builds a reference DSD64->WAV RIFF int24 request, sets planned_riff_non_audio_upper_bound_bytes to None (unknown), calls plan_reference_dsd, and asserts it errors with a specific "invalid settings for planned_riff_non_audio_upper_bound_bytes" message tied to the RiffSize error code — confirming the planner refuses to plan when that bound is unset.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `reference_wavpack_rejects_hybrid_and_correction_modes_verbatim`
- spec 3 · read at `227d392ee1a9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:37:45Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A unit test that builds a WavPack encoding config/policy request specifying hybrid mode and/or a correction file, calls the pipeline's planning/validation function, and asserts it returns an Err whose message exactly matches a hardcoded expected string, confirming hybrid and correction modes are explicitly rejected with a stable, verbatim error message.
- found: A test that toggles wavpack.hybrid and wavpack.correction_file individually on a reference request, asserting plan_reference_dsd rejects each with an exact error message naming the field and a shared CanonicalTarget error code text, then asserts the request succeeds once both are cleared.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `wavpack_int24_package_argv_freezes_authoritative_raw_depth` — QUIRKY
- spec 3 · read at `9b7e9497dc56` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T21:21:16Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds a WavPack int24 packaging plan/policy and asserts the generated argv exactly matches a hardcoded expected list, specifically pinning a raw-depth flag (e.g. -b24) as authoritative even if other computed inputs could suggest a different depth, guarding against silent depth drift.
- found: Plans a reference DSD-to-WavPack conversion at int24, extracts the ffmpeg packaging command's args, and asserts a hardcoded slice starting at "-c:a" exactly matches ["-c:a","wavpack","-bits_per_raw_sample","24","-compression_level","1"]; then separately asserts that requesting int16 for the same target fails planning with error code DSD-REF-P0-022.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `riff_capacity_refuses_an_unrepresentable_output_before_execution`
- spec 3 · read at `749738156079` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:20:31Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a planning input (sample rate/depth/duration or similar) whose resulting RIFF/WAV file size would exceed the representable 32-bit RIFF size limit, calls the pure planning function, and asserts it returns an error (fail-closed) rather than an Ok plan — verifying the check happens purely at planning time before any execution/I-O.
- found: Test constructs a 24-hour DSD64 reference request targeting WavRiff/Float64 and asserts plan_reference_dsd fails with a RiffSize error; then switches the target to WavRf64 and asserts it instead fails with a StreamedWavCapacity error, confirming both container-specific capacity checks are enforced at plan time.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `float32_w64_and_rf64_do_not_inherit_a_riff_intermediate_limit` — QUIRKY
- spec 3 · read at `9c1942c4f2f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:50:34Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test asserting that when the target PCM depth is Float32 and the output container is W64 or RF64, the pure planning policy does not apply the RIFF (32-bit size field) intermediate capacity limit that would apply to a plain WAV/RIFF container — mirroring the float64 case documented in the sibling test. It likely constructs the policy/plan for both W64 and RF64 targets and asserts the computed capacity/limit value reflects the extended (non-RIFF-bounded) container rather than the RIFF-derived bound, or that no capacity-related error is produced.
- found: For a 5-minute, 768kHz/Dsd64 Float32 request, asserts plan_reference_dsd succeeds for both WavW64 and WavRf64 targets (which a RIFF-bounded duration would reject), checks the QPCM carrier keeps a .w64 extension, and for RF64 verifies an extra ffmpeg packaging command with exact args (pcm_f32le, -rf64 always) exists while W64 needs none. Then, as a contrast, confirms a 15-minute plain WavRiff request at the same rate/depth is rejected with error code DSD-REF-P0-018.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: The RIFF-limit contrast is proven by an actual too-long duration triggering a specific frozen error code, not by inspecting an abstract capacity value as I'd guessed.

### `float64_w64_and_rf64_use_headerless_streaming_without_a_riff_intermediate_limit` — QUIRKY — TANGLED
- spec 3 · read at `4efb13a59b16` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:05:09Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A pinning test that computes the streaming/packaging capacity policy for float64 target depth using W64 and RF64 containers and asserts that neither imposes the RIFF-style intermediate size limit (e.g. the returned capacity bound is None/unbounded or a container-specific large bound), contrasting with plain RIFF WAV which is capped. Probably checks both W64 and RF64 in turn with similar assertions.
- found: For a 5-minute high-rate Float64 source targeting W64 and RF64, plans a reference DSD conversion and asserts it succeeds (not rejected for exceeding a RIFF cap): W64 writes directly to .w64 with no packaging command, while RF64 pipes the .w64 intermediate through a sox/ffmpeg-style producer/consumer pipeline with specific raw/f64le/rate/channel/-rf64-always args to produce the packaged output.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `terminal_bound_identity_is_rate_specific_and_numerically_conservative` — QUIRKY
- spec 3 · read at `de86dc5ba68e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:40:20Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A test that computes the "terminal bound" value for two or more different target sample rates and asserts they differ (rate-specific), and that each computed bound is conservative — i.e. rounds/clamps to a value that never overstates capacity/precision (e.g. floor rather than round, or less-than-or-equal to some independently computed reference value) — guarding against a future change that hardcodes or shares the same bound across rates.
- found: Computes terminal_realization_bound for two very different sample rates (44.1kHz and 768kHz) at Int24, and asserts the actual numeric bound fields (max_added_peak, safe_pre_terminal_ceiling_dbtp) are IDENTICAL across rates — only the derivation_digest (an identity/traceability hash) differs, proving 'rate-specific' means the digest captures rate as an input even though the numeric bound value doesn't change. Also asserts the ceiling stays under a fixed REFERENCE_CEILING constant (conservative).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: "Rate-specific" in the test name refers to the derivation_digest identity, not to the numeric bound values themselves, which are asserted equal across wildly different rates — easy to misread the test's intent from its name alone.

### `v9_inherits_corrected_float64_effects_bound_and_preserves_other_terminal_bounds` — QUIRKY
- spec 3 · read at `38812f7984fc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:57:30Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Test verifying that the v9 profile's float64 "effects" terminal bound reflects a corrected value, while other terminal bounds (other formats/profiles) remain at their original, uncorrected values — i.e., the correction is scoped only to v9 float64 effects and doesn't leak elsewhere. Likely computes bounds via some policy function and asserts equality against expected constants for several format/profile combos.
- found: A test that checks terminal_realization_bound(176_400, depth) for Int24/Float32/Float64 against exact expected q63 peak and safe-ceiling dB constants, and asserts each result respects the reference ceiling minus the post-final acceptance reserve.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The function name references 'v9' and 'inherits corrected float64 effects bound' but the body has no such concept visibly — it's a flat table test over three bit depths at one sample rate; the name seems to describe a change/regression rather than the mechanism.

### `package_compression_level_changes_native_behavior_identity`
- spec 3 · read at `d9aff8698352` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:51:23Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test verifying that when packaging arguments are built for different compression levels (likely WavPack), the resulting "native behavior identity" (a fingerprint/tag capturing argv/behavior for reference qualification) differs between levels — i.e., compression level isn't silently normalized away and is treated as behavior-affecting, not just a cosmetic argv difference.
- found: Builds two reference DSD plans differing only in FLAC compression_level (0 vs 8) and asserts their conversion_behavior_fingerprint_v1 differ; repeats the same check for WavPack mode (Fast vs VeryHigh), confirming both compression knobs are captured by the behavior fingerprint used for reference qualification identity.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `every_p0_error_message_is_frozen_verbatim`
- spec 3 · read at `9c05892059fc` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:09:59Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A regression-freeze test that enumerates every P0 error-producing scenario in the DSD-to-PCM policy (invalid depth, unsupported rate, disallowed gain mode, etc.), triggers each one, and asserts the resulting error's message text matches an exact hardcoded string for each case — so any wording change to a P0 error message fails the test and must be a deliberate edit.
- found: Table-driven test pairing every ReferenceErrorCode with its exact frozen message string, calling reference_error_text(code) for each and asserting equality, plus inserting each message into a BTreeSet to also assert no two error codes share the same text.
- predicted: most · documented: none · derivable: no · legible: most · trap: no
- note: Didn't anticipate the extra duplicate-message uniqueness check.

## tonepoet-pipeline/src/enums.rs

### the file itself
- spec 3 · read at `bfa37c125cd5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:17:14Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A central domain-model file defining the unified enums (AudioFormat, AudioCodec, PcmBitDepth, DsdRate, SsrcProfile, ModulatorOrder, DsdToPcmGainMode, etc.) used across the whole conversion pipeline, each with helper methods for capability queries (is_dsd, is_lossy, ffmpeg_encodable/sox_encodable), display/formatting (extension, display_name, fmt), and DSD-specific numeric/default derivations (sample rate in Hz, default PCM target/lowpass, default noise shaper/modulator order) — consolidating what used to be duplicated enum hierarchies across the main, backend, and pipeline crates into one shared source of truth.
- found: Matches prediction closely: a shared enum domain (AudioFormat, AudioCodec, SampleKind, PcmBitDepth, BitDepthTarget, RateTarget, DsdRate, DitherType, ResampleQuality, NyquistTransition, PreferredTool, Mp3Mode, AacProfile, ReplayGainMode, OpusContentType, WavPackMode, SoxSincPhase, SsrcProfile, SsrcPdfType, DsdNoiseShaper, ModulatorOrder, DsdFilterPreset, DsdLowpassMethod, GainCompensation, DsdToPcmGainMode) with capability/format helper methods and DSD-specific tuned defaults (lowpass cutoffs, noise shaper/modulator order per rate) each carrying explanatory rationale comments. I underestimated both the sheer number of enums (~25, vs. the handful implied by peers) and how thoroughly each variant is individually doc-commented — this is a much richer, better-documented domain file than the peers list alone suggested.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Several enums carry serde aliases for legacy/misspelled wire values (e.g. DitherType::SlopedTpdf accepting "SloppedTPDF"), which is easy to miss if only skimming variant names.

### `extension`
- spec 3 · read at `cf9a5440fa29` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:19:25Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A match on self (AudioFormat variants) returning a static string literal for each variant's conventional file extension without a leading dot, e.g. "flac", "wav", "dsf", "dff", "mp3", "aac", used elsewhere for constructing output file names.
- found: A match over AudioFormat variants returning the conventional extension string; Aac and Alac both return "m4a" (a detail I didn't predict), and there's a Custom variant that returns a caller-supplied extension string.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `display_name`
- spec 3 · read at `88cdcc46a0f7` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T21:24:56Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A match over AudioFormat variants (Flac, Wav, Mp3, Alac, Dsd, etc.) returning a static human-readable string like "FLAC" or "WAV", used for display in logs or UI rather than the lowercase file extension.
- found: Match over AudioFormat variants returning static uppercase format labels (FLAC, WAV, AIFF, etc.), plus a Custom variant that returns its stored display_name field.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `is_dsd`
- spec 3 · read at `8c9d74f61387` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:26Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A simple match on self returning true for AudioFormat::Dsf and AudioFormat::Dff variants, false for all other formats (PCM/lossy formats).
- found: matches!(self, Self::Dsf | Self::Dff) — true for DSF/DFF, false otherwise.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `is_pcm_lossless`
- spec 3 · read at `d0ebfaadd50f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:57:38Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A match statement over AudioFormat variants returning true for lossless PCM-capable formats (e.g. Wav, Flac, Aiff) and false for lossy codecs (Mp3, Aac, etc.) and DSD-native formats (Dsf, Dsdiff), used to filter candidate output formats during conversion planning.
- found: matches! against Flac, Wav, Aiff, WavPack, Alac; doc line fully captured the intent, code just enumerates the specific variant set.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `is_lossy`
- spec 3 · read at `c524632888c2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:00:04Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Matches on self (an AudioFormat variant) and returns true only for lossy codecs like MP3/AAC/Vorbis/Opus, false for FLAC/WAV/ALAC/DSD/PCM variants.
- found: matches!(self, Mp3 | Aac | Opus | Dts | Ac3) — no Vorbis variant in this enum, and includes surround-audio codecs DTS/AC3.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_encodable`
- spec 3 · read at `88a489f5af1b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:27:21Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Matches on self (the AudioFormat variant) and returns true for common formats FFmpeg can natively encode (e.g. Flac, Wav, Alac, Mp3, Aac, Opus, Vorbis) and false for formats that need special handling like DSD/DSF/DFF, which presumably route through sox_encodable instead.
- found: Matches self against a fixed list of formats (Flac, Wav, Aiff, WavPack, Mp3, Aac, Opus, Alac, Dts, Ac3) FFmpeg can encode, returning true only for those.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sox_encodable` — QUIRKY
- spec 3 · read at `9e9f922f963b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:32:45Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Matches on the AudioFormat variant and returns false for DSD formats (since SoX can't natively encode DSD) and true for standard PCM/lossless formats like WAV, FLAC, AIFF. It may also return false for certain lossy codecs that SoX doesn't support natively (e.g. Opus), returning true only for formats SoX has built-in encoder support for.
- found: A matches! on the AudioFormat variant listing exactly which formats SoX can write: Flac, Wav, Aiff, WavPack, Mp3, Opus, Dsf, Dff. I incorrectly guessed DSD (Dsf/Dff) would be excluded and lossy codecs (Mp3/Opus) might be excluded too — both are actually included as sox-encodable.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Nothing to flag for the next editor — the doc comment matches the code exactly and the logic is a flat, self-explanatory enum list.

### `fmt`
- spec 3 · read at `e3b34ee967b1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:38Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Display impl for AudioFormat that writes the result of self.display_name() (or an equivalent human-readable label) to the formatter, delegating to the existing display_name method rather than duplicating a match.
- found: Writes self.display_name() to the formatter via write_str, exactly delegating to the existing method.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `is_dsd` #2
- spec 3 · read at `bb4b67edfa78` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:10:43Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: matches!(self, AudioCodec::Dsd) — returns true only for the DSD codec variant, false for all others (PCM, MP3, AAC, etc.).
- found: matches!(self, Self::Dsd) — true only for the Dsd codec variant.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `is_lossy` #2
- spec 3 · read at `c95817563dc8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:16:27Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A match over AudioCodec variants returning true for known lossy codecs (MP3, AAC, Vorbis, Opus, etc.) and false for lossless/PCM/DSD variants, consistent with the doc that lossy codecs lack an authoritative PCM source representation despite decoding to PCM samples.
- found: matches! against exactly Mp3, Aac, and Opus variants.
- predicted: most · documented: full · derivable: no · legible: full · trap: no
- note: Vorbis is apparently not a variant of AudioCodec (or is not classified lossy here) — worth confirming the full variant list before assuming symmetry with AudioFormat::is_lossy.

### `bits`
- spec 3 · read at `24d0a893a948` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:36:30Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A match over the PcmBitDepth enum variants (e.g. Int16, Int24, Float32, Float64) returning the corresponding numeric bit depth as u32 - a simple lookup table with no computation.
- found: Match over PcmBitDepth variants returning bit depth; included an Int8 variant I didn't anticipate, and Int32/Float32 share the 32 arm.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `is_float`
- spec 3 · read at `5bffa7c399da` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:21:24Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A const match on self returning true only for the floating-point PCM bit-depth variant(s) (e.g. Float32), false for all integer bit-depth variants.
- found: matches!(self, Self::Float32 | Self::Float64) — exactly as predicted, both float variants included.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `sample_kind`
- spec 3 · read at `9c4495ae2767` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:37:46Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A const match over PcmBitDepth variants that returns SampleKind::Float for float variants (e.g. Float32/Float64) and SampleKind::Int for integer bit depths (16/24/32), essentially classifying the bit depth's sample representation.
- found: Delegates to is_float(): returns SampleKind::Float if true, otherwise SampleKind::SignedInteger.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `hz`
- spec 3 · read at `20167f9a2586` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:32:11Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A const fn that matches on `self` (the DsdRate enum variant, e.g. Dsd64, Dsd128, Dsd256, Dsd512) and returns the corresponding standard DSD sample rate in Hz as a u32, such as 2_822_400 for DSD64, doubling for each successive rate.
- found: Matches on DsdRate variants (Dsd64/128/256/512/1024) and returns the standard DSD sample rate in Hz, each doubling the previous: 2_822_400, 5_644_800, 11_289_600, 22_579_200, 45_158_400.
- predicted: full · documented: some · derivable: no · legible: full · trap: no

### `sox_effect`
- spec 3 · read at `0d02aa55cd12` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:42:03Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A const match over the DsdRate enum variants returning the corresponding SoX effect name as a &'static str, e.g. "dsd64", "dsd128", "dsd256", "dsd512" matching each DSD sample rate variant.
- found: Matches DsdRate variants to their SoX effect name strings; I missed the Dsd1024 variant but got the pattern and the other four exactly right.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default_pcm_target_hz`
- spec 3 · read at `0d28558e6261` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:37:19Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A match over DsdRate variants (DSD64, DSD128, DSD256, DSD512, etc.) returning a scaled-up conservative PCM rate for each — e.g. DSD64 maps to 88200 or 96000, and higher DSD multiples map to progressively higher PCM rates (176400, 352800...) rather than a single fixed constant, to keep the decimation ratio reasonable.
- found: Match over DsdRate variants scaling to PCM rate, correct in shape, but Dsd512 doesn't scale further (caps at 352_800 same as Dsd256) rather than continuing to double, which I didn't predict.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `default_pcm_lowpass_hz`
- spec 3 · read at `342841830016` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:31:33Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A match over self (DsdRate variants: DSD64, DSD128, DSD256, DSD512, DSD1024) returning Some(cutoff_hz) scaled to each rate's clean audio bandwidth for the lower rates, and None for DSD512 and DSD1024 as the docs state explicitly.
- found: Match on DsdRate returning Some(25_000) for Dsd64, Some(48_000) for Dsd128, Some(96_000) for Dsd256, and None for Dsd512/Dsd1024.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `default_noise_shaper`
- spec 3 · read at `9ad281083144` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:06:02Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A const match over self (DsdRate variants like Dsd64, Dsd128, Dsd256, Dsd512, Dsd1024) returning a DsdNoiseShaper: lower rates get higher-order CLANS shapers, and the highest rate (Dsd1024) returns a simple SDM shaper instead, per the doc note.
- found: Const match: Dsd64/128/256/512 all return DsdNoiseShaper::Clans; Dsd1024 returns DsdNoiseShaper::Sdm. Simple binary split, not per-rate CLANS orders as I guessed.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `default_modulator_order`
- spec 3 · read at `9400db5b5432` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:11:34Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A match over DsdRate variants (DSD64, DSD128, DSD256, DSD512, DSD1024) returning ModulatorOrder values stepping down from 8th order at DSD64 to 4th order at DSD1024, one order less per doubling of rate.
- found: Exact match over the 5 DsdRate variants mapping to ModulatorOrder::Order8 through Order4, one step down per rate doubling, as documented.
- predicted: full · documented: full · derivable: no · legible: full · trap: no

### `from_hz`
- spec 3 · read at `e2e02af33244` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:42:22Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const match on hz against known DSD sample rates (e.g., 2_822_400 -> DSD64, 5_644_800 -> DSD128, 11_289_600 -> DSD256, 22_579_200 -> DSD512), returning Some(variant) for exact matches and None otherwise.
- found: Const match on exact hz values (2822400, 5644800, 11289600, 22579200, 45158400) mapping to Dsd64/128/256/512/1024, else None.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `as_arg`
- spec 3 · read at `316e05970d27` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:52:19Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Maps each SsrcProfile enum variant to the static string token used as the SSRC command-line argument for that quality profile, via a match statement returning a &'static str per variant (e.g. "standard", "fast", "short" or similar).
- found: A plain match over the SsrcProfile variants returning each variant's lowercase name as a static string literal, used as the SSRC quality-profile argument.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `fft_length`
- spec 3 · read at `ab295793e932` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T21:09:25Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: SsrcProfile is likely an enum of SSRC resampler quality presets (e.g. Standard, High, VeryHigh). fft_length probably does a match over self returning a hardcoded power-of-two constant per variant (larger for higher-quality profiles), used to size the FFT for SSRC's brick-wall filter.
- found: A match over SsrcProfile variants (Insane, High, Long, Standard, Short, Fast, Lightning) returning a hardcoded power-of-two FFT length, descending from 262144 down to 256.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `value`
- spec 3 · read at `23123fe1a85f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:47:07Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: const fn match returning the numeric modulator order as u8 for each ModulatorOrder variant (e.g. First => 1, Second => 2, Third => 3, ...), a simple lookup table encoded as a match, per the "Numeric order" doc.
- found: Match returning u8 for variants Order4 through Order8, mapping each to its literal numeric value (4-8).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` — OBSCURE
- spec 3 · read at `ad69a2d65866` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:26:29Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Returns the default variant of DsdToPcmGainMode, likely the mode that automatically compensates for the ~6dB gain change inherent in DSD-to-PCM conversion (e.g. an "Auto" or "Compensated" variant), since that's the behavior most users would expect by default.
- found: Default is Self::Disabled — no automatic gain compensation by default, the opposite of what I guessed.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/src/error.rs

### the file itself
- spec 3 · read at `edb51c4612f7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:17:09Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A small file defining a PlanningError enum (variants roughly matching InvalidSettings, InvalidSource, UnsupportedFormat, PluginRejected, each likely carrying a String message), constructor associated functions (invalid_settings, invalid_source, unsupported_format, plugin_rejected) that build each variant from a message, a Display impl formatting each variant into a readable error string, and probably a std::error::Error impl so it composes with ? in pure planning code that has no I/O.
- found: A Result alias plus PlanningError enum with six variants (InvalidSettings, InvalidSource, NoPluginForOperation, PluginRejectedOperation, UnsupportedFormat, RegistryError), constructor helpers for four of them (invalid_settings, invalid_source, unsupported_format, plugin_rejected), a Display impl with per-variant messages, and an empty std::error::Error impl.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no

### `invalid_settings`
- spec 3 · read at `1f1b546f474b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:48:51Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A simple constructor that builds a PlanningError::InvalidSettings variant (or similar), storing field and reason.into() as its fields, and returns it as Self.
- found: Constructs Self::InvalidSettings { field, reason: reason.into() } exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `invalid_source`
- spec 3 · read at `05c2fea02dfb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:53:59Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs Self as a PlanningError::InvalidSource variant (or similarly named), storing field and reason.into() as its fields — a straightforward constructor with no other logic, mirroring the sibling invalid_settings/unsupported_format/plugin_rejected constructors.
- found: Constructs Self::InvalidSource { field, reason: reason.into() }.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `unsupported_format`
- spec 3 · read at `74f4ee840c68` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:43:33Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlanningError::UnsupportedFormat variant (or similarly named) storing the given AudioFormat and the reason string converted via .into(), analogous to the sibling constructors invalid_settings/invalid_source/plugin_rejected.
- found: Constructs Self::UnsupportedFormat with the given format and reason.into().
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `plugin_rejected`
- spec 3 · read at `3e21f4ef07d6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:48:35Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs a PlanningError::PluginRejected { tool, reason: reason.into() } variant (or similar named variant), a straightforward constructor mirroring the sibling invalid_settings/invalid_source/unsupported_format associated functions.
- found: Constructs Self::PluginRejectedOperation { tool, reason: reason.into() } — matches prediction except the exact variant name (PluginRejectedOperation vs my guessed PluginRejected).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `fmt`
- spec 3 · read at `c358d6551221` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:00:24Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: This is the Display impl for PlanningError, matching on the enum variant (InvalidSettings, InvalidSource, UnsupportedFormat, PluginRejected) and writing a human-readable error message for each one via write!(f, ...), likely including the relevant field data (e.g. the invalid value or format name) in the message.
- found: Display impl matching all 6 PlanningError variants (InvalidSettings, InvalidSource, NoPluginForOperation, PluginRejectedOperation, UnsupportedFormat, RegistryError), each writing a formatted message with its field data.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Peers list only exposed 4 of the 6 variants (constructor fns for InvalidSettings/InvalidSource/UnsupportedFormat/PluginRejected), so NoPluginForOperation and RegistryError were invisible until the reveal.

## tonepoet-pipeline/src/fingerprint.rs

### the file itself
- spec 3 · read at `a8ec43a9a01b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:09:59Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A versioned, layout-independent fingerprinting scheme for conversion settings. A FingerprintWriter builder hashes explicit field names/values; several fingerprint "layers" (legacy v1, settings snapshot v2, conversion behavior v1, execution v1, reference-source-probe v1) cover different comparison scopes; per-codec/per-feature push_* helpers (FLAC, MP3, AAC, Opus, WavPack, SSRC, sox/soxr resamplers, DSD, sinc, metadata, verification, replay gain) encode each settings struct's fields explicitly, plus canonical enum-to-string encoders so the digest is independent of Rust struct/enum layout. Tests assert identity and binding invariants across mutators.
- found: A multi-layer, layout-independent fingerprinting system for conversion settings: a FingerprintWriter SHA-256 builder, several digest "layers" (legacy v1, settings snapshot v2, conversion behavior v1, execution v1, semantic plan hash v1, reference-source-probe v1), a large family of per-codec/per-setting push_* helpers and canonical enum-to-string encoders, plus a substantial test suite for identity/binding invariants.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no
- note: The module-level doc comment only describes the legacy v1 settings fingerprint path and doesn't mention the v2/behavior/execution/semantic-plan/reference-probe layers that were added later — stale header, not wrong so much as incomplete.

### `as_bytes`
- spec 3 · read at `d6643ef6f2ae` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:47:28Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Returns a reference to the internal 32-byte array field holding the SHA-256 digest, a trivial const accessor with no computation.
- found: Returns a reference to the tuple struct's internal [u8; 32] field.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `to_hex`
- spec 3 · read at `1555b554ef20` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:11Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Consumes self, gets the byte representation via as_bytes(), and builds a lowercase hex string by iterating over each byte and appending two hex digits per byte, likely using the push_hex_byte helper or write! with {:02x}.
- found: Iterates over the tuple struct's inner byte array (self.0) directly, appending two lowercase hex chars per byte via push_hex_byte into a pre-sized 64-char String.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `fmt`
- spec 3 · read at `ecb3543d1523` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:21Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Implements Display (or Debug) for SettingsFingerprint by writing the hex-encoded representation of the fingerprint bytes (via to_hex or similar) to the formatter.
- found: Writes each byte of self.0 as two-digit lowercase hex directly to the formatter, producing the hex string representation.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `settings_fingerprint`
- spec 3 · read at `bf4c47a44853` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:18:28Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Creates a new FingerprintWriter, calls push_pipeline_settings (or the v2 variant) to feed all conversion-affecting fields of settings into it in a deterministic field-name-based encoding, then calls finish() to produce and return the SettingsFingerprint.
- found: Creates a FingerprintWriter, writes a static "schema" field with an explicit versioned schema string ("tonepoet-pipeline-settings-fingerprint/v1") to guard against cross-version collisions, then pushes the settings fields via push_pipeline_settings and wraps the finished digest in SettingsFingerprint.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: The explicit versioned schema-string field baked into the digest (so different fingerprint schema versions never collide) isn't mentioned in the doc comment.

### `legacy_settings_fingerprint_v1` — QUIRKY
- spec 3 · read at `7f70844ba34b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:52:37Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Thin wrapper that creates a FingerprintWriter, calls push_pipeline_settings (the original v1 field set, not the _v2 variant) to hash the settings' fields, then calls finish() and wraps the digest in a LegacySettingsFingerprintV1 newtype — reproducing manifest v1's exact fingerprint for backward compatibility.
- found: Just delegates directly to settings_fingerprint(settings) — a one-line pass-through, presumably because settings_fingerprint currently produces the v1-compatible output and this name exists as a stable alias/entry point.
- predicted: some · documented: most · derivable: no · legible: full · trap: no
- note: The return type LegacySettingsFingerprintV1 must be what pins this to v1 semantics even though the body just calls the generic settings_fingerprint — that coupling isn't visible without checking settings_fingerprint's signature/generic return type.

### `settings_snapshot_fingerprint_v2`
- spec 3 · read at `cc758c004703` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:56:35Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Creates a FingerprintWriter, calls push_pipeline_settings_v2(&mut writer, settings) to canonically encode every relevant field, then calls .finish() to produce a hash, wrapping the result in SettingsSnapshotFingerprintV2.
- found: Creates a FingerprintWriter, writes a static schema tag field ("tonepoet-settings-snapshot/v2"), pushes the pipeline settings fields via push_pipeline_settings_v2, and wraps writer.finish() in a Sha256Digest inside SettingsSnapshotFingerprintV2.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `conversion_behavior_fingerprint_v1`
- spec 3 · read at `76bd9018d169` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:38:18Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Creates a FingerprintWriter, feeds it explicit named fields extracted from the DsdReferencePlanSummary (e.g. pathway/decision fields) and the DsdSourceKind enum variant, then finishes the writer into a BehaviorFingerprintV1 hash. This is meant to produce a stable digest that captures only the settings that actually affect conversion output for a DSD-reference-based pathway, independent of struct layout changes.
- found: Writes a fixed schema tag plus explicit named fields (policy, target, profile, front_end, source_kind, final PCM sample rate/channels/kind/bit depth, gain policy, package compression level) from the summary/source_kind into a FingerprintWriter and wraps the finished bytes in a Sha256Digest as BehaviorFingerprintV1.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `execution_fingerprint_v1`
- spec 3 · read at `4318efe2b026` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:40:37Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Constructs a FingerprintWriter, feeds in the behavior fingerprint bytes, semantic plan hash, qualification manifest digest, and fields from identity (e.g. tool/runtime versions or paths) via field_static/field_string calls, then calls finish() to produce a combined SHA-256 digest wrapped in ExecutionFingerprintV1. Binds behavior + plan + environment so identical settings on different toolchains produce different fingerprints.
- found: Writes a long explicit sequence of named fields (schema version, behavior/plan/qualification hashes, planner build, platform ABI, runtime dispatch, sox_ng and ffmpeg sha256/version/closure/behavior-probe digests, per-metadata-mutator path/sha/version/closure, sacd_rs build, DST fixture digest, analyzer uncertainty/residual) into a FingerprintWriter and finishes it into a Sha256-based ExecutionFingerprintV1.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `reference_source_probe_digest_v1`
- spec 3 · read at `e176614d5da5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:38:19Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Creates a FingerprintWriter, writes explicit named fields from SourceInfo covering hard probe facts (audio format/codec, sample rate, bit depth, channel count/layout, DSD rate if applicable) via field_static/field_string calls, deliberately omitting paths, timestamps, duration estimates, and mutable tag metadata as the doc states, then calls finish() to return the Sha256Digest.
- found: Matches my prediction closely: writes a schema tag then explicit fields (format, codec, sample_rate_hz, bit_depth, true_source_depth, source_representation, sample_kind, channels, dsd_source_kind) via field_static/field_string, finishing into Sha256Digest. I didn't specifically predict true_source_depth, source_representation, or dsd_source_kind fields, or the schema-tag line, but the overall shape and intent were right.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `new`
- spec 3 · read at `290728691d6d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:59:52Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Constructs a new FingerprintWriter with an empty internal buffer/hasher state (e.g. a String or Vec<u8> accumulator, or a fresh hash algorithm instance) ready to accept field_static/field_string calls before finish() produces the digest.
- found: Initializes the struct with a fresh Sha256 hasher instance.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `field_static`
- spec 3 · read at `5efaa08235ef` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T09:30:44Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Writes the field's path name and its static string value into the internal hasher/digest state (likely delimited so field boundaries can't collide), functioning as a thin wrapper around field_string for &'static str or literal values used when encoding an enum variant or fixed label into the fingerprint.
- found: Updates the hasher with path, "=", the value's length as ascii digits, ":", the value bytes, and a trailing newline — a length-prefixed encoding that prevents ambiguity/collision between adjacent fields, exactly the delimiting scheme I guessed at, though implemented via explicit length-prefix rather than just a delimiter character.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `field_string`
- spec 3 · read at `ebeb132a27b1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:57:34Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Feeds the field's path name and its string value into the internal hasher (e.g. writing path bytes, a separator, then value bytes, possibly length-prefixed) so the fingerprint incorporates this named field deterministically, similar to field_static but for a dynamic String rather than a static str.
- found: Just delegates to self.field_static(path, &value) — a thin convenience wrapper so callers can pass an owned String, rather than doing its own hashing.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `finish`
- spec 3 · read at `6ef0a7d445e1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:42:36Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Consumes self and finalizes an internal hasher (likely SHA-256 given the 32-byte output) that has accumulated field data pushed via other FingerprintWriter methods, returning the resulting digest as a [u8; 32].
- found: Finalizes self.hasher and converts the output into a [u8; 32] array.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `push_hex_byte`
- spec 3 · read at `c8a66d8b2761` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:20:38Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Converts a single byte to its two-character hex representation and appends it to the output string, likely using a lookup table of hex digits or format!("{:02x}") to keep the fingerprint digest human-readable and deterministic across platforms.
- found: Exactly as predicted: uses a static hex digit lookup table and pushes the high and low nibble characters to the output string.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `push_pipeline_settings`
- spec 3 · read at `f3563ae811ac` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:39:06Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Given a PipelineSettings struct, this writes each conversion-affecting field (target format, sample rate/bit depth, gain policy, resampler/sinc settings, DSD-specific options, etc.) into the FingerprintWriter via named field_static/field_string calls, producing a stable ordered digest input. Since there's also a push_pipeline_settings_v2, this may be the legacy v1 version kept for backward-compatible fingerprint comparisons, or it may delegate to v2 internally.
- found: Writes core scalar settings (target format, sample rate, bit depth, resample quality, nyquist transition, dither, preferred tool, force_encode) directly, then delegates to per-codec/per-concern push helpers (flac, mp3, aac, opus, wavpack, ssrc, sox/soxr resampler, dsd, metadata, verification, replay_gain) to fold in the rest of PipelineSettings.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_pipeline_settings_v2`
- spec 3 · read at `2096cbe4f03b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:42:31Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Walks the fields of PipelineSettings and pushes each into the FingerprintWriter with explicit field names/enum encodings (via field_static/field_string), delegating to format-specific helpers (push_flac, push_mp3, push_aac), push_native_dsd_v2, push_sinc_v2, and canonical_* normalizers for structured subfields like source kind and gain policy, so the digest is stable across struct layout/serde changes.
- found: Writes scalar PipelineSettings fields (format, sample rate, bit depth, resample quality, nyquist transition, dither, preferred tool, force_encode) with explicit field names, then delegates to per-codec/per-feature push_* helpers (flac, mp3, aac, opus, wavpack, ssrc, sox/soxr resamplers, native dsd, metadata, verification, replay gain).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_native_dsd_v2`
- spec 3 · read at `ee080077946b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:33:50Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of DsdSettings (DSD rate/multiplier, output container/bitstream format, any noise-shaping or dither options) into writer via field_static/field_string calls with explicit field names, mirroring sibling push_flac/push_mp3/push_aac functions, so the fingerprint digest changes whenever a DSD-affecting setting changes.
- found: Pushes fields for both directions of DSD conversion: pcm_to_dsd settings (schema version, noise shaper, modulator order, trellis, filter preset, delegated sinc-v2 fields, gain compensation) and from_dsd settings (source pathway, reference policy, reconstruction profile, gain mode, fixed gain dB, normalize-peak target), each as an explicit named field on the writer.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_sinc_v2` — QUIRKY
- spec 3 · read at `b4d49135911a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:13:47Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Writes each relevant field of SincFilterSettings (e.g. quality/order, cutoff frequency, window function, taps) into the FingerprintWriter using explicit named field_static/field_string calls, so the digest reflects the sinc resampling filter configuration independent of struct layout.
- found: Writes seven explicit named fields (oversample_factor, taps, passband_hz, transition_hz, kaiser_beta, linear_phase, allow_aliasing) of a PCM-to-DSD sinc interpolation filter's settings into the FingerprintWriter, using field_string for numeric values and field_static/bool_value for booleans.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: This is specifically the PCM→DSD sinc interpolator, not a generic resampler — the field names (oversample_factor, kaiser_beta, allow_aliasing) only make sense in that context.

### `option_db_nano`
- spec 3 · read at `9e8fcdf1a850` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:47:48Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Converts an Option<DbNano> into a stable string for the fingerprint digest — None maps to a sentinel string like "none", and Some(v) maps to the raw nanounit integer formatted as a string, keeping the encoding independent of float formatting quirks.
- found: Maps None to the literal string \"None\" and Some(v) to \"Some(<rendered value>)\" using a DbNano::render(false) method rather than a raw integer/to_string — so it mimics Rust's Debug-style Option formatting but with a custom renderer for the inner value.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: DbNano has a render(bool) method for formatting; I guessed a raw to_string() instead.

### `sample_kind`
- spec 3 · read at `d99f49f0e358` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:40:51Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A match statement over the SampleKind enum variants that returns a stable static string name for each variant (e.g. "int" vs "float", or specific bit-depth/format tags), used so the fingerprint encoding of sample kind stays independent of the enum's discriminant values or Rust representation.
- found: Match over SampleKind variants (SignedInteger, UnsignedInteger, Float, Dsd) returning stable static string names.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `canonical_front_end`
- spec 3 · read at `0111c41af0d3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:33:26Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: It's a small match on the DsdInputFrontEnd enum variants, returning a stable, explicit string name for each (e.g. "sox"/"native") independent of Rust's Debug derive, to keep the fingerprint stable across refactors.
- found: Matches DsdInputFrontEnd variants to stable string names; NativeUncompressed maps to a fixed literal, while the DSDIFF/SACD variants embed their nested decoder/extractor sub-fields via Debug formatting into the string.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: The file doc claims the fingerprint is independent of debug formatting, but this function does use {:?} for nested decoder/extractor enums.

### `canonical_source_kind`
- spec 3 · read at `2324cac481f2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:40:16Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Matches on the DsdSourceKind enum variant and returns a fixed, stable string label for each variant (e.g. "iso", "dsf", "dsdiff", "dst") to be fed into the fingerprint hash, deliberately independent of the enum's Rust derive/debug output so the fingerprint doesn't change if variants are reordered or renamed internally.
- found: Matches DsdSourceKind variants to stable string labels; simple variants get flat names, but SacdTrack embeds its frame format, area, track index, start frame, frame count, and TOC digest hex into the string so the fingerprint captures the exact SACD track selection.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `canonical_gain_policy`
- spec 3 · read at `6810701d7a36` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:37:21Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Matches on the ResolvedGainPolicy enum variant and returns a stable string tag identifying that variant (e.g. "none", "track", "album", "fixed") for use in the fingerprint digest. If the policy carries an associated numeric value (like a target gain in dB or a fixed gain amount), it likely appends that value in a deterministic formatted form to the tag string, similar to how sibling canonical_* functions encode enum values independent of struct layout.
- found: Matches on the 4 variants of ResolvedGainPolicy, each producing a colon-delimited string tag with the variant name plus its associated fields rendered deterministically (gain/target values via .render(false), ceiling, terminal_bound's max peak and derivation digest hex) so equivalent settings hash identically regardless of struct layout.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_flac`
- spec 3 · read at `e4c7538a01be` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:29:44Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of FlacSettings (e.g. compression level, and any bit-depth/dithering options) into the FingerprintWriter using named-field pushes, similar to how push_mp3/push_aac push their own settings, so the digest reflects all FLAC-affecting encoder options.
- found: Pushes flac.compression_level, flac.verify, and flac.write_md5 fields into the writer.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_mp3`
- spec 3 · read at `317463c308fa` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:22:32Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Writes each MP3-relevant conversion-setting field (bitrate/quality mode, VBR/CBR flag, quality level, maybe channel mode) into the FingerprintWriter using explicit named field tags, mirroring the pattern of sibling push_flac/push_aac/etc functions, so the digest changes whenever any MP3 setting changes.
- found: Writes three named fields: mp3.mode (via mp3_mode canonicalization), mp3.bitrate_kbps, and mp3.vbr_quality, all into the FingerprintWriter.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_aac`
- spec 3 · read at `786403622b07` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:55:41Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Writes each conversion-relevant field of AacSettings (such as bitrate/quality and encoding mode) into the FingerprintWriter using explicit named keys, so the fingerprint captures AAC encoder settings independent of struct layout.
- found: Writes settings.profile (via an aac_profile canonicalization helper) and settings.bitrate_kbps (as a string) into the writer under explicit named keys "aac.profile" and "aac.bitrate_kbps".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_opus`
- spec 3 · read at `f1d5fcc8a325` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:56:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of OpusSettings (e.g. bitrate, VBR/CBR mode, complexity, application type) into the FingerprintWriter tagged with explicit field names, using helper functions like option_u8/option_db_nano for optional/numeric fields, so the resulting digest reflects all Opus encoding parameters that affect output.
- found: Writes exactly three Opus fields (content_type via a canonicalizing helper, bitrate_kbps, complexity) into the writer with explicit field names, not the larger field set (VBR mode, application type) I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Actual OpusSettings surface is smaller than I assumed — only content_type/bitrate/complexity, no VBR mode or application-type fields.

### `push_wavpack`
- spec 3 · read at `833d2135f6fd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:35:20Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of WavPackSettings into the FingerprintWriter using explicit named pushes (e.g. compression level, hybrid mode/bitrate, extra processing level, MD5 flag), mirroring the pattern of push_flac/push_mp3 so the fingerprint stays independent of struct layout.
- found: Writes mode, hybrid flag, hybrid bitrate, and correction_file flag into the fingerprint writer via field_static/field_string helpers.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Actual field set (mode, hybrid, hybrid_bitrate_kbps, correction_file) differs from my guessed field names (compression level, extra processing, md5) — same shape of function, different WavPackSettings fields than I assumed.

### `push_ssrc`
- spec 3 · read at `4ccc6dc4c861` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:41:15Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of SsrcSettings (e.g. quality/precision, dithering PDF type via ssrc_pdf_type, any bit-depth or gain-related options) into the writer under explicit named keys, so the SSRC resampler's settings contribute deterministically to the overall fingerprint regardless of struct layout.
- found: Writes named fields for force, insane_mode, profile, attenuation_db, min_phase, dither_id, and pdf_type — matches the predicted pattern of explicit named keys per setting, including the pdf_type field I guessed at, though I didn't anticipate force/insane_mode/min_phase specifically.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `ssrc_pdf_type`
- spec 3 · read at `fec6e7b97123` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:59:40Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Matches on the SsrcPdfType enum variant and returns a fixed, stable &'static str label for each (e.g. "rectangular", "triangular"), used to build the fingerprint independent of enum discriminant values or Debug formatting.
- found: Exactly as predicted: a match mapping the two SsrcPdfType variants to fixed stable string labels.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `push_sox_resampler`
- spec 3 · read at `513e34d7a3db` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:03:56Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Writes each relevant field of SoxResamplerSettings (quality/precision, phase response, bandwidth, allow-aliasing, dithering, etc.) into the FingerprintWriter with explicit named keys, in a stable fixed order, encoding enums to their canonical string/numeric representation so the digest is independent of struct layout.
- found: Writes each SoxResamplerSettings field (chebyshev flag, bandwidth_pct, phase, allow_aliasing, sinc_taps, sinc_attenuation_db, sinc_passband_hz, sinc_transition_hz, sinc_kaiser_beta, sinc_phase enum) into the FingerprintWriter under explicit named keys with None-safe string formatting.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_soxr_resampler`
- spec 3 · read at `eef38457c4fc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:45:40Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Pushes each field of SoxrResamplerSettings (quality/precision, phase response, passband, etc.) onto the FingerprintWriter with explicit named fields, mirroring sibling push_* functions, so the fingerprint digest changes whenever any soxr resampler setting changes.
- found: Pushes three specific SoxrResamplerSettings fields (chebyshev bool, cutoff, phase) onto the writer with explicit field names, matching the general push_* pattern I predicted but not the exact field set.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `option_u8`
- spec 3 · read at `213242216853` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:04:46Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Converts an Option<u8> into a stable string token for the fingerprint digest: None maps to a fixed sentinel string like "none", and Some(v) maps to a distinguishable string (e.g. "some:<v>" or similar) so that None and Some(0) can't collide, consistent with sibling helpers bool_value/string_value that produce explicit stable encodings.
- found: Some(v) becomes v.to_string() (bare digits) and None becomes the literal string \"None\" — simpler than the tagged encoding I predicted, though since u8 always stringifies as digits there's no actual collision risk.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_dsd` — QUIRKY
- spec 3 · read at `1f93879c822e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:24:26Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of DsdSettings (e.g. DSD rate/DoP mode, PCM conversion filter, gain/legacy-gain options, output format) into the FingerprintWriter as explicit named key/value pairs, using helper encoders like bool_value/string_value/option_u8 so the digest is stable regardless of struct layout.
- found: Writes DSD PCM-to-DSD conversion fields (noise shaper, modulator order, trellis settings with lookahead/nodes/latency or None sentinels, filter preset, gain compensation) plus DSD-to-PCM lowpass method and legacy gain, and delegates sinc filter fields to push_sinc — all as explicit named fields on the writer.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `push_legacy_dsd_to_pcm_gain`
- spec 3 · read at `0f9daaa2c073` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:50:59Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Writes the gain-related fields of the legacy DSD-to-PCM settings (e.g. a gain adjustment value in dB and/or a normalize flag) into the FingerprintWriter using explicit named fields, so the digest captures gain behavior independent of the wire struct's layout — likely calling helpers like bool_value/option_u8 for individual fields.
- found: Writes the gain mode field always, then branches on DsdToPcmGainMode (Disabled/Auto/Manual) to write different specific fields: Disabled writes an optional legacy gain_db only if present, Auto writes an auto-gain margin, Manual writes the gain_db as an Option.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_sinc`
- spec 3 · read at `e3c4333ee04c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:58:09Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of SincFilterSettings (likely filter length, passband/stopband edges, phase type) into the FingerprintWriter using named keys, in fixed order, so the fingerprint stays stable regardless of struct layout - similar pattern to push_ssrc/push_dsd.
- found: Writes each field of the DSD sinc-resampler SincFilterSettings (oversample_factor, taps, passband_hz, transition_hz, kaiser_beta, linear_phase, allow_aliasing) to the FingerprintWriter under fixed "dsd.sinc.*" keys.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_metadata`
- spec 3 · read at `0275d87a6bb7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:47:22Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of MetadataSettings into the FingerprintWriter as an explicit named entry (e.g. writer.field("metadata.some_flag", bool_value(...))), using the module's helper encoders (bool_value, string_value, etc.) so the digest is stable regardless of struct layout — mirroring the pattern of the other push_* functions for codec-specific settings.
- found: Writes three named boolean fields (transfer_tags, preserve_artwork, store_source_audio_md5) from MetadataSettings into the writer using field_static and bool_value.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `push_verification`
- spec 3 · read at `6878add3b4e0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:51:28Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of VerificationSettings into the fingerprint writer using explicit named-field pushes (e.g. a named "enabled" bool via bool_value, maybe a verification mode/algorithm via option_static or string_value), so that any change to verification-related settings changes the resulting digest. Likely calls writer.field("verification") or similar as a namespace prefix before pushing the sub-fields.
- found: Pushes exactly two bool fields — verify_after_encode and prefer_native_flac_verify — each via writer.field_static with a "verification."-prefixed dotted name and bool_value encoding.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_replay_gain`
- spec 3 · read at `de6aab1c00e2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:28:38Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Writes each field of ReplayGainSettings into the FingerprintWriter tagged with explicit field-name strings, using the module's helper functions (bool_value, option_f32, string_value, etc.) for each field type, so the fingerprint reflects replay gain mode/preamp/target settings independent of struct layout.
- found: Writes three explicitly-named fields (mode, prevent_clipping, existing_tags policy) from ReplayGainSettings into the writer, matching the existing_tags enum to string literals inline rather than via a shared helper.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `bool_value`
- spec 3 · read at `c2615b5e7512` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T23:36:23Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Returns a fixed static string, either "true" or "false", based on the input bool, for use as a stable token when building the settings fingerprint digest.
- found: Returns "true" or "false" as a static str depending on the bool value.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `audio_format`
- spec 3 · read at `c0bc47e9cd47` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:53:24Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Maps the AudioFormat enum value to a stable, explicit string label (e.g. "flac", "wav", "mp3") via a match statement, used as part of building the settings fingerprint so the digest doesn't depend on enum discriminant values or Rust's derived Debug output.
- found: Matches AudioFormat to a stable string label for each fixed variant, and for the Custom{extension, display_name} variant formats a string embedding both fields via string_value.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `audio_codec`
- spec 3 · read at `ece898a6e92d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:08:19Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Matches on the AudioCodec enum and returns a fixed, hand-chosen string literal per variant (e.g. "flac", "alac", "aac", "mp3", "pcm") so the fingerprint digest stays stable even if the enum's Debug output or declaration order changes.
- found: Matches AudioCodec variants to fixed string literals (flac, pcm_signed/unsigned/float, wavpack, mp3, aac, opus, alac, dsd) exactly as predicted, plus a Custom(name) variant that formats as custom(name) via the string_value helper — a detail I hadn't accounted for.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `string_value`
- spec 3 · read at `8e3850924dcf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:04:14Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Formats a string value into a canonical, unambiguous encoded form for the fingerprint digest input, likely wrapping it in quotes and escaping delimiter characters (quotes/backslashes) so that field separators in the digest input can't be spoofed by string content, keeping the fingerprint stable and collision-resistant.
- found: Length-prefixes the string (netstring-style "{len}:{value}") to make the encoding unambiguous for digest input, rather than quoting/escaping as I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Uses a length-prefix (netstring) scheme rather than quote-escaping — correct general idea (prevent delimiter spoofing) but the specific mechanism differed from my guess.

### `rate_target`
- spec 3 · read at `4e31d61f8739` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:56:14Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Matches on the RateTarget enum's variants and returns a stable, explicit string encoding for each (e.g. "keep", or "fixed:44100" including any numeric sample rate payload), used to build the settings fingerprint independent of Rust's derived formatting.
- found: Matches RateTarget::Source/PcmHz(hz)/Dsd(rate), producing a stable string like \"PcmHz(44100)\" or \"Dsd(...)\" where the DSD rate is itself run through another stable-encoding helper (dsd_rate).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `bit_depth_target`
- spec 3 · read at `1c8e8a222ce4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:09:45Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Maps a BitDepthTarget enum value to a stable string label (e.g. "16", "24", "source"/"unchanged") for inclusion in the fingerprint digest, independent of the enum's Rust discriminant/debug formatting — a small match returning static strings.
- found: Matches BitDepthTarget: Source -> "Source", Pcm(depth) -> "Pcm({encoded depth})" using pcm_bit_depth helper for the nested value's stable encoding.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `preferred_tool`
- spec 3 · read at `06ac3eaf286f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:23:36Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A match over the PreferredTool enum variants that returns a stable, hardcoded string for each variant (e.g. "native", "ffmpeg", "auto"), used so the fingerprint digest doesn't depend on Rust's Debug/serde output or enum discriminant values.
- found: Match over PreferredTool: Auto/Ffmpeg/Sox/Ssrc each map to their capitalized variant name string, and Custom(name) maps to "Custom(<string_value(name)>)".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `option_static`
- spec 3 · read at `6273f46101ef` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:15:41Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Converts an Option<&'static str> into a stable string token for the fingerprint digest — likely returning something like "none" when None and the string itself (or a "some:"-prefixed wrapper) when Some, mirroring the pattern of the other option_*/*_value helpers used to build enum-encoding-independent digests.
- found: Formats Option<&'static str> as literal "Some(value)" or "None" strings for the fingerprint digest.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `option_u16`
- spec 3 · read at `6273f46101ef` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:20:45Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Encodes an Option<u16> into a stable string for fingerprinting: returns something like "none" when the value is None, and a distinguishable "some:<value>" (or similar tagged) string when Some, so that None and any numeric value never collide in the digest.
- found: Formats Some(value) as "Some(value)" and None as "None", mirroring Rust's Debug-style tagging so the two never collide in the fingerprint digest.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `option_f32`
- spec 3 · read at `7ae048b39073` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:25:49Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A helper for the stable fingerprint that turns Option<f32> into a canonical string — match value { Some(v) => f32_value(v), None => "none".to_string() }, delegating the actual float encoding to the sibling f32_value function to keep bit-for-bit stability.
- found: Matches Some/None, delegating to f32_value for the float encoding but wrapping it as "Some(...)" / "None" text, exactly as predicted apart from the Rust-Debug-like wrapper formatting.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `option_trellis` — OBSCURE — TRAP
- spec 3 · read at `773d88a4a6f4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:30:51Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Encodes an Option<TrellisSettings> into a stable string for the fingerprint digest: returns something like "none" when None, and otherwise formats each field of TrellisSettings explicitly by name (e.g. "trellis{enabled=...,...}") so the encoding is independent of struct layout or Debug formatting.
- found: Just maps Some(_)/None to the literal strings "Some"/"None", discarding all the actual TrellisSettings field values rather than encoding them into the fingerprint.
- predicted: none · documented: none · derivable: yes · legible: full · trap: yes
- note: Despite the file doc's claim that "the fingerprint covers every setting that can alter conversion output," this collapses all TrellisSettings field values to a single "Some" marker — two different trellis configs produce the same fingerprint, silently breaking cache/dedup invalidation if TrellisSettings has more than an on/off field.

### `f32_value`
- spec 3 · read at `50c91caf81d4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:32:26Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Converts an f32 into a deterministic string for the fingerprint digest, likely by taking value.to_bits() and formatting it as hex, rather than using the default float-to-string formatter, to avoid any risk of formatting differences across platforms/Rust versions affecting the digest.
- found: Formats the f32's raw bit pattern as an 8-digit hex string prefixed with "f32bits:".
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `resample_quality`
- spec 3 · read at `f033d884b2e8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:03:15Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A simple match statement over the ResampleQuality enum variants, returning a fixed &'static str label for each (e.g. "fast", "medium", "best" or similar quality tiers), used so the fingerprint digest doesn't depend on Rust's derived Debug formatting or enum ordinal values.
- found: Match over ResampleQuality variants (Low, Medium, High, VeryHigh, Ultra, Insane) returning each variant's name as a static string.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: File_doc describes the whole fingerprint module, not this specific function; the actual variant names/tiers were unguessable without seeing them.

### `nyquist_transition`
- spec 3 · read at `25636704e003` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T23:15:52Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A simple match over the NyquistTransition enum's variants, returning a fixed static string name for each variant (e.g. "sharp", "gentle"/"gradual" or similar) so that the fingerprint digest encodes the enum by a stable name string rather than its discriminant value.
- found: Matches over five NyquistTransition variants (Gentle, Medium, Steep, Sharp, BrickWall) returning their name as a static string.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Correctly predicted the shape exactly; only missed the specific variant names, which was reasonable since I only had one field's worth of context.

### `dither_type`
- spec 3 · read at `208f9cccea1f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:13:20Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A match expression over the DitherType enum's variants, returning a stable static string label for each (e.g. "none", "triangular", "shaped") to be embedded in the settings fingerprint digest, independent of Rust enum/debug representation.
- found: A match over all DitherType enum variants, returning each variant's name as a stable static string for use in the settings fingerprint digest.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `mp3_mode`
- spec 3 · read at `c212018ddeca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:01:23Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A match expression over Mp3Mode variants (e.g. Cbr, Vbr, Abr) returning corresponding stable static string labels like "cbr", "vbr", "abr" for use in the fingerprint digest, ensuring the digest doesn't depend on enum discriminant values or debug formatting.
- found: Match over Mp3Mode::{Cbr,Vbr,Abr} returning static strings that are just the Rust variant names verbatim ("Cbr", "Vbr", "Abr") rather than lowercase abbreviations as I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `aac_profile`
- spec 3 · read at `16d36ad9219b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:06:25Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A match over AacProfile variants (e.g. Low/LC, HE, HEv2) returning a fixed static string label for each, used to build a stable fingerprint string that doesn't depend on the enum's Debug output or discriminant values.
- found: Matches AacProfile::LcAac/HeAac/HeAacV2 to fixed static string labels matching the variant names, exactly as predicted.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `replay_gain_mode`
- spec 3 · read at `13aa0ce58f48` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:11:57Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Matches on the ReplayGainMode enum and returns a fixed &'static str name for each variant (e.g. "off", "track", "album"), used as part of building a stable fingerprint digest that doesn't depend on Rust's internal enum representation.
- found: Matches ReplayGainMode variants (Track, Album, Both) and returns the capitalized variant name as a static string, for stable fingerprint encoding.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `opus_content_type`
- spec 3 · read at `a2473933b343` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:17:02Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A match statement mapping each OpusContentType enum variant (e.g. Voip, Audio, RestrictedLowDelay) to a fixed &'static str label, used to build a stable, layout-independent fingerprint string for that setting.
- found: Match mapping OpusContentType::Auto/Music/Speech to their literal string names, for a stable fingerprint encoding.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `wavpack_mode`
- spec 3 · read at `216cbffc3ef0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:12:19Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A match over the WavPackMode enum's variants (e.g. fast/high/extra-high/lossless/hybrid compression levels) returning a fixed, stable &'static str label for each, used to build a layout-independent fingerprint of conversion settings.
- found: Exactly the predicted shape — a match over WavPackMode variants to stable strings — though the actual variants (Normal/Fast/High/VeryHigh) differ from my guessed lossless/hybrid naming.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_profile`
- spec 3 · read at `0bbb99000257` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:41:40Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Matches on the SsrcProfile enum variants and returns a fixed static string label per variant (e.g. names like "standard", "high_quality"), used so the fingerprint stays stable regardless of enum ordinal or debug formatting.
- found: A simple exhaustive match converting each SsrcProfile enum variant to its exact-name static string (Insane, High, Long, Standard, Short, Fast, Lightning), for stable fingerprinting.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `dsd_noise_shaper`
- spec 3 · read at `dbfa1824ba05` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:22:10Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A match over DsdNoiseShaper's variants, returning a fixed, stable &'static str literal for each variant name (e.g. "sdm", "triangular", etc.) to encode it into the fingerprint independent of enum discriminant order or Debug formatting.
- found: Exactly as predicted: three-arm match over DsdNoiseShaper (Clans, Sdm, Crfb) returning the variant name as a literal string.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `modulator_order`
- spec 3 · read at `8bcd111f8806` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:06:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A match over ModulatorOrder enum variants returning a stable static string label for each variant, used to encode this setting into the fingerprint digest independent of variant declaration order.
- found: Match over ModulatorOrder variants (Order4..Order8) returning their name as a static string.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `dsd_filter_preset`
- spec 3 · read at `83b8cde4676a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:35:49Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A match over DsdFilterPreset's variants, returning a stable static string label for each one (e.g. "sharp", "slow", "apodizing" or similar names), used to encode the enum into the settings fingerprint independent of Rust's Debug output or discriminant values.
- found: Match over DsdFilterPreset returning "Auto" or "Sinc" — correct shape, but only two variants (matching Enum variant names verbatim) rather than the invented descriptive names ("sharp"/"slow") I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_lowpass_method`
- spec 3 · read at `3caaaef9245e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:27:16Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A match statement over DsdLowpassMethod variants, returning a fixed &'static str label for each variant (e.g. "brickwall", "gentle", etc.) so that the fingerprint digest stays stable independent of enum discriminant values or naming changes.
- found: Match over three DsdLowpassMethod variants (Auto, SoxUltra, Sinc) returning their variant names as string literals verbatim.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Labels are just the Rust variant names verbatim, not custom stable identifiers as I'd guessed.

### `dsd_to_pcm_gain_mode`
- spec 3 · read at `41edae5b5a0d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:32:18Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A match over the DsdToPcmGainMode enum variants (likely Auto, Fixed/some legacy variant) returning a stable static string label for each, used to build the fingerprint digest independent of derive(Debug) formatting.
- found: Match over DsdToPcmGainMode::{Disabled, Auto, Manual} returning their literal variant name as a static str.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `gain_compensation`
- spec 3 · read at `26b4208db260` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:43:09Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Matches on the GainCompensation enum variant and returns a stable string tag for each (e.g. "none", "auto", or "manual:<db-value>" for a manual gain value), keeping the fingerprint independent of derive-based Debug formatting.
- found: Matches on GainCompensation's four variants (Auto, Linear(f32), Decibels(f32), Disabled), returning a fixed name string, with Linear/Decibels embedding a stably-formatted f32 value via f32_value.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_rate`
- spec 3 · read at `1931aa74df41` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:12:28Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A simple match over the DsdRate enum variants that returns a stable, explicit string label for each rate (e.g. "dsd64", "dsd128", "dsd256", "dsd512") for use in the settings fingerprint, independent of the enum's Rust declaration order or Debug output.
- found: Matches each DsdRate variant to a stable string label matching the exact variant name (Dsd64, Dsd128, Dsd256, Dsd512, Dsd1024), missing the Dsd1024 variant in my guess and using PascalCase rather than lowercase.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `pcm_bit_depth`
- spec 3 · read at `e7c854b0bc2e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:46:01Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A match over the PcmBitDepth enum variants that returns a stable static string encoding for each (e.g. "16", "24", "32-float"), used to feed into the settings fingerprint hash independent of the enum's Rust representation.
- found: Matches the six PcmBitDepth variants (Int8/Int16/Int24/Int32/Float32/Float64) and returns their variant names as static strings, matching the predicted mechanism though I guessed different label text ("16"/"32-float") instead of the actual "Int16"/"Float32" names.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `legacy_dsd`
- spec 3 · read at `59bcae68d5d5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:02:26Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A test-support builder function that constructs a DsdSettings value with sensible/default values for most fields (noise shaper, filter preset, lowpass method, rate, etc.), setting gain_mode, margin_db, and gain_db from the given parameters. Used as a fixture helper for fingerprint identity tests concerning legacy DSD-to-PCM gain behavior.
- found: Builds a default LegacyDsdSettingsWireV1, overrides its gain_mode/margin/gain fields from the params, then converts it into a DsdSettings via from_legacy_wire — a test fixture helper going through the legacy wire-format conversion path rather than constructing DsdSettings directly.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `fingerprint_with`
- spec 3 · read at `6796c74afa41` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:25:38Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Test helper that creates a default PipelineSettings, applies the given `update` closure to mutate it, then computes and returns its SettingsFingerprint — used to concisely build fingerprints for settings variations in tests.
- found: Exactly as predicted: default PipelineSettings, apply update closure, compute settings_fingerprint.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `test_metadata_identity` — QUIRKY
- spec 3 · read at `8cd1002f9b5b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:48:25Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A small test-fixture builder that constructs a ReferenceMetadataMutatorIdentityInput with the given `name` set and all other fields at trivial/default values, used by the fingerprint tests to construct distinct "metadata mutator identity" inputs to compare fingerprints against.
- found: Builds a ReferenceMetadataMutatorIdentityInput (a fixture representing a tool executable's identity for fingerprinting) where canonical_path, executable_sha256, reported_version, and closure_digest are all deterministically derived from the given `name` string, so distinct names produce distinct, reproducible identity inputs for tests.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `test_reference_execution_identity`
- spec 3 · read at `247b0ddf77c2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:43:22Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture helper that builds and returns a baseline/canonical ReferenceExecutionIdentityInput struct populated with representative default values for its fields, used as a starting point that other fingerprint tests clone and mutate one field at a time to check fingerprint sensitivity.
- found: Builds a baseline ReferenceExecutionIdentityInput fixture with placeholder digests/versions for every toolchain component (planner, platform, sox, ffmpeg, metadata mutators, sacd-rs, dst fixture) plus small nonzero uncertainty/residual values, used as a base for other tests to mutate.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `execution_fingerprint_binds_every_metadata_mutator_identity_component`
- spec 3 · read at `5f113ca4c5ca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:11:39Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: This test builds a baseline execution fingerprint including a metadata mutator with several identity components (e.g. plugin id, version, config params), then for each component individually changes its value and asserts the resulting fingerprint differs from the baseline — proving every component of the mutator's identity is actually incorporated into the hash, not just some subset.
- found: Computes a baseline execution fingerprint, then for each of 3 metadata mutators (metaflac, wvtag, atomic_parsley) and each of 4 identity fields (canonical_path, executable_sha256, reported_version, closure_digest) mutates just that field and asserts the fingerprint changes; also checks that removing metadata_mutators entirely (None) changes the fingerprint.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `dither_explicit_is_mode_scoped_in_the_settings_fingerprint` — QUIRKY
- spec 3 · read at `15a373faa7b1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:56:56Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A unit test verifying that the dither_explicit setting only affects the fingerprint digest when the active dither mode uses it - constructing settings pairs that differ only in dither_explicit under a relevant mode (asserting fingerprints differ) and under an irrelevant mode (asserting fingerprints are equal, i.e. the field is ignored/scoped out there).
- found: With dither_type fixed to Tpdf, asserts the fingerprint with dither_explicit=false equals the default (unset) fingerprint, and differs from dither_explicit=true — showing the explicit flag is meaningfully encoded but false is the implicit default, not that it varies across different dither modes.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `disabled_dsd_to_pcm_fingerprint_ignores_auto_margin_without_legacy_gain`
- spec 3 · read at `750d2351c647` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:35:54Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Unit test that builds two conversion settings identical except for the DSD-to-PCM "auto margin" value, both with dsd_to_pcm_gain_mode = Disabled and no legacy DSD gain set, computes fingerprints for each, and asserts they're equal — proving the auto-margin field is excluded from the fingerprint when gain mode is disabled and legacy gain is absent (paired with a sibling test that it's honored when legacy gain IS present).
- found: Builds two settings via legacy_dsd() with Disabled gain mode, no legacy gain, differing only in the margin value (0.15 vs 1.0), computes fingerprints, and asserts they're equal — confirming margin is ignored when disabled and legacy gain is absent.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `disabled_dsd_to_pcm_fingerprint_honors_legacy_gain_only_when_present`
- spec 3 · read at `e3c44cb00bb5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:51:45Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: This test compares fingerprints for settings where DSD-to-PCM conversion is disabled, varying the legacy gain field between Some(...) and None — asserting the resulting fingerprint changes when legacy gain is present (Some) but stays stable when it's absent (None), mirroring the sibling test that shows auto margin is ignored entirely when DSD-to-PCM is disabled.
- found: Builds four fingerprints with DSD-to-PCM disabled: no legacy gain, legacy gain Some(2.0), same gain with a different (stale) margin value, and a different legacy gain Some(3.0). Asserts presence of legacy gain changes the fingerprint, that margin changes are ignored when disabled, and that differing legacy gain values themselves produce different fingerprints.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `auto_dsd_to_pcm_fingerprint_includes_margin_and_ignores_manual_gain`
- spec 3 · read at `dff5feb719f9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:41:11Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test that constructs settings with dsd_to_pcm_gain_mode set to auto, varies the auto margin value and asserts fingerprints differ, then varies manual gain (irrelevant in auto mode) and asserts fingerprints stay equal — confirming the fingerprint only depends on fields that actually affect output in auto mode.
- found: Confirms exactly as predicted: base vs stale manual gain (same margin, differing manual gain) fingerprints equal; base vs changed margin fingerprints differ, using legacy_dsd(Auto, margin, manual_gain_option) helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `manual_dsd_to_pcm_fingerprint_includes_manual_gain_and_ignores_auto_margin`
- spec 3 · read at `e3fff815a1a9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:46:30Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds two settings structs with dsd_to_pcm_gain_mode set to manual, varying the manual gain value between them, and asserts their fingerprints differ; then builds two more with the same manual gain but different auto margin values and asserts the fingerprints are equal, confirming the fingerprint includes manual gain but ignores auto margin when in manual mode.
- found: Builds three fingerprints via legacy_dsd(Manual, ...): base and one with changed auto-margin value (asserted equal, i.e. ignored), and one with changed manual gain value (asserted different, i.e. included).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/src/mapping.rs

### the file itself
- spec 3 · read at `936a2ddf9d7e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:18:03Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A pure-function utility module translating the domain enums (from enums.rs) into concrete CLI arguments/flags for each external tool (SoX, SSRC, FFmpeg, WavPack): things like resample quality to SoX rolloff/bandwidth/rate-quality flags, DitherType to SoX dither args or an approximated SSRC dither/ATH-curve ID (with explicit fallback notes when there's no exact native equivalent, especially rate-dependent SSRC dither availability), plus format/codec mapping helpers (FFmpeg PCM codec/sample format/AAC profile, Opus application, MP3/WavPack compression settings, DSD shaper names) — all deterministic, side-effect-free, and covered by inline unit tests validating specific mapping/fallback/clamping behaviors.
- found: Matches prediction well: pure deterministic mapping functions from domain enums to tool CLI args (SoXR precision, SoX rate/rolloff/bandwidth/dither flags, FFmpeg cutoff/PCM codec/sample format/AAC profile, Opus application, MP3/WavPack compression, DSD shaper name) plus, as predicted, a rate-aware SSRC dither approximation system with explicit user-facing fallback notes when a named shaper has no SSRC-native equivalent, and validation/clamping of the resulting ATH Curve A intensity against what's actually available at the destination sample rate. I underestimated how load-bearing that SSRC subsystem is — it's not a minor helper but has its own struct (SsrcDitherSelection), a two-stage id-then-validate API, and half the file's test coverage.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The header's "no ambient state, deterministic" claim covers the whole file honestly, but undersells that this is really two modules in one file: simple 1:1 enum-to-flag tables, and a much more involved rate-dependent SSRC dither resolution/validation subsystem with its own error type usage.

### `soxr_precision`
- spec 3 · read at `a136bd9d67f3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:33:17Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const match over ResampleQuality variants returning fixed u8 precision values, with the Ultra variant returning 33 (max ffmpeg SoXR precision) and other tiers returning progressively lower standard precision values (e.g. 16, 20, 28).
- found: Match over ResampleQuality: Insane|Ultra=>33, VeryHigh=>28, High=>24, Medium=>20, Low=>16. Got the shape and Ultra=33 right; missed that Insane also maps to 33 and the exact intermediate values (24 not guessed, exact 16/20/28 order was close).
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `sox_rate_quality_flag`
- spec 3 · read at `03e00121d60e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:53:43Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A const match over ResampleQuality variants (e.g. Low/Medium/High/VeryHigh or similar) returning the corresponding SoX "rate" effect quality flag string (like "-q", "-l", "-m", "-h", "-v") used to build the SoX command line for resampling.
- found: Const match over six ResampleQuality variants (Insane, Ultra, VeryHigh, High, Medium, Low) each returning its SoX rate-effect flag string (-u, -v, -h, -m, -l, -q).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sox_dsd_auto_rate_flag`
- spec 3 · read at `2455108615d6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:56:02Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Returns the constant string literal "-v" (SoX's very-high-quality rate conversion flag) with no branching logic, since it's a const fn and the doc says it maps DSD auto presets to that fixed flag.
- found: Returns the constant string literal "-u" (SoX's very-high-quality rate flag), no branching.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `sox_dsd_lowpass_rate_flag`
- spec 3 · read at `ae915689f8fc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:54:11Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Since DSD paths always use SoX's undocumented "-u" ultra quality flag per the doc comment, this likely just returns "-u" unconditionally, ignoring both the lowpass method and the (deliberately unused, underscore-prefixed) quality parameter — the quality param exists only to keep this function's signature symmetric with sibling mapping functions like sox_rate_quality_flag that do use it.
- found: Matches on lowpass method but every arm (Auto, SoxUltra, Sinc) returns "-u", so it's effectively unconditional as predicted; quality is unused.
- predicted: full · documented: most · derivable: no · legible: full · trap: no
- note: The exhaustive match with three identical arms (rather than a bare unconditional return) hints the function is a deliberate seam for a future divergent Sinc flag, which isn't visible from the signature alone.

### `ffmpeg_cutoff`
- spec 3 · read at `d230b44449b5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:06:57Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const fn that matches on the NyquistTransition enum variants and returns a fixed f32 cutoff fraction (roughly 0.90-0.99 range) for each variant, used as FFmpeg's swresample/aresample cutoff parameter. No computation, just a lookup table.
- found: Const fn matching NyquistTransition variants (Gentle/Medium/Steep|Sharp|BrickWall) to fixed f32 cutoff values 0.95, 0.97, 0.997 respectively.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `sox_rolloff`
- spec 3 · read at `5849045ffb09` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:01:09Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A const-fn match over NyquistTransition variants (e.g. Sharp, Medium, Slow) returning a static string literal representing the rolloff fraction (like "0.99") for each named transition steepness, with None for a variant that doesn't map to an explicit SoX rolloff (e.g. an Auto/Default case).
- found: Matches NyquistTransition variants (Gentle, Medium, Steep/Sharp combined, BrickWall) to static rolloff fraction strings, returning None for BrickWall since it has no fractional rolloff value.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sox_bandwidth_percent`
- spec 3 · read at `0b7ff1e4cb6c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:41:53Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A const fn match over NyquistTransition enum variants, returning Some("74")..Some("99.7")-style static percentage strings for named transition steepness levels, and None for a variant that means "use SoX default" (no -b flag emitted).
- found: Matches NyquistTransition variants to static SoX -b percentage strings: Gentle->95, Medium->97, Steep and Sharp both->99.7, and BrickWall->None (no flag emitted).
- predicted: most · documented: some · derivable: no · legible: full · trap: no
- note: Steep and Sharp share the same percentage (99.7), which isn't obvious from the enum names alone.

### `sox_dither_args`
- spec 3 · read at `a0517cfd01ab` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:23:36Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Matches on the `DitherType` enum and returns a Vec<String> of SoX command-line arguments for the `dither` effect corresponding to that type — e.g. an empty vec or a flag like \"-D\"/\"--no-dither\" for a none variant, and specific shaping flags (e.g. \"-s\", a shape name) for triangular/shaped dither variants.
- found: Match over 11 DitherType variants each returning specific \"dither\"/\"-s\"/\"-f <name>\" SoX arg combos; more variants than I guessed but same overall shape.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `soxr_dither_method` — QUIRKY
- spec 3 · read at `38dc891f9ca4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:43:21Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A match over DitherType variants returning Some("...") string literal for dither types SoXR supports (e.g. triangular/TPDF), and None for types with no SoXR equivalent (e.g. a "none" variant or shaper types specific to another tool like SSRC).
- found: Match over DitherType, mapping almost every variant (including None -> "none") to its SoXR string name; only Gesemann returns None since SoXR (mislabeled "no ffmpeg equivalent" in the comment) has no equivalent for it.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The inline comment says "no ffmpeg equivalent" on a function about SoXR mapping — likely copy-pasted from a sibling ffmpeg mapping function, which could confuse whoever edits this next.

### `new`
- spec 3 · read at `1bc3970ccd13` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:49:22Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Trivial const constructor: builds Self { dither_id, pdf_type } (or similarly named fields), directly assigning the two parameters with no validation or transformation, consistent with this module being pure/deterministic parameter mapping.
- found: Trivial const constructor directly assigning dither_id and pdf_type into the struct fields, no validation or logic.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_dither_approximation_note`
- spec 3 · read at `fc2ec99cf5ce` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:13:50Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A const fn that matches on dither and returns Some("...") with a human-readable explanation for the specific DitherType variants that get approximated (e.g. Shibata-family mapped to ATH curve A, or other named shapers falling back to a conservative shape), returning None for dither types that map natively/exactly to an SSRC option.
- found: Matches DitherType: None/Tpdf return None; SlopedTpdf, Shibata family, and other named shapers (Lipshitz, FWeighted, etc.) each return a specific explanatory Some(&str) about the SSRC-native approximation used.
- predicted: full · documented: full · derivable: no · legible: full · trap: no

### `ssrc_dither_selection_is_approximation`
- spec 3 · read at `b789ff1d424f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:59:47Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A const fn matching on DitherType: returns false for dither types that have an exact native SSRC equivalent (e.g. plain TPDF/no-shaper, Shibata-family mapped to ATH curve A), and true for the remaining variants that don't have a direct SSRC mapping and are approximated via a fallback/conservative shape.
- found: Delegates to ssrc_dither_approximation_note(dither) and returns true if it yields Some (i.e. there's an approximation note explaining a non-native mapping), false if None.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `ssrc_dither_selection`
- spec 3 · read at `ac9c2d1469c5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:09:57Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: const fn match over DitherType returning the corresponding SsrcDitherSelection (SSRC --dither/--pdf pair). Natively supported dither families map directly; others without a native SSRC equivalent fall back to an approximate SSRC shape (e.g. an ATH curve) paired with triangular PDF.
- found: Const match: None -> ath id 99/no pdf; Tpdf/SlopedTpdf -> ath 99 + Triangular pdf; Low/normal/High Shibata -> ath 0/2/6 + Triangular; all other named shapers (Lipshitz, FWeighted, ModifiedEWeighted, ImprovedEWeighted, Gesemann) fall back to ath 0 + Triangular as the conservative approximation.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `ssrc_dither_id` — QUIRKY
- spec 3 · read at `1ab03e93ac91` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:01:53Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A const match over DitherType variants returning the corresponding SSRC numeric dither/noise-shaping ID as a u8 literal, independent of sample rate (the legacy, rate-unaware mapping the docs warn against preferring).
- found: Delegates to ssrc_dither_selection(dither).dither_id rather than doing its own match — reuses the richer selection function and just extracts the ID field, consistent with the docs' framing but a single delegation rather than an independent literal table.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `ssrc_dither_id_available_for_rate` — QUIRKY
- spec 3 · read at `fcf35e7c70c1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:38:20Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Returns true immediately if dither_id is 98 or 99 (rate-independent). Otherwise it looks up the rate-specific ATH-shaped dither id for target_rate_hz (probably via ath_curve_a_id_for_rate or a match on known sample rates) and returns true only if dither_id equals that rate's shaped id, false for unlisted rates.
- found: After the 98/99 shortcut, matches target_rate_hz against a fixed set of known rates (44100, 48000, 88200/96000/192000, 8000/11025/22050) each with its own explicit valid id-range set via `matches!`, and returns false for any unlisted rate — a direct lookup table, not a delegated single-id comparison.
- predicted: some · documented: full · derivable: no · legible: full · trap: no

### `validate_ssrc_dither_id_for_rate`
- spec 3 · read at `a59cdcbf4d47` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:23:36Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Calls ssrc_dither_id_available_for_rate(dither_id, target_rate_hz); if it returns false, returns an Err with a message naming the unsupported dither id and rate; otherwise returns Ok(()).
- found: Exactly as predicted, using PlanningError::invalid_settings with field name "ssrc.dither_id" for the error case.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_dither_selection_for_rate` — QUIRKY
- spec 3 · read at `e225c4dba5d8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:26:15Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Matches on DitherType: TPDF maps to a no-shaper/triangular-pdf pair; named Shibata-style variants map to an ATH Curve A ID looked up for target_rate_hz via a helper, clamping the intensity down to the strongest available ID if the requested one isn't valid at that rate rather than erroring. Returns Ok(SsrcDitherSelection::new(...)) for supported cases and an error for unlisted/unsupported rates.
- found: Matches DitherType: LowShibata/Shibata/HighShibata map to ATH Curve A IDs 0/2/6 respectively at the target rate with triangular PDF; the other named non-native shapers (Lipshitz, FWeighted, ModifiedEWeighted, ImprovedEWeighted, Gesemann) all fall back to ATH Curve A ID 0 with triangular PDF too; None/Tpdf/SlopedTpdf delegate to ssrc_dither_selection(dither) directly. After the match, calls validate_ssrc_dither_id_for_rate on the chosen id before returning Ok.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `ath_curve_a_id_for_rate`
- spec 3 · read at `dfa7ddac1d73` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:03:12Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Looks up the set of ATH curve A intensity IDs available at target_rate_hz, and clamps requested_intensity to the maximum available value for that rate if it exceeds it (per the "clamps_to_available_ath_intensity" peer test), returning Ok(clamped_id). Returns an Err if target_rate_hz isn't a recognized/supported sample rate for this shaping curve.
- found: Matches target_rate_hz against grouped rate buckets to get a max_intensity (6 for 44.1/48k, 2 for higher rates, 1 for lower rates), returns Err for unrecognized rates, else clamps requested_intensity via min() — matched exactly.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_tpdf_maps_to_no_shaper_with_triangular_pdf`
- spec 3 · read at `418db528b3e5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:49:29Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A unit test that calls a mapping function like ssrc_dither_selection with input "tpdf" and asserts the resulting SsrcDitherSelection has no shaper (shaper = None) and dither method = TriangularPdf, verifying SSRC's plain "tpdf" dither name maps to no noise-shaping curve plus triangular PDF dithering.
- found: A unit test asserting ssrc_dither_selection(DitherType::Tpdf) returns SsrcDitherSelection::new(99, Some(SsrcPdfType::Triangular)) — confirming plain TPDF dither maps to shaper id 99 (a sentinel, presumably meaning 'no shaper') with triangular PDF.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_shibata_family_maps_to_ath_curve_a_with_triangular_pdf`
- spec 3 · read at `e15776305df5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:43:56Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test that calls ssrc_dither_selection (or similar mapping function) with a Shibata-family dither input, then asserts the returned SsrcDitherSelection has ATH curve "A" and a triangular PDF, verifying the pure mapping table's behavior for that specific family.
- found: Asserts ssrc_dither_selection for LowShibata, Shibata, and HighShibata each map to specific numeric ATH ids (0, 2, 6) paired with SsrcPdfType::Triangular.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `unsupported_named_shapers_fall_back_to_conservative_ath_shape`
- spec 3 · read at `f057759b9c23` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:07:49Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test that calls the dither/shaper mapping function with one or more shaper names that aren't natively supported by SSRC, and asserts the mapping falls back to a conservative ATH curve (like ath_curve_a) rather than failing or picking an aggressive shaper, verifying safe default behavior for unrecognized/unsupported shaper requests.
- found: Test asserting that Lipshitz and Gesemann dither types (unsupported named shapers in SSRC) map to selection id 0 (no shaper) with triangular PDF, rather than to a specific ATH-curve shaper id.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_approximation_notes_are_explicit_for_non_native_mappings` — QUIRKY
- spec 3 · read at `a46cda38856f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:28:45Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Unit test: calls ssrc_dither_selection (or ssrc_dither_selection_for_rate) with a few non-native shaper names, and asserts each result carries an explicit approximation note (e.g. Option is Some / a specific string), verifying that non-native mappings are never silently marked as exact.
- found: Asserts native dither types (None, Tpdf) have no approximation note, while several named shapers (SlopedTpdf, Shibata, HighShibata, Lipshitz, F/E-weighted variants, Gesemann) are flagged as approximations via ssrc_dither_selection_is_approximation, and checks the actual note text for Lipshitz mentions 'does not expose' / 'ATH Curve A'.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Uses two distinct helper functions (ssrc_dither_approximation_note returning Option<&str> text, and ssrc_dither_selection_is_approximation returning bool) not visible from the peer list names I guessed at.

### `ssrc_dither_id_availability_is_rate_dependent`
- spec 3 · read at `ccbdf018bed6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:33:02Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: This test picks an SSRC dither ID that maps to an ATH-curve shaped dither and checks ssrc_dither_id_available_for_rate for multiple sample rates, asserting it returns true for some rates (e.g. 44.1/48kHz) and false for others (e.g. higher rates) where no matching ATH curve entry exists.
- found: Checks ssrc_dither_id_available_for_rate across several dither IDs (16, 6, 9, 98, 99) and rates (22050 to 176400), asserting availability differs by rate per ID — e.g. id 16 available at 44.1/48kHz but not 96kHz, id 9 available at 22050 but not 44100 — confirming availability is a per-ID, per-rate lookup rather than uniform.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `rate_aware_ssrc_shibata_mapping_clamps_to_available_ath_intensity`
- spec 3 · read at `c13df9d2170b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:19:11Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A #[test] that requests an SSRC shibata-family dither mapping at a sample rate where the requested ATH curve intensity isn't natively available, then asserts the mapping function (likely ssrc_dither_selection_for_rate or similar) clamps to the nearest/highest available intensity for that rate rather than returning an error or an unsupported value.
- found: Asserts ssrc_dither_selection_for_rate returns specific clamped SsrcDitherSelection ids (6, 2, 1) with Triangular PDF for HighShibata/Shibata dither types at different sample rates (44100, 96000, 22050), confirming the ATH intensity id varies (clamps) by rate rather than staying fixed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `rate_aware_ssrc_shaped_mapping_rejects_unlisted_rates`
- spec 3 · read at `11b0c0f064fd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:14:09Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Short unit test that calls the rate-aware SSRC dither/shaping mapping function with a sample rate not in the supported list, and asserts it returns None (or an equivalent rejection) rather than producing a bogus mapping.
- found: Asserts that Shibata dither at 176,400 Hz is rejected (Err) since that shaped dither isn't listed for that rate, while Tpdf (unshaped) dither at the same rate succeeds (Ok) — confirming rejection is specific to shaped dither types, not the rate generally.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: I guessed the general rejection behavior correctly but returned a Result-based ssrc_dither_selection_for_rate rather than the named "mapping" function, and missed that the test also confirms Tpdf succeeds at the same rate to isolate the shaped-vs-unshaped distinction.

### `ssrc_profile`
- spec 3 · read at `79d443a8ac8c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:48:41Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks if settings has an explicit profile set and returns that directly; otherwise if insane mode is on, returns the highest-quality/"insane" SSRC profile regardless of quality; otherwise maps the generic ResampleQuality enum to a corresponding SsrcProfile variant.
- found: Checks settings.insane_mode first and returns SsrcProfile::Insane if set; then checks settings.profile for an explicit override; otherwise maps each ResampleQuality variant to a specific SsrcProfile via a match (Insane->Insane, Ultra->High, VeryHigh->Long, High->Standard, Medium->Short, Low->Fast).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_pcm_codec`
- spec 3 · read at `a3d9f1260636` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:51:05Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Maps a PcmBitDepth and AudioFormat to an ffmpeg PCM codec name string (e.g. "pcm_s16le", "pcm_s24le", "pcm_u8"), branching on bit depth and possibly on container endianness (e.g. AIFF being big-endian vs WAV little-endian), returning Err for unsupported combinations.
- found: Maps PcmBitDepth + AudioFormat to an ffmpeg codec name string, using AIFF to determine big-endian vs little-endian suffix, and erroring only when a float depth is requested for a format that doesn't support float PCM (checked via supports_float helper).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `supports_float`
- spec 3 · read at `d75f4ca4503e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:07:28Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A simple match/pattern over the AudioFormat enum returning true for container formats known to support floating-point PCM samples (e.g. WAV, AIFF/AIFC, possibly CAF), and false for formats that don't (e.g. FLAC, MP3, ALAC, WavPack).
- found: matches! against Wav, Aiff, and WavPack, returning true for those three formats only.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_sample_fmt` — QUIRKY — TRAP
- spec 3 · read at `18cf5f334943` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:55:13Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A const fn that matches on PcmBitDepth variants and returns the corresponding FFmpeg sample format string literal (e.g. "u8" for 8-bit, "s16" for 16-bit, "s24" for 24-bit, "s32" for 32-bit).
- found: Matches PcmBitDepth to FFmpeg sample format strings: Int8→"u8", Int16→"s16", both Int24 and Int32→"s32" (24-bit is packed into a 32-bit container, not "s24"), Float32→"flt", Float64→"dbl".
- predicted: some · documented: none · derivable: yes · legible: full · trap: yes
- note: Int24 and Int32 both map to "s32" — a reader might assume Int24 gets its own "s24" format; nothing in the signature hints float variants exist too.

### `ffmpeg_aac_profile`
- spec 3 · read at `a2bd4656b4d2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:16:08Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A simple const match over the AacProfile enum, mapping each variant to the corresponding FFmpeg -profile:a string literal (e.g. Low -> "aac_low", He -> "aac_he", HeV2 -> "aac_he_v2"), returning a &'static str with no other logic.
- found: Const match mapping AacProfile::LcAac/HeAac/HeAacV2 to "aac_low"/"aac_he"/"aac_he_v2" respectively.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `opus_application` — QUIRKY
- spec 3 · read at `2a21e0391337` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:04:44Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A const fn match over OpusContentType variants (likely Voip, Audio, LowDelay or similar) returning the corresponding FFmpeg/libopus -application string literal ("voip", "audio", "lowdelay").
- found: Matches OpusContentType: Auto and Music both map to "audio", Speech maps to "voip". No "lowdelay" variant exists as I'd guessed.
- predicted: some · documented: some · derivable: no · legible: full · trap: no

### `sox_mp3_compression`
- spec 3 · read at `c2b5c3fdea72` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:57:44Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Matches on Mp3Mode: for a CBR/bitrate mode returns the bitrate_kbps formatted as a positive number string, and for VBR mode returns the vbr_quality formatted as a negative number string (SoX's -C convention: positive values select CBR bitrate, negative values select VBR quality level).
- found: Matches on Mp3Mode with three arms: Cbr returns bare bitrate string, Abr returns bitrate prefixed with '~', Vbr returns vbr_quality prefixed with '-'. I missed the Abr arm entirely.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `wavpack_compression_level`
- spec 3 · read at `d64c48d33c63` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:12:53Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A match over WavPackMode variants returning a fixed u8 compression level for each (e.g. Fast -> 0/1, Normal -> some middle value, High/VeryHigh -> higher numbers), used as the FFmpeg -compression_level argument for WavPack encoding.
- found: Match over WavPackMode variants (Fast/Normal/High/VeryHigh) returning 0/1/2/3 respectively.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `wavpack_mode_flag`
- spec 3 · read at `9b4f2079cfc8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:18:10Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A match over WavPackMode variants, returning the corresponding native wavpack CLI flag as a static &str (e.g. "-f" for Fast, "-h" for High, "-hh" for VeryHigh, "-x"/"-xx" for Extra), one arm per variant with no other logic.
- found: A match over WavPackMode variants returning the native wavpack CLI flag: Fast->"-f", Normal->"" (no flag, i.e. default mode), High->"-h", VeryHigh->"-hh". No Extra/hybrid variant existed as I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sox_sinc_phase_flag`
- spec 3 · read at `d918caa84f98` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:21:13Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A pure match over SoxSincPhase variants that returns the corresponding SoX CLI flag string literal for each phase response type (e.g. linear, minimum, intermediate phase flags like "-steep"/"-linear"/"-minphase").
- found: A const match mapping SoxSincPhase::Linear/Minimum/Intermediate to the single-letter SoX CLI flags "-L"/"-M"/"-I" respectively.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_shaper_name`
- spec 3 · read at `a785c607b9e7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:34:10Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Matches on the DsdNoiseShaper enum variant to pick a base shaper name string (e.g. "clans"), then formats it together with the numeric value of ModulatorOrder into a "<name>-<order>" string like "clans-8" for passing to SoX.
- found: Matches shaper variant (Clans/Sdm/Crfb) to a prefix string, formats as "{prefix}-{order.value()}", matching my prediction closely including exact variant names guessed for Clans.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `requires_sox_dither`
- spec 3 · read at `e1a2473f188e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:55:08Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A const function that matches on the DitherType enum and returns true for the variant(s) not natively supported by FFmpeg's SoXR resampler (so dithering must be delegated to SoX instead), false for variants SoXR can handle directly.
- found: Matches on three specific DitherType variants (Lipshitz, Gesemann, SlopedTpdf) and returns true for those, false otherwise; I got the shape right but not which/how many variants.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

## tonepoet-pipeline/src/plan.rs

### the file itself
- spec 3 · read at `7adc28a14f41` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:20:41Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Core deterministic planner turning a PlanRequest (source + target format/settings) into a ConversionPlan: an ordered sequence of PlanSteps/PlannedCommands to execute (encode, resample, dither, metadata transfer, verification, cleanup). Builds PlanContext (intermediate/final work paths), decides between passthrough/stream-copy and full re-encode by comparing requested vs source codec/rate/depth, branches into DSD-specific vs PCM-specific planning (plan_to_dsd/plan_from_dsd/plan_from_pcm), prunes redundant metadata-transfer steps using the tool registry's typed metadata effects, validates request semantics (container extensions, atomic work paths), and includes a large embedded unit-test suite covering edge cases like bit-depth resolution, AAC/ALAC container-extension rules, DSD reference admission, and metadata pruning correctness. Largest and most central file in the pipeline crate.
- found: Pure, side-effect-free planner turning PlanRequest (paths, probed SourceInfo, PipelineSettings) into a ConversionPlan (ordered PlannedExecutionSteps or a passthrough copy). plan_conversion_with_registry first checks for reference-DSD-to-PCM admission and delegates entirely to a separate plan_reference_dsd path if so; otherwise plan_topology validates, detects passthrough vs stream-copy-only (metadata rewrite without re-encode) vs full conversion, branches DSD-out/DSD-in/PCM via plan_to_dsd/plan_from_dsd/plan_from_pcm, appends post-processing (metadata/MD5/replaygain/verify), then prune_redundant_metadata_steps walks steps tracking per-path MetadataPlanEffect and collapses redundant MetadataTransfer steps, rewriting downstream path references in-place, with strip-mode explicitly never pruned. Contains multiple #[cfg(test)] modules covering DSD reference admission, resolved bit-depth rejection (ALAC 32-bit etc.), AAC/ALAC container extension rules, and metadata pruning with a fake ToolPlugin.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no
- note: The reveal payload exceeded the tool's context budget and had to be read via a subagent summary rather than directly.

### `selects_reference_dsd_to_pcm`
- spec 3 · read at `e70f8afac321` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:37Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Returns true only when source_is_dsd is true AND the settings indicate the native-v2 Reference DSD decoder/pathway is enabled (e.g. a settings flag or engine selection field equals "Reference" or similar) — a simple boolean AND/match gate with no side effects.
- found: Returns source_is_dsd && settings.dsd.is_native_v2() && target format is not itself DSD (i.e. only applies when actually converting DSD down to PCM, not DSD passthrough/DSD-to-DSD).
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `context`
- spec 3 · read at `2ce94078feca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:00:07Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A method on PlanRequest that constructs and returns a borrowed PlanContext, likely just wrapping self (and maybe a couple of its fields) into the PlanContext<'_> type so plugin-planning code can operate against a narrower, purpose-built view without owning the whole request.
- found: Wraps self in a PlanContext { request: self } struct literal — a trivial borrowing constructor, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `target_container_extension` — QUIRKY
- spec 3 · read at `60ce4f777f0d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:08:34Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A simple accessor that returns a clone of a stored container-extension field on the context/request (e.g. self.request.container_extension.clone()), not derived from the codec.
- found: Extracts the extension from the caller-chosen output_path, lowercases it, and falls back to a format-derived default extension (via default_container_extension_for_format) if the output path has no extension or it's blank.
- predicted: some · documented: some · derivable: no · legible: full · trap: no
- note: The doc explains the intent (respect caller's choice) but not that it's actually parsed from output_path with a codec-derived fallback when absent.

### `intermediate_path`
- spec 3 · read at `3f7865be2e1f` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:20:43Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds a deterministic filesystem path for an intermediate/temporary file used by a given step in the conversion chain, likely combining a working directory (from the PlanContext), some stable identifier, the step_index, and the given extension — e.g. work_dir/step_{step_index}.{extension} — so that repeated planning runs produce the same path.
- found: Picks a base dir (explicit intermediate_dir, else output_path's parent, else "."), takes the output path's file stem (defaulting to "tonepoet-output"), and joins a hidden dotfile name ".{stem}.tonepoet-stage-{step_index:02}.{extension}" — I got the general shape right but missed the specific hidden-dotfile naming convention and the stem-based fallback logic.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `final_work_path`
- spec 3 · read at `8ac1f924a602` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T06:13:51Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a temporary working path in the same directory as the eventual output path (required for atomic rename on the same filesystem), based on the output's file stem plus the target container extension, and appends some temp/partial marker (like a suffix such as ".part" or a random/pid-based token) so it doesn't collide with the real destination filename.
- found: Constructs a hidden dotfile-style temp path (.{stem}.tonepoet-final.{ext}) in the intermediate_dir if set, else the output's parent directory, else cwd, using the output file stem and target container extension.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `as_path`
- spec 3 · read at `3376c8830f60` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:04:44Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Matches on self (an InputSource enum), returning Some(&Path) for a path-backed variant and None for any non-path variants like in-memory data.
- found: Matches InputSource::Path to return Some(path), InputSource::Stdin to return None.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `as_path` #2
- spec 3 · read at `fe091de759c0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:09:45Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Matches on self, an OutputSink enum, returning Some(&Path) when the variant wraps a filesystem path and None for other variants (e.g. an in-memory or non-path-backed sink).
- found: Matches OutputSink: Path and InPlace variants both return Some(path), Stdout returns None.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `none`
- spec 3 · read at `be69f7e2c2e6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:13:59Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A const fn constructor returning a MetadataPlanEffect value that represents "no metadata effect" — likely a struct literal with all fields set to false/None/default (e.g. no tags added, no tags stripped, no cover art changes), used as the neutral/identity element for MetadataPlanEffect::merge.
- found: Struct literal with 5 named boolean fields all false, covering source tag/artwork transfer, tag/artwork preservation from command input, and source audio md5 writing — more specific fields than I guessed.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `merge`
- spec 3 · read at `4c0ec76d3ba5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:48:18Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: MetadataPlanEffect is a struct of boolean-like flags describing which metadata operations a plan step needs (e.g. tag rewrite, cover art, replaygain). merge combines self and other field-by-field with logical OR, so the result requires an effect if either side required it, and is a const fn since it's just simple boolean combination.
- found: Field-wise logical OR across five boolean flags tracking metadata provenance (tags/artwork transferred from original source vs preserved from command input, and whether source audio MD5 was written), producing a merged record where any flag true in either input stays true.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `new`
- spec 3 · read at `e3317ab75168` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:47:06Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A simple constructor that builds a PlannedCommand struct literal, assigning each parameter to a like-named field (tool, args, input, output, expected_duration), converting description via .into(), defaulting metadata_effect to MetadataPlanEffect::none(), and setting env to empty/None per the "no special environment" doc comment.
- found: Builds the PlannedCommand struct literal, setting fields to the params directly (tool/args/input/output/expected_duration), description.into(), metadata_effect defaulted via MetadataPlanEffect::none(), and env empty BTreeMap — but also sets an environment_policy field to CommandEnvironmentPolicy::InheritAndSet, which I did not predict and which somewhat contradicts the doc comment's 'no special environment' framing since it still inherits+sets rather than being fully neutral.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The doc comment 'no special environment' undersells that environment_policy is still explicitly InheritAndSet, not e.g. a bare Inherit — worth flagging for whoever writes docs next.

### `with_metadata_effect`
- spec 3 · read at `bce118952c9b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:46Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Simple builder-style setter: assigns the passed metadata_effect to a field on self (PlannedCommand), then returns self, enabling fluent chaining when constructing a PlannedCommand.
- found: Fluent builder setter: assigns metadata_effect to self.metadata_effect and returns self.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `output_path`
- spec 3 · read at `600108f1c1d5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:50:13Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Returns Some(path) if this step's output sink is path-backed, by delegating to OutputSink::as_path() on the step's output field; returns None if the sink is some other non-path kind (e.g. in-memory buffer or stdout).
- found: Matches over the four PlannedExecutionStep variants (Command, Pipeline, Measurement, DeferredCommand), each delegating to its inner command's output.as_path() to get an Option<&Path>.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `passthrough` — QUIRKY
- spec 3 · read at `47bdf90b4ad6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:39:48Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs a ConversionPlan consisting of a single PlanStep/PlannedCommand that copies the input file to the output path (possibly via work_path), with no real conversion happening. The reason string is stored/labeled on the plan or step for diagnostic/logging purposes, and metadata effect is likely none since nothing is transformed.
- found: Builds a ConversionPlan with a PlanAction::PassthroughCopy variant holding input/output/work_path, cleanup_paths for the work file, an AtomicRename finalization from work_path to output, and the reason string; reference is None.
- predicted: some · documented: some · derivable: no · legible: full · trap: no
- note: The one-line doc ("Create a passthrough-copy plan") tells you the intent but not that it's structured as a PlanAction enum variant with explicit Finalization/cleanup_paths fields rather than a generic step list.

### `execute`
- spec 3 · read at `51768ea6762a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:12:34Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Simple constructor that wraps commands and finalization into a ConversionPlan value representing an executable plan (likely an enum variant construction), with no significant additional logic beyond field assignment.
- found: Delegates to execute_with_cleanup(commands, Vec::new(), finalization) — a convenience constructor with no cleanup paths, rather than directly building the value itself.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `execute_with_cleanup`
- spec 3 · read at `9c1d39e0c107` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:48:34Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A constructor that builds a ConversionPlan::Execute-style variant (as opposed to ::passthrough) wrapping the given commands into a single execution step, storing cleanup_paths so the executor knows which temp/work paths to remove deterministically after running, and attaching the optional finalization step to run at the end.
- found: Constructs a ConversionPlan with PlanAction::Execute holding the given commands, an empty steps list, the cleanup_paths, and optional finalization; reference is left None.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `execute_steps_with_cleanup`
- spec 3 · read at `8da7d688c2db` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:23:36Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A straightforward constructor that packages the given steps, cleanup_paths, optional finalization, and reference DSD summary into Self (a ConversionPlan-like struct) — mostly field assignment despite the "execute" in its name; it likely doesn't actually execute anything itself.
- found: Constructs Self with action = PlanAction::Execute{commands: Vec::new(), steps, cleanup_paths, finalization} and reference = Some(reference); pure field packaging, no execution.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The one-line doc 'Create a measurement-aware Reference plan.' appears to belong to a different function/overload, not this one — mismatch.

### `commands`
- spec 3 · read at `90f285550665` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:15:44Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Simple accessor returning &self.commands (a Vec<PlannedCommand> field) as a slice. For a passthrough plan the vec is empty, matching the doc's "empty slice for passthrough" note. No computation, just a field borrow.
- found: Matches on self.action enum: PassthroughCopy variant returns empty slice, Execute variant returns its commands field as a slice.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I predicted a plain field access; actual implementation dispatches on a PlanAction enum with PassthroughCopy vs Execute variants, which the doc comment hints at but doesn't spell out.

### `steps` — QUIRKY
- spec 3 · read at `b2ee1f73cf85` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:20:48Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns a reference to the plan's measurement-aware execution steps, likely by unwrapping an internal Option<Vec<PlannedExecutionStep>> field to an empty slice when it's None — i.e. self.steps.as_deref().unwrap_or(&[]), since legacy plans (constructed before this field existed, e.g. via passthrough) never populate it.
- found: Matches on the plan's PlanAction enum: PassthroughCopy variants return an empty slice, Execute variants return their embedded steps field. "Legacy" turns out to mean "passthrough-copy plans," not an Option field left unset by older code.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Got the overall behavior (empty vs populated) right but guessed an Option<Vec> field instead of the actual mechanism, a match over a PlanAction enum with PassthroughCopy/Execute variants.

### `cleanup_paths`
- spec 3 · read at `0dbf86165466` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:26:18Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A simple accessor returning &self.cleanup_paths, a Vec<PathBuf> field on ConversionPlan populated during planning with intermediate/temp file paths that the executor should remove once the pipeline finishes (success or failure).
- found: Returns cleanup paths by matching on the plan's action enum (PassthroughCopy or Execute), extracting the cleanup_paths field from whichever variant applies, rather than a direct field access on ConversionPlan itself.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `label`
- spec 3 · read at `e9987f9af2cc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:58:33Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A match statement over the PlanOperation enum variants that returns a fixed, stable string identifier for each variant (e.g. "convert", "transcode", "metadata_transfer", "cleanup"), used for logging, diagnostics, or serialization where a stable name is needed independent of Debug formatting.
- found: A match over PlanOperation variants (DecodeToPcm, ResamplePcm, EncodePcm, EncodeLossy, PcmToDsd, DsdToPcm, DsdRateChange, MetadataTransfer, StoreSourceAudioMd5, ReplayGain, Verify) returning a fixed snake_case &'static str label for each.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `new` #2
- spec 3 · read at `74af5eb7de4a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:57:09Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A simple constructor that builds a PlanStep by storing index, operation, input, output, and description (converted into a String) as struct fields, with no additional validation or logic.
- found: Trivial constructor assigning all five parameters directly to struct fields, converting description via .into().
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `plan_topology`
- spec 3 · read at `255e469aebd2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:51:06Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Given a PlanRequest (source/target format info), determines the logical sequence of conversion steps (decode/filter/encode stages) needed to go from input to output as abstract PlanStep/PlanOperation entries, resolving intermediate formats and dependencies but not yet building argv command arrays. Returns a TopologyPlan, erroring if no valid conversion path exists between the given formats.
- found: Validates the request (settings, source, paths, semantics, post-processing inputs), then short-circuits to a Passthrough plan if nothing needs to change. Otherwise dispatches to one of several sub-planners (stream-copy-only metadata transfer, DSD target, DSD source, or PCM) to build a Vec of PlanSteps, appends post-processing steps, adds an AtomicRename finalization step, validates all step paths, and returns an Execute variant of TopologyPlan.
- predicted: most · documented: some · derivable: no · legible: most · trap: no

### `plan_conversion`
- spec 3 · read at `1102e056e888` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:41:28Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Thin wrapper that calls plan_conversion_with_registry(request, &built_in_registry()) (or similar), delegating to the registry-based planner with the crate's default/built-in command registry.
- found: Delegates to plan_conversion_with_registry using ToolRegistry::with_builtin_tools() as the registry, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `plan_conversion_with_registry` — QUIRKY
- spec 3 · read at `d4ad8be67795` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:56:11Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Orchestrates the other planner helpers: validates request paths and the requested container extension, computes the conversion topology (chain of steps) via plan_topology using the given ToolRegistry, prunes redundant metadata-transfer steps, records/resolves the original-source metadata effect, and collects cleanup paths for temp files. Assembles all of this into a ConversionPlan and returns it wrapped in Result, erroring out early if validation fails.
- found: Has a special-case early return for reference DSD-to-PCM conversions (plan_reference_dsd), then dispatches on plan_topology's result: Passthrough builds a passthrough ConversionPlan directly, while Execute prunes redundant metadata steps, builds a command per step via the registry, collects cleanup paths, and returns an execute_with_cleanup plan.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: I predicted the general orchestration shape but missed the DSD special-case branch and the Passthrough/Execute split on plan_topology's result.

### `prune_redundant_metadata_steps` — QUIRKY — TANGLED
- spec 3 · read at `aa4fef44e68e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:53:26Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: This scans the planned conversion steps and finalization for metadata-transfer operations, using metadata_transfer_required_effect/metadata_effect_satisfies_original_source_transfer to determine which are actually needed given the source/target formats, then removes (prunes) any metadata-tagging or transfer steps whose effect is already satisfied elsewhere (e.g., a later step already carries the same metadata), returning the trimmed step list and possibly-adjusted finalization.
- found: Walks the step list tracking per-path accumulated metadata effects; for each MetadataTransfer step it special-cases a "strip" (both transfer flags false) as never prunable, otherwise checks if the already-available metadata effect for that input already satisfies what the transfer would add — if so it removes the step and rewrites every later step's input/output paths (and any AtomicRename finalization) to splice around the removed step's now-skipped output file, rather than merely dropping redundant steps in place.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: The pruning isn't just deletion — it rewires downstream path references to skip the removed step's intermediate file, which is easy to miss from the name alone.

### `metadata_transfer_required_effect`
- spec 3 · read at `832b4799d507` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:55:22Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Matches on the PlanOperation variant and, for operations that need metadata carried over from the original source (e.g. a transcode/convert step), returns Some(MetadataPlanEffect) describing what metadata effect is required; for operations that don't need it (e.g. cleanup, validation-only steps) returns None.
- found: Only matches the single MetadataTransfer variant specifically (not any operation needing metadata generally), extracting transfer_tags and preserve_artwork flags into a MetadataPlanEffect; all other variants return None.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `metadata_effect_satisfies_original_source_transfer` — QUIRKY
- spec 3 · read at `e5c8991bcd77` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:43:06Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Compares two MetadataPlanEffect enum values to decide if the `available` effect (produced by an earlier plan step) is strong enough to fulfill the `required` effect for preserving original-source metadata. Likely implemented as a match/comparison where an effect that already carries full/complete metadata satisfies a lesser or equal requirement, returning a bool rather than requiring an exact enum equality.
- found: MetadataPlanEffect is a struct of two independent bool flags (source_tags_transferred_from_original_source, artwork_transferred_from_original_source); the function does a per-flag implication check (required implies available) rather than any enum-strength comparison.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `record_original_source_metadata_effect` — QUIRKY
- spec 3 · read at `9cad805c3df0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:48:14Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: This records, into a map keyed by file path, the metadata effect (e.g. tag/cover-art transfer) produced by a plan step — likely keyed by the step's output path — so a later pass (prune_redundant_metadata_steps) can look up whether a given path already has the original-source metadata applied and skip redundant metadata-copy steps. It probably merges with any existing entry rather than blindly overwriting, since two steps could touch the same path.
- found: Computes the metadata state for a step's output path and stores it in by_path (skipping if the step has no path output). It looks up the input path's already-recorded state, sets tags/artwork-transferred-from-original-source flags directly from the effect, but for "preserved from command input" flags it ORs in whatever the input path already had recorded (propagating provenance through the chain); if the step is in-place (input path equals output path, or an InPlace sink), it merges the new state with the input state instead of just propagating the preserved flags.
- predicted: some · documented: none · derivable: no · legible: most · trap: no

### `replace_input_path`
- spec 3 · read at `37d7a57e22e9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:26Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Checks whether input's current path equals `from`; if so, sets it to `to.to_path_buf()` (or similar), leaving it unchanged otherwise. Used to rewrite a plan step's input source when an earlier step's output path changes.
- found: InputSource is an enum; only matches the Path(path) variant, and if that path equals `from`, replaces it with to.to_path_buf(). Other InputSource variants (if any) are left untouched, and non-matching paths are untouched too — matches my prediction except I didn't know it was an enum with possibly non-Path variants.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `replace_output_path`
- spec 3 · read at `59cf77d9ad6c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:24:12Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Matches on the OutputSink variant(s) to find the path field(s) it holds, and if that path equals `from`, replaces it with `to` — a small in-place path substitution used when rewriting a planned step's output location, mirroring replace_input_path for inputs.
- found: Matches OutputSink::Path/InPlace, replacing the inner path if it equals `from`; Stdout variant is a no-op. Matches prediction exactly, including the OutputSink::Stdout case I inferred implicitly.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `collect_cleanup_paths`
- spec 3 · read at `9b5447672a33` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:15:23Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Walks the list of PlannedCommand steps, gathering every intermediate output path they produce, then filters out the paths that must survive: the requested_output path and whatever finalization's target path is (if present). Returns the remaining paths as the set of temporary/intermediate files that should be deleted after a successful (or failed) conversion run.
- found: Collects each command's output path (excluding the requested_output) into a deduped/sorted BTreeSet, and if finalization is an AtomicRename with a different from/to (and from isn't the requested output), adds the rename source too — returning the set as a Vec of paths to clean up.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default_container_extension_for_format`
- spec 3 · read at `0b9f47e98d59` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:31:21Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A match statement over AudioFormat variants returning the corresponding default file extension as a &str (e.g. "flac", "wav", "dsf", "dff"), used by the planner to pick an output file extension when none is explicitly requested.
- found: Special-cases AAC and ALAC to the shared "m4a" container extension (since both are typically muxed in MPEG-4 containers), and otherwise delegates to format.extension() for the default per-format extension — so most formats already have a self-describing extension() method rather than being enumerated here.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate_requested_container_extension`
- spec 3 · read at `bda08520c6cc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:43:46Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: This checks that the output path's file extension (from `request`) matches an expected/allowed extension for the requested output format/container — comparing against `default_container_extension_for_format` or a set of valid extensions for that format — and returns an error if the user-specified extension is wrong or unrecognized, otherwise `Ok(())`.
- found: Only validates extension for AAC (must be m4a/mp4, explicitly rejects .aac with a message explaining raw AAC isn't implemented) and ALAC (must be m4a/mp4); all other target formats pass unconditionally.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate_request_paths`
- spec 3 · read at `2697f64d5422` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:26:06Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Validates the input/output path fields on a PlanRequest before planning begins: checks that input and output paths are non-empty/well-formed, that output doesn't collide with input (same path), and possibly that any working/temp path doesn't collide with either. Likely returns an error via the crate's Result type if any of these checks fail, and may delegate to related helpers like validate_atomic_work_path for pieces of the check.
- found: Validates a PlanRequest's paths: rejects empty input_path, empty output_path, input==output (with a helpful message pointing to work-file+atomic-rename pattern), delegates extension validation to validate_requested_container_extension, and rejects an empty intermediate_dir if one was provided.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate_atomic_work_path`
- spec 3 · read at `ab9c7119add8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:08:31Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Checks that work_path (the temp file used for atomic rename into the final output) sits in the same directory as the request's output path — since atomic rename requires same filesystem/directory — and that it doesn't collide with the input or output path itself, returning an error via Result<()> if either invariant is violated.
- found: Just two equality checks: errors if work_path equals input_path (would overwrite input), and errors if work_path equals output_path (must differ from requested output). No same-directory/filesystem check.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `validate_step_paths` — QUIRKY
- spec 3 · read at `1bf82482ab93` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:07:13Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Validates that the chain of PlanStep input/output paths is internally consistent: the first step's input should match the request's source path, each subsequent step's input should match the previous step's output, and the final step's output should align with the finalization/destination path. Returns an Err (likely a plan-construction/invariant error) if any link in that chain doesn't match, and Ok(()) otherwise.
- found: For each step, validates that any output path is a deterministic atomic work path, and that in-place outputs never target the real request input/output paths. Then, if finalization is an AtomicRename, validates the rename source is an atomic work path and that the rename target equals the request's output_path.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `validate_request_semantics` — QUIRKY
- spec 3 · read at `9ab7e841acc3` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:57:18Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Given a PlanRequest, checks higher-level semantic consistency of the requested conversion (distinct from path validation) — e.g. that requested format/rate/depth/verify flags are mutually coherent — returning Err with a descriptive message if the request doesn't make sense, likely delegating to sibling helpers like flac_verify_requested or requested_rate_matches_source.
- found: Very narrow single-rule check: if the request forces SSRC resampling, it validates that source and target are both PCM (not DSD) and that there's an actual sample-rate change to perform, else returns an invalid_settings error. My prediction correctly guessed the general shape (semantic coherence check, Err on inconsistency) but expected broader coverage across multiple settings; it's actually just this one forced-SSRC guard.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `validate_post_processing_inputs`
- spec 3 · read at `3c2296194dc4` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:02:19Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Checks the PlanRequest for post-processing-related fields (e.g. loudnorm settings, FLAC verify flags) and returns an error if they're inconsistent or missing required companions — e.g. requesting a post-processing step that depends on a setting that wasn't provided, or combining options that are mutually incompatible. Returns Ok(()) if the request's post-processing inputs are internally consistent.
- found: Single check: if settings.metadata.store_source_audio_md5 is set but source.audio_md5 is None, returns a PlanningError::invalid_source; otherwise Ok. Much narrower than I predicted — I imagined multiple post-processing consistency checks, but there's exactly one specific dependency guard here.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `is_passthrough` — QUIRKY
- spec 3 · read at `b09444d0e0a1` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:56:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Calls into helper predicates like audio_content_matches_requested, requested_rate_matches_source, requested_depth_matches_source, and source_codec_matches_target, combining them with && to determine whether the requested output is identical to the source (so the plan can be a stream copy / no-op passthrough).
- found: Returns true when audio content matches the request, metadata handling is passthrough-safe, and no post-processing is required — combining three predicates with &&.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `metadata_passthrough_safe` — QUIRKY
- spec 3 · read at `484ba0425b9d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:17:39Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Returns true if the requested settings don't force any metadata-altering operations (e.g. no ReplayGain recalculation, no forced tag stripping/rewriting) so that source tags can be passed through to the output unmodified rather than requiring the pipeline to rewrite them — likely used alongside conversion_is_stream_copy_only to decide if a fast path is available.
- found: Returns settings.metadata.transfer_tags && settings.metadata.preserve_artwork — a simple two-flag AND check, not a broader inference over other settings.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `requires_post_processing`
- spec 3 · read at `561fe0283004` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:51:15Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks fields on the PlanRequest (e.g. normalize, tag/metadata write, verify flags) and returns true if any of them require an extra post-processing step beyond the core conversion. Likely a simple OR of several boolean/Option checks.
- found: Returns true if the plan requires storing source MD5, post-encode verification, FLAC-specific verification, or replay gain is configured — an OR of four specific settings checks rather than a generic loop over flags.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `flac_verify_requested`
- spec 3 · read at `c2c8401631ea` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:23:06Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Checks the PlanRequest to determine whether FLAC verification was requested, likely by checking if the target format is FLAC and a verify/checksum option flag is set true in the request's encoder settings, returning that boolean.
- found: Returns true only if target format is FLAC and request.settings.flac.verify is set.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `conversion_is_stream_copy_only` — QUIRKY
- spec 3 · read at `8dca9f7c3b40` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:33:11Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A 4-line boolean function that ANDs together several of its sibling predicate helpers (likely source_codec_matches_target, requested_rate_matches_source, requested_depth_matches_source, and encoder_settings_allow_stream_copy) to determine whether the requested conversion can be satisfied purely by copying the existing audio stream without any re-encoding.
- found: Returns true when the audio content already matches what was requested AND either metadata passthrough is unsafe or post-processing is required — i.e. this identifies the case where the stream itself can be copied but something else (metadata rewrite or post-processing) still needs to happen, distinguishing it from a pure full passthrough.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: I expected a simple conjunction of codec/rate/depth-match helpers; the actual logic composes audio_content_matches_requested with a negated/OR condition distinguishing 'stream copy plus extra work' from full passthrough, which isn't obvious from the name alone.

### `audio_content_matches_requested` — QUIRKY
- spec 3 · read at `0c15506e3db8` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:39:20Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Combines the sibling predicates requested_rate_matches_source, requested_depth_matches_source, and source_codec_matches_target with a boolean AND, returning true when the source's actual audio content already matches everything the request asks for — signaling no transcoding of the audio stream is needed.
- found: Series of early-return false checks: force_encode flag, dither needing a depth change, target format mismatch, codec mismatch, encoder settings disallowing stream copy, and rate/depth mismatch; only true if source content already satisfies every requested constraint.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `requested_rate_matches_source`
- spec 3 · read at `02138d863946` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:23:06Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the requested target sample rate in PlanRequest equals the source file's sample rate (or is unspecified/None, meaning "no change requested"), returning true if no resampling is actually needed. Used alongside similar 'matches_source' helpers (depth, codec) to decide whether steps can be skipped/stream-copied.
- found: Matches on RateTarget enum: Source is always a match, PcmHz compares against source.sample_rate_hz, and Dsd compares against source.dsd_rate() — a three-way match I only partly anticipated (missed the DSD variant).
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `requested_depth_matches_source`
- spec 3 · read at `c1287207a065` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:37:54Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Compares the requested output bit depth in request to the source file's bit depth (from request's source metadata), returning true if they're equal. Used alongside requested_rate_matches_source and source_codec_matches_target to decide whether the conversion can be a stream copy / passthrough rather than a re-encode.
- found: Matches on target_bit_depth: BitDepthTarget::Source always returns true (source depth is definitionally requested), while BitDepthTarget::Pcm(depth) checks that the source's optional bit_depth equals the requested depth.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `source_codec_matches_target`
- spec 3 · read at `d66f47bb1106` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:13:35Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Compares the source file's codec/format against the requested target codec, returning true if they're the same codec family — used to help decide whether encoding can be skipped in favor of a stream copy/passthrough.
- found: Matches per-target-format: FLAC/WavPack/ALAC require exact codec match; WAV/AIFF accept any PCM variant with compatible sample_kind; DSF/DFF require source.is_dsd(); lossy formats (MP3/AAC/Opus/DTS/AC3) and Custom formats always return false, forcing re-encode since equality can't be proven for user-controlled rate-control settings or plugin-owned semantics.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Lossy and Custom targets deliberately always return false, not because source never matches, but because SourceInfo can't prove the settings are equivalent — worth knowing before assuming this is a pure codec-identity check.

### `encoder_settings_allow_stream_copy` — QUIRKY
- spec 3 · read at `c2a0aa53652e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:07:44Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Checks fields on PipelineSettings (quality, bitrate, compression level, bit depth, sample rate, dither, etc.) and returns true only if none of them are set to values that would force re-encoding, i.e. all are None/default so a plain stream copy is possible.
- found: Matches on settings.target_format: FLAC allows stream copy only if compression_level is default; WavPack only if settings are entirely default; WAV/AIFF/ALAC/DSF/DFF always allow it; all lossy formats (MP3/AAC/Opus/DTS/AC3/Custom) never allow it.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `plan_to_dsd` — QUIRKY
- spec 3 · read at `c6a999a76ac5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:42:57Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds the plan steps needed to produce a DSD target: checks if the source can simply be stream-copied/passed through (via is_passthrough/conversion_is_stream_copy_only-style checks), and if not, pushes a final DSD encode step onto `steps`, updates `current_input` to reflect the new work file, and appends any needed post-processing or metadata-transfer steps.
- found: Resolves the target DSD rate, then constructs either a DsdRateChange operation (if source is already DSD) or a PcmToDsd operation (otherwise), pushes that single step onto the plan, and updates current_input to point at the final work path.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `plan_from_dsd` — QUIRKY
- spec 3 · read at `a4dd06d98860` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:38:41Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Called when the source audio is DSD; appends the appropriate PlanSteps to `steps`, possibly delegating to plan_to_dsd if the target is also DSD/stream-copyable, or otherwise decoding/converting DSD to PCM, updating current_input, and pushing a final encode step plus metadata transfer/post-processing steps based on the target requested in `request`.
- found: Resolves target PCM sample rate (from explicit target, source-derived DSD default, or errors on RateTarget::Dsd) and target bit depth, rejecting unsupported depths. If the target format is sox-encodable lossless PCM and not one of the format/depth combos sox silently substitutes, it pushes a single DsdToPcm step straight to the final output. Otherwise it converts DSD to a WAV intermediate first, then calls push_encode_final to produce the final encoded output from that intermediate.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: A comment cites internal spec IDs (D1/D4) for why sox-substitution combos must route through WAV — worth knowing if editing the substitution list.

### `plan_from_pcm` — QUIRKY
- spec 3 · read at `48f3ef50bfa8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:19:08Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the PCM source can be handled as a stream copy (codec/rate/depth already match target and encoder settings allow it); if so, skips transcoding steps. Otherwise resolves the target bit depth and sample rate (rejecting unsupported resolved depths), pushes resample/dither/encode steps via push_step/push_encode_final, then calls append_post_processing and push_metadata_transfer as needed, mutating steps and current_input, returning Ok(()) or a planning error.
- found: Resolves the target rate/depth, then branches three ways: (1) if brick-wall/forced SSRC resampling is needed, decode to float64 PCM, run SSRC resample, then push_encode_final; (2) else if dithering requires a SoX-only preprocessing pass (because the target format can't be sox-encoded directly but is ffmpeg-encodable), encode an intermediate WAV with processing applied, then push_encode_final; (3) otherwise just calls push_encode_final directly with the rate/depth/needs_processing flags — no separate metadata-transfer/post-processing calls appear in this function.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: No stream-copy short-circuit here despite peers like encoder_settings_allow_stream_copy existing — that decision must live in push_encode_final or a caller, not in plan_from_pcm itself.

### `push_encode_final` — QUIRKY
- spec 3 · read at `89a5f2a4c256` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:33:17Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Appends the final encoding step(s) to the `steps` plan, targeting `target_rate_hz`/`target_depth` at `final_work`. It likely checks whether a stream copy is possible (skipping actual encoding, just remuxing) versus needing a real encode, optionally calls append_post_processing when apply_processing is true, pushes the step(s) via push_step, and updates `current_input` to point at the new output file for subsequent steps. Returns Ok(()) or an error if the requested rate/depth/codec combination is unsupported.
- found: Branches on the target format category (lossy, PCM lossless, custom, or unsupported) and pushes the corresponding PlanOperation (EncodeLossy or EncodePcm) as a single step to final_work, with a special description string for true 32-bit FLAC via FFmpeg's experimental encoder. Returns an error for unhandled formats, and otherwise updates current_input to point at final_work.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: I incorrectly guessed a stream-copy short-circuit path existed here; it doesn't — that logic must live elsewhere (probably in the caller, given the sibling `encoder_settings_allow_stream_copy`/`conversion_is_stream_copy_only` peers).

### `append_post_processing` — QUIRKY
- spec 3 · read at `9c04b20acadd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:09:37Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: After the main encode step has been planned, checks whether metadata transfer is needed (via needs_metadata_transfer_step/metadata_policy_requires_command) and if so pushes a metadata-transfer PlanStep via push_metadata_transfer, updating current_output_path to reflect the step's output; may also append other post-encode steps (e.g. replaygain), mutating `steps` in place and returning Ok(()) or an error if a step can't be constructed.
- found: Conditionally appends up to four post-encode steps in sequence: metadata transfer (if needed), source-audio-MD5 storage (if configured), ReplayGain scan (if a mode is set), and a decode-to-verify step (if requested), each pushed in-place or to Stdout and threading current_output_path through.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `needs_metadata_transfer_step` — QUIRKY
- spec 3 · read at `c9eb65ae3674` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:41:42Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the planned conversion needs a separate metadata-transfer step appended, likely because the target codec/container (e.g. DSD/DSF) can't have tags embedded during encoding. It probably inspects request.target format/codec and maybe a metadata policy flag, ignoring the already-built steps list (hence the unused _steps parameter) and returning true when a standalone tagging step is required after encoding.
- found: Returns false if the conversion is a pure stream copy (no re-encode, so no separate metadata step needed); otherwise delegates to metadata_policy_requires_command(request) to decide if a standalone metadata-transfer step is required.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `metadata_policy_requires_command` — QUIRKY
- spec 3 · read at `b36c0b79a913` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:52:13Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Checks a metadata-policy field on `request` (e.g. request.metadata_policy) and returns true unless the policy is a "skip"/"none" variant that means no metadata transfer step is needed, i.e. it returns whether an external metadata-transfer command must be run as part of the plan.
- found: Returns true if either request.settings.metadata.transfer_tags or preserve_artwork is set, meaning some metadata transfer step is needed; a comment notes format-specific support is registry-owned and handled elsewhere.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: The inline comment explains a related registry-ownership subtlety not otherwise derivable from this function's body alone.

### `push_metadata_transfer`
- spec 3 · read at `8f9313ed6801` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:05:07Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds a metadata-transfer PlanStep (copying tags/metadata from the original input into the file at current_output_path) and appends it to `steps`, likely via the `push_step` helper, using `input` to determine the source of the metadata and `request` for any policy flags (e.g. strip mode) that affect how the transfer step is constructed.
- found: Wraps current_output_path as an OutputSink::Path and calls push_step with a PlanOperation::MetadataTransfer carrying target_format, transfer_tags, and preserve_artwork pulled from request.settings.metadata, plus a fixed description string.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `push_step`
- spec 3 · read at `13939af35570` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:40:04Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Constructs a PlanStep from the operation, input, output, and description arguments and pushes it onto the steps vector. Likely minimal logic beyond building the struct literal, perhaps assigning a sequential step index/id.
- found: Computes the next index from steps.len(), constructs a PlanStep via PlanStep::new with that index plus the given operation/input/output/description, and pushes it onto the vector.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `reject_unsupported_resolved_depth`
- spec 3 · read at `76ea2b023e32` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:04:36Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: reject_unsupported_resolved_depth checks the resolved PcmBitDepth against what the target AudioFormat's encoder actually supports (e.g., ALAC tops out at 24-bit) — after a BitDepthTarget::Source resolves against a 32-bit source, if the format is ALAC (or another encoder with a hard depth ceiling) and the resolved depth is 32-bit, it returns an error rather than allowing a silent downgrade to 24-bit; for supported depths/formats it returns Ok(()).
- found: Matches (format, depth) pairs and rejects three unsupported combinations with descriptive errors: ALAC+Int32, FLAC/ALAC+Float32/Float64, and WavPack+Float32/Float64; everything else returns Ok(()).
- predicted: most · documented: some · derivable: no · legible: full · trap: no
- note: The docstring only motivates the ALAC 32-bit case; the function actually guards three separate format/depth combinations.

### `resolve_target_bit_depth` — QUIRKY
- spec 3 · read at `3cf143466103` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:12:51Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Looks at the PlanRequest for an explicit target bit-depth override; if present, validates it via reject_unsupported_resolved_depth and returns it. If absent, falls back to deriving a depth from the source (e.g. same as source PCM depth, or a fixed default like 24-bit when converting from DSD), returning an Err if the request's combination of source format and desired depth isn't supported by the pipeline.
- found: Matches on request.settings.target_bit_depth: if explicit BitDepthTarget::Pcm(depth), returns it directly; if Source, resolves based on the source's representation kind — non-PCM-lossless targets get the source's authoritative PCM depth or a format default, DSD/Lossy sources always get the format default, PCM sources require an authoritative depth (erroring if absent), and Unknown/Unspecified sources fail closed with an error asking for an explicit depth.
- predicted: some · documented: none · derivable: no · legible: most · trap: no
- note: The comment notes representation_kind() never actually returns Unspecified (it's pre-resolved), so that arm is dead-but-defensive, grouped with Unknown for exhaustiveness.

### `rate_change_for_pcm`
- spec 3 · read at `d3fce188558b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:34:16Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Compares the source PCM sample rate to the requested target sample rate in the PlanRequest, returning Some(target_rate) if they differ (so a resample step must be inserted) or None if the rate is unchanged.
- found: Matches on the request's target_sample_rate enum: Source or Dsd targets need no PCM rate change (None); a PcmHz target matching the source's existing rate is None; otherwise returns Some(hz) for the requested rate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `resolve_target_dsd_rate`
- spec 3 · read at `63ce7ba2258a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:36:55Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Examines the PlanRequest's target DSD rate settings — if the request specifies an explicit DSD rate (e.g. DSD64/128/256), it validates and returns that; otherwise it falls back to deriving the target rate from the source's native DSD rate, returning an Err if the resulting rate isn't supported by the pipeline.
- found: Matches on request.settings.target_sample_rate: an explicit DSD rate is returned directly; RateTarget::Source falls back to the source's own dsd_rate() or errors if the source isn't DSD (e.g. PCM-to-DSD requires an explicit target); RateTarget::PcmHz is always rejected since DSD targets can't use a PCM rate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_source`
- spec 3 · read at `74d5f614e64e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:12:46Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test helper that constructs and returns a SourceInfo value representing a DSD-format audio source (format/codec DSD, a typical DSD sample rate like DSD64, bit depth 1), used as fixture data for planner tests in this file.
- found: Test fixture constructing a SourceInfo for a DSF/DSD source: format Dsf, codec Dsd, sample_rate DSD64 Hz, bit_depth None, source_representation Dsd, sample_kind Dsd, 2 channels, other fields (duration, dsd_source_kind, audio_md5, true_source_depth) None.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `shared_reference_admission_requires_native_dsd_to_pcm` — QUIRKY
- spec 3 · read at `39cc002b9fd3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:44:51Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a plan/request involving a DSD source targeting PCM output and checks that "shared reference admission" (reusing an already-planned/shared intermediate step across tracks) is only allowed when the DSD-to-PCM conversion is a native path — asserting admission is granted for the native case and rejected/denied otherwise.
- found: Tests selects_reference_dsd_to_pcm across combinations: native DSD settings + FLAC target (true), native DSD + DSD target formats DSF/DFF (false), non-native DSD settings + FLAC (false), a DSD source detected purely by codec/sample_kind-less heuristic (true), and a PCM source (false).
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `alac_int32_resolved_from_source_is_rejected_at_plan_time`
- spec 3 · read at `bdcfdcdc6539` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:10:34Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A unit test that builds a plan request where the source format is ALAC with 32-bit integer depth and the target also resolves to int32, then calls the planner and asserts that planning fails/returns an error (rejected), because ALAC doesn't support encoding at 32-bit integer depth. Likely uses helper builders like resolve_target_bit_depth or reject_unsupported_resolved_depth and checks the resulting error variant/message.
- found: A unit test directly calling reject_unsupported_resolved_depth(ALAC, Int32) and asserting it returns an error whose message mentions "ALAC 32-bit"; the inline comment explains this guards against a BitDepthTarget::Source resolving to 32-bit after validation, since the settings validator only checks the Pcm target variant.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `honored_resolved_depths_pass` — QUIRKY
- spec 3 · read at `0f0edf025046` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:53:18Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A small helper (likely test support) that returns a list of bit-depth values considered valid/"honored" by the planner (e.g., 16, 24, 32), used by nearby tests such as reject_unsupported_resolved_depth to iterate over the set of depths that should be accepted versus rejected.
- found: A test that calls reject_unsupported_resolved_depth for four specific (format, bit-depth) pairs — ALAC/Int24, FLAC/Int32, WavPack/Int32, AIFF/Float32 — and expects each to succeed (.expect), confirming these combinations are accepted rather than rejected as unsupported.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `strip_mode_metadata_transfer_is_never_pruned` — OBSCURE — TRAP
- spec 3 · read at `db9ba65234d9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:03:30Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a plan request where the metadata pruning plugin is set to "strip mode" (strip all tags), runs the planner, and asserts that the metadata transfer step (added via push_metadata_transfer) is still present in the resulting plan's step list — i.e. strip mode governs which tags get carried over, but never removes the transfer step itself from the plan.
- found: Not a planner/plan-steps test at all: it computes the "required" metadata effect for a strip-mode MetadataTransfer operation (transfer_tags=false, preserve_artwork=false) and asserts that MetadataPlanEffect::none() vacuously satisfies that requirement via metadata_effect_satisfies_original_source_transfer — documenting, per the inline comment, that an empty effect is deceptively \"satisfiable\" so pruning logic elsewhere needs an explicit guard against treating strip mode as prunable.
- predicted: none · documented: none · derivable: no · legible: most · trap: yes
- note: The test name describes an invariant enforced by code elsewhere (an explicit guard, per the comment) — this test only proves the premise that makes that guard necessary, it does not exercise the guard or the pruner itself, which a reader needs to know to find where the actual protection lives.

### `id`
- spec 3 · read at `554bb314d769` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:28:12Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Returns a fixed/constant ToolIdentifier value identifying the MetadataPruningPlugin, likely a single enum variant or string-based identifier literal with no branching logic.
- found: Returns ToolIdentifier::Custom("metadata-pruning-test".into()) — a fixed value as predicted, but it's the Custom variant with a string tag rather than a plain enum variant, and this is evidently a test-only plugin impl (name suffix "-test").
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `supports` — QUIRKY
- spec 3 · read at `d145563a5589` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:36:28Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether `step` is a metadata-pruning operation this plugin can handle, inspecting the step's kind/tool field and returning ToolSupport::Supported if it matches, Unsupported otherwise. Since metadata pruning is likely done in-process (no external tool needed), it may just unconditionally return Supported regardless of `_context`.
- found: Matches on step.operation: EncodePcm and MetadataTransfer return ToolSupport::CANONICAL, everything else returns ToolSupport::UNSUPPORTED.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `metadata_effect`
- spec 3 · read at `25a814f23b9b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:59:40Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A trait-implementation method for MetadataPruningPlugin that inspects the given PlanStep (ignoring PlanContext, since it's unused) and returns a typed MetadataPlanEffect describing what this step does to metadata — e.g. matching on the step's action/tool kind to decide whether metadata is stripped, preserved, or transferred during this step of the conversion chain.
- found: Matches on the step's PlanOperation: for EncodePcm it returns a pre-stored self.encode_effect field; for MetadataTransfer it builds a MetadataPlanEffect from the transfer_tags/preserve_artwork flags (defaulting other fields via MetadataPlanEffect::none()); any other operation returns MetadataPlanEffect::none().
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `metadata_disposition`
- spec 3 · read at `80f2296336d2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:33:55Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Since both parameters are unused (_context, _step), this simply returns a fixed constant variant — likely MetadataDisposition::Prune (or similar) — reflecting that MetadataPruningPlugin's whole purpose is stripping metadata, so the disposition doesn't depend on context/step.
- found: Returns self.disposition — an instance field on the plugin rather than a hardcoded constant, so the disposition is configurable per plugin instance even though params are unused.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_command`
- spec 3 · read at `04c78b9754c3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:48:09Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Since metadata pruning is expressed via metadata_effect/metadata_disposition rather than an actual external command, this likely returns an Err (e.g. unsupported/unreachable) or builds a trivial passthrough PlannedCommand, ignoring the unused _context parameter.
- found: Builds a trivial PlannedCommand with no args (passthrough of step input/output/description) and attaches the plugin's metadata_effect via with_metadata_effect - matches my "trivial passthrough" guess but I didn't predict the explicit metadata_effect wiring.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `metadata_pruning_request`
- spec 3 · read at `6ab2a6ccb1e2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:33:28Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A test helper that constructs a default/minimal PlanRequest suitable for exercising the MetadataPruningPlugin in other tests in this file (e.g. pruning_ignores_legacy_metadata_disposition_without_typed_effect, pruning_uses_typed_original_source_effects). It likely sets a source path, target format, and some baseline options while leaving metadata-effect-related fields to be overridden by each calling test.
- found: Builds a full PlanRequest with WAV/PCM16 source info, FLAC target format, metadata.transfer_tags=true and preserve_artwork=false, and default/empty values for the remaining fields (paths, dirs, flags) — matching my prediction of a shared minimal PlanRequest helper for the pruning tests, though the specific source/target formats and field-by-field construction were more detailed than I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `plan_context_uses_requested_output_container_extension_for_work_paths`
- spec 3 · read at `d21cb2811b56` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:32:48Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Unit test constructing a plan/plan-context with a specific requested output container extension, then asserting that the generated work/intermediate file path(s) carry that same extension rather than a default or source-derived one.
- found: Unit test: builds a request targeting AAC with output_path track.m4a and an intermediate_dir, gets its plan context, and asserts target_container_extension() is 'm4a', that final_work_path() is 'work/.track.tonepoet-final.m4a', and that intermediate_path(2, ext) is 'work/.track.tonepoet-stage-02.m4a' — confirming work paths use the requested container extension with a dotfile-hidden, zero-padded staged naming scheme.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `plan_context_defaults_aac_to_m4a_when_no_extension_is_requested`
- spec 3 · read at `ed013ec44f53` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:28:19Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Test that builds a plan context for an AAC target without specifying an explicit output extension, then asserts the resolved/default output extension is "m4a" rather than "aac" or something else.
- found: Builds a pruning request with target_format AAC and an extensionless output_path, derives the plan context, and asserts target_container_extension() is "m4a" and final_work_path() is a dotfile-prefixed temp path (\".track.tonepoet-final.m4a\").
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Didn't anticipate the final_work_path naming convention (hidden dotfile with .tonepoet-final. infix) being asserted alongside the extension.

### `plan_rejects_aac_with_raw_aac_suffix_without_explicit_raw_mode`
- spec 3 · read at `53e5cec20904` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:15:41Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a plan request targeting AAC output with a ".aac" file extension but without explicitly setting a raw-AAC mode flag, then asserts that planning fails/rejects with an error indicating raw AAC output requires explicit opt-in (mirroring the sibling test plan_rejects_alac_with_non_mp4_suffix which rejects mismatched container/codec combos).
- found: Sets target_format to Aac with output_path track.aac, and asserts plan_conversion errors with a message containing 'raw .aac output is not implemented' — it's an unimplemented-feature rejection, not a flag-gated raw-mode opt-in as the test name suggested to me.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Test name says 'without explicit raw mode' but there's no raw-mode flag in the code path shown — the rejection is simply because raw .aac muxing isn't implemented yet.

### `plan_rejects_alac_with_non_mp4_suffix`
- spec 3 · read at `1391b4716bfa` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:45:24Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a plan request targeting ALAC output but with a requested output extension that isn't .m4a/.mp4 (e.g. .alac or .flac), calls the planner, and asserts it returns an error at plan time rejecting the mismatched container suffix, since ALAC must be packaged in an MP4 container.
- found: Sets target_format=Alac with output_path "track.alac", asserts plan_conversion errors with a message containing "ALAC output must use".
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `pruning_ignores_legacy_metadata_disposition_without_typed_effect`
- spec 3 · read at `fb988f785578` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:48:23Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A unit test that sets up a conversion plan/plugin where only the legacy `metadata_disposition` method returns a pruning-relevant value but the newer typed `metadata_effect` is absent/default, then asserts the planner does NOT prune metadata based on the legacy disposition alone — i.e. the typed effect is required for pruning to happen, so metadata passes through unpruned.
- found: Registers a plugin with a coarse legacy MetadataDisposition (WritesRequestedPolicy) but a no-op typed MetadataPlanEffect, plans a conversion, and asserts the metadata-transfer command is still present (not pruned) and its typed effect still shows source tags transferred from the original source — confirming the legacy disposition alone can't drive pruning.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `pruning_uses_typed_original_source_effects`
- spec 3 · read at `7c9fc90c88b7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:58:24Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Test: constructs a plan context/request where a MetadataPruningPlugin (or similar) reports a typed metadata_effect referencing the original source (e.g. an enum variant carrying the source's metadata effect) rather than a legacy string disposition, runs the planner's pruning logic, and asserts the resulting plan honors that typed effect — e.g. metadata from the original source is preserved/pruned as the typed effect dictates, contrasting with the legacy-disposition-without-typed-effect test which is ignored.
- found: Registers a MetadataPruningPlugin whose encode step reports a typed MetadataPlanEffect (source_tags_transferred_from_original_source=true) with a DoesNotWrite disposition, plans a conversion, and asserts the planner prunes the redundant metadata-transfer command down to a single command whose effect still reports the tags as transferred.
- predicted: most · documented: none · derivable: no · legible: most · trap: no

## tonepoet-pipeline/src/plugins.rs

### the file itself
- spec 3 · read at `a6657634f24e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:43:30Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: This file implements the built-in tool plugins referenced by the pipeline's planner: FfmpegPlugin, SoxPlugin, SsrcPlugin, LoudgainPlugin, MetaflacPlugin, FlacPlugin, each exposing id/supports/build_command (and some metadata_effect) to translate planning steps into actual external command-line invocations. The bulk of the file is helper functions building up ffmpeg/sox argument lists (resampling, PCM/DSD encode, dither, container/metadata flags) plus a large embedded test suite verifying exact command construction for dither, gain, and format-capability edge cases.
- found: Could not actually read the file body — sanity_reveal's output exceeded the display/Read token limits (single-line ~47k-token JSON string) and Bash access was denied in this environment, so slicing the saved dump wasn't possible. Grading is based on the peer list (FfmpegPlugin/SoxPlugin/SsrcPlugin/LoudgainPlugin/MetaflacPlugin/FlacPlugin methods, build_ffmpeg_*/build_sox_*/add_ffmpeg_*/add_sox_* helpers, and a large test suite on dither/gain/ssrc behavior) and file doc, which is consistent with the prediction made from those same peers.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no
- note: Environment limitation: could not read the actual function/file body content for this task due to output size exceeding Read's token cap and no Bash/grep access to slice it; grading reflects peer-list/doc consistency only, not verified source reading.

### `id`
- spec 3 · read at `2906bd274f34` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:41:03Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Returns a constant ToolIdentifier value representing ffmpeg, e.g. ToolIdentifier::Ffmpeg or ToolIdentifier::new("ffmpeg") — a fixed literal, no computation.
- found: Returns the constant ToolIdentifier::Ffmpeg literal, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `supports` — QUIRKY
- spec 3 · read at `8df9a49b432a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:17:39Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the requested PlanStep's operation (e.g. format conversion, transcoding, resampling) is something ffmpeg can perform given the context's input/output formats. Likely matches on step kind and returns a ToolSupport variant indicating full/partial/no support, possibly with a priority/confidence level so the planner can pick among multiple candidate plugins (ffmpeg being a broad fallback that supports many container/codec combos, unlike more specialized tools like Sox/Ssrc/Metaflac).
- found: Matches on the PlanStep's operation variant to return a graded ToolSupport level (CANONICAL/SUPPORTED/PREFERRED/UNSUPPORTED/FALLBACK) rather than a simple yes/no. Contains specific domain logic: decode is always canonical, brick-wall resampling is unsupported (ffmpeg can't do it), PCM encoding checks for a WavPack 24-bit encoder limitation (silently stores as 32-bit ints unless hybrid mode delegates to native CLI) and dither compatibility, lossy encoding and metadata transfer are canonical when format-supported, and Verify is a fallback option.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `metadata_effect` — QUIRKY
- spec 3 · read at `9f60a1f73dbf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:44:46Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Inspects the plan step's arguments/context to determine whether the ffmpeg invocation preserves or strips metadata (e.g. checking for -map_metadata flags), returning a MetadataPlanEffect variant. Likely defaults to a "strips" or "unknown" effect since ffmpeg transcodes are often metadata-lossy unless explicitly told to copy tags.
- found: Matches on the plan step's operation type: for EncodePcm/EncodeLossy it delegates to a helper ffmpeg_encode_metadata_effect; for MetadataTransfer it checks ffmpeg_metadata_transfer_supported and if so returns an effect reflecting the transfer_tags/preserve_artwork flags; otherwise returns MetadataPlanEffect::none().
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `metadata_disposition` — QUIRKY
- spec 3 · read at `bbca4114fe81` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:45:18Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Determines whether this ffmpeg step preserves, strips, or leaves metadata untouched, based on the step/context (e.g. output codec or whether -map_metadata is used), returning the appropriate MetadataDisposition variant. Likely defaults toward metadata being stripped/dropped since actual tag-writing for FLAC etc. is handled by other plugins like MetaflacPlugin.
- found: Calls self.metadata_effect() then matches on step.operation: for EncodePcm/EncodeLossy where ffmpeg_encoder_transfers_original_source_metadata is true, or for MetadataTransfer where the effect shows tags/artwork transferred from the source, returns WritesRequestedPolicy; all other operations return DoesNotWrite.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `build_command` — QUIRKY
- spec 3 · read at `5c1afd3fb6f8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:18Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds the ffmpeg CLI invocation for a pipeline step: constructs an argument vector including "-i" with the input path from context/step, codec/format flags derived from the step's target format, and the output path, returning a PlannedCommand. Likely handles a few conditional flags (e.g. sample rate, bit depth, overwrite flag) and may incorporate metadata_effect/disposition from sibling methods.
- found: Matches on the step's PlanOperation variant and delegates to a specialized build_ffmpeg_* free function per case (decode, resample, encode pcm/lossy, metadata transfer, verify), returning a plugin-rejected error for unsupported operations.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The real logic lives in separate build_ffmpeg_* functions not shown here; this function is purely a dispatch table.

### `id` #2
- spec 3 · read at `ced0c1634ee1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:46:31Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Trivial getter returning a constant ToolIdentifier representing the "sox" tool, e.g. ToolIdentifier::Sox or ToolIdentifier::new("sox"), matching the pattern of the other *Plugin::id methods.
- found: Returns the constant ToolIdentifier::Sox.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `supports` #2 — QUIRKY
- spec 3 · read at `8e1b203a5f45` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:23:07Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the SoX tool can handle the given PlanStep (probably resample/dither/format-conversion steps) — inspecting the step's kind and the context's source/target formats to decide if SoX is a valid candidate, returning a ToolSupport variant (e.g. Supported/Unsupported/Preferred) possibly with a reason string when unsupported.
- found: Matches on the step's PlanOperation with a much finer-grained tiered ToolSupport (UNSUPPORTED/SUPPORTED/FALLBACK/PREFERRED/CANONICAL) than I predicted a binary yes/no: EncodePcm is scored based on silent-substitution correctness bugs, dither requirements, and whether processing is applied; lossy MP3/Opus and non-brick-wall resampling are FALLBACK/PREFERRED; DSD conversions are CANONICAL; everything else UNSUPPORTED.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: The ToolSupport tiering (UNSUPPORTED/SUPPORTED/FALLBACK/PREFERRED/CANONICAL) and the silently_substituted correctness-bug guard aren't discoverable from the signature or peers.

### `build_command` #2 — QUIRKY
- spec 3 · read at `aaa36f9975fe` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:51:28Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds the sox command-line invocation for a conversion step: sets input path/format flags, output path/format flags, and rate/bit-depth conversion options (e.g. -r for sample rate, -b for bit depth) plus dithering flags derived from the plan step's settings, returning a PlannedCommand with the sox binary and its argument list. May special-case things like when no rate/bit-depth conversion is actually needed (passthrough) versus when resampling parameters must be added.
- found: This is a match dispatcher over step.operation (EncodePcm, EncodeLossy, ResamplePcm, PcmToDsd, DsdToPcm, DsdRateChange) delegating to separate build_sox_* helper functions per operation type, erroring on unsupported operations. I predicted it directly built sox args for rate/bit-depth conversion, but it's actually a router covering a much wider range of operations including DSD/PCM conversions, with the actual arg-building logic living elsewhere.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `id` #3
- spec 3 · read at `0bcfe81a7870` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:51:31Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Trivial accessor returning the static ToolIdentifier for the SSRC tool plugin (e.g. ToolIdentifier::Ssrc or similar), a one-liner matching the pattern of the other *Plugin::id methods.
- found: Returns ToolIdentifier::Ssrc, a static one-liner accessor.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `supports` #3
- spec 3 · read at `e696ac3ddf9f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:10:24Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given PlanStep is a sample-rate-conversion/resample operation (ssrc's specialty) and returns ToolSupport::Supported if so, ToolSupport::Unsupported otherwise — possibly with an additional constraint on input format (e.g. only PCM/WAV) since ssrc is a narrow single-purpose resampler unlike ffmpeg/sox.
- found: Matches only PlanOperation::ResamplePcm with brick_wall: true, returning ToolSupport::CANONICAL; everything else (including non-brick-wall resamples) is UNSUPPORTED.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_command` #3
- spec 3 · read at `6b11c98a1fab` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:49:38Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: SsrcPlugin::build_command constructs a PlannedCommand invoking the `ssrc` CLI (a sample-rate converter), setting flags for target sample rate, bit depth (via the ssrc_bits_arg helper), dithering/quality options, and the input/output file paths derived from the PlanStep/PlanContext. It likely returns an error if required parameters (like target rate) are missing from the step.
- found: Only handles the ResamplePcm{brick_wall:true} operation; builds ssrc CLI args (--rate, --profile resolved from settings or mapping, conditionally --dither/--bits/--att/--minPhase/--pdf where dither+pdf are deliberately kept as one coupled pair validated against the target rate), appends input/output paths, and returns a PlannedCommand; rejects any other operation as unsupported.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `ssrc_bits_arg`
- spec 3 · read at `8141ecbad92a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:35:43Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Maps a PcmBitDepth enum variant (e.g. 16/24/32-bit int, or float) to the string SSRC's command-line expects for its bit-depth argument, via a match statement returning a String like "16", "24", or "32".
- found: Maps PcmBitDepth to SSRC's bit-depth CLI arg string: float depths become negative-prefixed ("-32"/"-64"), while integer depths fall through to depth.bits().to_string().
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `id` #4
- spec 3 · read at `89b4d838a36a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:57:03Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Trivial accessor returning a hardcoded ToolIdentifier for the loudgain tool, e.g. ToolIdentifier::Loudgain or ToolIdentifier::new("loudgain"), mirroring the other plugin ::id methods (SoxPlugin::id, SsrcPlugin::id, etc.) which each just return their tool's fixed identifier.
- found: Exactly as predicted: returns the fixed ToolIdentifier::Loudgain variant.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `supports` #4
- spec 3 · read at `51394b5b6fde` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:08:39Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given PlanStep is a loudness-normalization/ReplayGain operation that the loudgain tool can perform, likely matching on a step kind/operation enum and possibly restricted to certain audio formats. Ignores _context since it's unused. Returns a ToolSupport variant (yes/no/maybe).
- found: Matches on PlanOperation::ReplayGain, checking target_format via loudgain_supports_format helper; returns ToolSupport::CANONICAL if supported else ToolSupport::UNSUPPORTED.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_command` #4
- spec 3 · read at `425b773a5164` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:59:42Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds a loudgain command-line invocation (executable "loudgain"), translating scan mode (track vs. album) and target loudness settings from step/context into flags like -a/-t, -s, appending the input file path(s), and wrapping it all in a PlannedCommand.
- found: Matches step.operation for ReplayGain, builds -a/-t (or both) mode flags, optional -k for clip prevention, fixed "-s e" tag-write mode, and the input path, returning a PlannedCommand for Loudgain; rejects any other operation with a plugin_rejected error.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `id` #5
- spec 3 · read at `de529afdc19a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:42:38Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Returns a ToolIdentifier constructed with the literal string "metaflac", identifying this plugin as the metaflac tool, mirroring how sibling plugins (SoxPlugin, SsrcPlugin, LoudgainPlugin, FlacPlugin) each return their own tool name in their id() method.
- found: Returns the ToolIdentifier::Metaflac enum variant, identifying this plugin as the metaflac tool.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `supports` #5 — QUIRKY
- spec 3 · read at `360eb6b769d4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:07:19Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given PlanStep is a metadata-tagging step for a FLAC output, returning a ToolSupport variant indicating support (e.g. Supported) if so, and an unsupported/none variant otherwise; the context argument is unused.
- found: Matches specifically on PlanOperation::StoreSourceAudioMd5 with target_format Flac, returning ToolSupport::CANONICAL for that case and UNSUPPORTED for everything else including other operations.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `metadata_effect` #2 — QUIRKY
- spec 3 · read at `1f0079e085f7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:42:01Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Since MetaflacPlugin's whole purpose is to write metadata tags via the metaflac tool, this returns a MetadataPlanEffect variant indicating metadata is written/applied at this step (e.g. Writes or Applied), ignoring the unused _context parameter and possibly inspecting `step` only trivially or not at all.
- found: Matches on step.operation: only for StoreSourceAudioMd5 targeting Flac format does it return a MetadataPlanEffect with source_audio_md5_written=true; every other operation returns MetadataPlanEffect::none().
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `build_command` #5 — QUIRKY
- spec 3 · read at `74cf47d84d56` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:03:30Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds a `metaflac` command line to write metadata tags onto a FLAC file: something like `metaflac --remove-all-tags --set-tag=KEY=VALUE ... <path>`, pulling the tag key/value pairs from the plan context/step's metadata request and the target file path, returning a PlannedCommand wrapping the binary name and args.
- found: Only handles the single StoreSourceAudioMd5 operation for FLAC targets: builds a `metaflac --set-tag=SOURCE_AUDIO_MD5=<md5> <path>` command, erroring if audio_md5 is missing; rejects every other operation as unsupported by this plugin.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Signature suggested a general tag-writer, but it's a narrow single-purpose handler with a catch-all rejection branch for anything else.

### `id` #6
- spec 3 · read at `bd8039179b69` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:47:47Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns a constant ToolIdentifier value identifying this plugin as "flac" (likely ToolIdentifier::Flac or similar enum variant / constructed from a static string), with no logic beyond that constant construction.
- found: Returns the constant ToolIdentifier::Flac.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `supports` #6 — OBSCURE
- spec 3 · read at `f64261366b5a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:54:39Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Checks whether this PlanStep is a FLAC encode (or decode) step by matching step.action/format against Flac, returning ToolSupport::Supported when it matches and ToolSupport::Unsupported (with a reason) otherwise — gating whether the FLAC CLI tool is the right plugin for this step.
- found: Only supports Verify operations targeting FLAC: returns ToolSupport::PREFERRED if the request settings explicitly prefer native flac verify, FALLBACK if it's a FLAC verify without that preference, and UNSUPPORTED for every other operation (encode/decode not handled by this plugin at all).
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: I expected this plugin to handle FLAC encode/decode, but it's scoped only to Verify — encode/decode for FLAC must be handled elsewhere (likely ffmpeg or a different plugin), which isn't obvious from the type name FlacPlugin.

### `build_command` #6 — OBSCURE
- spec 3 · read at `6e6cd84607b6` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:38:59Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs a PlannedCommand invoking the flac CLI encoder — assembling compression-level flags (e.g. -8), force/overwrite flags, output path (-o <path>), and the input path from step/context, returning Ok(PlannedCommand).
- found: Only handles PlanOperation::Verify for FLAC targets, building a `flac -t -s <input>` test/silent-check command; rejects any other operation (including presumably encoding, which must live in another plugin) with a plugin_rejected error.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: FlacPlugin only implements verification, not encoding — encoding for FLAC targets is handled elsewhere (likely an ffmpeg-based plugin), which is not obvious from the name/owner alone.

### `build_ffmpeg_decode`
- spec 3 · read at `9e4ba6817f16` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:13:38Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand that invokes ffmpeg to decode the source input into raw PCM at the given bit_depth (selecting the matching pcm_s16le/pcm_s24le/etc codec flag), reading the step's input path and writing to an intermediate/output target, following the same command-building pattern as its sibling build_ffmpeg_resample/build_ffmpeg_encode_pcm.
- found: Builds an ffmpeg command decoding the step's input audio stream (mapping 0:a:0, dropping video) to a WAV container with a PCM codec chosen for the given bit_depth, wrapping the args plus io paths/duration/description into a PlannedCommand.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_ffmpeg_resample`
- spec 3 · read at `9e12b86f1bcc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:58:29Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs an ffmpeg PlannedCommand that resamples audio to target_rate_hz (via -ar) and optionally sets sample depth/codec (e.g. pcm_s16le/pcm_s24le) if target_depth is given, pulling input/output paths from context/step and returning the assembled command with appropriate args.
- found: Builds an ffmpeg PlannedCommand using required input/output paths, defaults depth to Float64 if unset, maps depth to a PCM codec via mapping::ffmpeg_pcm_codec, builds an -af filter string for rate/depth via ffmpeg_audio_filter, and assembles args including -map 0:a:0, -vn, -af, -c:a.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_ffmpeg_encode_pcm` — QUIRKY
- spec 3 · read at `fe25b3a30ff6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:17:47Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds an ffmpeg invocation for encoding to an uncompressed PCM format (WAV/AIFF/WavPack), mapping target_rate_hz/target_depth to ffmpeg args like -ar and a -sample_fmt/-c:a pcm_sXXle codec selection, conditionally adding processing filters (e.g. -af for loudness/resampling) when apply_processing is true, and wrapping input/output paths from step/context into a PlannedCommand.
- found: Special-cases WavPack hybrid mode to delegate to the native wavpack CLI builder; otherwise validates the target format is ffmpeg-encodable and AAC-family container is valid, then assembles ffmpeg args by composing helpers (base input args, metadata args, optional audio filter args when apply_processing, PCM encoder args, container format/flags args) plus the output path, returning a PlannedCommand with an attached metadata effect.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `build_ffmpeg_encode_lossy`
- spec 3 · read at `fe425358ba15` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:20:39Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Constructs an ffmpeg PlannedCommand for encoding into a lossy target_format (e.g. mp3/aac/opus/vorbis), selecting the right codec/quality flags, adding a -ar target_rate_hz resample flag when given, and applying filter/processing args when apply_processing is true. Likely also wires up metadata transfer flags by consulting helpers like ffmpeg_encoder_writes_requested_metadata_policy and format_supports_tags.
- found: Builds an ffmpeg command for lossy encoding, validating aac-family container compatibility, setting up input/output paths and metadata args, optionally applying a resample rate, then dispatching per-format (mp3/aac/opus/dts/ac3) to set codec-specific quality/bitrate flags from request settings, erroring for unsupported formats, then adding container format/flags and attaching a metadata effect closure.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `ffmpeg_encode_metadata_effect` — QUIRKY
- spec 3 · read at `99ea1dd1486d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:00:16Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Computes whether this ffmpeg encode step writes/transfers metadata by checking if target_format supports tags and whether ffmpeg can rewrite/transfer metadata for this format, then returns the appropriate MetadataPlanEffect variant reflecting that policy for this step/context.
- found: Computes tags/artwork transfer eligibility from request settings gated by target format support, then branches on whether the step reads the original request input vs a command-chain intermediate, populating different fields of MetadataPlanEffect (source-transferred-from-original vs preserved-from-command-input) accordingly.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `ffmpeg_step_reads_original_request_input`
- spec 3 · read at `911071bd43f1` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:35:24Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Checks whether a given ffmpeg-based plan step reads directly from the original request's input file (as opposed to consuming the output of a prior pipeline step), likely by comparing the step's declared input path/index against the plan's first step or the original request input path.
- found: Returns true iff the step's input is an InputSource::Path variant whose path equals the original request's input_path — a simple pattern match, not an index/lookup comparison.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `metadata_rewritable_by_ffmpeg` — OBSCURE
- spec 3 · read at `f3d3ff8fba21` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:55:56Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Matches on the AudioFormat variant and returns true for formats whose metadata ffmpeg can rewrite in place (e.g. MP3, common lossy/lossless container formats), false for formats that need a dedicated tool instead (e.g. FLAC, which uses MetaflacPlugin) or formats ffmpeg can't handle at all.
- found: One-line delegation to format.ffmpeg_encodable(); no per-format match logic here, and it conflates "metadata rewritable" with "ffmpeg can encode this format" rather than a metadata-specific check.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_metadata_transfer_supported` — QUIRKY
- spec 3 · read at `319a71335e9e` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:40:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: This likely returns true if ffmpeg-based metadata transfer is needed and possible for the given format: checking `transfer_tags && format_supports_tags(format)` or `preserve_artwork && format_supports_artwork(format)`, combined with OR, gating whether a metadata-transfer step should be built at all.
- found: Requires the format be rewritable by ffmpeg at all (metadata_rewritable_by_ffmpeg), AND if tags are requested to be transferred the format must support tags, AND if artwork is requested to be preserved the format must support artwork — all conjoined, not the OR-based flag-check I predicted.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_encoder_writes_requested_metadata_policy`
- spec 3 · read at `1c58a78183c6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:54:51Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Returns true when the ffmpeg encode step itself will write the metadata called for by the request's policy for this format (tags/artwork), as opposed to needing a separate metadata-transfer step afterward — likely combining format_supports_tags/format_supports_artwork checks with what the context's request actually asks to be transferred.
- found: Returns true only if the format is generally rewritable by ffmpeg, AND (tag transfer isn't requested OR the format supports tags), AND (artwork preservation isn't requested OR the format supports artwork) — i.e. ffmpeg can satisfy the encoder-side metadata policy for whatever was actually requested.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_encoder_transfers_original_source_metadata`
- spec 3 · read at `0e915b08a4bb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:20:46Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Checks whether a given ffmpeg encode step should pull metadata directly from the original source file (rather than from an intermediate decoded/resampled stream), likely combining ffmpeg_step_reads_original_request_input(step) with a format capability check like format.supports_planner_source_tag_transfer() and/or metadata_rewritable_by_ffmpeg. Returns true only when the step is the one directly reading original input AND the target format supports transferring source metadata via ffmpeg.
- found: Returns true only if the step reads the original request input directly AND the ffmpeg encoder for this format writes the requested metadata policy — an AND of two other named predicate helpers, matching my predicted structure exactly though I guessed slightly different helper names/semantics.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `supports_planner_source_tag_transfer`
- spec 3 · read at `80e00f3bc4a8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:55:46Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Matches on `self` (the AudioFormat) and returns true for formats capable of carrying text tags/metadata (e.g. Mp3, Flac, Vorbis, Aac, Opus) and false for raw/PCM-only or tagless formats. Essentially a capability lookup table similar to format_supports_tags, but specifically scoped to what the planner/plugin bridge can transfer as source tags.
- found: Returns true via a matches! macro for a fixed whitelist of formats (Flac, Wav, Aiff, WavPack, Mp3, Aac, Alac) that can carry source tags through the planner path; false for anything else (e.g. Vorbis, Opus not included).
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `supports_planner_embedded_artwork_transfer`
- spec 3 · read at `002596a0972d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:56:51Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A match self { ... } on AudioFormat variants returning true only for the subset of formats whose containers support embedded artwork/video streams (e.g. Flac, Mp3, M4a/Aac, Opus/Ogg — container formats with picture-frame support), false for others (e.g. raw PCM, WAV, or formats lacking artwork frames), narrower than the sibling supports_planner_source_tag_transfer.
- found: matches!() returning true only for Flac, Mp3, Aac, and Alac; false for all other AudioFormat variants (notably Opus is excluded despite being a common artwork-capable container).
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `supports_cue_post_encode_artwork_embedding`
- spec 3 · read at `51fde0c67c88` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:00:51Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A match on `self` (AudioFormat variants) returning true only for the formats that have a concrete post-encode artwork writer implemented (e.g. Flac, Mp3, maybe Ogg/Opus), and false for formats without one (e.g. Wav, or lossless-but-unsupported formats), mirroring the pattern of the other `supports_*` predicate methods listed as peers.
- found: Simple matches! predicate returning true for Flac, Mp3, Aac, Alac, and WavPack — the formats with a concrete post-encode artwork writer — false for everything else.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `format_supports_tags`
- spec 3 · read at `320f13b4a8b6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:04:10Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Matches on the AudioFormat variant, returning true for formats that support embedded metadata tags (e.g. FLAC, MP3/ID3, etc.) and false for formats without a tagging mechanism (e.g. raw PCM/WAV).
- found: One-line delegation to format.supports_planner_source_tag_transfer(), not an inline match — the semantics live on the AudioFormat method itself.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `format_supports_artwork` — QUIRKY
- spec 3 · read at `4053481d89ed` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:32:24Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A simple match over AudioFormat variants returning true for formats known to support embedded artwork (e.g. FLAC, MP3, M4A/ALAC) and false for formats that don't typically support embedded art (e.g. WAV, DSF/DSDIFF, or other raw PCM formats).
- found: One-line delegation to AudioFormat::supports_planner_embedded_artwork_transfer(), not an inline match — the actual variant logic lives on the AudioFormat type itself, which was listed right there in peers.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The real per-format logic is in AudioFormat::supports_planner_embedded_artwork_transfer, not in this wrapper.

### `loudgain_supports_format`
- spec 3 · read at `bedc7fc5cbf0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:29:18Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Matches on the AudioFormat enum and returns true for the specific set of formats loudgain (ReplayGain tool) supports tagging — likely FLAC, MP3, Ogg Vorbis, Opus, WavPack, MP4/ALAC — and false for unsupported formats like WAV or DSD.
- found: matches! allowlist: Flac, Mp3, Aac, Opus, Alac, WavPack return true; all else false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_ffmpeg_metadata_transfer`
- spec 3 · read at `bcad21eebf9d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:33:14Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand that runs ffmpeg to copy metadata (via -map_metadata) and/or embedded artwork (via stream mapping and codec copy) from the source onto the target file, conditionally including tag transfer and artwork transfer based on the transfer_tags/preserve_artwork flags, using shared arg-building helpers and the target_format to decide container-specific flags, returning an error if the format doesn't support the requested transfer.
- found: Validates the format/policy combo and AAC container, then builds a two-input ffmpeg command (encoded audio + original source) mapping the encoded audio stream, conditionally mapping metadata (-map_metadata 1 or -1) and artwork (-map 1:v? -c:v copy or -vn), adds container flags, and wraps it as a PlannedCommand with a metadata_effect. Got the overall shape right but missed specifics: explicit unsupported-format error, AAC container validation, the exact two-input structure, and the metadata_effect attachment.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `build_ffmpeg_verify`
- spec 3 · read at `94556712087c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:29:44Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Constructs a PlannedCommand that runs ffmpeg in a verification mode against the encoded output file — likely `ffmpeg -v error -i <output> -f null -` — to decode-check the file for corruption/errors without producing real output, pulling the target path from context/step and returning it wrapped in Ok, with Err if required paths/args are missing.
- found: Exactly as predicted: builds ffmpeg -v error -i <input> -f null - args and wraps in a PlannedCommand, using step.input/output and context.request.source.duration/step.description for the rest of the command metadata.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `build_sox_encode_pcm`
- spec 3 · read at `28389778b47f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:24:23Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a `sox` command that encodes PCM audio to `target_format`, adding a `-r` rate arg if `target_rate_hz` is set, a bit-depth arg derived from `target_depth`, and appends processing effects (dither/gain/etc) only when `apply_processing` is true. Assembles input/output paths from `context`/`step` into a `PlannedCommand`.
- found: Builds a sox PlannedCommand: -S flag, input path, output-format args via add_sox_output_format_args, output path, then conditionally appends PCM effects (rate/depth conversion) via add_sox_pcm_effects when apply_processing is true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_sox_encode_lossy` — QUIRKY
- spec 3 · read at `2cd614374d9f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:27:02Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand invoking SoX to encode audio into a lossy target_format, using the step's input/output paths from context. If apply_processing is true and target_rate_hz differs from source, it inserts SoX "rate"/dither arguments for resampling; otherwise it just passes the input straight to the lossy encoder with format-specific quality/bitrate flags. Returns an error if the format or rate combination isn't supported by SoX.
- found: Builds a sox command line: input, then a -C compression arg computed per-format (MP3 via mapping::sox_mp3_compression with mode/bitrate/vbr settings, Opus via bitrate_kbps directly), erroring for any other format since this planner's SoX lossy path only supports MP3/Opus; then output, then optionally appends PCM effects (rate/dither) via add_sox_pcm_effects when apply_processing is true.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `build_sox_resample`
- spec 3 · read at `202c8dc41b06` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:45:15Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs a sox command-line invocation for resampling PCM audio: input file args from context/step, a `rate` effect set to target_rate_hz, an optional bit-depth output flag if target_depth is Some, and output file path, returning a PlannedCommand or an error if the step/context data needed to build it is missing or invalid.
- found: Builds sox argv: -S flag, input path, optional bit-depth output args (via add_sox_bit_depth_args) if target_depth is set, output path, then delegates to add_sox_pcm_effects (shared helper) to append rate/dither effect args using target_rate_hz and target_depth. Wraps into a PlannedCommand with Sox identifier, step's input/output metadata, source duration, and description.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_sox_pcm_to_dsd`
- spec 3 · read at `94d4dff545a1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:24:44Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand invoking sox to convert the step's PCM input into DSD output at target_rate, adding a --dsd-filter-preset-style argument for `filter` and the target container/format flags derived from target_format, along with input/output paths pulled from context and step. Returns Ok(PlannedCommand) or an error if the combination is unsupported.
- found: Validates target_format.is_dsd(), resolves input/output paths from the step, builds base sox args (-S input output), delegates rate/filter effect args to add_sox_pcm_to_dsd_effects, and wraps it all in a PlannedCommand carrying the Sox tool id, step input/output, source duration, and description.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `build_sox_dsd_to_pcm`
- spec 3 · read at `48b44f5a2eb6` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T23:50:19Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand invoking `sox` to convert a DSD source into PCM output: sets input file/type args from context/step, adds a rate conversion to target_rate_hz, sets output bit depth to target_depth, and applies the requested lowpass filtering method (translating the DsdLowpassMethod enum into the corresponding sox filter/rate options), then sets the output path/format based on target_format.
- found: Validates target_format is PCM-lossless-capable (else errors), gets input/output paths from the step, builds sox args (-S, input, output format args, output path), then delegates rate/depth/lowpass effect args to add_sox_dsd_to_pcm_effects, and wraps it all in a PlannedCommand with duration and description metadata.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `build_sox_dsd_rate_change`
- spec 3 · read at `a7420156b3da` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:35:10Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a sox command that changes the DSD rate (e.g. DSD64->DSD128) to target_rate, adding lowpass-filter-specific sox arguments based on the DsdLowpassMethod variant, sets output format args for target_format, and returns a PlannedCommand with the assembled sox invocation.
- found: Validates target_format is DSD, gets input/output paths from the step, builds base sox args with -S flag plus input/output, delegates the actual rate-change effect chain to add_sox_dsd_rate_change_effects (which handles the lowpass method), and wraps everything into a PlannedCommand with duration and description metadata.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_base_input_args`
- spec 3 · read at `e491116ffdf4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:12:40Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds a Vec<String> of common ffmpeg CLI flags used before specifying the input file, likely including things like "-nostdin", "-hide_banner", "-y" or similar, followed by "-i" and the input path, returning the full argument vector to be reused by multiple ffmpeg-based builders.
- found: Builds base ffmpeg input args as predicted (-y, -hide_banner, -nostdin, -i, input) but also adds "-map 0:a:0" to select the first audio stream, which I did not anticipate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `add_ffmpeg_container_flags` — QUIRKY
- spec 3 · read at `4ad1d4960c7f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:58:14Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Matches on context's container-override option (e.g. an enum like ContainerOverride::Rf64), and if set, pushes the corresponding ffmpeg flag pair (e.g. "-rf64", "auto") onto args; if no override is set, returns immediately without touching args.
- found: Simply iterates context.request.container_ffmpeg_flags (a pre-built Vec<String> already carried on the request) and clones each into args — no enum matching or flag construction here at all; that logic lives upstream wherever container_ffmpeg_flags is populated.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The actual flag values/logic (e.g. rf64 auto) are decided elsewhere when request.container_ffmpeg_flags is built, not in this function.

### `validate_aac_family_container`
- spec 3 · read at `39bc4a1611e8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:03:21Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Checks that the target_format (AAC or a related codec) is paired with a container setting that's actually valid for it — e.g. rejecting an ADTS-only encoder configuration if the user requested an M4A container, or vice versa — pulling the relevant container/encoder settings off context and returning Err with a descriptive message if the combination isn't supported, Ok(()) otherwise.
- found: Checks the target output container extension against the codec: AAC must be .m4a/.mp4 (explicitly rejecting raw .aac as unimplemented), ALAC must be .m4a/.mp4, and any other format passes through unchecked (Ok).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `add_ffmpeg_container_format_args`
- spec 3 · read at `a15146501510` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:57:48Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Given a mutable args vector and a target AudioFormat, appends the ffmpeg -f <container> argument selecting the correct muxer/container format string for that target format, via a match over format variants.
- found: Only handles AAC/ALAC specially, pushing "-f ipod" to force the MP4/iPod muxer (needed for metadata/artwork support instead of raw ADTS); all other formats get no explicit container args (ffmpeg infers from extension). My general mechanism prediction was right but I expected broader per-format coverage rather than this narrow special-case-only match.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `add_ffmpeg_metadata_args` — QUIRKY
- spec 3 · read at `852a191b67cf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:38:16Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Pushes ffmpeg `-metadata key=value` argument pairs onto `args` for each tag/metadata field present in `context` (e.g. title, artist, album), likely skipping or special-casing fields when `target_format` doesn't support embedded tags (e.g. raw PCM/WAV), and possibly adds a `-map_metadata` or cover-art flag.
- found: Sets -map_metadata to 0 or -1 based on a transfer_tags setting and whether the target format supports tags at all (not per-field key=value pairs), and separately maps/copies or strips the video stream based on preserve_artwork and artwork format support.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `add_ffmpeg_audio_filter_args`
- spec 3 · read at `0343fc77dab7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:45:46Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Calls ffmpeg_audio_filter(context, target_rate_hz, target_depth) to compute an ffmpeg -af filter string, and if it returns something non-empty, appends "-af" and the filter string to args; returns Ok(()) otherwise, propagating any error from filter construction.
- found: Computes filter via ffmpeg_audio_filter(context, target_rate_hz, target_depth); if non-empty pushes "-af" and the filter string onto args; returns Ok(()).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_audio_filter` — QUIRKY
- spec 3 · read at `50c26563ed25` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:18:09Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds the ffmpeg `-af`/audio filter chain string by combining filters such as `aresample` (when target_rate_hz differs from source) and `aformat`/dithering options (when target_depth is set), joining them with commas. Returns Ok with the filter string (possibly empty) or an Err if the combination of context/rate/depth is unsupported.
- found: Builds a single ffmpeg `aresample=` filter with colon-separated soxr options (resampler=soxr, out_sample_rate, precision, cutoff, cheby, phase_shift, dither_method, out_sample_fmt) derived from settings and target rate/depth; errors if a requested dither type isn't supported by soxr; returns empty string if no rate/depth/dither change is needed.
- predicted: some · documented: none · derivable: no · legible: most · trap: no

### `add_ffmpeg_pcm_encoder_args` — QUIRKY
- spec 3 · read at `a33c2de88314` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:47:59Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: This appends ffmpeg CLI arguments to encode PCM output at the given target bit depth/format — picking the right -sample_fmt/codec pair (e.g. pcm_s16le, pcm_s24le, pcm_f32le) based on target_depth, and possibly invoking dither-related logic (using sibling helpers like explicit_int32_dither_requested/target_depth_needs_dither) when the conversion reduces bit depth. It returns an error if the target format/depth combination is unsupported.
- found: Switches on target_format to append ffmpeg codec args: FLAC gets "-c:a flac" plus "-strict experimental" when target_depth is Int32 (since ffmpeg otherwise silently downgrades s32 to 24-bit FLAC), plus compression_level and optional -flags -md5; WAV/AIFF delegate codec selection to mapping::ffmpeg_pcm_codec; WavPack gets "-c:a wavpack" plus a compression level derived from settings; ALAC just sets "-c:a alac"; any other format returns an unsupported_format error.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `build_wavpack_hybrid_encode`
- spec 3 · read at `1ec791d89e8e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:22:41Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand invoking the `wavpack` binary with the input file, sets hybrid-mode flags (like -b<bitrate> for target bitrate and -c to also emit a .wvc correction file), computes the output .wv path (and .wvc path if correction requested) from the step's output naming, and returns a PlannedCommand with program, args, and declared outputs.
- found: Builds args for the wavpack CLI: pushes a compression mode flag from settings, the -b hybrid bitrate flag, optionally -c for correction file, then input path, -o, and output path. Wraps into a PlannedCommand with a Custom("wavpack") tool identifier and metadata (input/output specs, duration, description) rather than computing paths itself.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: I expected it to derive .wvc output paths itself, but path/output handling is delegated to step.input/step.output and required_output_path — the function doesn't compute correction-file paths at all.

### `add_sox_output_format_args`
- spec 3 · read at `fcfaec4bc01f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:06:49Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Pushes sox CLI arguments describing the desired output format onto `args`, mapping target_format to sox's `-t <type>` encoding flag and target_depth to a `-b <bits>` bit-depth flag, possibly branching on context for format-specific extras (e.g. compression level for FLAC, encoding subtype for WAV/AIFF).
- found: Delegates bit-depth flags to add_sox_bit_depth_args, then adds a `-C` compression-level arg only for FLAC (using request settings directly) and WavPack (via mapping::wavpack_compression_level); other formats get no extra args. No `-t` type flag as I'd guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `add_sox_bit_depth_args`
- spec 3 · read at `a808f9d36088` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:12:22Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: This appends sox output-format bit-depth args to the args vector, mapping the PcmBitDepth enum to sox's -b/--bits flag with the numeric depth value (e.g. 16, 24, 32).
- found: Pushes "-b" and the bit count (32/64 for float variants, else depth.bits()); if the depth is a float type, also pushes "-e float" to set sox's sample encoding explicitly.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `add_sox_pcm_effects` — QUIRKY
- spec 3 · read at `8c532fc214e5` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:28:10Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Appends sox effect arguments (like a rate effect if target_rate_hz differs from the source rate, and a dither effect if target_depth reduces the bit depth per pcm_conversion_reduces_depth/target_depth_needs_dither/explicit_int32_dither_requested) onto args, using context to know the source sample rate/depth, mutating the vec in place with no return value.
- found: Much richer than predicted: builds an optional sinc pre-filter effect (taps/attenuation/passband/transition/kaiser-beta/phase) before the rate effect, adds the rate effect with quality flag plus mutually-exclusive chebyshev/bandwidth options, phase, and aliasing flags, then separately decides whether to append a dither effect — explicitly excluding 32-bit int dither from the depth-reduction check because SoX's command syntax doesn't prove the DSP actually ran at that depth.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: The comment on depth_allows_dither explains a real correctness subtlety (SoX 14.4.2 silently no-ops dither for s32 while still accepting the flag) that isn't visible anywhere else in the signature/peers — a future editor extending dither support needs that context or risks reintroducing an unverified no-op path.

### `add_sox_pcm_to_dsd_effects` — QUIRKY
- spec 3 · read at `d1bbe4945369` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:30:44Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds the SoX effect-chain arguments to convert a PCM stream to DSD: computes/adds a rate-change effect to the oversampled rate implied by target_rate, then adds the sigma-delta modulator (sdm) effect configured according to the given DsdFilterPreset, pushing the resulting flags into `args`. Likely mirrors add_sox_dsd_to_pcm_effects but in the opposite conversion direction, delegating rate-specific details to helpers like add_sox_sdm_args.
- found: Branches on DsdFilterPreset: Auto just adds a simple "rate" effect with an auto-rate flag to the target DSD-equivalent rate; Sinc manually builds an "upsample"+"sinc" filter chain from detailed settings (passband, taps, transition, linear/min phase, kaiser beta, aliasing), applies gain compensation (vol/gain/disabled per config), then a final "rate -I" step. Both paths end by delegating to add_sox_sdm_args to append the sigma-delta modulator stage.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `add_sox_dsd_to_pcm_effects` — TRAP
- spec 3 · read at `c3222940beb2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:12:24Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Appends sox effect chain arguments to `args` for converting DSD input down to PCM at target_rate_hz/target_depth: selects a lowpass filter (rate change or explicit filter) based on the DsdLowpassMethod variant, applies the sample rate conversion, adds dither if the target depth needs it (per target_depth_needs_dither), and may call add_sox_dsd_to_pcm_gain for level compensation. Returns Result<()> erroring on unsupported parameter combinations.
- found: For DsdLowpassMethod::Sinc, builds an explicit sinc filter + rate effect from settings' sinc params. For Auto/SoxUltra, uses a rate effect with a lowpass rate flag from mapping::sox_dsd_lowpass_rate_flag, then conditionally appends a second sinc filter post-rate-conversion to strip residual DSD noise, guarded so it's skipped when the cutoff would be at/above the target Nyquist (avoiding a sox error). Then always adds DSD gain compensation and, if the target depth needs dither and dither isn't None, appends dither args.
- predicted: most · documented: none · derivable: yes · legible: most · trap: yes
- note: The post-rate sinc strip is order-sensitive and guarded by a Nyquist check with no assertion enforcing it — extensive inline comments explain why (sox rejects filter freq >= samplerate/2), so a future editor moving this before the rate effect or removing the guard will silently break several DSD rate conversions (DSD256→<192k etc.) per the comment's own account.

### `effective_target_depth`
- spec 3 · read at `cb1914d2a303` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:15:27Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: If target_depth is Some, returns it directly; otherwise falls back to a depth derived from context (e.g. the source/input PCM bit depth), returning None if no depth can be determined either way.
- found: target_depth.or_else falls back to context.request.settings.target_bit_depth: either an explicit Pcm(depth) setting or, for BitDepthTarget::Source, the source's authoritative_pcm_depth().
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `explicit_int32_dither_requested`
- spec 3 · read at `79d4e3aa3e06` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:51:19Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the user explicitly configured dithering in `settings` while `target_depth` is 32-bit integer PCM — a case where dithering normally wouldn't be needed/applied. Returns true only if there's an explicit dither setting present AND target_depth is Int32, so the pipeline can honor an explicit override rather than silently skipping dither for the "no dither needed" 32-bit case.
- found: Returns true only if settings.dither_explicit is set, dither_type is not None, and target_depth is exactly Int32 — i.e. user explicitly asked for dither on a 32-bit int target.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `target_depth_needs_dither`
- spec 3 · read at `8dbcce46a3f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:39:01Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A match/matches! over PcmBitDepth variants returning false for Int32, Float32, and Float64 (which don't need dither), and true for lower integer depths like Int16/Int24 that benefit from dither when reducing bit depth.
- found: matches! over Int8|Int16|Int24 returns true; everything else (Int32, Float32, Float64) false, exactly matching the doc comment and my prediction.
- predicted: full · documented: full · derivable: no · legible: full · trap: no

### `pcm_conversion_reduces_depth`
- spec 3 · read at `b7e52a1fb03f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:58:17Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: If source_depth is None, returns true (conservative default); otherwise compares the numeric bit depths and returns true if target_depth is less than source_depth, false otherwise.
- found: First checks target_depth_needs_dither and returns false immediately if the target doesn't need dither at all; otherwise compares bit counts (source.bits() vs target.bits()) when source is known, or returns true conservatively when source is None.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `add_sox_dsd_to_pcm_gain` — QUIRKY — TRAP
- spec 3 · read at `fa6f22e133b5` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:13:03Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads a gain-related field from DsdSettings (e.g. a dB offset or headroom compensation value needed when converting DSD to PCM), and if it's non-zero/non-default, pushes "gain" plus the formatted dB value onto the sox args vector; returns Ok(()) normally, with Err only for an invalid/out-of-range value.
- found: Matches on dsd.legacy_dsd_to_pcm_gain_mode(): Auto pushes sox "norm" with a negative margin dB; Manual requires a finite gain_db (else errors) and pushes "gain" with a signed formatted value; Disabled still honors a legacy optional gain_db field for backward compatibility if present, without making auto-gain the default.
- predicted: some · documented: none · derivable: no · legible: full · trap: yes
- note: The Disabled branch silently applying a legacy gain value is easy to miss when reasoning about the mode enum by name alone — 'Disabled' still emits a gain arg under one condition, contrary to what the variant name implies.

### `add_sox_dsd_rate_change_effects`
- spec 3 · read at `44f4129e5c81` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:34:28Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds sox CLI arguments for changing the DSD sample rate (e.g. DSD64->DSD128) directly in the DSD domain, likely pushing a "rate" effect with the target_rate's numeric value and selecting sox rate-quality flags based on the given DsdLowpassMethod (e.g. steep vs default filter). It probably doesn't touch bit depth/dither since it stays in DSD.
- found: Pushes a "rate" effect for the target DSD rate, but with a twist: if the lowpass method is Sinc, it first pushes an explicit "sinc" filter effect built from configured passband/taps/transition/phase-linearity/kaiser-beta settings, and the "rate" effect's quality flag itself is looked up via a mapping helper keyed on lowpass method and resample_quality (rather than hardcoded). Then it appends SDM (sigma-delta modulator) args via add_sox_sdm_args.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The rate quality flag isn't a fixed choice here — it's resolved via mapping::sox_dsd_lowpass_rate_flag(lowpass, resample_quality), so the effective sox flag depends on a table elsewhere in the codebase.

### `add_sox_sdm_args` — QUIRKY
- spec 3 · read at `17e284d54c15` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:43:22Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Appends sox command-line arguments controlling sigma-delta modulation (SDM/DSD) encoding — likely pulling a target DSD rate and/or filter type from PlanContext and pushing corresponding sox effect flags (e.g. "rate", "dsd" filter options) onto the args Vec, mirroring the sibling add_sox_pcm_to_dsd_effects/add_sox_dsd_rate_change_effects helpers.
- found: Pushes the "sdm" effect name and "-f <shaper_name>" (from mapping::dsd_shaper_name given noise_shaper and modulator_order) onto args, then if a trellis config is present, appends "-t <lookahead>" and "-n <nodes>", and optionally "-l <latency>".
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `required_input_path`
- spec 3 · read at `94b4bd4efd83` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:31:25Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads step's input path field (likely step.input or similar Option<PathBuf>), converts it to a String via path_to_string, and returns an error (e.g. "missing input path") if it's absent; mirrors required_output_path's pattern for the output side.
- found: Converts step.input (via as_path/path_to_string) to a String, returning a PlanningError::invalid_settings("input", ...) if there's no path.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `required_output_path`
- spec 3 · read at `9580013a73a2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:36:55Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Looks up the output path field on a PlanStep (likely step.output) and returns it as a String, returning an Err with a descriptive message if the field is missing/empty — mirrors a sibling required_input_path that does the same for input paths.
- found: Extracts a path from step.output via as_path()/path_to_string, erroring with PlanningError::invalid_settings("output", ...) if step.output isn't a path variant.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `path_to_string`
- spec 3 · read at `a552ebdb8e2d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:47:14Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Converts a Path to a String for building command-line args, using to_string_lossy().into_owned() (or similar) to handle non-UTF-8 paths gracefully rather than panicking.
- found: path.to_string_lossy().into_owned() — exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `format_float`
- spec 3 · read at `5e126057735f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:05:57Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Formats an f32 value for use as a command-line argument to sox/ffmpeg (e.g. gain values), likely formatting with fixed precision and trimming trailing zeros/decimal point so it doesn't produce something like "1.5000000" or scientific notation.
- found: Formats to 3 decimal places, then strips trailing zeros and a trailing decimal point.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `legacy_dsd`
- spec 3 · read at `12afb8bec35f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:35:51Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs and returns a DsdSettings value representing the "legacy" DSD-to-PCM conversion configuration, plugging in the given gain_mode, margin_db, and gain_db while filling in other fields with fixed defaults appropriate to the legacy conversion filter path.
- found: Builds a default LegacyDsdSettingsWireV1, sets the three gain-related fields from params, and converts it to DsdSettings via from_legacy_wire.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `ssrc_resample_command_with` — QUIRKY
- spec 3 · read at `f7d1044560e2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:18:44Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds a PlannedCommand invoking the external `ssrc` resampler binary, constructing its argument list from settings (input/output paths via required_input_path/required_output_path) plus flags for target_rate_hz and, if target_bit_depth is Some, a bit-depth/dither argument; returns Err if required settings (like input/output path) are missing.
- found: A test-fixture helper: constructs a fixed 44.1kHz/24-bit stereo WAV SourceInfo and PlanRequest with hardcoded source.wav/output.wav paths, builds a PlanStep with PlanOperation::ResamplePcm (brick_wall true) using the given target_rate_hz/target_bit_depth, and delegates to SsrcPlugin.build_command — it doesn't construct the command's argument list itself, it just wires up test scaffolding around the real plugin.
- predicted: some · documented: none · derivable: no · legible: most · trap: no

### `arg_value`
- spec 3 · read at `3bf773612368` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:59:56Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test-helper function that scans the args slice for an occurrence of flag, and returns the immediately following element as the flag's value (Option<&str>), or None if the flag isn't present or has no following value — used by tests like assert_arg to verify emitted command-line arguments.
- found: Uses windows(2) to find an adjacent pair where the first element equals flag, then returns the second element of that pair as the value; None if no such pair exists.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `assert_arg`
- spec 3 · read at `526ddd6363ae` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:12:09Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Test helper that finds `flag` in the `args` slice, then asserts that the following element equals `expected` (panicking with a message if the flag is missing or the value doesn't match). Used across the plugin tests to check that a built command-line arg list contains a specific flag/value pair.
- found: Delegates to arg_value(args, flag) and asserts it equals Some(expected), matching the prediction exactly (just implemented via a helper rather than manual scanning).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `assert_no_arg`
- spec 3 · read at `82beb89d2bd5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:41:08Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Test helper that asserts the given flag string does not appear anywhere in the args slice, panicking with a descriptive message if it is found — used to verify a command builder did NOT emit a particular flag.
- found: Asserts flag is absent by delegating to arg_value(args, flag).is_none(), with a descriptive panic message — matches prediction exactly, just implemented via the sibling arg_value helper rather than a raw contains check.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `pcm_request_with` — QUIRKY
- spec 3 · read at `b4dd3334046f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:49:45Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test-helper fixture that builds a PlanRequest for a PCM conversion using the given settings and source_depth: constructs dummy input/output paths, sets the target codec/format to PCM with source_depth, and returns the populated PlanRequest for use by the surrounding dither-emission tests.
- found: Builds a PlanRequest fixture with fixed dummy paths (source.wav/output.wav), a WAV SourceInfo whose codec/sample_kind are chosen based on whether source_depth is a float depth, fixed 96kHz/2-channel source metadata, the given source_depth as both bit_depth and true_source_depth, and the given settings plugged in, with other fields (resolved_output_target, intermediate_dir, container_ffmpeg_flags, etc.) left at None/default.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `explicit_int32_dither_is_emitted_by_ffmpeg_for_float_requantization`
- spec 3 · read at `ccbcd378a445` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:48:26Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: This test builds a PCM conversion request targeting int32 output from a float source with dithering explicitly requested (not automatic), runs it through the ffmpeg command builder, and asserts the resulting command includes a dither-related argument — contrasting with the peer test showing automatic dither stays disabled for ffmpeg.
- found: Builds a float32-source PCM request targeting int32 with explicit TPDF dither, calls ffmpeg_audio_filter, and asserts the filter string contains dither_method=triangular and out_sample_fmt=s32.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `ffmpeg_int32_explicitness_uses_settings_depth_when_step_depth_is_absent`
- spec 3 · read at `df70ae48d7c0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:17:13Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This test builds an ffmpeg PCM conversion request where the step itself has no explicit bit-depth override, but the top-level settings specify one (likely 32-bit/int32). It asserts the generated ffmpeg command uses that settings-level depth to decide int32 "explicitness" (e.g., emits the correct sample-format/dither args), confirming the fallback from step to settings depth works.
- found: Builds a PCM request with settings.target_bit_depth=Int32 and explicit TPDF dither, but the request's own PCM depth is Float32 (depth carried elsewhere). Asserts the generated ffmpeg audio filter still contains "dither_method=triangular", confirming settings-level dither config applies even when the immediate request depth doesn't itself indicate int32.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `automatic_int32_dither_stays_disabled_for_ffmpeg_and_sox`
- spec 3 · read at `ff1147a908bf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:42:42Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds a PCM conversion request targeting int32 depth without any explicit dither setting (using pcm_request_with), runs it through both the ffmpeg and sox command builders, and asserts via assert_no_arg that neither emits a dither-related argument — confirming that automatic/implicit dither is not silently turned on for int32 targets even though it might be for float requantization.
- found: Directly calls ffmpeg_audio_filter and add_sox_pcm_effects (not higher-level command builders) with a float32-source, int32-target request that has dither_type=Tpdf set, and asserts neither emits a dither arg despite the requested dither, plus checks the ffmpeg filter carries out_sample_fmt=s32 and that sox omits dither even when depth is passed via a different arg slot.
- predicted: most · documented: none · derivable: no · legible: most · trap: no

### `explicit_int32_dither_is_refused_by_unqualified_sox_pcm_builders` — QUIRKY
- spec 3 · read at `183ff4dda3b3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:13:54Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds a pcm_request targeting 32-bit integer output with an explicit dither setting requested, runs it through the generic/unqualified sox PCM command builder (as opposed to the ffmpeg or ssrc-specific builders in sibling tests), and asserts that the builder rejects/errors on this combination — since dithering to 32-bit int is semantically meaningless — rather than silently emitting a dither flag.
- found: Across three scenarios (ordinary Int32 target with/without step depth, source-preserved Int32, and DSD-to-PCM Int32), builds sox effect args with explicit TPDF dither requested and asserts the "dither" argument never appears in the emitted args list — the unqualified sox builders silently refuse to emit dither for Int32 rather than erroring.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `explicit_gesemann_int32_plans_without_ffmpeg_dither`
- spec 3 · read at `14dd0ad6416a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:03:37Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A #[test] that builds a PCM/int32 conversion request with an explicit "Gesemann" dither type specified, runs it through the ffmpeg command builder, and asserts that no ffmpeg dither-related argument appears in the resulting command — because Gesemann isn't one of ffmpeg's supported dither algorithms, so explicitly requesting it should be silently dropped (not emitted) rather than erroring.
- found: Test builds a PCM request with explicit Gesemann dither targeting Int32, calls ffmpeg_audio_filter directly (not a full command builder), asserts it succeeds (not a planning failure) rather than erroring, and checks the produced filter string lacks 'dither_method=' while still containing 'out_sample_fmt=s32'.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: I predicted this would go through a full command builder and guessed 'silently dropped'/no-op framing, but the real assertion is specifically that unsupported explicit dither doesn't become a planning failure while still applying the bit-depth conversion.

### `explicit_dither_is_never_emitted_for_float_targets`
- spec 3 · read at `74ec0a527e4c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:28:06Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a PCM request/plan with an explicit dither setting but a float sample format target, generates the command (likely for ffmpeg and/or sox), and asserts via assert_no_arg that no dither-related argument appears in the emitted command, since dithering doesn't apply to float output.
- found: Builds a PcmBitDepth::Int24 request but with explicit TPDF dither settings and a Float32 target bit depth; checks that the generated ffmpeg audio filter string doesn't contain 'dither_method=' and that the sox effects args don't contain a 'dither' token, directly testing both backends in one function.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `explicit_int32_dither_is_not_emitted_by_ssrc`
- spec 3 · read at `20f29fd36fad` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:49:50Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds a PCM request targeting explicit int32 depth with dither requested, runs it through the SSRC command builder, and asserts that no dither-related argument is emitted — because SSRC (unlike ffmpeg, per the sibling test) doesn't support/emit dithering for integer bit-depth requantization.
- found: Sets explicit int32 dither settings, builds the SSRC command two ways (bit depth passed as explicit step arg vs. carried only via settings), and asserts neither --dither nor --pdf args appear in either case — confirming SSRC never emits dither/pdf args for int32 regardless of how the depth was specified.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_emits_global_tpdf_as_no_shaper_with_triangular_pdf` — QUIRKY
- spec 3 · read at `bd1e5262df07` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:18:54Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds a pcm_request_with global dither set to TPDF, runs the ssrc command builder, then uses assert_no_arg to confirm no shaper/ATH-curve flag is present and assert_arg to confirm a triangular-PDF flag is included in the emitted ssrc command arguments.
- found: Sets PipelineSettings dither_type to Tpdf, builds an ssrc resample command at 44100/Int16, and asserts specific arg values: --dither 99 (no-shaper code), --pdf 1 (triangular), --bits 16.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_emits_global_none_as_no_shaper_without_pdf_override`
- spec 3 · read at `7304bf684d9a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:00:01Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a unit test that builds a plan/request with the global dither setting set to "none" and no explicit PDF override, runs it through the SSRC command builder, and asserts that the resulting command arguments contain no shaper/dither flags (i.e., no -shaper or similar option is emitted). It's checking that "none" globally disables dithering shaping for ssrc when nothing overrides it.
- found: Builds default settings with dither_type=None, generates an ssrc resample command, and asserts the args contain --dither 99 (ssrc's explicit "no dither" code) but no --pdf argument.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_emits_shibata_family_as_rate_valid_ath_curve_a_with_triangular_pdf` — QUIRKY
- spec 3 · read at `f5a64d0cfd5d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:39:32Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a PCM request selecting the Shibata noise-shaping family at a particular sample rate, runs it through the SSRC command builder, and asserts the emitted command-line arguments contain the rate-appropriate ATH "curve A" shaper flag together with a triangular PDF dither flag.
- found: A table-driven test over 5 (dither_type, target_rate_hz, expected_dither_id) cases spanning LowShibata/Shibata/HighShibata at various rates; for each it builds an ssrc resample command and asserts the emitted --dither arg matches the expected numeric ID for that rate and --pdf equals 1 (triangular). I correctly predicted the rate-dependent curve/triangular-PDF relationship but not the parameterized table of exact numeric IDs or the literal --dither/--pdf arg names.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_honors_explicit_dither_id_only`
- spec 3 · read at `c03bdb9c459b` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T22:17:27Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs SSRC command-building input with an explicit dither ID set but no explicit PDF type override, builds the command args, and asserts the args include the explicit dither ID flag/value while the PDF-related argument remains whatever the derived/default behavior produces (not overridden by the explicit path).
- found: Sets dither_type=Tpdf and ssrc.dither_id=Some(0) with pdf_type=None, builds the ssrc resample command, and asserts --dither 0 (explicit) and --pdf 1 (derived from the Tpdf dither_type) — confirms the explicit/derived split predicted, with the concrete derivation (Tpdf maps to pdf value "1") being the unknowable detail.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_honors_explicit_pdf_type_only`
- spec 3 · read at `0881aa492f4d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:22:14Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A unit test that builds a plan/settings with only an explicit PDF (dither shape, e.g. triangular) override set and no explicit dither-id override, generates the ssrc command, and asserts the resulting command line includes the PDF type flag while the dither-id remains derived/default rather than being forced.
- found: Test sets dither_type=Shibata (which derives dither_id=2) with ssrc.dither_id explicitly None but ssrc.pdf_type explicitly Some(Rectangular), builds the ssrc command, and asserts --dither is derived (\"2\") from the dither_type while --pdf is forced to the explicit override (\"0\" for rectangular) rather than derived from Shibata's usual triangular pdf.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `ssrc_command_honors_both_explicit_overrides`
- spec 3 · read at `124cc5b0cbca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:10:12Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Unit test that builds an SSRC conversion plan with both an explicit dither ID and an explicit PDF type override set simultaneously, then asserts the generated command line includes flags reflecting both overrides together (not falling back to derived/global defaults for either).
- found: Sets global dither_type to None but explicit ssrc.dither_id and ssrc.pdf_type overrides, then asserts the built ssrc command's --dither and --pdf args reflect the explicit overrides (as numeric codes) rather than the disabled global dither setting.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: My general shape was right but I didn't anticipate the specific interesting case: global dither_type=None while explicit overrides still win, and that pdf_type Triangular maps to numeric code \"1\".

### `ssrc_command_suppresses_dither_and_pdf_for_float_output`
- spec 3 · read at `950e4bc974d5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:21:49Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A test that constructs an ssrc command plan with a float sample format target (e.g. 32-bit float), even with a dither/pdf setting requested, and asserts the resulting command arguments omit dither-id and pdf-type flags, since dither/noise-shaping is meaningless for float output.
- found: Test sets dither_type/dither_id/pdf_type explicitly, builds an ssrc command targeting Float32 output, and asserts --bits -32 is present while --dither and --pdf args are absent, confirming float output suppresses dither/pdf regardless of explicit settings.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_rejects_explicit_high_rate_unavailable_dither_id`
- spec 3 · read at `314fd3d717c5` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:07:53Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A unit test that constructs an SSRC plan/command request specifying an explicit dither ID that's only valid at a low sample rate, but pairs it with a high output sample rate, then asserts that building the SSRC command fails/returns an error (rejecting the unavailable dither ID at that rate) rather than panicking or silently ignoring it.
- found: Sets settings.ssrc.dither_id to 16, calls ssrc_resample_command_with at 96_000 Hz Int16, expects an Err, and asserts the error message text contains both "96" and "16" to confirm it names the offending rate and dither id.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_rejects_explicit_low_rate_unavailable_dither_id`
- spec 3 · read at `8b1647f74260` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:08:34Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A #[test] that constructs an SSRC resample command request targeting a low output sample rate with an explicitly-specified dither ID only valid at higher rates, invokes the plugin's command builder, and asserts the call returns an Err (or specific rejection message) rather than silently ignoring the invalid dither choice.
- found: Sets dither_id=6 with a low output rate (22050) and Int16 depth, calls ssrc_resample_command_with, asserts it errors, and checks the error message mentions both the rate and the dither id — matches prediction.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_command_rejects_derived_global_shaper_at_unlisted_rate`
- spec 3 · read at `3b712d6734ac` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:51:55Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test that sets up an SSRC conversion where the global dither/noise-shaper is derived (not explicitly specified) and the target sample rate is not in the list of rates that shaper supports. It asserts that building the SSRC command fails (returns an error) rather than silently emitting a shaper flag invalid for that rate.
- found: Sets dither_type to HighShibata (a derived/global shaper), builds an SSRC resample command targeting 176400 Hz at Int16, asserts it errors, and checks the error message mentions the offending rate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `planner_format_metadata_capabilities_are_centralized_on_audio_format`
- spec 3 · read at `85b63948aca1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:13:52Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A test asserting that metadata-capability queries (e.g. which tag fields or metadata operations a format supports) are defined once on AudioFormat and that planner code paths consult that single source rather than duplicating format-specific metadata logic. It likely iterates over several AudioFormat variants, calls a capability method, and checks the results match expected per-format values, guarding against capability logic being reimplemented ad hoc in individual planners.
- found: A table-style test asserting three distinct AudioFormat capability predicates (source tag transfer, embedded artwork transfer, cue post-encode artwork embedding) across many format variants, confirming each format's capabilities are queried from AudioFormat methods rather than reimplemented per-planner.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Three separate capability methods rather than one generic query, and no iteration/loop — just a flat list of asserts per variant.

### `ffmpeg_is_unsupported_for_wavpack_int24_but_sox_still_encodes_it`
- spec 3 · read at `f1d73a01689b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:55:12Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a test asserting that for a WavPack int24 encode target, the ffmpeg encoder plugin reports itself as unsupported/incapable (e.g. returns None or an error/unsupported-capability result) for that format+bitdepth combination, while the sox encoder plugin still successfully builds a valid encode command/plan for the same target. It likely constructs a minimal encode request/spec with format=WavPack, bit depth=24, then calls each plugin's capability-check or command-building function and asserts the differing outcomes.
- found: A test verifying FfmpegPlugin.supports() returns unsupported for WavPack+Int24 encode (both apply_processing states) while SoxPlugin remains supported, that ffmpeg stays eligible for Int16/Int32 WavPack, and that hybrid WavPack mode re-enables ffmpeg since it delegates to the native wavpack CLI.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_aac_command_rejects_raw_aac_suffix_without_raw_mode`
- spec 3 · read at `80a49f90e612` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:08:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds AAC encoder settings/output path with a raw .aac suffix but without enabling an explicit raw-AAC-mode flag, calls the ffmpeg AAC command builder, and asserts it returns an error (rejects raw AAC output) since raw bitstream output requires opting in explicitly rather than defaulting to the M4A/MP4 container.
- found: Builds a full PlanRequest/PlanStep encoding WAV->AAC to output path "track.aac", calls FfmpegPlugin::build_command, and asserts it errors with a message containing "raw .aac output is not implemented" — raw mode isn't a settings flag, it's inferred from the .aac output extension not matching the expected M4A/MP4 muxer path.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_aac_and_alac_commands_pin_mp4_m4a_muxer`
- spec 3 · read at `3faf3d537417` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:25:40Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test that builds ffmpeg commands for both AAC and ALAC encode targets and asserts the generated argument list explicitly pins the muxer format flag (e.g. -f mp4/-f ipod) rather than leaving it to be inferred from the output file extension, since m4a/mp4 extensions are ambiguous for ffmpeg's muxer autodetection.
- found: Builds an AAC (EncodeLossy) and an ALAC (EncodePcm) plan step for a .m4a output and asserts each generated ffmpeg command explicitly includes `-f ipod` (the muxer format for mp4/m4a in ffmpeg) rather than relying on extension-based autodetection.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_metadata_transfer_carries_typed_metadata_effects`
- spec 3 · read at `f760a0a9a0df` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:32:12Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A unit test (#[test]) that builds a plan/command for an ffmpeg-based metadata transfer operation and asserts the result includes a specific typed "metadata effect" value (structured, not just a raw CLI arg) — verifying the planner correctly surfaces metadata-transfer semantics as a typed effect rather than only as command-line flags/strings.
- found: Constructs a DSF-to-FLAC PlanRequest with tag-transfer and artwork-preserve settings, builds a MetadataTransfer PlanStep, runs it through FfmpegPlugin::build_command, and asserts the resulting command's metadata_effect field equals a MetadataPlanEffect with both source_tags_transferred_from_original_source and artwork_transferred_from_original_source set true.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_encode_from_original_source_carries_original_source_effects`
- spec 3 · read at `1d283fe5f307` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T00:37:16Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test verifies that when building an ffmpeg encode step whose input is the original source file (not an intermediate), the resulting plan step's effects list declares/carries "original source" metadata-transfer effects — contrasting with the intermediate-input variant which must NOT claim original-source transfer. It likely builds the ffmpeg plan step and asserts the effects include a typed marker indicating original source metadata carries through.
- found: Builds an ffmpeg encode-PCM step whose input path matches the original request's input path, and asserts the built command's metadata_effect has both source_tags_transferred_from_original_source and artwork_transferred_from_original_source set true, plus that metadata_disposition returns WritesRequestedPolicy (meaning a later MetadataTransfer step could be redundant) since the step reads the original source directly.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `ffmpeg_encode_from_intermediate_preserves_current_input_without_claiming_original_source_transfer` — QUIRKY
- spec 3 · read at `5019f5f37dda` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:53:00Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test that builds an ffmpeg encode command/plan step where the input is an intermediate file (e.g. output of a prior pipeline stage) rather than the user's original source file, and asserts the resulting command/step uses that intermediate as its input path while NOT attaching the "original source transfer" effect/metadata marker that the sibling test (ffmpeg_encode_from_original_source_carries_original_source_effects) expects when encoding directly from the original. This distinguishes direct-from-original encodes (which can carry over original-file metadata/effects) from chained encodes off an intermediate.
- found: Builds an EncodePcm step whose input is an intermediate.wav (not the original source), then asserts FfmpegPlugin.build_command's metadata_effect only marks tags/artwork preserved from the command's current input (not from the original source), and that metadata_disposition is DoesNotWrite so this step won't cause an explicit original-source MetadataTransfer step to be pruned downstream.
- predicted: some · documented: none · derivable: no · legible: most · trap: no
- note: The real assertion targets are metadata_effect fields and metadata_disposition, not the command's input path itself as I guessed.

### `metaflac_source_audio_md5_carries_typed_metadata_effect`
- spec 3 · read at `b704c48ae50a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:11:05Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a metaflac-based plan/command for reading the source FLAC's STREAMINFO MD5 signature, then asserts the resulting plan step carries a typed metadata effect (e.g. a MetadataEffect variant tagging it as the source audio MD5) rather than an untyped/generic effect, verifying the planner's typed-effect wiring for this specific metaflac operation.
- found: Builds a full PlanRequest (FLAC source/target, store_source_audio_md5=true) and a PlanStep with PlanOperation::StoreSourceAudioMd5, runs it through MetaflacPlugin.build_command, and asserts the resulting command's metadata_effect equals MetadataPlanEffect{source_audio_md5_written: true, ..none()} — verifying the plugin correctly flags this typed effect field.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_to_pcm_manual_gain_without_value_fails_loudly`
- spec 3 · read at `33c55fc0b8ff` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:29:54Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A test that configures DSD-to-PCM conversion with manual gain mode but omits the required gain value, then asserts building the sox/ffmpeg command returns an explicit error (or panics) rather than silently falling back to a default gain — verifying "fails loudly" instead of silent misconfiguration.
- found: Test calls add_sox_dsd_to_pcm_gain with Manual gain mode and no value, asserts it returns Err and leaves args empty (no partial writes).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_to_pcm_manual_gain_with_value_emits_gain`
- spec 3 · read at `4db693f9fa87` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:22:32Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Test builds a DSD-to-PCM conversion configured with manual gain mode and an explicit gain value, generates the resulting command (likely sox), and asserts that the produced command args include a gain effect/flag carrying that specific value.
- found: Builds a legacy_dsd config with Manual gain mode and value 2.25, calls add_sox_dsd_to_pcm_gain, asserts args == ["gain", "+2.25"].
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `dsd_to_pcm_auto_gain_emits_norm_margin`
- spec 3 · read at `72a04bef5532` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:25:49Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Test builds a DSD-to-PCM conversion plan with "auto gain" mode selected (instead of manual/disabled), and asserts the resulting sox command chain includes a normalization margin argument, verifying the auto-gain path emits the margin value rather than a fixed dB gain.
- found: Directly calls add_sox_dsd_to_pcm_gain with Auto mode and a 0.50 margin, asserting emitted args are exactly [\"norm\", \"-0.50\"].
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `dsd_to_pcm_disabled_gain_preserves_legacy_fixed_db`
- spec 3 · read at `3f60249c294c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:30:50Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a DSD-to-PCM sox command with gain disabled, and asserts that the emitted command still includes the legacy fixed dB gain value (for backward compatibility with prior behavior) rather than omitting the gain argument or using auto/norm-margin gain.
- found: Builds a legacy DSD config with gain mode Disabled and a fixed -1.5 dB value, calls add_sox_dsd_to_pcm_gain, and asserts the emitted args are exactly ["gain", "-1.50"].
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `dsd_sinc_guard_command` — OBSCURE
- spec 3 · read at `a852412b6d17` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:40:17Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: This builds a PlannedCommand representing a sox 'sinc' low-pass filter step used to guard against DSD noise-shaping artifacts above the target sample rate's Nyquist frequency when downsampling from source_hz to target_rate_hz. It likely computes a cutoff frequency based on target_rate_hz (e.g. slightly below half of it) and formats the sox sinc filter arguments accordingly, only emitting a filter when the cutoff is actually needed given the source rate.
- found: Test helper that builds a full PlanRequest/context for a DSF DSD source at source_hz being converted to FLAC at target_rate_hz with the SoxUltra lowpass method, then delegates to SoxPlugin.build_command to produce the actual PlannedCommand (the real sox sinc/lowpass argument logic lives in SoxPlugin, not here).
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: Name suggested it directly computes/formats a sinc filter's cutoff, but it's actually a test fixture constructor that delegates all filter logic to SoxPlugin::build_command.

### `dsd_noise_strip_sinc_is_skipped_when_cutoff_reaches_target_nyquist`
- spec 3 · read at `52c6816d91b7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:53:30Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test that configures a DSD-to-PCM conversion where the noise-strip sinc filter's cutoff frequency equals the target sample rate's Nyquist frequency, then asserts the generated sox/ffmpeg command chain omits the sinc filter step since it would be redundant with the final resample's own band-limiting.
- found: Tests dsd_sinc_guard_command across multiple DSD rate/target pairs (DSD64->44.1k, DSD256->88.2k, and the exact equality boundary DSD128->96k and DSD256->192k) asserting the "sinc" arg is omitted since sox rejects cutoff>=Nyquist; then a final case (DSD256->352.8kHz) where cutoff is well below Nyquist, asserting the sinc filter IS present with specific args ["sinc","-a","180","-96000"].
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Inline comments explain the real-world sox failure motivating this guard and the exact equality boundary (< vs <=), which isn't derivable from the function name alone.

### `dsd_to_pcm_auto_gain_golden_sox_command_chain`
- spec 3 · read at `1e698711fc8a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:16:06Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A golden test that constructs a DSD-to-PCM conversion request with auto-gain enabled, invokes the sox command builder plugin, and asserts the resulting full sox argument list (including rate/filter/gain/norm-margin flags and input/output paths) matches an exact expected sequence of strings, to catch any unintended change to the generated command chain.
- found: Constructs a PipelineSettings/SourceInfo/PlanRequest for a DSF->FLAC DSD-to-PCM conversion with auto-gain margin 0.15dB, builds a DsdToPcm PlanStep targeting 88200Hz/16-bit with SoxUltra lowpass, invokes SoxPlugin.build_command, and asserts the exact resulting sox arg list (rate/sinc filter/norm margin/dither flags) matches a hardcoded golden sequence.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/src/qualification_schema.rs

### the file itself
- spec 3 · read at `5a8d6f98b411` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:17:02Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A Rust module defining strongly-typed, serde-based structs (with deny_unknown_fields) for the "streamed WAV capacity evidence" schemas at multiple versions (V2 for policy v12, V3 for a later policy v13), used both by the Python qualification-derivation scripts (as the JSON schema they must produce) and by runtime release-certification code that validates evidence against a policy. Each versioned struct has methods computing canonical boundary constants (largest_frame_aligned_admitted_payload, expected_transition_count) and an is_canonical_vN check, plus unit tests asserting the boundary math is exact/frame-aligned and that the schema strictly rejects unknown fields (both nested and root) and only accepts the correct contiguous boundary shape.
- found: Matches prediction closely: V2 (policy v12) and V3 (policy v13) structs with deny_unknown_fields, each carrying a full contiguous boundary-scan of the streamed-WAV RIFF-size overflow near 4 GiB, with is_canonical_vN validating every field/relationship (not just the top-level ones), plus unit tests asserting exact boundary constants and that both valid-topology and unknown-field rejection work.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no
- note: The is_canonical_v12/v13 methods are far more intricate than 'validate against constants' suggests — they reconstruct expected per-observation values (payload, RIFF size, admission/rejection, error code) for every entry in transition_scan and cross-check accepted_edge/first_policy_rejected_edge/data_wrap_witness all independently derive from the same scan, which the docstring undersells as 'validates their topology.'

### `largest_frame_aligned_admitted_payload`
- spec 3 · read at `984bc731a8ed` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:27:21Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Computes a constant equal to the largest payload size in bytes that is a whole multiple of the frame size and still fits within some fixed maximum boundary defined by policy v12, via integer floor-division and multiplication (max_bound / frame_size * frame_size), returned as u64.
- found: Floor-divides V12_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES by REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE then multiplies back, aligning the max payload down to a whole-sample boundary.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `expected_transition_count` — QUIRKY
- spec 3 · read at `973765620454` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:32:40Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Returns a hardcoded u64 constant representing the fixed number of contiguous frame observations required for v2 evidence to be considered canonical/valid — a simple one-line literal return, analogous to sibling constant-like methods on the same struct (e.g. largest_frame_aligned_admitted_payload, is_canonical_v12).
- found: Computes the count as (DATA_WRAP_PAYLOAD_BYTES - largest_frame_aligned_admitted_payload) / BYTES_PER_SAMPLE + 1, a derived formula rather than a hardcoded literal.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `is_canonical_v12` — TANGLED
- spec 3 · read at `709629243dba` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:33:54Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Checks multiple fields of the ReferenceStreamedWavCapacityEvidenceV2 struct against hardcoded/compiled policy-v12 constants and cross-field relationships (e.g. that boundary probe values are contiguous, frame-aligned, and consistent with largest_frame_aligned_admitted_payload and expected_transition_count), returning true only if every check passes. It's long because it validates many discrete fields/edges rather than deriving a single formula.
- found: Validates every field of the report against compiled constants and cross-checks the transition_scan observations against a computed closure that verifies frame/payload/RIFF-size math and planner admission status per index, plus checks the accepted edge, first-rejected edge, wrap offset, and data-wrap witness values all match derived expectations — returning true only if the entire topology is internally consistent.
- predicted: most · documented: most · derivable: no · legible: some · trap: no
- note: Predicted the general shape (many field checks) correctly but underestimated the depth: it recomputes exact byte-level RIFF/data-size arithmetic per scan entry via checked_add/checked_mul and validates planner_admission/error_code semantics per index, which is much more involved than 'cross-field relationships'.

### `largest_frame_aligned_admitted_payload` #2
- spec 3 · read at `bfc6d5721226` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:37:44Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Computes the largest payload size in bytes that is (a) at or below the v13 policy's maximum admitted payload constant and (b) an exact multiple of the WAV frame size, by integer-dividing the max by the frame size and multiplying back — mirroring the V2 version but using V3's corrected constants.
- found: Integer-divides the max admitted payload constant by bytes-per-sample and multiplies back, rounding down to the nearest sample-aligned boundary; uses the same constant names as V2 rather than V3-specific ones.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `expected_transition_count` #2 — QUIRKY
- spec 3 · read at `418ae4206c37` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:42:46Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns a hardcoded u64 constant representing the required number of contiguous frame observations for the V3 evidence schema, likely a corrected value distinct from the V2 constant given the "corrected contiguous boundary" test peer.
- found: Computes the gap in bytes between DATA_WRAP_PAYLOAD_BYTES and the largest frame-aligned admitted payload, divides by bytes-per-sample, and adds 1 — a derived/computed value rather than a hardcoded literal.
- predicted: some · documented: most · derivable: no · legible: full · trap: no
- note: The doc comment tells you what it means but not that it's derived arithmetically from two other constants/methods rather than a literal, so you can't tell from the doc alone whether changing DATA_WRAP_PAYLOAD_BYTES would silently change this too.

### `is_canonical_v13` — QUIRKY — TANGLED
- spec 3 · read at `b4603225f9bf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:48:20Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Validates that the evidence's edge-probe data forms a consistent, contiguous topology expected under policy-v13: checks the sequence of probes for correct ordering/contiguity, cross-references largest_frame_aligned_admitted_payload() and expected_transition_count() to confirm the boundary and transition count match what policy predicts, and returns true only if everything lines up structurally (not validating the specific defective-writer field values, per the doc comment).
- found: A large conjunction validating every field of the report against fixed policy-v13 constants (contract, sample rate, channels, sizes, error code), then checks the transition_scan sequence has the expected length and each entry matches an expected structural computation (payload size, RIFF/data size fields, planner admission status), verifies the accepted_edge and first_policy_rejected_edge appear at the right positions in the scan and satisfy expected structural properties, locates the RIFF-size wrap-around offset via a windows(2) scan and checks it matches the reported field, and validates the data_wrap_witness against fixed expected constants.
- predicted: some · documented: some · derivable: no · legible: some · trap: no
- note: The doc comment says it validates topology rather than exact writer fields, but the body actually pins many exact field values (data_wrap_witness constants like 58/8, sample rate, channels) alongside topology checks.

### `synthetic_canonical_evidence` — QUIRKY — TANGLED
- spec 3 · read at `0aaefcb35e2f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:12:40Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture builder that constructs and returns a ReferenceStreamedWavCapacityEvidenceV2 instance populated with the known-correct canonical boundary constants (largest frame-aligned admitted payload size, expected transition count, etc.) so that other tests (like v2_schema_accepts_only_a_contiguous_canonical_boundary and is_canonical_v12 checks) can reuse a single valid baseline value rather than re-deriving the constants themselves.
- found: Builds a full synthetic ReferenceStreamedWavCapacityEvidenceV2: scans through expected_transition_count synthetic boundary observations (accepted edge, first-rejected edge, data-wrap edge) computing payload sizes, RIFF/data size fields (real or synthetic depending on admission), then assembles a data-wrap witness and the complete evidence struct with all its metadata fields (sample rate, channels, encoding, overhead constants, etc.) — far more than plugging in two constants.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `v2_boundary_constants_are_exact_and_frame_aligned`
- spec 3 · read at `beb625ba2061` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:45:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A unit test that asserts the V2 evidence schema's boundary constants (like largest_frame_aligned_admitted_payload) equal specific expected numeric literals and are evenly divisible by the PCM frame size, guarding against silent drift in the hardcoded canonical boundary values.
- found: A unit test asserting exact numeric values for four V2 evidence schema constants/methods: STREAM_HEADER_BYTES==66, largest_frame_aligned_admitted_payload()==4,294,967,232, expected_transition_count()==10, and DATA_WRAP_PAYLOAD_BYTES==4,294,967,304 — pinning down the boundary constants for WAV RIFF 32-bit size wraparound near the 4GiB limit.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `v2_schema_accepts_only_a_contiguous_canonical_boundary` — QUIRKY
- spec 3 · read at `08be5a738a43` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:47:49Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds synthetic ReferenceStreamedWavCapacityEvidenceV2 instances near the canonical boundary constants and asserts is_canonical_v12() returns true exactly at the correct contiguous boundary value, and false for values one below or one above (or with a gap), confirming the canonical-acceptance check has no off-by-one slack.
- found: Starts from a synthetic canonical evidence fixture asserted true, then tests three independent mutations each individually flip is_canonical_v12() to false: a discontinuity injected into transition_scan (adding one sample's worth of bytes mid-scan), decrementing the accepted edge's observed_data_size_field by 1, and setting an unrelated data_wrap_witness.consumer_completeness_claim flag to true (false claim of completeness).
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: The completeness-claim mutation is not a boundary/contiguity issue at all — it shows is_canonical_v12 also checks a consumer-completeness witness field unrelated to the byte-boundary math the function name implies.

### `v2_schema_rejects_unknown_root_and_nested_fields`
- spec 3 · read at `bc85d00f36b0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:40:24Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a valid JSON value via synthetic_canonical_evidence, then creates two variants: one with an extra unknown field injected at the root, and one with an extra unknown field injected in a nested object. It asserts that deserializing each into ReferenceStreamedWavCapacityEvidenceV2 fails, proving the schema uses deny_unknown_fields at both root and nested levels.
- found: Serializes synthetic_canonical_evidence to JSON, injects an unknown bool field at root and asserts deserialization to V2 fails; then does the same inside the nested "accepted_edge" object and asserts that fails too.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `synthetic_canonical_evidence_v3` — QUIRKY — TANGLED
- spec 3 · read at `fea3e602d0df` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:18:19Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture builder that constructs a ReferenceStreamedWavCapacityEvidenceV3 populated with hardcoded canonical/expected values (largest frame-aligned admitted payload, expected transition count, etc.) such that is_canonical_v13() on it returns true — used as a known-good baseline in the v3 schema tests referenced among its peers.
- found: Builds a full ReferenceStreamedWavCapacityEvidenceV3 fixture by simulating a scan across boundary transitions: it generates a sequence of boundary observations (accepted edge, first rejected edge, and further synthetic entries up to a data-wrap case), computing riff/data size fields differently depending on whether each entry is the accepted one, the wrap case, or a generic rejected one, then assembles accepted_edge, first_policy_rejected_edge, transition_scan and a data_wrap_witness into the final struct with status "passed".
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `v3_boundary_constants_are_exact_and_frame_aligned`
- spec 3 · read at `f2bd0f7879d2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:45:26Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Test asserts that the V3 boundary/threshold constants (byte offsets or sizes tied to ReferenceStreamedWavCapacityEvidenceV3) equal specific expected literal values and are evenly divisible by the WAV frame size, ensuring no off-by-one or misalignment relative to V2's corrected boundary.
- found: Asserts exact literal values for WAV stream header size (58), RIFF size overhead (50, header-8), max audio payload near u32::MAX (4_294_967_245), the largest frame-aligned admitted payload, expected transition count (9), and the data-wrap payload size — pinning V3's near-4GiB boundary constants.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `v3_schema_accepts_only_the_corrected_contiguous_boundary` — QUIRKY
- spec 3 · read at `f95c0b2ccd40` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:53:33Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds synthetic V3 evidence at the exact expected/corrected boundary payload size (via largest_frame_aligned_admitted_payload/expected_transition_count) and asserts it validates as canonical (is_canonical_v13), then perturbs that boundary by +/-1 and asserts those values are rejected — verifying V3's fix to an off-by-one boundary bug present in V2.
- found: Builds synthetic canonical V3 evidence and confirms is_canonical_v13() passes; then independently mutates stream_header_bytes to a stale value, riff_size_overhead_bytes to a stale value, and makes the transition scan discontinuous by bumping one entry's audio_payload_bytes by one sample-width — each mutation individually should fail is_canonical_v13().
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The exact stale magic numbers (66, 58) come from a prior V2 header/overhead layout that V3 corrected, not derivable from names alone.

### `v3_schema_rejects_unknown_root_and_nested_fields`
- spec 3 · read at `95cad0c598ca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:34:51Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A test that serializes synthetic_canonical_evidence_v3 to JSON, injects an extra unknown field at the root level and separately an extra unknown field in a nested object, and asserts that deserializing back into the V3 evidence struct fails in both cases (because the struct uses deny_unknown_fields / #[serde(deny_unknown_fields)]), guarding against silently accepting schema drift.
- found: Serializes canonical v3 evidence to JSON, inserts an unknown field at root and asserts deserialization fails, then separately inserts an unknown field into a nested "accepted_edge" object and asserts that also fails.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

## tonepoet-pipeline/src/settings.rs

### the file itself
- spec 3 · read at `c12e7a1309f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:25:28Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This file defines PipelineSettings as the central config schema for the pipeline crate — a struct aggregating per-format sub-settings (Flac, Mp3, Aac, Opus, WavPack, SSRC, resamplers, PCM<->DSD, metadata, verification, ReplayGain), each with Default impls, plus a validate method and helper validate_* functions enforcing cross-field consistency (target format/rate/encoder/dither compatibility). A large chunk is dedicated to DsdSettings legacy-vs-native-v2 wire format migration/compat shims, and the file ends with an extensive unit-test suite covering validation edge cases and legacy migration correctness.
- found: PipelineSettings aggregates per-format sub-settings structs (Flac, Mp3, Aac, Opus, WavPack, SSRC, resamplers, PcmToDsd, metadata, verification, ReplayGain) each with Default, plus validate() and validate_* helpers for cross-field consistency. Roughly half the file is DsdSettings legacy-vs-native-v2 wire migration/compat machinery, and the final ~15% is an extensive test suite. The doc header only describes the "unified conversion settings" framing and says nothing about the migration machinery or tests, which make up the majority of the file.
- predicted: full · documented: some · derivable: no · legible: not judged · trap: no
- note: Header doc names crate::SourceInfo as a related type and draws a boundary (runtime facts vs user config) but is silent on the DSD legacy/native-v2 migration machinery that dominates the file's size.

### `default`
- spec 3 · read at `fce98c429322` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:10:19Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs the default PipelineSettings by combining the Default impls of the various format-specific sub-settings (Flac, Mp3, Aac, Opus, WavPack, resamplers, PcmToDsd, etc.) with a handful of explicit baseline scalar values (e.g. target format, target rate, preferred tool), mostly via Self { field: Default::default(), ... }.
- found: Builds PipelineSettings with explicit baseline scalars (target format Flac, source rate/depth, Ultra resample quality, Gentle Nyquist transition, no dither, Auto preferred tool, no force encode) plus each sub-settings struct's own Default::default().
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `validate`
- spec 3 · read at `b015419b1fad` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:16:41Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Orchestrates validation by calling out to the sibling free functions (validate_target_format, validate_preferred_tool, validate_target_rate, validate_encoder_settings, validate_metadata, validate_dsd_settings) in sequence, propagating the first error with `?`, and returning Ok(()) if all pass. It likely also checks for incompatible combinations directly, such as a target format/rate pairing or DSD settings that conflict with the chosen preferred tool.
- found: Calls the six sibling validate_* helpers in sequence via `?`, then does substantially more inline: checks DSD/PCM target_format vs target_sample_rate/target_bit_depth consistency, a match over (format, bit_depth) pairs rejecting several unsupported combos (ALAC 32-bit, FLAC/ALAC float, float WavPack), and two more standalone checks (MD5 storage only for FLAC, flac.verify only for FLAC targets) — far more incompatible-combination logic than just delegating out.
- predicted: most · documented: some · derivable: no · legible: most · trap: no
- note: The doc comment ('validate value ranges and incompatible target combinations') undersells how much of the actual compatibility-matrix logic lives directly in this function body rather than in the named validate_* helpers.

### `explicit_dsd_rate`
- spec 3 · read at `55bd2260b16c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:01:47Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Looks at the DSD target rate field on PipelineSettings and returns Some(rate) when it's a concrete/explicit rate, but returns None when the configured target rate is the "Source" variant (meaning "keep whatever the source file's rate is"), since that case requires runtime SourceInfo which this settings-only method doesn't have access to.
- found: Matches on self.target_sample_rate: returns Some(rate) for RateTarget::Dsd(rate), and None for both RateTarget::Source and RateTarget::PcmHz(_), since neither is an explicit DSD rate.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `validate_target_format` — QUIRKY
- spec 3 · read at `ea3ac9183839` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:36:53Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Checks that `format` is an allowed conversion target — e.g. rejecting formats that can only be sources/passthrough (perhaps something like a raw or unsupported container) — returning Ok(()) if valid or an Err with a descriptive message if the format cannot be used as an output target.
- found: Only validates the AudioFormat::Custom variant: ensures extension is non-empty and contains no dot/slash/backslash, and display_name is non-empty after trimming; other format variants pass through with no checks.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `validate_preferred_tool`
- spec 3 · read at `59ea11be37c8` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:58:45Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Matches on the PreferredTool enum; for an "automatic" or unset variant it's a no-op returning Ok. For a variant that names a specific external tool/binary, it checks the name/path isn't empty or otherwise malformed and returns an Err with a descriptive message if it is.
- found: For PreferredTool::Custom(name), rejects an empty/whitespace-only name and also rejects names containing path separators (must be a bare binary name, not a path). Other variants are a no-op returning Ok.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate_target_rate`
- spec 3 · read at `2fcca1a36588` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:53:19Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Matches on the RateTarget enum; for a variant carrying an explicit numeric sample rate, checks it's within a plausible bounds (nonzero, not exceeding some max like 768000 Hz) and returns an error via the Result if out of range; other variants like "match source"/"auto" are always Ok.
- found: Matches RateTarget: Source and Dsd(_) are always Ok; PcmHz(hz) is Ok only within 8,000..=1,536,000, otherwise returns a PlanningError::invalid_settings with a descriptive message.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate_encoder_settings`
- spec 3 · read at `cfb9879ce2a5` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T21:19:31Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks the encoder-specific settings struct matching settings.target_format (Flac/Mp3/Aac/Opus/WavPack/etc.) and validates each field is within a legal range (e.g. FLAC compression level 0-8, MP3 bitrate/quality range, AAC/Opus bitrate bounds), returning Err with a descriptive message for any out-of-range or invalid value.
- found: Validates every encoder and resampler-related field in PipelineSettings unconditionally (not dispatched by target_format) — FLAC compression level, MP3/AAC/Opus bitrate and quality ranges, WavPack hybrid bitrate, SoX/SoXR resampler params (bandwidth, phase, sinc taps/attenuation/passband/transition/kaiser beta), and SSRC attenuation — plus a cross-field check that chebyshev filtering requires resample_quality >= High, then delegates to validate_ssrc_dither_settings.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate_metadata` — QUIRKY
- spec 3 · read at `6d072a153f67` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:00:40Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Validates the metadata-related fields of PipelineSettings (e.g. tag-copying/embedding options, cover-art handling, or metadata field mappings), returning an Err with a descriptive message if any combination is invalid or a referenced field/template is malformed, and Ok(()) otherwise — following the same validate_* helper pattern as its peers (validate_target_format, validate_encoder_settings, etc.).
- found: Much narrower than predicted: it checks exactly one specific cross-field constraint — that store_source_audio_md5 requires transfer_tags to also be enabled — and explicitly punts format-specific/ReplayGain validation to plugin dispatch elsewhere. I expected broader validation (cover art, field mappings, etc.) but it's a single targeted invariant check.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: The inline comment explains why validation scope is intentionally narrow (plugin dispatch handles the rest), which isn't visible from the signature/peers alone.

### `validate_dsd_settings` — QUIRKY — TRAP
- spec 3 · read at `37cf34502fa0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:44:07Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Validates a DsdSettings struct for a DSD audio pipeline: checks target DSD rate against supported values (e.g. DSD64/128/256/512), validates any numeric fields (e.g. via validate_finite_f32) are finite/in range, and checks enum-like fields (format/tool) are recognized. Likely sequentially validates several sub-fields and returns descriptive Err on the first invalid one, possibly also cross-checking compatibility with PCM-to-DSD or legacy wire-format settings.
- found: Validates DsdSettings: if a legacy wire-format is present, validates only legacy DSD-to-PCM gain/margin fields; otherwise validates from_dsd pathway/reference-policy/gain-mode fields (fixed gain range, normalize peak target). Then, regardless of legacy/non-legacy branch, always validates pcm_to_dsd settings: gain compensation (linear/dB ranges), optional trellis lookahead/nodes bounds, and sinc filter params (oversample factor power-of-two, taps power-of-two ≥1024, passband/transition/kaiser-beta ranges).
- predicted: some · documented: none · derivable: yes · legible: most · trap: yes
- note: The legacy vs. non-legacy branch is mutually exclusive: when legacy_wire() is Some, the newer from_dsd pathway/reference_policy/gain_mode checks are silently skipped entirely, so a future field added to from_dsd validation won't be enforced for legacy-wire settings unless added to both branches.

### `validate_finite_f32`
- spec 3 · read at `d90b5ae2a6f3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:24:12Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Checks value.is_finite(); if not (NaN or infinite), returns an Err containing an error message that includes the field name, otherwise returns Ok(()).
- found: Returns Ok(()) if finite, else Err(PlanningError::invalid_settings(field, "value must be finite")).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `default_true`
- spec 3 · read at `ca753ab7c1d2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:56:02Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns the literal boolean true; used as a serde default-value function for fields that should default to enabled.
- found: Trivial one-liner returning true, a serde default-value helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `default` #2
- spec 3 · read at `ed1d641c51ad` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:43:37Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Constructs a FlacSettings struct literal with sensible defaults for FLAC encoding, most likely a compression_level field set to a moderate/high value (e.g. 5 or 8), and possibly a boolean flag or two for things like verify-on-encode, mirroring the shape of sibling *Settings::default implementations (Mp3Settings, AacSettings, etc.) in this file.
- found: Returns FlacSettings { compression_level: 8, verify: false, write_md5: true }.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` #3
- spec 3 · read at `b51a09d5933d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:48:37Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs default Mp3Settings — a struct literal with sensible defaults like a VBR quality level (e.g. V0/V2-equivalent) or a default bitrate (e.g. 320kbps CBR), plus maybe a default encoding mode field.
- found: Struct literal: mode Vbr, bitrate_kbps 320 (unused in VBR mode but kept as fallback), vbr_quality 0 (highest VBR quality) — matches prediction.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `default` #4
- spec 3 · read at `a0bff3116d58` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:57:11Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Returns a default AacSettings struct literal with hardcoded reasonable defaults: something like a target bitrate (e.g. 256 kbps) or VBR quality level, a default encoding mode, and possibly a preferred encoder tool field set to None/Auto. Straightforward field initialization, no branching logic.
- found: Returns AacSettings with profile LcAac and bitrate_kbps 256 — a simple two-field struct literal.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` #5 — QUIRKY
- spec 3 · read at `9d21be20cc9a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:05:06Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs OpusSettings with sensible default encoder values: a default bitrate (likely 128 or 160 kbps), default VBR enabled, and default complexity, similar in shape to the other codec Settings::default() peers.
- found: Struct literal setting content_type to Auto, bitrate_kbps to 192, and complexity to 10 (max complexity), with no VBR field.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `default` #6
- spec 3 · read at `d97a5475e01c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:27:23Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns a WavPackSettings with sensible baseline defaults: a "normal" or middle compression level, hybrid/lossy mode disabled, no correction file, and extra processing off, mirroring the pattern of the sibling *Settings::default() impls in this file.
- found: mode: Normal, hybrid: false, hybrid_bitrate_kbps: 320, correction_file: true — I got mode and hybrid right but guessed correction_file would default off and missed the bitrate field entirely.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` #7
- spec 3 · read at `4a5d88d6b501` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:32:50Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Constructs Self as a struct literal with default values for SSRC (Shibatch-style sample rate converter) resampler settings — likely a default quality/profile enum variant, plus flags like dithering or bit-depth handling, mirroring the pattern of sibling *Settings::default implementations elsewhere in the file.
- found: Struct literal with all-off/None defaults: force=false, insane_mode=false, profile=None, attenuation_db=None, min_phase=false, dither_id=None, pdf_type=None.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `default` #8 — QUIRKY
- spec 3 · read at `bda496dcabc8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:34:47Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Returns a SoxResamplerSettings struct populated with sane default field values for invoking sox's resampler (e.g. a "high" or "very high" quality preset, steep filter disabled, default bandwidth/phase settings), mirroring sox's own command-line defaults.
- found: Returns SoxResamplerSettings with chebyshev and allow_aliasing false, and every other sinc/bandwidth/phase-related field defaulted to None (letting sox itself pick its own defaults rather than the app specifying a preset).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `default` #9 — QUIRKY
- spec 3 · read at `f821832d148a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:33:14Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Constructs a SoxrResamplerSettings default with reasonable resampling quality defaults — likely a high/very-high quality preset with standard phase response and passband/stopband percentages, set as a plain struct literal.
- found: Sets chebyshev: false and leaves cutoff and phase as None, i.e. defers to soxr library defaults rather than specifying concrete quality values.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `default` #10 — QUIRKY
- spec 3 · read at `82484b21a36d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:43:54Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Constructs default PcmToDsdSettings, setting fields like DSD rate (e.g. DSD64) and noise-shaping/modulator filter choice to reasonable defaults, matching the style of sibling *Settings::default() implementations in the file.
- found: Sets six fields to specific defaults: noise_shaper=Clans, modulator_order=Order8, trellis=None, filter=Auto, sinc=PcmToDsdSincSettings::default(), gain_compensation=Auto.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `default` #11 — QUIRKY
- spec 3 · read at `b5cd847b6930` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:21:52Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Constructs a LegacyDsdSettingsWireV1 struct with the legacy DSD-to-PCM default field values that predate the native v2 settings format — e.g. a default gain margin sourced from default_dsd_to_pcm_auto_gain_margin_db(), a default dither/auto-gain boolean via default_true(), and other legacy wire fields set to whatever historically shipped as defaults, so old serialized configs deserialize consistently.
- found: Sets legacy DSD wire defaults: noise_shaper=Clans, modulator_order=Order8, trellis=None, pcm_to_dsd_filter=Auto, dsd_to_pcm_lowpass=Auto, dsd_to_pcm_gain_mode=Disabled, gain margin via the helper I guessed, gain_db=None, nested sinc settings default, gain_compensation=Auto. I correctly guessed the gain-margin helper usage but the rest of the fields (noise shaper, modulator order, filter presets, gain mode) were specific DSD/SACD-domain enums I couldn't have named from the signature alone.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `default_dsd_to_pcm_auto_gain_margin_db`
- spec 3 · read at `7604f5b74106` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:01:56Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns a hardcoded f32 constant, likely used as a serde default value, representing a safety headroom margin in decibels (e.g. 1.0-3.0 dB) subtracted from the auto-gain calculation when converting DSD to PCM, to avoid clipping.
- found: Returns a hardcoded f32 constant 0.15 (dB), smaller than the 1-3 dB range I guessed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Correct purpose and mechanism, but the actual magnitude (0.15 dB) is much smaller than typical clipping-headroom values I'd expect — worth a comment explaining why such a small margin was chosen.

### `default` #12 — OBSCURE — TRAP
- spec 3 · read at `12c9d0558264` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:31:47Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Returns Self::native_v2(), delegating to the dedicated constructor for the modern native-v2 DSD settings shape rather than building default field values inline.
- found: Defaults to Self::from_legacy_wire(LegacyDsdSettingsWireV1::default()) rather than the native_v2 constructor, deliberately keeping the application default fail-closed/frozen on legacy behavior until a policy is promoted, per the inline comment.
- predicted: none · documented: none · derivable: no · legible: full · trap: yes
- note: The comment explains a non-obvious intent (fail-closed until promotion) that the code alone would not convey — anyone assuming default() means 'the modern native_v2 shape' (a reasonable assumption given sibling methods) would be wrong, and would silently change behavior for ordinary DSD-to-PCM conversions if they 'simplified' this to native_v2().

### `native_v2`
- spec 3 · read at `5c2a0a4bcb11` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:39:32Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs a DsdSettings in the "native v2" mode/version explicitly — sets an internal version/schema field to the NativeV2 variant and leaves the reference-policy field disabled/None (matching the doc's "opt-in" note), with other fields set to their sensible defaults.
- found: Builds DsdSettings with default PcmToDsdSettings and DsdSourceSettings, and origin explicitly set to DsdSettingsOrigin::NativeV2.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `from_legacy_wire` — QUIRKY
- spec 3 · read at `8f35505301cd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:42:12Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Constructs a DsdSettings value representing the legacy (non-native-v2) representation, likely wrapping the given LegacyDsdSettingsWireV1 in an enum variant (e.g. DsdSettings::Legacy(wire)) so is_native_v2 returns false and legacy_wire can retrieve it back out. Probably a simple one-line variant construction despite being 7 lines, maybe with a small transformation of fields.
- found: Builds a DsdSettings struct with pcm_to_dsd and from_dsd fields derived from the wire via helper functions, and stores the raw wire in an origin field tagged LegacyFlatV1 for round-tripping.
- predicted: some · documented: some · derivable: no · legible: most · trap: no
- note: DsdSettings is a struct with derived sub-settings plus an origin tag, not an enum wrapping the wire directly as I guessed.

### `is_native_v2`
- spec 3 · read at `a8e7200982cf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:07:35Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: DsdSettings stores an internal representation distinguishing native-v2 settings from ones derived/migrated from legacy wire format; this const fn matches on that discriminant and returns true only for the native variant.
- found: Matches self.origin against DsdSettingsOrigin::NativeV2, returning true only when the settings originated as native (as opposed to being migrated/derived from legacy wire format).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `legacy_wire`
- spec 3 · read at `ca6455077dd9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:55:25Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A const accessor that pattern-matches DsdSettings' internal representation and returns Some(&LegacyDsdSettingsWireV1) if it is currently in the legacy/v1 variant, or None if it has been migrated to native v2 form, without performing any conversion itself.
- found: Matches on self.origin (an "origin" tracking field, not the settings variant itself) returning Some(wire) for LegacyFlatV1 and None for NativeV2, matching my prediction closely though the exact field name/design (an origin tag rather than the settings enum itself) I didn't know.
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `migrate_to_native_v2`
- spec 3 · read at `ee19cdb99123` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:03:47Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Starts from Self::native_v2() (fresh native-v2 defaults) and overwrites its pcm_to_dsd field with self's original pcm_to_dsd settings, since those are unrelated to the legacy-vs-native-v2 distinction, then returns the resulting DsdSettings.
- found: If already NativeV2, returns self unchanged (no-op). If legacy, builds a new DsdSettings keeping pcm_to_dsd from self but resetting from_dsd to default and setting origin to NativeV2 -- so "native_v2 defaults" is really just DsdSourceSettings::default(), not a full native_v2() constructor call.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `legacy_compat_wire`
- spec 3 · read at `238d20865944` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:06:55Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: This builds a `LegacyDsdSettingsWireV1` struct by reading the current `DsdSettings` (whether native-v2 or legacy-wire internally) and flattening its fields into the old v1 wire shape — pulling values via the various `legacy_dsd_to_pcm_*` accessors seen in peers, filling in fixed/default values for any fields that don't exist in the native representation, so the fingerprint hash stays stable regardless of which internal representation is active.
- found: Matches on self.origin: if LegacyFlatV1, pulls the four dsd_to_pcm_* fields straight from the stored wire struct; if NativeV2, substitutes fixed defaults (Auto lowpass, Disabled gain mode, default margin, no gain). Then builds the flat LegacyDsdSettingsWireV1 combining those with pcm_to_dsd fields (noise_shaper, modulator_order, trellis, filter, sinc, gain_compensation) read directly off self.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: The one-line doc comment ("materialize the exact flat compatibility view") captures intent but not the origin-based branching mechanism, which is the actual substance.

### `set_legacy_dsd_to_pcm_gain`
- spec 3 · read at `65ef77681c00` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:58:47Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns an error if the settings are already in native-v2 form (since this API is only for the legacy wire). Otherwise sets the legacy gain mode field, and depending on whether mode is Auto or Manual, stores the relevant value (auto_gain_margin_db or gain_db) while resetting/canonicalizing the other, irrelevant field to a fixed default so serialized output stays byte-identical to the legacy format regardless of which mode was previously active.
- found: Rejects native-v2 settings, validates auto_gain_margin_db is finite and within 0-6dB (Auto mode) and gain_db is present, finite, and within -24..24dB (Manual mode), then rebuilds via legacy_compat_wire with the irrelevant field canonicalized (default margin or None gain) and reconstructs self via from_legacy_wire.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `legacy_behavior`
- spec 3 · read at `3e77616ffa00` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:38:03Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Returns None if the settings are already in native_v2 mode (self.is_native_v2()), otherwise constructs and returns Some(LegacyDsdBehavior) populated from the stored legacy wire fields (gain mode, lowpass, auto-gain margin, etc.) — a read-only accessor exposing legacy compatibility behavior without allowing mutation.
- found: Maps self.legacy_wire() (an Option) into Some(LegacyDsdBehavior) with the four fields lowpass, gain_mode, auto_gain_margin_db, gain_db copied straight across; None if legacy_wire() returns None.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `legacy_dsd_to_pcm_lowpass`
- spec 3 · read at `113661afd004` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:12:38Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A simple accessor that reads the low-pass method field out of an internal legacy-compatibility struct (mirroring sibling getters like legacy_dsd_to_pcm_gain_mode/gain_db/auto_gain_margin_db), returning the raw stored DsdLowpassMethod used only by the legacy/pre-v2 wire format planner.
- found: Returns the dsd_to_pcm_lowpass field from legacy_compat_wire(), an accessor into the legacy wire-format compatibility struct.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `legacy_dsd_to_pcm_gain_mode` — QUIRKY
- spec 3 · read at `177c739a9ff9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:17:41Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A small derived accessor that inspects the settings' legacy gain fields (e.g., whether a manual gain_db override is set vs. an auto-gain margin) and returns the corresponding DsdToPcmGainMode::Fixed(db) or DsdToPcmGainMode::Auto(margin) variant — pure read-only logic, no mutation.
- found: Simply delegates to self.legacy_compat_wire().dsd_to_pcm_gain_mode — a one-line field pluck off the constructed legacy wire struct, not the field-inspection logic I predicted.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `legacy_dsd_to_pcm_auto_gain_margin_db` — OBSCURE
- spec 3 · read at `2503b321e270` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:52:24Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Returns a hardcoded constant dB margin value (e.g. 1.0-3.0) used as headroom for the legacy DSD-to-PCM auto-gain compatibility planner, rather than reading a stored settings field.
- found: A simple accessor that delegates to legacy_compat_wire() and returns its dsd_to_pcm_auto_gain_margin_db field — a thin getter over a legacy wire/compat struct, not a computed or constant value.
- predicted: none · documented: some · derivable: no · legible: full · trap: no
- note: The doc describes what the value is for but not that it's a passthrough getter to a separate legacy_compat_wire() struct; the actual storage location isn't derivable from this function alone.

### `legacy_dsd_to_pcm_gain_db`
- spec 3 · read at `342ac82666a3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:23:15Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns the fixed gain value (in dB) used by the legacy DSD-to-PCM compatibility planner, likely reading a stored field like self.legacy_gain_db, returning None if not set or if the settings are already migrated to native v2 mode.
- found: Delegates to legacy_compat_wire() and returns its dsd_to_pcm_gain_db field, rather than reading a field directly on self.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `legacy_pcm_to_dsd`
- spec 3 · read at `06760fc6f56a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:46:31Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Extracts the PCM-to-DSD-relevant fields from the legacy wire-format struct (e.g. sinc converter parameters, oversampling settings) and constructs the modern PcmToDsdSettings equivalent, likely with defaults filling in any fields the legacy format didn't have. Used as part of migrating old serialized settings to the current native representation.
- found: Direct field-by-field mapping from the legacy wire struct's PCM-to-DSD fields (noise_shaper, modulator_order, trellis, pcm_to_dsd_filter→filter, sinc, gain_compensation) into a new PcmToDsdSettings, no defaults or transformation logic involved.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `legacy_from_dsd_mirror` — QUIRKY
- spec 3 · read at `6e98e15da53c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:38:53Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A test-support conversion helper that takes a LegacyDsdSettingsWireV1 (deserialized legacy wire struct) and manually constructs the equivalent DsdSourceSettings, copying/mapping each legacy field (lowpass filter, gain mode, gain dB, auto-gain margin dB, etc.) into the corresponding new-format field — used to verify that DsdSettings::legacy_wire/migrate_to_native_v2 correctly mirror the legacy behavior in round-trip tests.
- found: Maps a LegacyDsdSettingsWireV1 into DsdSourceSettings: derives gain_mode from a 4-way match on the legacy gain mode enum + whether a gain_db was set, hardcodes pathway/reference_policy/profile to fixed reference values, and converts fixed_gain_db and normalize_peak_target_dbfs via string-formatted round-trips into DbNano, with a fallback default on parse failure.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: I predicted a generic field-by-field mirror; the actual logic has nontrivial semantic remapping (gain mode inference, hardcoded pathway/profile, string-based DbNano conversion) that isn't guessable from the name alone.

### `legacy_mirror_matches` — QUIRKY
- spec 3 · read at `0ce98ad53b53` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:28:19Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Computes settings.legacy_compat_wire() and compares it to the given wire value for equality, returning true when the legacy mirrored wire representation matches, used presumably as a validation/assertion helper to check the legacy mirror hasn't drifted from the native settings.
- found: Converts the wire value into a from_dsd representation via legacy_from_dsd_mirror(wire) and compares it against settings.from_dsd for equality — direction is wire-to-native, not native-to-wire as I guessed, and it only checks the from_dsd sub-field, not the whole settings/wire struct.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Comparison direction is wire→native (via legacy_from_dsd_mirror) and scoped to just the from_dsd field, not a whole-struct round-trip check.

### `serialize` — QUIRKY — TRAP
- spec 3 · read at `31dc7f659d02` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:26:21Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Custom Serialize impl for DsdSettings that converts self into a legacy-compatible wire representation (via legacy_compat_wire or similar) before delegating to that struct's derived serialization, ensuring the on-disk/config format stays backward compatible with older field layouts even as the in-memory struct evolves.
- found: Branches on self.origin: for NativeV2 it serializes a versioned wire struct directly from pcm_to_dsd/from_dsd; for LegacyFlatV1 it first checks that the stashed legacy wire value still mirrors the current settings (legacy_mirror_matches) and errors out if they've diverged (settings edited without migrating), otherwise serializes via legacy_compat_wire().
- predicted: some · documented: none · derivable: yes · legible: full · trap: yes
- note: The error path (legacy origin whose mirror no longer matches) is a real trap for anyone extending DsdSettings fields: forgetting to update legacy_mirror_matches/legacy_compat_wire when adding a field will silently pass until a legacy-origin settings value is edited and serialization suddenly fails at runtime with a config-migration error rather than a compile error.

### `deserialize` — QUIRKY
- spec 3 · read at `ff5b12818702` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:03:39Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Custom Deserialize impl for DsdSettings: deserializes into an intermediate/raw representation capturing both the current nested layout and legacy flat fields (dsd_to_pcm_gain_db, dsd_to_pcm_gain_mode, dsd_to_pcm_lowpass, etc.), migrates/merges legacy fields into the current representation, validates numeric fields (rejecting non-finite gain values), and cross-checks legacy vs new fields agree (legacy_mirror_matches), erroring on divergence.
- found: Hand-written serde Visitor/MapAccess implementation that collects each field by key with duplicate/unknown-field checks, then branches: if any native-v2 keys (schema_version/pcm_to_dsd/from_dsd) are present, legacy keys must be entirely absent and schema_version must equal 2, producing DsdSettings{origin: NativeV2} directly; otherwise it requires all legacy-v1 fields (erroring on missing ones) and builds a LegacyDsdSettingsWireV1, converting via DsdSettings::from_legacy_wire.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: The actual legacy-field migration/validation logic (e.g. non-finite gain rejection) lives in DsdSettings::from_legacy_wire, not in this visitor.

### `default` #13
- spec 3 · read at `948e28c29867` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:37:51Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Default impl for PcmToDsdSincSettings, returning Self with reasonable default sinc-conversion parameters (e.g., filter taps/order, oversampling factor, cutoff frequency) chosen as sane defaults for PCM-to-DSD conversion quality.
- found: Returns default sinc-filter settings: oversample_factor 8, 262144 taps, 25kHz passband with 500Hz transition, Kaiser beta 16.0, linear_phase true, allow_aliasing false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` #14
- spec 3 · read at `cd3549c1eb4a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:42:55Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Returns a MetadataSettings struct with sensible defaults for tag handling during conversion — likely preserving/copying source tags and other metadata fields set to conservative "keep existing metadata" defaults.
- found: Returns MetadataSettings with transfer_tags: true, preserve_artwork: true, and store_source_audio_md5: false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate_ssrc_dither_settings` — QUIRKY
- spec 3 · read at `0c31828edf8d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:21:14Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Checks that if the settings specify an explicit SSRC dither ID along with an explicit target PCM rate, that dither ID is actually available/valid at that rate (some dither presets are unavailable for low sample rates), returning an actionable Err if not; otherwise returns Ok(()).
- found: First validates any explicit dither_id is in range 0-99. Then bails out Ok(()) if the settings won't emit SSRC integer dither at all, or if the target rate isn't a concrete PCM Hz value. If those gates pass, validates the explicit dither_id against the target rate, or if no explicit id was given, checks that automatic selection (ssrc_dither_selection_for_rate) succeeds for the configured dither_type/rate.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `settings_may_emit_ssrc_integer_dither`
- spec 3 · read at `0d401c39b8ea` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:10:51Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Returns true if the pipeline settings indicate SSRC-based resampling is active and the target PCM format is an integer format (as opposed to float), since integer dither only applies when both SSRC resampling is used and the output isn't floating-point — likely checking the resample config's dither field is non-default alongside an integer PCM depth.
- found: Checks uses_ssrc (preferred_tool is Ssrc, or ssrc.force, or brick-wall Nyquist transition) AND target_bit_depth is Source or an integer PcmBitDepth (8/16/24); returns true only if both hold.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` #15 — QUIRKY
- spec 3 · read at `e4d0ea182e95` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:47:47Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs a VerificationSettings struct literal with default field values — likely verification enabled at a basic/fast level (e.g. decode-compare) rather than the most expensive option, with any threshold or tolerance fields set to reasonable defaults.
- found: Two-field struct: verify_after_encode defaults to false (verification is opt-in), prefer_native_flac_verify defaults to true (governs which verify method is used when enabled).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `default` #16
- spec 3 · read at `72d8305c5830` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:34:00Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns a single enum variant constant, e.g. Self::Recalculate, representing the pipeline's default policy of recomputing ReplayGain values rather than trusting/preserving pre-existing tags on the source file.
- found: Returns Self::Rescan, a variant name close in spirit to my guessed Recalculate/Overwrite — default is to recompute rather than trust existing tags.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default` #17 — QUIRKY
- spec 3 · read at `fc70101def44` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:48:00Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Returns a ReplayGainSettings struct with conservative defaults: ReplayGain scanning/tagging disabled by default, some default target loudness value, album-mode enabled, and existing_tag_policy set to its own Default (likely referenced from ReplayGainExistingTagPolicy::default seen as a peer).
- found: Default ReplayGainSettings: mode is None (no track/album mode selected by default), prevent_clipping is true, and existing_tags defaults to ReplayGainExistingTagPolicy::Rescan.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: mode: None presumably means ReplayGain is off/unset by default rather than an explicit enabled flag — the on/off switch is encoded as an Option rather than a bool, not obvious from the name.

### `default_pcm_depth_for_format` — QUIRKY
- spec 3 · read at `75295f509d09` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:51:46Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A match over AudioFormat variants (Flac, Alac, WavPack, Mp3, Aac, Opus, Wav, Aiff, etc.) mostly returning PcmBitDepth::Int16 as the safe/compatible default, with a few format-specific exceptions where 16-bit isn't the sensible default.
- found: Match over AudioFormat: Wav/Aiff/Flac/WavPack/Alac/Dsf/Dff/Custom all default to Int24; only the lossy formats (Mp3, Aac, Opus, Dts, Ac3) default to Int16 — I had the majority backwards, guessing Int16 as the common default when Int24 is actually the majority case.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `alac_int32_is_rejected_with_actionable_message`
- spec 3 · read at `cfe1b9fadf3f` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:54:26Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test that builds conversion settings targeting ALAC output with an explicit 32-bit integer PCM depth (which ALAC doesn't support, capping at 24-bit), runs settings validation, and asserts it returns an error whose message clearly explains the ALAC bit-depth limitation rather than a generic failure.
- found: Sets target_format to Alac and target_bit_depth to Int32 on default PipelineSettings, calls validate(), and asserts the resulting error message contains "ALAC 32-bit".
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `wavpack_float_targets_are_rejected`
- spec 3 · read at `4b0ac809f35d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:16:33Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a settings/validation scenario targeting WavPack output with a float PCM depth, then asserts that validation returns an error (since WavPack doesn't support float samples), mirroring the neighboring alac_int32_is_rejected_with_actionable_message test which checks for a specific, actionable error message.
- found: Loops over Float32 and Float64 PCM depths, sets target format to WavPack and that bit depth on default settings, and asserts validate() errors for each (no message content check, just expect_err).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `flac_int32_and_aiff_float_remain_valid_settings`
- spec 3 · read at `36e44a3b6c71` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:44:43Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Test that builds settings targeting FLAC with 32-bit int depth and separately AIFF with float sample format, then asserts validation succeeds (no error/Ok), contrasting with sibling tests that show ALAC int32 and WavPack float are rejected — establishing that FLAC/AIFF support these encodings while ALAC/WavPack don't.
- found: Exactly as predicted: sets FLAC + Int32 and asserts validate() succeeds, then AIFF + Float32 and asserts validate() succeeds, contrasting with sibling ALAC/WavPack rejection tests.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `validates_explicit_ssrc_dither_id_against_explicit_pcm_rate`
- spec 3 · read at `08b65d574689` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:58:18Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A unit test that builds a settings struct with both an explicit SSRC dither ID and an explicit target PCM sample rate set, then calls validate_ssrc_dither_settings (or similar) and asserts the combination is accepted/validated correctly (e.g. valid dither id for that rate passes, or a mismatched one produces an error), exercising the "explicit x explicit" validation path as opposed to the "derived"/deferred paths seen in sibling tests.
- found: Sets SSRC as preferred tool, brickwall transition, explicit 96kHz target rate, and an invalid dither_id (16) which fails settings.validate(); then sets a valid dither_id (2) which passes validate(). Confirms invalid-dither-id-for-explicit-rate is rejected and a valid one is accepted, via the overall settings.validate() call rather than a dedicated dither-only function.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I guessed a dedicated validate_ssrc_dither_settings call; it actually goes through the top-level settings.validate().

### `rejects_low_rate_unavailable_ssrc_dither_id`
- spec 3 · read at `df9bebc5cb31` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:32:01Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Constructs settings with an explicit low target sample rate and an explicit SSRC dither id that is only available at higher rates, calls validate_ssrc_dither_settings, and asserts the result is an Err indicating the dither id is unavailable for the given rate.
- found: Sets SSRC as preferred tool, brickwall transition, low target rate 22050Hz, and dither_id 6 (unavailable at that rate) -> validate() errs; then switches dither_id to 1 (valid) -> validate() succeeds, testing both the rejection and acceptance boundary via settings.validate().
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `defers_rate_dependent_ssrc_dither_validation_when_target_rate_is_source`
- spec 3 · read at `7b2de90722c4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:13:11Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test constructs pipeline settings where the target sample rate is set to "Source" (i.e., pass-through/no explicit resampling) combined with an SSRC dither id that would normally need validation against a concrete rate. It asserts that settings construction succeeds without error, because the validator can't know the real rate until it's resolved from the actual source file at runtime, so it defers/skips that rate-dependent check at config-build time.
- found: Sets target_sample_rate to RateTarget::Source and an ssrc dither_id, then asserts settings.validate() succeeds, confirming rate-dependent dither validation is skipped when the target rate is unresolved/source-relative.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `validates_derived_global_ssrc_dither_mapping_for_explicit_pcm_rate` — QUIRKY
- spec 3 · read at `ad792216d060` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:53:19Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a Settings/PipelineSettings with an explicit output PCM sample rate (not "same as source") and a global/default SSRC dither setting (not an explicit per-field dither id), then asserts validation succeeds and derives the correct SSRC dither id/mapping for that rate, contrasting with a sibling test where validation is deferred because target rate equals source rate.
- found: Builds default PipelineSettings with SSRC as preferred tool, explicit target rate 176400Hz PCM, 16-bit depth. Sets dither_type to HighShibata and asserts validate() fails (HighShibata presumably unsupported/invalid for this rate combination via SSRC's dither mapping), then switches dither_type to Tpdf and asserts validate() succeeds.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The 'global ssrc dither mapping' concept refers to DitherType being validated against rate-dependent support tables, not something visible from the name/signature alone.

### `validates_derived_ssrc_mapping_when_only_pdf_is_explicit` — OBSCURE
- spec 3 · read at `c4e7b8a0af51` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T20:33:52Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: This test builds a Settings/PipelineSettings value where only the PCM depth/format field is explicitly set (sample rate left as "derive from source" / implicit), then calls the settings validation routine and asserts it succeeds — verifying that the SSRC dither mapping can still be correctly derived and validated even though only the depth, not the rate, was explicit.
- found: Sets SSRC as the preferred tool with an explicit target rate/depth/dither type, but only sets ssrc.pdf_type (the SSRC noise-shaping PDF/dither probability density function type, not \"PCM depth format\" as I guessed) explicitly — then asserts validate() returns Err, i.e. this combination is actually invalid, not valid as I predicted.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: I misread "pdf" as PCM-depth-format; it's actually SsrcPdfType (dither probability density function), and the test asserts validation fails, not succeeds.

### `explicit_sample_rate_independent_ssrc_dither_id_can_override_invalid_global_mapping`
- spec 3 · read at `5ccddaa00cdd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:26:55Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A unit test constructing settings where the globally-derived SSRC dither mapping would be invalid for the target/output sample rate, but an explicit, sample-rate-independent SSRC dither id is also set. It asserts that validation succeeds (does not reject) because the explicit dither id choice takes precedence over the otherwise-invalid derived global mapping.
- found: Builds default PipelineSettings with SSRC as preferred tool, a target rate/depth that would otherwise conflict with the DitherType::HighShibata global mapping, but sets an explicit ssrc.dither_id (99) and pdf_type, then asserts settings.validate() is Ok — the explicit id overrides the otherwise-invalid derived mapping.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `skips_ssrc_dither_mapping_validation_for_int32_output`
- spec 3 · read at `ba88938a4c95` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:49:28Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds settings targeting int32 PCM output with an SSRC dither mapping that would otherwise be invalid, then asserts settings validation succeeds (returns Ok) because int32 output paths bypass SSRC dither mapping validation.
- found: Constructs default settings with WAV target, SSRC tool, 176.4kHz rate, Int32 bit depth, and an explicit HighShibata dither type, then asserts validate() succeeds since int32 output skips SSRC dither mapping validation.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: Inline comment clarifies WAV was chosen just to keep int32 container-valid, not as part of what's being tested.

### `skips_derived_ssrc_dither_validation_for_explicit_float_output`
- spec 3 · read at `ab60114fa1c4` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:08:52Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Test that constructs pipeline settings with an explicit float output format (no PCM/integer depth specified), leaves SSRC dither id unset/derived, and asserts validation succeeds (returns Ok / no dither-mapping error) because float output doesn't require derived SSRC dither validation.
- found: Builds default PipelineSettings, sets target format to Wav with Ssrc tool, explicit 176.4kHz rate, Float32 bit depth, and an explicit dither type (HighShibata), then asserts validate() succeeds since float output doesn't need SSRC dither rate validation.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validates_derived_ssrc_mapping_for_brickwall_auto_tool_settings` — QUIRKY
- spec 3 · read at `7c5b36432346` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:36:39Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: This test builds pipeline settings with an SSRC profile of "brickwall" and tool selection set to "auto" (no explicit dither id), then calls the settings validation function and asserts it succeeds — verifying that the validator correctly derives/maps an appropriate SSRC dither ID for this brickwall+auto combination rather than rejecting it as invalid.
- found: Builds default PipelineSettings with preferred_tool=Auto, nyquist_transition=BrickWall, target rate 176400 PCM, target bit depth Int16, and dither_type=HighShibata, then asserts settings.validate() returns an error — this combination (BrickWall transition with HighShibata dither) is invalid.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Test name says \"validates derived ssrc mapping\" but asserts an error, not success — the name describes the code path exercised (derived mapping logic), not the expected outcome, which could mislead a skimmer into expecting Ok().

### `exact_legacy_gain_mutation_canonicalizes_non_authoritative_fields` — QUIRKY
- spec 3 · read at `d20bcea3a06e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:50:25Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test that constructs settings with a legacy gain value set as authoritative alongside other replaygain-related fields set to stale/inconsistent values, applies the "exact legacy gain" mutation, and asserts that the non-authoritative fields get reset/canonicalized to consistent derived values rather than being left as-is. It's checking that mutating one authoritative field doesn't leave other related fields in a mixed/invalid state.
- found: Test verifying set_legacy_dsd_to_pcm_gain canonicalizes the fields not relevant to the given gain_mode: switching to Auto keeps auto_gain_margin_db and clears gain_db; switching to Manual resets auto_gain_margin_db to its default and sets gain_db; switching to Disabled resets auto_gain_margin_db to default and clears gain_db.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The "gain mutation" is specifically DSD-to-PCM conversion gain (Auto/Manual/Disabled modes), not general replaygain — the function name's genericness ('legacy gain') hides that domain specificity.

### `exact_legacy_gain_mutation_rejects_invalid_authority_without_mutation` — QUIRKY
- spec 3 · read at `84660b5f7aee` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:56:42Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Test that constructs settings with some invalid "authority" value for the legacy exact gain mutation path (e.g. mismatched or unrecognized origin), calls the mutation function, asserts it returns an error/rejection, and then checks that the original settings struct was left unchanged (no partial mutation occurred).
- found: Tests DsdSettings::set_legacy_dsd_to_pcm_gain rejects several invalid mode/value combinations (Auto mode with non-default gain, Manual mode with an out-of-range/invalid gain, Manual with an invalid extra parameter) and confirms the settings struct is unchanged (equal to snapshot taken before) after each rejected call.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: 'authority' in the test name refers to which field (mode/value) is authoritative for DSD-to-PCM gain, not a generic auth/permission concept as the name alone suggested.

### `exact_legacy_gain_mutation_never_creates_a_mixed_native_origin` — QUIRKY
- spec 3 · read at `5690a8551b9a` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:14:25Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Applies an exact-legacy-gain mutation to settings and asserts the resulting settings' origin marker(s) for gain-related fields stay uniformly consistent (all legacy or all native) rather than ending up as a mix of native and legacy origins across different fields, since a mixed-origin state would be invalid/ambiguous for downstream consumers.
- found: Starts with DsdSettings::native_v2() (a "native" DSD-to-PCM gain origin), attempts to set a legacy-style manual gain via set_legacy_dsd_to_pcm_gain, asserts that call errors (legacy mutation is rejected on native-origin settings), and confirms the settings remain native_v2 afterward — i.e. a rejected legacy mutation leaves origin untouched rather than partially applying and creating a mixed state.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: This is about DSD-to-PCM gain origin (native_v2 vs legacy), not ReplayGain — the neighboring ssrc/dither-focused peer list is misleading about the domain.

### `legacy_pipeline_settings_without_dither_explicit_default_to_automatic` — QUIRKY
- spec 3 · read at `383a11e98893` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:39:41Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This test constructs pipeline settings from a legacy format/source that lacks an explicit dither field, then asserts the resulting settings' dither mode defaults to an "Automatic" variant (rather than None/Off), verifying backward-compatible default behavior for old configs.
- found: Serializes default PipelineSettings to JSON, strips the dither_explicit field to simulate an old config missing that key, deserializes back, and asserts dither_explicit defaults to false (automatic) rather than true.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/src/source.rs

### the file itself
- spec 3 · read at `9ff87df106d8` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:17:27Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A small data-only module defining SourceInfo (a struct holding facts about an audio source gathered by upstream probing/extraction — likely sample rate, bit depth, channel count, DSD/PCM flags, format/container info) and a SourceRepresentationKind enum (e.g. DSD vs PCM, raw/decoded/packaged) with a Default impl. Methods are simple derived-fact accessors: is_dsd() checks a discriminant, representation_kind() classifies the source, authoritative_pcm_depth() returns the "real" bit depth to trust, dsd_rate() returns the DSD sample rate if applicable, and validate() checks internal field consistency and returns a Result.
- found: Defines SourceRepresentationKind (Pcm/Dsd/Lossy/Unknown/Unspecified, default Unspecified for backward compat) and SourceInfo, a struct of caller-supplied probed facts (format, codec, sample_rate_hz, bit_depth vs true_source_depth carrier/source split, source_representation, sample_kind, channels, duration, dsd_source_kind, audio_md5). Methods: is_dsd() checks format/codec/sample_kind; representation_kind() prefers the explicit field but infers from codec/sample_kind for legacy unspecified data; authoritative_pcm_depth() picks true_source_depth vs bit_depth based on representation kind; dsd_rate() resolves sample_rate_hz to a DsdRate only if is_dsd(); validate() rejects zero sample rate/channels, DSD facts with unresolvable rate, PCM sample_kind conflicting with DSD format/codec, DSD sources reporting any bit depth, and malformed (non-32-hex) audio_md5.
- predicted: most · documented: some · derivable: no · legible: not judged · trap: no
- note: Field-level doc comments explain the carrier-vs-source depth split and backward-compat inference well, but the module header alone gives no hint of that nuance or of the specific validate() invariants.

### `default` — OBSCURE
- spec 3 · read at `81c9db419c30` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:55:56Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns the default variant of SourceRepresentationKind, most likely SourceRepresentationKind::Pcm since PCM is the common/default audio representation and DSD is treated as a special case elsewhere (is_dsd, dsd_rate).
- found: Returns Self::Unspecified, not Pcm — the enum has a distinct "unknown/unspecified" variant separate from Pcm and Dsd.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `is_dsd`
- spec 3 · read at `6c09add7861d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:26Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Returns true only if an explicit representation_kind (or similar codec/format field) on SourceInfo is set to a DSD variant, deliberately avoiding any inference from sample rate alone since high-rate PCM could coincidentally match a DSD rate.
- found: Checks three explicit fields — format.is_dsd(), codec.is_dsd(), and sample_kind == Some(SampleKind::Dsd) — ORed together; I predicted a single explicit field check but it's actually three independent explicit signals.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `representation_kind`
- spec 3 · read at `d2d0ef62a19b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:11:37Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Checks self.is_dsd() first and returns Dsd if true; then checks a lossy-codec flag/field and returns Lossy if set; then checks whether sample-kind/bit-depth probing succeeded and returns Pcm; otherwise falls through to Unknown, explicitly not inferring PCM just from container/extension name per the doc's warning.
- found: First returns source_representation directly if it's already been explicitly set (not Unspecified); otherwise falls back to inference: Dsd if is_dsd(), Lossy if codec or format report lossy, Pcm if codec is one of a known PCM-capable set (Flac, PcmSigned/Unsigned/Float, WavPack, Alac) or sample_kind is a known PCM sample kind, else Unknown.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `authoritative_pcm_depth` — QUIRKY
- spec 3 · read at `8f4fe83af13b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:07:44Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Checks a dedicated field for decoded-carrier source depth (something like self.source_pcm_depth) and returns it if set; otherwise falls back to the realized/probed input depth field (e.g. self.pcm_depth or self.input_depth), likely via .or_else/.or.
- found: Matches on self.source_representation: Unspecified falls back to true_source_depth.or(bit_depth) for legacy callers; Pcm returns true_source_depth directly (no fallback); Dsd/Lossy/Unknown all return None since PCM depth isn't meaningful for those representations.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `dsd_rate`
- spec 3 · read at `726b42507e51` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:36:27Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Returns None if the source isn't DSD; otherwise derives a DsdRate by comparing the sample_rate against known DSD multiples (DSD64, DSD128, etc., multiples of 44100 or 48000 base rates), returning the matching DsdRate variant.
- found: Returns None if not DSD, else maps sample_rate_hz through DsdRate::from_hz (which itself returns Option, chained with and_then).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `validate` — QUIRKY
- spec 3 · read at `c0c0270d05fb` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:18:42Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Checks internal consistency of the SourceInfo's caller-supplied fields that the planner trusts rather than infers — e.g. that DSD sources have a valid dsd_rate and non-DSD sources have a valid PCM bit depth/sample rate, that channel counts are nonzero, and other invariants tied to representation_kind. Returns Err (likely a PlanningError variant) describing the first inconsistency found, or Ok(()) if everything checks out.
- found: Checks: sample_rate_hz nonzero if present, channels nonzero if present, DSD sources with an explicit sample rate must resolve to a known dsd_rate, PCM sample_kind conflicting with DSD format/codec, DSD sources must not report a PCM bit_depth/true_source_depth, and audio_md5 must be exactly 32 hex chars. Returns PlanningError::invalid_source on the first failure, Ok(()) otherwise.
- predicted: some · documented: some · derivable: no · legible: full · trap: no

## tonepoet-pipeline/src/tools.rs

### the file itself
- spec 3 · read at `130c519f3f69` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:17:06Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Core plugin abstraction for external conversion tools (ffmpeg, sox, metaflac, wvtag, AtomicParsley, etc.) used by the pipeline: a ToolPlugin trait each tool implements, an identifier/preference type (ToolIdentifier) for matching a user's preferred tool, a ToolSupport/scoring mechanism for ranking which registered tool best supports a given conversion step, a MetadataDisposition enum describing how a tool handles metadata writes, and a ToolRegistry that holds all registered plugins, deterministically selects the best-scoring one for a step, and builds the actual subprocess command to run it. "Deterministic" in the doc header suggests tie-breaking is stable/reproducible rather than depending on registration/iteration order.
- found: Confirms prediction closely: ToolIdentifier enum (Ffmpeg/Sox/Ssrc/Loudgain/Metaflac/Flac/Custom) with program-name and preference-matching; ToolSupport as a scored capability (UNSUPPORTED..CANONICAL constants); MetadataDisposition describing whether a plugin writes requested metadata; ToolPlugin trait (pure command builder, no I/O) with supports/metadata_effect/metadata_disposition/build_command; ToolRegistry holding sorted plugins, registering built-ins, and select_plugin doing deterministic tie-broken selection (score desc, then id asc), honoring user tool preference first when set.
- predicted: full · documented: none · derivable: yes · legible: not judged · trap: no

### `program`
- spec 3 · read at `a240e89caac0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:44:49Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Matches on the ToolIdentifier enum variant (e.g. Ffmpeg, Sox, Flac, WavPack, etc.) and returns the corresponding literal program name string ("ffmpeg", "sox", "flac", "wavpack"...) to be used as the executable name when spawning that tool's process.
- found: Match on ToolIdentifier variant returning literal program name (ffmpeg/sox/ssrc/loudgain/metaflac/flac), plus a Custom(name) variant returning name.as_str() for user-specified tools.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `matches_preference` — QUIRKY
- spec 3 · read at `c01955c42db3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:40:28Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Compares self against the PreferredTool enum — if the preference is something like Any/Default, returns true unconditionally; otherwise compares the identifier's name/program string against the preferred tool's name (probably case-insensitive or exact string match).
- found: Matches self's enum variant against PreferredTool's variant: PreferredTool::Auto always returns false (not true as I guessed), built-in variants (Ffmpeg/Sox/Ssrc) match by discriminant, and Custom matches by name equality; anything else is false.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Auto is a non-match, not a wildcard match — easy to get backwards, as I did.

### `fmt`
- spec 3 · read at `e3fde2978515` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:18Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Display impl for ToolIdentifier: writes the identifier's underlying name/program string to the formatter via write!(f, "{}", self.<field>), a simple one-line delegation.
- found: Writes self.program() (a &str) to the formatter via f.write_str, delegating to the program() accessor listed among peers rather than a raw field.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `new`
- spec 3 · read at `5effc1559708` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:00:05Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A const fn trivial constructor that wraps the given score: u8 into Self (a newtype/variant holding a support score), likely just storing it directly or clamping it into a valid documented range — a one-liner given it's const.
- found: Self { score } — plain struct literal construction, no clamping or validation despite the doc mentioning a "supported range."
- predicted: full · documented: some · derivable: no · legible: full · trap: no
- note: The doc says "in the supported range" but the function does not enforce or clamp any range — that's left to the caller/type's meaning elsewhere.

### `is_supported` — QUIRKY
- spec 3 · read at `c166da6ad659` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:09Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: ToolSupport is likely an enum (e.g. Supported, Unsupported, PreferredButUnsupported) representing how well a plugin supports an operation. is_supported matches self and returns true for variants that indicate the operation can be performed, false otherwise.
- found: ToolSupport is actually a struct with a numeric score field; is_supported just checks score > 0, not an enum match as I expected.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: ToolSupport is a struct with a score field (used elsewhere via ToolSupport::score), not the enum I assumed from the name.

### `score` — QUIRKY
- spec 3 · read at `b11abd935413` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:10:17Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: const fn match on the ToolSupport enum variants, returning a fixed u8 ranking for each (e.g. Unsupported=0, Supported=1, Preferred=2), used by ToolRegistry::select_plugin to pick the highest-scoring available tool.
- found: Trivial const fn returning the stored score field on ToolSupport, which is a struct (not the enum I assumed) — so this is a plain accessor, not a variant-to-value match.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `writes_requested_policy`
- spec 3 · read at `7fa75b90bfca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:15:18Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A const match over the MetadataDisposition enum variants, returning true for the variant(s) indicating the tool already wrote metadata matching the requested policy (making a later transfer step redundant), and false for variants where the tool didn't write metadata or wrote something not matching the request.
- found: matches!(self, Self::WritesRequestedPolicy) — true only for that single variant.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `metadata_effect`
- spec 3 · read at `5a43574639f4` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:20:53Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Default trait-method implementation on ToolPlugin with unused, underscore-prefixed parameters; it just returns a neutral default variant (e.g. MetadataPlanEffect::None), meant to be overridden by concrete plugins that actually produce metadata effects for a step.
- found: Default trait method with unused params that returns MetadataPlanEffect::none(), to be overridden by plugins that actually affect metadata.
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `metadata_disposition` — OBSCURE
- spec 3 · read at `76655c0b49a5` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:25:58Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A default trait method (deprecated compat shim) that derives the coarse MetadataDisposition from the newer, more precise self.metadata_effect(context, step) call, collapsing its finer-grained result into the legacy yes/no-style disposition so old callers keep working without each plugin having to implement both.
- found: Default trait implementation simply returns a hardcoded MetadataDisposition::DoesNotWrite, ignoring both parameters entirely — it does not derive anything from metadata_effect as I guessed.
- predicted: none · documented: most · derivable: no · legible: full · trap: no

### `default`
- spec 3 · read at `d79683e5f737` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:31:12Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: default() delegates to Self::with_builtin_tools() so that a default ToolRegistry comes pre-populated with the crate's built-in tool plugins rather than being empty.
- found: Delegates to Self::with_builtin_tools(), exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `empty`
- spec 3 · read at `65898523a718` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:48:24Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Constructs a ToolRegistry with an empty internal collection (e.g. an empty Vec/HashMap of plugins), doing no registration — a minimal constructor distinct from default/with_builtin_tools, likely just Self { plugins: Vec::new() } or similar, possibly a few lines to initialize multiple empty fields.
- found: Exactly as predicted: Self { plugins: Vec::new() }.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `with_builtin_tools`
- spec 3 · read at `02ccbcb41714` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:42:50Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Starts from ToolRegistry::empty() (or default), then calls register() for each built-in plugin instance — FFmpeg, SoX, SSRC, loudgain, metaflac — in a fixed order so the registry is deterministic, then returns the populated registry.
- found: Starts from an empty registry, then registers six built-in plugins (Ffmpeg, Sox, Ssrc, Loudgain, Metaflac, and an extra Flac plugin not mentioned in the doc) in a fixed order, each .expect()'d to be unique, then returns the registry.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The doc/module comment lists only 5 built-in tools but the code registers 6 (a FlacPlugin is also included) — doc is slightly stale.

### `register`
- spec 3 · read at `fd439350424b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:11:23Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Gets the plugin's id (via a trait method), checks whether that id already exists in the registry's internal collection, and returns an Err if it's a duplicate. Otherwise it appends the plugin to an internal Vec (preserving insertion order for deterministic selection) and returns Ok(()).
- found: Rejects duplicate plugin ids with a RegistryError, otherwise pushes the plugin and re-sorts the whole plugins Vec by id after every insert, so ordering is deterministic by id rather than by insertion order.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `tool_ids`
- spec 3 · read at `5d89707a4488` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:36:11Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the registry's collection of registered ToolPlugin entries and collects each one's identifier into a BTreeSet<ToolIdentifier>, giving a deterministic, deduplicated, sorted set of registered tool IDs.
- found: Maps self.plugins to their .id() and collects into a BTreeSet, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `selected_tool_id`
- spec 3 · read at `4a23a9db9586` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:41:56Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Calls self.select_plugin(context, step) to find the matching ToolPlugin per the registry's selection logic, then extracts and returns just its ToolIdentifier (e.g. plugin.id()) wrapped in Ok, propagating any error from selection via ?. Essentially a thin wrapper around select_plugin that avoids the cost of building the full command.
- found: Thin wrapper: calls self.select_plugin(context, step)?.id() and returns it.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `metadata_effect_for_step`
- spec 3 · read at `e769f6c8a6da` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:46:58Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Calls self.select_plugin(context, step) or similar to find the plugin registered/selected for this step, then calls plugin.metadata_effect(context, step) on it and returns/propagates the Result, erroring if no plugin is selected for the step.
- found: Exactly as predicted: select_plugin then plugin.metadata_effect, wrapped in Ok.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `metadata_disposition_for_step`
- spec 3 · read at `9fe1acc5f2e3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:52:17Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Delegates to the plugin selected for this step: calls self.select_plugin(context, step) to resolve which ToolPlugin applies, then calls that plugin's metadata_disposition(context, step) method and returns its Result, mirroring the sibling metadata_effect_for_step function.
- found: Selects the plugin for the step via select_plugin, then calls plugin.metadata_disposition(context, step) and wraps it in Ok.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `build_command`
- spec 3 · read at `105697ad5015` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:57:23Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Calls self.select_plugin (or similar) to deterministically pick the registered ToolPlugin that supports this PlanStep's tool/action given the context, then delegates to that plugin to build the actual PlannedCommand (program + args). Returns an Err if no registered plugin supports the step.
- found: Exactly as predicted: select_plugin(context, step)? then plugin.build_command(context, step).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `select_plugin`
- spec 3 · read at `3e89ce55984f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:51:41Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Iterates the registered ToolPlugins, filters to those whose ToolSupport::is_supported is true for this step/context, prefers one matching an explicit tool preference (via ToolIdentifier::matches_preference) if present, and otherwise picks the highest ToolSupport::score among supported plugins deterministically (e.g. breaking ties by registration order), returning an error if no plugin supports the step.
- found: Filters plugins to supported ones, errors with NoPluginForOperation if none; if a non-Auto preference is set, filters to preference-matching plugins and if any exist, sorts by score desc then plugin id (tie-break) and returns the top one; otherwise falls back to sorting all supported plugins the same way and returns the top. Matches my prediction closely, including the deterministic id-based tiebreak I guessed at.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/src/w64.rs

### the file itself
- spec 3 · read at `adf9a299c99c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:17:18Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A from-scratch, standalone Wave64 (Sony/Broadcast W64) container structural validator, intentionally independent of any audio-decoding library so it can double-check a decoder's own container-authority claims. It parses only the root GUID chunk header, fmt/fact/data sub-chunk headers, format metadata (sample rate, channels, bit depth, encoding — PCM int vs float, the latter requiring a fact chunk), and 8-byte alignment padding, all via manual little-endian offset reads (le_u16/le_u32/le_u64, read_exact_at) with checked arithmetic to avoid overflow, without ever buffering the audio payload bytes themselves. It strictly rejects malformed files: mismatched declared vs actual extents for root/data chunks, duplicate data chunks, nonzero alignment padding, undeclared trailing bytes, and frame-count/format mismatches, surfacing a typed W64ValidationError. An extensive #[cfg(test)] suite builds synthetic byte-level fixtures (push_chunk helpers) to exercise each accept/reject path individually.
- found: Matches prediction closely: manual GUID/chunk parsing of the Wave64 root/fmt/fact/data chunks with checked arithmetic, strict extent equality (declared==physical), 8-byte alignment with zero-padding enforcement, duplicate-chunk rejection, WAVEFORMATEXTENSIBLE support with subformat GUID resolution, exact PCM format-field cross-checks (block align, byte rate, valid bits), and byte-level fixture-based tests for each accept/reject path.
- predicted: full · documented: most · derivable: no · legible: not judged · trap: no
- note: Header doesn't mention WAVEFORMATEXTENSIBLE (0xfffe) support with channel-mask/subformat-GUID validation, which is a meaningfully complex branch not implied by 'format metadata.'

### `key`
- spec 3 · read at `c7d8ef83c6af` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:54:02Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A const fn that matches on the W64SampleEncoding enum variant (e.g. Integer, Float) and returns a corresponding stable lowercase string literal like "integer" or "float", used as a key in validation error messages or evidence reporting.
- found: Matches on the two W64SampleEncoding variants (SignedInteger, FloatingPoint) and returns their stable string keys "signed_integer" / "floating_point".
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `from`
- spec 3 · read at `b6f1de666303` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:48:54Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Converts a W64PcmExpectation (a higher-level, presumably user-facing description of expected PCM properties like sample rate, channels, bit depth, encoding) into the more format-chunk-specific W64PcmFormatExpectation struct, mapping/copying the relevant fields one to one.
- found: Exactly as predicted: straightforward field-by-field copy of sample_rate_hz, channels, bits_per_sample, and encoding from the input struct into Self.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `invalid`
- spec 3 · read at `10d62e7f94cf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:35Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A simple constructor on W64ValidationError: wraps the given message into an "Invalid"-style variant, e.g. Self::Invalid(message.into()), used throughout the Wave64 parser to signal structural validation failures with a descriptive message.
- found: Exactly as predicted: Self::Invalid(message.into()).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `fmt` — QUIRKY
- spec 3 · read at `b7a540e36fdd` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:58:58Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Display impl for W64ValidationError that matches on the error variant and writes a human-readable description of the specific Wave64 structural violation (e.g. bad GUID, chunk size overflow, malformed fmt/fact chunk) to the formatter.
- found: Display impl with just two variants: Io wraps an underlying I/O error and Invalid wraps a pre-formatted message string; each is printed with a fixed prefix ("Wave64 structural I/O failed: " / "Wave64 structural validation failed: ").
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `source`
- spec 3 · read at `08ada70b350d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:59:40Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: This is the std::error::Error trait's source() implementation for W64ValidationError. It matches on the error variant and returns Some(&io_error) for a variant that wraps an underlying std::io::Error (e.g. a read failure), and None for all the purely-structural validation variants (bad chunk size, bad magic, etc).
- found: Exactly as predicted: matches Io(error) => Some(error), Invalid(_) => None.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `from` #2
- spec 3 · read at `14434389f702` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:00:04Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Implements From<std::io::Error> for W64ValidationError, wrapping the IO error into a variant of the error enum (e.g. W64ValidationError::Io(error)), enabling ? to convert file read errors into the validator's own error type.
- found: Wraps the io::Error into Self::Io(error), as predicted exactly.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `read_exact_at`
- spec 3 · read at `44215a1dd6dd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:24Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Seeks the reader to `offset` via SeekFrom::Start, then reads exactly buffer.len() bytes with read_exact, mapping any seek or read I/O error into a W64ValidationError variant (likely wrapping the underlying io::Error, possibly distinguishing truncated-file/unexpected-EOF specifically).
- found: Seeks to offset, reads exactly buffer.len() bytes, relies on `?` and a presumed From<io::Error> impl to convert errors into W64ValidationError.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `le_u16`
- spec 3 · read at `2011b6f4a507` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:45Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Reads the first two bytes of the given slice and combines them as a little-endian u16, likely via bytes.try_into().unwrap() and u16::from_le_bytes, mirroring the sibling le_u32/le_u64 helpers.
- found: u16::from_le_bytes([bytes[0], bytes[1]]) — exactly as predicted, just indexed literally rather than via try_into.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `le_u32`
- spec 3 · read at `3d9012d8cfe0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:10:45Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads the first 4 bytes of the slice and interprets them as a little-endian u32 via u32::from_le_bytes, likely panicking (via unwrap/expect or slice indexing) if the slice is shorter than 4 bytes since callers are expected to pass pre-sliced buffers of the right length.
- found: Exactly as predicted: u32::from_le_bytes on the first 4 indexed bytes, panics on short slices.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `le_u64`
- spec 3 · read at `d534435d98e1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:55:05Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Takes a byte slice, reads the first 8 bytes, converts them into a fixed-size array, and calls u64::from_le_bytes to interpret them as a little-endian unsigned 64-bit integer, returning that value (probably panicking or via try_into().unwrap() if fewer than 8 bytes are present).
- found: Exactly as predicted: builds a [u8;8] array from indices 0-7 and calls u64::from_le_bytes on it.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `checked_add`
- spec 3 · read at `3fd67133413a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:49:36Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Adds left and right as u64, and if it overflows, returns a W64ValidationError::invalid (or similar) constructed with the description string explaining what overflowed; otherwise returns Ok(sum). Used to safely compute chunk offsets/sizes during Wave64 header parsing.
- found: Uses checked_add and maps None to a W64ValidationError::invalid with a formatted "{description} overflowed u64" message.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `align_up_8`
- spec 3 · read at `c6cfabb9f38a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:16:22Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Rounds `value` up to the next multiple of 8 (Wave64 chunks are 8-byte aligned), using checked arithmetic to avoid overflow and returning a W64ValidationError if adding the padding would overflow u64.
- found: Adds 7 via checked_add (erroring with an alignment-labeled overflow error), then masks off the low 3 bits with & !7 to round up to a multiple of 8 — the standard align-up bit trick.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `parse_format`
- spec 3 · read at `2b45c6d05196` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:00:39Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Reads the fmt chunk fields (format tag, channel count, sample rate, byte rate, block align, bits per sample, and possibly cbSize/extension fields for WAVE_FORMAT_EXTENSIBLE) at payload_offset using read_exact_at and the le_u16/le_u32 helpers, validates the payload_bytes length is sufficient for the fields being read (erroring via W64ValidationError if too short), and returns a ParsedFormat struct populated with these values.
- found: Reads up to 40 bytes of fmt chunk data, parses base PCM/float/extensible fields, and for each format tag (0x0001 PCM, 0x0003 float, 0xfffe extensible) enforces exact payload-size requirements and, for extensible, validates cbSize, valid-bits-vs-stored-bits, channel mask popcount against channel count, and subformat GUID against known PCM/float GUIDs, rejecting anything else.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `validate_exact_w64_pcm_inner` — TANGLED
- spec 3 · read at `2d53ebaa8c69` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:48:20Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Reads the Wave64 root GUID/size header and checks the declared root extent matches the actual file length (seeking to end to compare), then walks every chunk header sequentially (fmt, fact, data, and any others) via read_exact_at, validating each chunk's 8-byte alignment padding and rejecting a second occurrence of fmt/data/fact. It parses the fmt chunk with parse_format and checks it against `expected`, computes an exact frame count from the data chunk's declared size divided by block alignment, cross-checks that against `expected_sample_frames` if provided, and never reads/buffers the actual audio payload bytes — returning a W64ExactStructure summarizing offsets/sizes or a W64ValidationError on any mismatch.
- found: Validates `expected` itself first (non-zero rate/channels, supported bit-widths per encoding), then reads root header and requires declared==physical file size, derives expected block-align/byte-rate from `expected`, walks chunks checking 8-byte alignment/zero padding and rejecting duplicate fmt/fact/data chunks, requires traversal to land exactly on the declared end, then cross-checks every format field (encoding, channels, rate, bits, valid-bits, channel mask, block-align, byte-rate) against `expected`, validates data-chunk byte count against block-align and optional expected_sample_frames, and cross-validates a fact chunk's frame count against the derived sample_frames for both float (required) and integer (optional) encodings before returning the full W64ExactStructure.
- predicted: most · documented: most · derivable: no · legible: some · trap: no

### `inspect_exact_w64_pcm` — QUIRKY
- spec 3 · read at `628b01144905` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:21:29Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Calls the inner validation routine (validate_exact_w64_pcm_inner) against the reader with the expected format, then computes the frame count from the validated data chunk's byte extent divided by the format's block alignment, returning a W64ExactStructure bundling that frame count with other validated metadata.
- found: A one-line wrapper that just calls validate_exact_w64_pcm_inner(reader, expected, None) — all the actual frame-count/extent logic lives in the inner function, not here.
- predicted: some · documented: some · derivable: no · legible: full · trap: no

### `validate_exact_w64_pcm`
- spec 3 · read at `2e976ecc9be4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:43:38Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Thin public wrapper that delegates to validate_exact_w64_pcm_inner, passing the reader and expected frame count/format, and converts/propagates any error into W64ValidationError via the From impl.
- found: Thin wrapper delegating to validate_exact_w64_pcm_inner, converting expected into the inner format type via .into() and passing expected.sample_frames wrapped in Some as the required exact frame count.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `push_chunk`
- spec 3 · read at `9a39e4829152` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:10:57Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture helper that appends one Wave64 chunk to the byte buffer being built — writing the 16-byte GUID, an 8-byte little-endian chunk size (covering GUID + size field + payload), then the payload bytes, and if pad is true, appending a padding byte so the chunk ends on an 8-byte boundary (mirroring Wave64's alignment rule).
- found: Appends GUID, an 8-byte LE chunk size (CHUNK_HEADER_BYTES + payload length), and the payload to the buffer, then pads with zero bytes in a loop until the buffer length is a multiple of 8 if pad is true.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `direct_format`
- spec 3 · read at `5b0845f352c2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:12:35Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture helper that serializes a WAVEFORMATEX-style fmt chunk payload (format tag, channel count, sample rate, byte rate, block align, bits per sample) as little-endian bytes derived from fields on expectation, for building synthetic Wave64 fixtures alongside push_chunk.
- found: Computes block align/byte rate from expectation fields, picks format tag 1 (int) or 3 (float), and packs all 16 bytes of a standard fmt chunk body in little-endian, matching prediction closely.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `fixture`
- spec 3 · read at `0fb05389e31d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:07:31Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Test-fixture builder that assembles a minimal, well-formed Wave64 byte buffer (root header, fmt chunk derived from W64PcmExpectation's fields, an optional fact chunk when fact=true, and a data chunk sized/aligned to match), using the push_chunk helper. Other tests in the file then take this valid buffer and mutate/corrupt specific bytes to exercise the various rejects_* validation error paths.
- found: Builds a minimal valid Wave64 file: RIFF/WAVE root GUIDs with a placeholder length, fmt chunk from direct_format(expectation), optional fact chunk with sample_frames, a zero-filled data chunk sized from sample_frames*block_align, then backpatches the root extent field (bytes 16..24) with the final total file length.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `accepts_exact_integer_with_unaligned_final_data`
- spec 3 · read at `ff94f09eb5cc` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:25:15Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs a fixture Wave64 file with an integer PCM format whose data chunk length is exact for the declared frame count but not a multiple of 8 bytes (needing alignment padding at end-of-file), and asserts validate_exact_w64_pcm returns Ok, confirming the validator tolerates a final data chunk that isn't 8-byte aligned as long as the declared size is correct.
- found: Builds a 24-bit mono 3-frame fixture whose total byte length isn't a multiple of 8, asserts validate_exact_w64_pcm succeeds with correct sample_frames and declared_data_bytes (9, i.e. 3 frames * 3 bytes), confirming unaligned-but-exact data is accepted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `accepts_exact_float_with_fact`
- spec 3 · read at `b7818b78a854` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:10:34Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A test that constructs a fixture Wave64 file with a float-format fmt chunk and a fact chunk (required for float PCM), then calls the exact validator and asserts it returns Ok/accepted, confirming the validator correctly handles the float+fact case.
- found: Builds a W64PcmExpectation for 88.2kHz float64 stereo, 4 frames; constructs a fixture with fact chunk included; validates via validate_exact_w64_pcm and asserts success plus that fact_chunk_offset is Some.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `rejects_exact_frame_count_mismatch` — QUIRKY
- spec 3 · read at `7cc6d0fa06fd` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:34:34Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds a Wave64 fixture (likely float format with a fact chunk) where the declared frame count in the fact chunk doesn't match what the data chunk's byte size implies given the format's block alignment, then asserts that validate_exact_w64_pcm (or its inner variant) returns an error rather than silently accepting the inconsistent metadata.
- found: Builds a fixture from an 'actual' W64PcmExpectation (4 sample frames, int PCM), then validates it against a different 'expected' W64PcmExpectation claiming 5 frames — validate_exact_w64_pcm compares the caller-supplied expectation against the file's actual computed frame count and errors with a message containing the expected byte count ('expected 30'), not an internal fact-vs-data chunk cross-check.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The mismatch check is between an externally supplied expectation struct and the file, not between two chunks inside the file itself as the name alone might suggest.

### `rejects_float_without_fact_chunk`
- spec 3 · read at `75781395fb0c` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:33:09Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds a Wave64 fixture with a float-format fmt chunk (via direct_format/fixture/push_chunk) but omits the fact chunk that WAVE_FORMAT_IEEE_FLOAT requires, then calls validate_exact_w64_pcm (or inspect_exact_w64_pcm) and asserts it returns an error, confirming the validator enforces that float PCM must be accompanied by a fact chunk.
- found: Builds a W64PcmExpectation for 48kHz mono 32-bit float, generates a fixture with fact-chunk generation disabled (fixture(expected, false)), runs validate_exact_w64_pcm, and asserts the returned error's message contains 'missing its fact chunk'.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The fixture helper's boolean second argument (fact-chunk presence) isn't visible from this test alone — its meaning had to be inferred from the test name and peer list.

### `rejects_zero_channel_expectation_without_panicking` — QUIRKY
- spec 3 · read at `01b690ae067a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:38:34Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a minimal valid Wave64 fixture (via push_chunk/direct_format helpers) and calls the validator (validate_exact_w64_pcm or its inner variant) with an expected channel count of 0, asserting that it returns an Err rather than panicking — likely guarding against a divide-by-zero or overflow when zero channels is used to compute block alignment or frame counts.
- found: Test asserts that validate_exact_w64_pcm rejects channels=0 with an error containing "channel count must be non-zero", using an empty Cursor (no fixture bytes needed since validation is checked before any input is read).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `rejects_false_root_extent`
- spec 3 · read at `96115b23cbc0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:53:58Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A unit test that builds a Wave64 fixture whose root RIFF chunk declares a total file size (extent) that doesn't match the actual byte length of the buffer (e.g. claims a larger size than what's really there), then calls the exact-PCM validator and asserts it returns an error rather than panicking or succeeding.
- found: Builds a valid w64 fixture then overwrites the root chunk's declared size field (bytes 16..24) with a false value not matching the actual buffer length, then asserts validate_exact_w64_pcm errors with a message mentioning "physical file".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `rejects_false_data_extent`
- spec 3 · read at `724e0c07e39f` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:28:07Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds a Wave64 fixture where the data chunk header declares a size larger than the number of bytes actually present in the buffer (a false/lying extent), then calls the validator (validate_exact_w64_pcm or its inner variant) and asserts it returns an error rather than succeeding or panicking, mirroring rejects_false_root_extent but for the data chunk specifically.
- found: Builds a valid float32 fixture, locates the data chunk's GUID, then overwrites its declared 64-bit size field with 24 (too small/inconsistent given the real payload), and asserts validate_exact_w64_pcm returns an error mentioning data chunk / chunk traversal / undeclared-truncated.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `rejects_nonzero_alignment_padding`
- spec 3 · read at `331d3ef7cbc8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:38:45Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds a Wave64 fixture with a data chunk whose length requires 8-byte alignment padding, sets that padding byte to a non-zero value, and asserts the validator returns an error rejecting the file because Wave64 alignment padding must be zero.
- found: Confirms the core prediction (nonzero padding byte -> rejected with an alignment-padding error), but the actual mechanism is more specific: it inserts an extra unknown chunk with odd length before the real data (via push_chunk with an unrecognized GUID) and corrupts that chunk's trailing pad byte, then patches the file's declared total length, rather than padding after the data chunk itself as I guessed.
- predicted: most · documented: some · derivable: no · legible: most · trap: no

### `rejects_duplicate_data_chunk`
- spec 3 · read at `db8e559d6bc2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:43:43Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a fixture Wave64 file with two "data" chunks appended via push_chunk, then calls validate_exact_w64_pcm (or validate_exact_w64_pcm_inner) on it and asserts that it returns an Err, since a spec-compliant Wave64 PCM file must have exactly one data chunk.
- found: Builds a valid fixture (already containing one data chunk), pads to 8-byte alignment, appends a second empty "data" chunk via push_chunk, patches the root extent field to match the new length, then asserts validate_exact_w64_pcm returns an error whose message contains "duplicate data chunk".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `rejects_undeclared_trailing_bytes`
- spec 3 · read at `45b6103aa752` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:48:56Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Test constructs a valid Wave64 fixture, then appends extra trailing bytes to the file buffer beyond what the root chunk's declared size accounts for, and asserts that validate_exact_w64_pcm (or inspect_exact_w64_pcm) returns an error, since the "exact" validator should reject any undeclared/unaccounted-for trailing data rather than silently ignoring it.
- found: Builds a valid fixture via the fixture() helper, pushes one extra byte onto the buffer (not accounted for by the declared root extent), and asserts validate_exact_w64_pcm errors with a message containing "physical file" — confirming the validator compares the declared size against actual physical file length.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/tests/planning.rs

### the file itself
- spec 3 · read at `472bb7a451b8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:23:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: This is the test suite for the conversion planner (plan.rs), exercising plan_conversion/plan_conversion_with_registry across many scenarios: passthrough eligibility, metadata policy effects, DSD-to-PCM routing (sox/ffmpeg selection, brickwall vs ssrc-force), bit-depth/representation resolution rules, custom encode plugin registration, and various invalid/rejected target combinations. No file-level doc comment exists, so the file's purpose is only inferable from its long list of test function names.
- found: Integration test suite for the conversion planner: passthrough vs execute decisions, metadata policy effects, DSD<->PCM routing (sox/ffmpeg/ssrc/brickwall), bit-depth/source-representation resolution rules, custom ToolPlugin registry routing, and various validation/rejection error paths. Uses real plan_conversion/plan_topology/plan_conversion_with_registry entry points against PlanRequest fixtures.
- predicted: full · documented: none · derivable: yes · legible: not judged · trap: no

### `flac_source` — QUIRKY
- spec 3 · read at `112b85034b8e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:47:47Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Test fixture helper that constructs and returns a SourceInfo describing a plain PCM/FLAC source (codec=FLAC, some default sample rate and bit depth like 44100/16), used as shared input across many of the planning tests in this file for building a "source" argument to pass into the planner.
- found: Builds a SourceInfo fixture for a hi-res (96kHz/24-bit int) stereo FLAC source with all the extended provenance fields (true_source_depth, source_representation, sample_kind, etc.) set explicitly, not just a bare codec+rate+depth guess.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `legacy_dsd_settings`
- spec 3 · read at `d79ba0091307` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:29:49Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Test helper that constructs and returns a DsdSettings struct literal, setting the given lowpass, gain_mode, margin_db, and gain_db fields, with the remaining fields filled in with fixed values representing the historical/legacy default configuration (used by tests asserting frozen legacy DSD-to-PCM planning behavior).
- found: Builds DsdSettings by serializing a JSON object through serde: pcm-to-dsd fields pulled from DsdSettings::native_v2() defaults, dsd-to-pcm fields set from the function's params, then deserializes into DsdSettings, expecting success.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Constructed via a serde_json round-trip (json! macro + from_value) rather than a direct struct literal, presumably to exercise/pin the wire format rather than the Rust struct shape directly.

### `request`
- spec 3 · read at `85ebbe6a246f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:57:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Test helper that builds a PlanRequest for use in planning tests, taking the given PipelineSettings and filling in the rest of the struct (source track info, target format, etc.) with sensible test defaults — likely reusing a helper like flac_source() for the source. Keeps individual tests short by letting them only specify the settings under test.
- found: Builds a PlanRequest with fixed test paths (in.flac/out.flac/work dir), flac_source() as the source, empty container ffmpeg flags, default/None for the resolved output target, reference scope, and RIFF bound fields, and the passed-in settings.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `pre_promotion_dsd_default_origin_does_not_change_pcm_planning` — QUIRKY
- spec 3 · read at `2f931bac1ad7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:40:09Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds a planning request around a FLAC/PCM source (via flac_source) with legacy_dsd_settings configured to use their "default origin" flag (a marker predating some promotion of DSD defaults to explicit values), then plans it and asserts the resulting PCM plan is identical to what it would be without that DSD-specific setting — i.e. a DSD-only default-origin flag must not leak into or alter PCM planning since the source isn't DSD.
- found: Plans a conversion request twice — once with default PipelineSettings (legacy DSD default-origin settings) and once with DsdSettings::native_v2() explicitly set — and asserts the two resulting plans are equal, confirming the newer explicit "native_v2" DSD settings variant produces the same plan as the legacy default for the (implicitly PCM) request.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `passthrough_is_explicit_when_metadata_policy_is_copy_safe`
- spec 3 · read at `74ce0f7fb942` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:12:12Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs a pipeline request (likely FLAC-to-FLAC or similar identical-format case) with a metadata policy considered "copy-safe" (no re-encode needed to satisfy metadata handling), runs the planner, and asserts the resulting plan explicitly indicates passthrough (e.g. an explicit Passthrough variant/flag) rather than merely happening to skip re-encoding as a side effect.
- found: Builds a default-settings request, plans it, then asserts plan.action matches the explicit PlanAction::PassthroughCopy variant with the expected input/output paths, panicking if it's an Execute action instead.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `metadata_strip_blocks_passthrough_and_rewrites_without_reencoding`
- spec 3 · read at `35e140b76eb4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:11:30Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A planning test verifying that when the metadata policy is set to "strip" (removing metadata), the planner correctly refuses to use a pure bit-identical passthrough (since output would differ from source), but also avoids a full audio re-encode — instead choosing a metadata-only rewrite plan step. It likely builds a FLAC source + request with strip policy, runs the planner, and asserts the resulting plan has no re-encode step but does have metadata stripping/rewriting.
- found: With tag-transfer and artwork-preservation disabled, the planner emits a single ffmpeg command using stream copy (-c:a copy) plus -map_metadata -1 to strip metadata, confirming no re-encode occurs and metadata stripping is done via ffmpeg args rather than a separate rewrite step.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sox_flac_resample_dither_plan_is_deterministic_and_preserves_metadata_by_post_step`
- spec 3 · read at `c4ce0293be11` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:26:25Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds a conversion request from a flac_source() with a target sample rate/bit depth that forces resampling and dithering (so sox rather than a passthrough must handle it), calls the planning function twice on identical input, and asserts the two resulting plans are equal to prove determinism. It also asserts the plan applies metadata via a separate post-processing step after the sox encode rather than embedding metadata handling inside the encode command itself.
- found: Builds settings requiring 44100Hz/16-bit output with Shibata dither, plans twice and asserts equality for determinism, then checks the plan's two commands are sox (with rate/dither/-s args) followed by ffmpeg (with -map_metadata) as the post-step that carries metadata through.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `brickwall_uses_ffmpeg_ssrc_final_encode_plus_original_source_metadata_transfer`
- spec 3 · read at `1b746734df6d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:21:23Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a conversion request with a FLAC source and a target requiring brickwall filtering/resampling, runs the planner, and asserts the resulting plan uses ffmpeg (not sox) for the final SSRC-style encode step while a separate step transfers/copies the original source's metadata rather than relying on passthrough.
- found: Builds a brickwall-transition, int16/44.1kHz downsample request and asserts the plan is exactly Ffmpeg, Ssrc, Ffmpeg, Ffmpeg (pre-process, SSRC resample, final encode, metadata transfer), then checks the final transfer command reads both the encoded file and original source (two -i flags) and uses -map_metadata, guarding against a regression where the SSRC path silently dropped tags.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_force_routes_rate_change_through_ssrc_without_brickwall_transition`
- spec 3 · read at `08e340667339` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:20:30Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a source/request with a PCM sample-rate change and a setting forcing SSRC as the resampler, builds the conversion plan, and asserts the resulting plan step uses SSRC for the rate conversion rather than the brickwall/ffmpeg transition path — i.e. forcing SSRC bypasses whatever default brickwall-filter logic would otherwise be chosen.
- found: Sets target sample rate to 44.1kHz and settings.ssrc.force = true, builds a request/plan, and asserts one of the plan's commands uses ToolIdentifier::Ssrc — a fairly minimal existence check rather than asserting absence of a brickwall/ffmpeg command.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Test only asserts SSRC is present among commands, not that a brickwall/ffmpeg transition is absent, despite the name implying exclusivity.

### `preferred_ffmpeg_is_honored_when_supported`
- spec 3 · read at `51c9e7657a45` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T08:25:34Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds a conversion request with a "prefer ffmpeg" engine setting for a case ffmpeg can actually handle, runs it through the pipeline planner, and asserts the resulting plan selects an ffmpeg-based step rather than falling back to the default engine (e.g. sox), confirming the preference is respected when supported.
- found: Matches my prediction closely: sets force_encode=true and preferred_tool=Ffmpeg on PipelineSettings, plans the conversion, and asserts the first command's tool is Ffmpeg. Only difference from my guess is the explicit force_encode=true flag, which I didn't anticipate was needed to make the scenario "supported"/applicable.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `invalid_pcm_target_rejects_dsd_rate`
- spec 3 · read at `ffb791a4af88` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:26:21Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Constructs a planning request targeting PCM but with a DSD-typical sample rate value, then asserts the planner returns an error rejecting that rate as invalid for a PCM target.
- found: Sets settings.target_sample_rate to RateTarget::Dsd(DsdRate::Dsd64) (implying a default/PCM target format elsewhere) and asserts plan_conversion returns Err(PlanningError::InvalidSettings { field: \"target_sample_rate\", .. }).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `lossy_targets_resolve_source_depth_to_the_format_default` — QUIRKY
- spec 3 · read at `717121650f5a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:12:26Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds a conversion request targeting a lossy output format (e.g. MP3/AAC/Opus) from a source with some actual/decoded bit depth, plans it, and asserts that the plan's recorded "source depth" is the lossy format's documented default depth rather than the source's real decoded depth — since lossy encoders don't preserve exact bit depth. It's an assert_eq against a fixed expected default value pulled from planning output.
- found: For both PCM and Unknown source representations with no bit_depth/true_source_depth set, targeting MP3 with BitDepthTarget::Source must still plan successfully (not error out) and produce a non-empty command list — the test only asserts planning succeeds, not what depth value it resolves to.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: I predicted an assert_eq on a specific resolved depth value; the test actually only asserts plan success/non-empty commands, contrasting with the fail-closed behavior reserved for PCM-lossless targets per the inline comment.

### `pcm_lossless_source_target_requires_authoritative_source_depth`
- spec 3 · read at `44503d2c7fff` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:24:59Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a planning request targeting lossless PCM where the source's bit depth is not authoritatively known (e.g. only a decoded/carrier depth is available, not an explicit source-depth fact). It asserts that planning either fails/errors or refuses to silently assume a depth, requiring an authoritative source depth fact to proceed.
- found: Sets target_bit_depth to Source with force_encode, clears both bit_depth and true_source_depth on the request's source, and asserts plan_conversion returns Err(PlanningError::InvalidSource { field: \"bit_depth\", .. }).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `explicit_pcm_representation_does_not_promote_carrier_depth_to_source_truth`
- spec 3 · read at `82ab48c833fa` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:36:26Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds a SourceInfo with an explicit PCM source_representation and a decoded/carrier bit_depth set, but without an authoritative true_source_depth declared, then plans a conversion and asserts the carrier's bit depth is not silently promoted to be treated as the authoritative source depth — e.g. planning either fails or falls back to a documented default rather than trusting the carrier value.
- found: With target_bit_depth=Source and force_encode=true, sets source.bit_depth=Int32 (the decoded PCM carrier) but true_source_depth=None and source_representation=Pcm explicitly; asserts plan_conversion errors with PlanningError::InvalidSource{field:\"bit_depth\"} rather than trusting the carrier's Int32 as the authoritative source depth.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `explicit_unknown_representation_ignores_decoded_pcm_carrier` — QUIRKY
- spec 3 · read at `7110d38465fb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:24:21Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds a source with representation explicitly set to Unknown and a decoded PCM carrier with some depth/rate, then asserts the planner does not use the carrier's decoded properties to infer source depth/format - the explicit "unknown" representation stays authoritative rather than being promoted/overridden by the carrier.
- found: Builds a request targeting Source bit depth with force_encode, where the source's decoded carrier is Int32 PCM but source_representation is explicitly Unknown and true_source_depth is None; asserts plan_conversion fails outright with PlanningError::InvalidSource for field bit_depth, since with an unknown representation the decoded carrier depth cannot be trusted as the authoritative source depth needed to plan a Source-depth target.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The name suggests the carrier is merely 'ignored' in favor of some fallback, but the actual behavior is a hard planning error, not a silent fallback.

### `legacy_unspecified_representation_keeps_single_depth_fact_authoritative`
- spec 3 · read at `60e736c76374` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:01:45Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a source with no explicit representation set (the legacy/unspecified case) and a single known source bit-depth fact, runs it through planning, and asserts the plan treats that one depth fact as authoritative for the source depth rather than being overridden or second-guessed by a decoded PCM carrier's depth — preserving old behavior for inputs that predate explicit representation tagging.
- found: Builds a forced WAV->WAV (Source bit depth target) request where source_representation is Unspecified and true_source_depth is None, only source.bit_depth=Int24 is known; plans topology and asserts the resulting EncodePcm step targets Int24, i.e. the lone bit_depth fact is trusted as source depth when representation is unspecified/legacy.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `high_rate_pcm_is_not_misclassified_as_dsd` — QUIRKY
- spec 3 · read at `8cb3184401b5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:42:25Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A regression test that builds a plan request with a very high but valid PCM sample rate (e.g. 352.8kHz/384kHz, close to but not a DSD rate) and asserts the planner classifies/plans it as a PCM conversion rather than mistakenly treating it as DSD, likely checking the resulting plan steps or resolved format.
- found: Constructs a SourceInfo whose sample_rate_hz numerically equals a DSD64 rate but whose codec is PcmSigned (a WAV), and asserts source.is_dsd() returns false directly — testing the classification predicate itself rather than the full planner output.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `flac_verify_uses_real_decode_test_not_metaflac_streaminfo_listing`
- spec 3 · read at `0f9bfec9633d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:32:14Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a FLAC-to-FLAC plan with verification enabled, then asserts the resulting plan step performs a real decode test (e.g. `flac -t` or equivalent) rather than just listing STREAMINFO metadata via metaflac, guarding against a shallow verification regression.
- found: Builds a request with force_encode and flac.verify true, plans the conversion, and asserts the last command is the flac tool invoked with the -t decode-test flag.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `default_dsd_to_pcm_is_exact_frozen_legacy_plan` — QUIRKY
- spec 3 · read at `cf78101ea256` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:29:22Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A regression/snapshot-style test that builds a plan for converting a DSD source to PCM with default settings, then asserts the exact resulting plan (filter chain, resampler choice, bit depth, dithering, etc.) matches a hardcoded "frozen legacy" expected plan, to catch accidental behavior changes to the default DSD→PCM pipeline.
- found: Builds a DSD-to-PCM plan request with default dsd settings and asserts specific properties (Sox tool, 88200 rate arg), then builds a second request with settings explicitly set to legacy_dsd_settings() and asserts the two resulting plans are exactly equal, proving default settings are equivalent to the explicit frozen legacy configuration.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `dsd_to_pcm_source_depth_uses_documented_target_default`
- spec 3 · read at `793980509ec3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:51:30Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a plan converting a DSD source to a PCM target format without an explicit bit depth specified, then asserts the resulting plan's source/target depth matches the documented default for that PCM format (e.g. 24-bit) rather than something derived from the DSD carrier width or an arbitrary fallback.
- found: Builds a PlanRequest for a DSD64 DSF source with BitDepthTarget::Source (no explicit depth), target rate 88.2kHz PCM/FLAC, runs plan_topology, and asserts the resulting Execute plan contains a DsdToPcm step whose target_bit_depth is Int24 — the documented default depth for DSD-sourced PCM conversion.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `lossy_source_depth_uses_documented_target_default` — QUIRKY
- spec 3 · read at `d95d6df52b88` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:11:53Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: This test builds a conversion plan for a lossy target format where the source's bit depth isn't authoritatively known, and asserts that the planner falls back to a specific "documented default" depth value (likely 16-bit) for that target format rather than erroring, using an arbitrary sentinel, or inferring depth from a decoded PCM carrier.
- found: Builds a PlanRequest converting a lossy MP3 source (with no known bit_depth/true_source_depth) to WAV with target_bit_depth: Source and force_encode true, then asserts the resulting plan executes an EncodePcm step with target_bit_depth Int24 — i.e. when the source format is lossy and offers no real depth fact, "Source" resolves to a documented 24-bit default rather than erroring or defaulting to 16.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: I had the source/target relationship backwards — it's a lossy MP3 SOURCE being decoded to PCM, not a lossy target — and guessed the default depth as 16 when it's actually 24.

### `lossy_source_default_ignores_decoded_integer_carrier_width`
- spec 3 · read at `d5d8797e9eeb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:06:45Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: This test builds a plan for a lossy source format (e.g. MP3) that, when decoded, exposes an integer PCM carrier width (like 16-bit from the decoder), and asserts the planner's resolved source depth uses the documented lossy-format default rather than being swayed by that decoded carrier width — confirming lossy sources don't get their depth "corrected" by probing the decoded PCM.
- found: Builds a PlanRequest with source bit_depth=Int32 (the realized decoder carrier) but source_representation=Lossy, target_bit_depth=Source, force_encode=true; calls plan_topology and asserts the resulting Execute plan's EncodePcm step targets Int24 (the documented lossy default), not the decoded Int32 carrier — matching my prediction exactly including the specific mechanism.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `changed_flac_compression_blocks_passthrough`
- spec 3 · read at `f7ae946e48d4` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:54:04Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a planning request converting FLAC to FLAC where the requested compression level differs from the source file's existing compression level, then asserts the resulting plan does NOT choose a stream-copy/passthrough path but instead re-encodes, since a mismatched compression setting can't be satisfied by copying.
- found: Sets flac.compression_level to a non-default value (5, vs default 8) in settings, plans a conversion, and asserts the plan action is Execute (i.e., not a passthrough/skip) since the request implies re-encoding is required.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Test relies on default compression_level being 8 — comment calls this out but it's an easy invariant to silently break by changing the default.

### `lossy_same_format_never_passes_through_without_proven_encoder_settings`
- spec 3 · read at `e73f0f3e29da` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:31:25Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Sets up a plan request where source and target are the same lossy format but the encoder settings used to produce the source are not verified/proven to match the target's requested settings; calls the planner and asserts the resulting plan requires re-encoding rather than a passthrough/stream-copy, since format equality alone cannot guarantee bit-identical encoder settings for lossy codecs.
- found: Builds a plan request for MP3-to-MP3 (source format/codec Mp3, target Mp3 CBR 192kbps) with no proof the source's encoder settings match, calls plan_conversion, and asserts the plan is an Execute action whose first command uses the Ffmpeg tool (i.e. re-encodes rather than stream-copies).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `replaygain_only_uses_stream_copy_then_post_processing_not_reencode`
- spec 3 · read at `4672f22f6efc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:06:13Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a planning scenario where the only requested change is adding/updating ReplayGain tags (source and target audio format otherwise identical), then asserts the resulting plan consists of a stream-copy (passthrough) step followed by a post-processing/tagging step for ReplayGain, and explicitly does not contain a re-encode step.
- found: Builds default PipelineSettings with only ReplayGain album mode set, plans the conversion, and asserts exactly 2 commands: an ffmpeg stream-copy command followed by a loudgain command for tagging.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `flac_md5_only_uses_stream_copy_then_metaflac_tagging`
- spec 3 · read at `a55076c442de` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:11:53Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a plan for a FLAC target where only MD5 checksum verification is requested (not full re-encode/verify), then asserts the generated plan consists of a stream-copy step followed by a metaflac invocation to compute/write the MD5 signature into the FLAC STREAMINFO, rather than any re-encoding step.
- found: Asserts that when store_source_audio_md5 is enabled and a source audio_md5 is present, the plan is exactly two commands: an ffmpeg stream-copy followed by a metaflac call that sets a SOURCE_AUDIO_MD5 tag with that value.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `flac_verify_is_rejected_for_non_flac_targets`
- spec 3 · read at `6d25183c00b6` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:09:40Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a plan/request with a non-FLAC target format (e.g. WAV or MP3) while requesting flac_verify, and asserts that planning returns an error (or rejects/ignores the flac_verify option) since native FLAC decode verification is meaningless for non-FLAC output.
- found: Sets target_format to Mp3 with flac.verify=true, then asserts plan_conversion returns Err(PlanningError::InvalidSettings { field: "flac.verify", .. }) — matches prediction exactly, including the specific error variant and field name.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `id`
- spec 3 · read at `2fd320c94238` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:56:03Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Returns a hardcoded/constant ToolIdentifier representing this test-only CustomEncodePlugin, used so the test's planning/topology code can identify it as a distinct encoder tool.
- found: Returns ToolIdentifier::Custom(\"customenc\".into()), a hardcoded identifier for this test plugin.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `supports`
- spec 3 · read at `3d7b80eb2ecb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:01:02Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture plugin's `supports` method that checks whether the given PlanStep is the kind of encode step this custom plugin claims to handle (likely matching on step type/target format), ignoring the PlanContext, and returning ToolSupport::Supported for a matching step or ToolSupport::Unsupported/NotApplicable otherwise.
- found: Matches on step.operation: returns CANONICAL support for EncodePcm or ReplayGain operations targeting a Custom AudioFormat, and UNSUPPORTED for everything else, ignoring context.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I predicted a generic Supported/Unsupported binary; missed that it also covers ReplayGain ops and uses the CANONICAL support-level variant rather than a plain boolean-like enum.

### `metadata_disposition` — QUIRKY
- spec 3 · read at `38b5ddcefb26` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:50:15Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test-fixture plugin's implementation of the metadata_disposition trait method (part of the CustomEncodePlugin test double), which ignores the actual step/context content and just returns a fixed, hardcoded MetadataDisposition variant so test assertions elsewhere can verify that custom-registered plugins are consulted for their declared metadata behavior rather than being overridden by the built-in topology logic.
- found: Matches on step.operation: if it's an EncodePcm targeting a custom AudioFormat, returns MetadataDisposition::WritesRequestedPolicy; otherwise returns MetadataDisposition::DoesNotWrite. It does inspect step content rather than returning a fixed value.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `metadata_effect`
- spec 3 · read at `6cb67075e628` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:16:51Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test implementation of CustomEncodePlugin's metadata_effect trait method, returning a MetadataPlanEffect value that explicitly marks the metadata-transfer source as the original request input (not just the step's literal input), so the pruner can correctly determine this step's provenance — likely a single-variant construction like MetadataPlanEffect::TransferFrom(original_input).
- found: Matches on step.operation: for EncodePcm targeting a Custom audio format, returns a MetadataPlanEffect with source_tags_transferred_from_original_source and artwork_transferred_from_original_source both true (rest defaulted via MetadataPlanEffect::none()); all other operations get MetadataPlanEffect::none().
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `build_command`
- spec 3 · read at `2b52a8755aa4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:55:45Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Since CustomEncodePlugin is a test double used to verify custom targets get routed through the plugin registry rather than rejected, build_command probably just returns a minimal/fake PlannedCommand (e.g. a trivial program+args) built from the step's paths, without doing real encoding logic — just enough to prove the plan pipeline picked this plugin.
- found: Matches step.operation: for EncodePcm builds a PlannedCommand with --input/--output args from the step paths; for ReplayGain builds one with a --replaygain arg; any other operation panics as unexpected.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `custom_target_is_routed_through_registry_not_rejected_by_topology`
- spec 3 · read at `ad0b7924f122` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T19:37:20Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Registers a custom target format (via CustomEncodePlugin or similar) in the plugin registry, then builds a conversion plan targeting that custom format. Asserts that planning succeeds (doesn't error out as an unsupported/unknown format) because the custom plugin registry is consulted before the built-in topology rejection logic runs, proving custom formats bypass the hardcoded topology validation.
- found: Builds a PipelineSettings with a Custom target format, registers a CustomEncodePlugin into an empty ToolRegistry, plans the conversion, and asserts the resulting plan's first command uses the custom tool identifier "customenc" — confirming the registry-based plugin path is used instead of the built-in topology rejecting an unrecognized format.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `custom_target_can_supply_custom_replaygain_plugin`
- spec 3 · read at `487051f13e51` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:16:18Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Registers a custom encode plugin for a custom target extension that also supplies replaygain handling, builds a conversion plan targeting that custom format, and asserts the plan uses the custom plugin's replaygain capability rather than requiring/falling back to the built-in replaygain post-processing step.
- found: Builds a request targeting a custom "cust" format with track replaygain enabled, registers CustomEncodePlugin, plans the conversion, and asserts the resulting plan's two commands both use the custom "customenc" tool (encode step and replaygain step both handled by the custom plugin).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsf_metadata_requires_a_registered_metadata_plugin`
- spec 3 · read at `4e47cba3f681` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:59:07Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test that attempts to plan a conversion involving a DSF source/target without a metadata plugin registered for that format, and asserts the planner returns an error (rather than silently skipping metadata handling) because DSF metadata transfer requires an explicitly registered plugin.
- found: Builds a request targeting DSF/DSD64 with no metadata plugin registered, then asserts plan_conversion returns Err(PlanningError::NoPluginForOperation).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `flac_verify_only_uses_stream_copy_then_native_verify`
- spec 3 · read at `a7af55c44aa7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:41:28Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A planning test asserting that when the target is FLAC and only "verify" (not full re-encode) is requested, the generated pipeline plan uses a stream-copy step followed by a native FLAC verify step (e.g. invoking flac --test), rather than a full re-encode — mirroring the sibling flac_md5_only_uses_stream_copy_then_metaflac_tagging test but checking for a verify step instead of metaflac tagging.
- found: Builds a request with flac.verify=true, plans it, and asserts the plan has exactly two commands: ffmpeg stream copy followed by the flac tool (for verification).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_sinc_transition_width_shapes_sox_command`
- spec 3 · read at `a7fc3557c481` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:11:17Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test constructs a plan/config for a DSD source using a sinc-based lowpass filter, sets a specific transition-width parameter, runs plan-building, and asserts that the generated sox command string includes a flag encoding that transition width value.
- found: Builds a PCM-to-DSD (DSF, DSD128) conversion request with a sinc lowpass filter preset and a 750Hz transition width, generates the sox command plan, and asserts the sox args contain "-t" followed by "750".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_lowpass_paths_all_use_sox_ultra_rate_flag`
- spec 3 · read at `d5880e3b008d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:06:18Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds several DSD-to-PCM downsampling encode plans (varying by lowpass/transition or target format) and asserts that each generated SoX command includes the "ultra" rate-quality flag, ensuring high-quality resampling is used consistently across all DSD lowpass paths.
- found: Builds two conversion plans for a DSD64 DSF source targeting 24-bit/88.2kHz FLAC, one with lowpass method Auto and one with SoxUltra, and asserts both produce sox commands containing the -u flag (701 taps ultra quality) and that resample_quality (Low) does NOT add a -q flag, confirming DSD rate conversion always uses -u regardless of the resample_quality setting.
- predicted: most · documented: none · derivable: no · legible: most · trap: no
- note: The inline comment explains the intent (resample_quality no longer affects DSD rate conversion) — without it the -u/-q assertions would look like arbitrary flag checks.

### `dsd_source_rejects_pcm_bit_depth_fact`
- spec 3 · read at `45ddba72fb7a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:46:32Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a pipeline planning request where the source format is DSD (DSF/DFF) and attempts to attach or supply a PCM bit-depth fact/field to it, then asserts that plan construction fails (returns an error) because bit depth is a PCM-only concept and doesn't apply to DSD sources.
- found: Constructs a SourceInfo with DSD format/codec but a Some(bit_depth) set, and asserts source.validate() returns PlanningError::InvalidSource with field "bit_depth".
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `execute_plan_lists_deterministic_cleanup_paths` — OBSCURE
- spec 3 · read at `2e69570b9055` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:32:03Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds a multi-step pipeline plan (e.g. involving an atomic work path from a peer test) and asserts that the resulting cleanup path list is in a fixed, deterministic order — likely comparing against a hardcoded expected Vec<PathBuf> or asserting equal results across two builds of the same plan, to guard against nondeterminism from HashSet/HashMap iteration order leaking into cleanup ordering.
- found: Builds a plan for a resample-to-44.1kHz conversion request, then just asserts cleanup_paths() is non-empty and none of those paths equal the final output path "out.flac" — i.e. it guards against the plan trying to delete the output it just produced, not against nondeterministic ordering despite the test's name.
- predicted: none · documented: none · derivable: no · legible: full · trap: no
- note: The test name says "deterministic" but the assertions don't check ordering or reproducibility at all — it only checks non-emptiness and that the output path isn't in the cleanup list, which is misleading if taken at face value.

### `identical_input_output_paths_are_rejected`
- spec 3 · read at `47623ef4cbfd` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:33:21Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs a conversion request where the output path equals the input path, calls the plan-building function, and asserts it returns an Err (rather than panicking or silently producing a plan that would overwrite/destroy the source file), likely checking the error message mentions the paths being identical.
- found: Sets output_path equal to input_path on a default request, calls plan_conversion, and asserts it returns Err(PlanningError::InvalidSettings { field: "output_path", .. }).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `invalid_custom_extension_is_rejected`
- spec 3 · read at `ee69636ade33` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T08:15:26Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a plan request targeting a custom encode format with a malformed extension string (e.g. containing a dot, slash, or being empty), calls the planner, and asserts it returns an error rather than a valid plan.
- found: Builds a request with a Custom target format whose extension is ".bad" (leading dot), calls plan_conversion, and asserts it returns InvalidSettings error on field "target_format.extension".
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `passthrough_plan_includes_atomic_work_path_and_cleanup`
- spec 3 · read at `503d9b96a4a2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:19:07Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a plan request for a passthrough (stream-copy, no transcode) conversion, runs the planner, and asserts the resulting plan still uses a distinct atomic work path (temp file separate from the final output path) plus a corresponding cleanup entry for it — confirming passthrough plans get the same atomic-write/cleanup guarantees as transcoding plans.
- found: Plans a default-settings conversion request, matches on PlanAction::PassthroughCopy (panicking if it planned an Execute instead), and asserts cleanup_paths is exactly [work_path] and finalization is an AtomicRename from work_path to the expected output path.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `non_finite_legacy_dsd_gain_is_rejected_at_wire_boundary`
- spec 3 · read at `fb644319ec94` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:02:56Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs a DSD conversion request/plan input with a legacy gain value set to NaN or infinity, calls the planner (likely via a public/wire-level entry point), and asserts it returns an error rejecting the non-finite value rather than silently propagating it.
- found: Deserializes a raw JSON DsdSettings literal where dsd_to_pcm_gain_db is 1e400 (overflows f64 to infinity) via serde_json::from_str, and asserts the deserialization itself fails — i.e. rejection happens at serde parsing, not inside planner logic.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sox_selected_encode_gets_metadata_transfer_step`
- spec 3 · read at `6192d9c67919` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:37:21Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A test that builds a planning request/config where sox is selected as the encoding tool (rather than ffmpeg), runs the planner, and asserts that the generated plan includes a metadata-transfer step after the encode step — since sox itself doesn't preserve/transfer tags, the planner must insert a separate step to copy metadata into the output file.
- found: Forces sox as the preferred encode tool, plans a conversion, and asserts the resulting command list is [Sox, Ffmpeg] with the second (ffmpeg) command including a -map_metadata arg to transfer tags.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `flac_int32_forced_sox_routes_encode_through_ffmpeg_experimental`
- spec 3 · read at `6c16c08b05ce` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T21:19:26Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test builds a plan request that forces sox as the tool but targets FLAC int32 output, then asserts the planner overrides that choice and routes the encode step through ffmpeg (since sox can't handle int32 FLAC), with some "experimental" flag/codec option set on that ffmpeg step.
- found: Test forces sox as preferred tool with FLAC int32 target, then asserts planner still routes the encode through ffmpeg with -strict experimental flag, and that sox never appears as the encoding tool anywhere in the plan.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `wav_artwork_preservation_needs_a_metadata_plugin`
- spec 3 · read at `69a162fd6e3d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:38:34Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test builds a plan targeting WAV output where the source has embedded artwork, calls the planner without a metadata plugin registered, and asserts the plan either fails/rejects or drops the artwork-preservation step, mirroring the sibling wav_replaygain_needs_a_replaygain_plugin test which requires a specific plugin for a WAV-specific feature.
- found: Builds default PipelineSettings targeting WAV output, creates a request, and asserts plan_conversion fails with PlanningError::NoPluginForOperation because no metadata plugin is registered to handle artwork preservation for WAV.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `wav_replaygain_needs_a_replaygain_plugin`
- spec 3 · read at `09169b18518b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:48:57Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test builds a conversion request targeting WAV output with ReplayGain tagging requested, but without registering/providing a ReplayGain plugin in the planner context, then asserts that planning fails with an error indicating a ReplayGain plugin is required (mirroring the sibling wav_artwork_preservation_needs_a_metadata_plugin pattern for metadata plugins).
- found: Builds a PipelineSettings targeting WAV with artwork preservation off and ReplayGain track mode set, then asserts plan_conversion returns Err(PlanningError::NoPluginForOperation) since no ReplayGain plugin is registered.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `generated_final_work_path_cannot_equal_input_path`
- spec 3 · read at `a34437640dd8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:52:51Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Test constructs a scenario where the planner's generated final/work path (used for atomic write before rename) would coincide with the input path, then asserts that plan generation returns an error rejecting this collision rather than allowing an unsafe overwrite of the input file.
- found: Sets the input path to a name matching the planner's own generated final-work-path naming convention (.out.tonepoet-final.flac), then asserts plan_conversion errors with InvalidSettings on field "intermediate_dir/output_path" because the generated work path would collide with the input path.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `metadata_pruning_updates_later_verify_input`
- spec 3 · read at `3fdc7cafa4c5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:35:00Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test builds an execution plan where a metadata-pruning step is inserted into the pipeline, then asserts that a subsequent "verify" step's input path is updated to reference the pruned/intermediate output file rather than the original input path, confirming that plan steps correctly thread their outputs into the next step's input.
- found: Builds a plan with force_encode=true and flac.verify=true, expects exactly 2 commands: an ffmpeg encode followed by a flac verify command, and asserts the flac command's input path equals the ffmpeg command's output path (chaining).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_source_rejects_bit_depth_even_without_sample_kind`
- spec 3 · read at `a200e860e9b0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:45:09Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds a DSD source spec that has bit_depth set but sample_kind left unset (unlike the neighboring dsd_source_rejects_pcm_bit_depth_fact test), runs it through the planner, and asserts planning fails with an error — establishing that any bit_depth on a DSD source is invalid regardless of whether a PCM sample_kind fact accompanies it.
- found: Constructs a DSF/DSD SourceInfo with bit_depth and true_source_depth set to Int24 but sample_kind left None, calls source.validate() directly, and asserts it returns PlanningError::InvalidSource with field \"bit_depth\" — confirming any bit_depth on a DSD source is rejected regardless of sample_kind.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ssrc_force_without_rate_change_is_rejected`
- spec 3 · read at `377bde66cac9` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:43:52Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Constructs a planning request where the user forces the SSRC resampler tool, but the source and target sample rates are equal (no rate conversion needed), and asserts that plan construction returns an Err, since forcing SSRC without an actual rate change is invalid/pointless.
- found: Uses default pipeline settings (implying no sample rate change) with ssrc.force set to true, builds a request, and asserts plan_conversion returns Err(PlanningError::InvalidSettings { field: \"ssrc.force\", .. }).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_request_for`
- spec 3 · read at `125d411bcfcb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:55:54Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Test helper that builds a PlanRequest representing a DSD source being converted to the given output AudioFormat/PcmBitDepth/extension, filling in other required fields (paths, sample rate, etc.) with sensible test defaults. Used across many DSD-related planning tests to avoid repeating boilerplate construction.
- found: Builds a PlanRequest with a fixed DSD64 DSF source (in.dsf, 2ch), target settings forcing encode to the given format/bit-depth at 88.2kHz, and an output path using the given extension; other fields (intermediate_dir, scope, etc.) use fixed test defaults.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `source_resolved_int32_alac_is_rejected_through_public_planner`
- spec 3 · read at `f82a2580fce2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:56:37Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a plan request where the resolved source is 32-bit integer PCM and the target format is ALAC (which tops out at 24-bit), calls the public planner entry point, and asserts the result is an error/rejection rather than a plan, since ALAC cannot represent int32 depth.
- found: Builds a PlanRequest with a WAV/int32 source, target_format ALAC and target_bit_depth Source (i.e. "keep source depth"), calls plan_conversion, and asserts it returns PlanningError::InvalidSettings on field "target_bit_depth" with a reason mentioning "ALAC 32-bit".
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: I correctly predicted rejection and the reason (ALAC can't do 32-bit), but missed the specific mechanism: it's target_bit_depth=Source resolving to int32 that's rejected, not a directly-requested int32 target, and the error is a structured InvalidSettings{field,reason} rather than a generic error.

### `dsd_to_flac_int32_routes_through_wav_then_ffmpeg_experimental`
- spec 3 · read at `ecb8cb0f72ad` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:28:17Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a conversion plan/request for a DSD source targeting FLAC with int32 bit depth, resolves it through the planner, and asserts the resulting plan has an intermediate WAV step (DSD decode) followed by an encode step that uses ffmpeg's experimental path rather than the normal FLAC/sox encoder, since int32 output isn't supported by the standard encode route.
- found: Plans a DSD→32-bit-FLAC conversion and asserts two commands: sox producing an intermediate WAV, then ffmpeg with `-strict experimental` (needed for true 32-bit FLAC) and a description mentioning "32-bit FLAC".
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `dsd_to_aiff_float32_routes_through_wav_then_ffmpeg`
- spec 3 · read at `1b63492f3c1d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:47:09Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds a DSD source request (likely via dsd_request_for) targeting AIFF float32 output, runs it through the planner, and asserts the resulting plan has two stages: a DSD-to-WAV conversion (sox) followed by a WAV-to-AIFF encode step via ffmpeg (not the experimental variant, since AIFF float32 isn't the edge case flac_int32 is).
- found: Builds a DSD-to-AIFF-float32 request, manually downgrades the metadata policy (disabling tag transfer/artwork) to mirror what the production bridge does since ffmpeg has no AIFF metadata support, plans it, and asserts the first command is sox then ffmpeg with a pcm_f32be codec arg.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `dsd_to_wavpack_float32_is_rejected_through_public_planner`
- spec 3 · read at `c491a4a023de` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:01:39Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Constructs a plan request with a DSD source and a WavPack target specifying float32 sample format, calls the public planner entry point, and asserts the result is an error (rejection) because WavPack doesn't support float32 output — likely checking the error message/kind mentions the unsupported format combination.
- found: Calls plan_conversion on a DSD-source request targeting WavPack/Float32, expects an Err, and asserts it's specifically PlanningError::InvalidSettings with field "target_bit_depth" and a reason mentioning floating-point WavPack being unsupported.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tonepoet-pipeline/tests/settings_fingerprint.rs

### the file itself
- spec 3 · read at `795eb1f07e7c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:23:39Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This test file verifies the correctness of a "settings fingerprint" (probably a hash/digest of conversion settings used for caching/invalidation). It defines legacy/sentinel settings fixtures, checks the fingerprint is stable and exact for a known sentinel value, checks default vs sentinel settings produce different fingerprints, and — most importantly — has a guard test that recursively counts serializable fields on the settings struct(s) and compares against a hard-coded expected inventory size, to catch someone adding a new settings field without updating the fingerprint logic (no duplicates, no missing fields). It likely also verifies every conversion-affecting field actually changes the fingerprint when varied.
- found: Exactly as predicted: tests a settings_fingerprint function over PipelineSettings — checks field inventory size/no-duplicates (71 fields), fingerprint stability against a hardcoded hex hash for a sentinel settings struct, default vs sentinel differ, an exhaustive macro-driven test mutating every conversion-affecting field one at a time asserting the fingerprint changes (and cross-checking the covered field list against the canonical field path list), plus a recursive serde key-count pin test for default/sentinel JSON shapes.
- predicted: full · documented: none · derivable: yes · legible: not judged · trap: no

### `legacy_dsd_settings` — QUIRKY
- spec 3 · read at `d79ba0091307` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:29:48Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test helper that constructs a DsdSettings value, setting lowpass, gain_mode, margin_db, and gain_db from the passed-in arguments while filling the rest of the struct's fields with fixed "legacy" defaults, for use in fingerprint-stability test cases.
- found: Builds a DsdSettings by constructing a frozen legacy JSON wire shape (flat field names mixing native_v2 defaults with the passed-in lowpass/gain params) and deserializing it via serde, ensuring the old wire format still parses.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `sentinel_dsd_settings`
- spec 3 · read at `27a0b5bf1f71` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:39:58Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: This constructs a DsdSettings struct literal with every field explicitly set to the specific sentinel values enumerated in the docstring (noise_shaper: Crfb, modulator_order: Order7, trellis lookahead/nodes/latency, sinc filter params, gain compensation, etc.) — a fixed, non-default fixture used to detect when new/changed fields silently escape the settings fingerprint.
- found: Builds a DsdSettings by deserializing a serde_json JSON literal with all sentinel field values (matching the docstring), rather than constructing the struct directly — framed as testing a frozen legacy wire format, and expects deserialization to succeed.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: The docstring lists the sentinel values but doesn't mention this goes through serde_json deserialization of a 'frozen legacy' wire shape rather than a direct struct literal — that's only visible in the body.

### `flac_md5_sentinel`
- spec 3 · read at `9099490e43f1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:43:03Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Fixture function that builds a fully-specified PipelineSettings for FLAC output with every field set to a distinctive non-default sentinel value, including enabling the FLAC MD5 checksum, so the fingerprint tests can verify every field is accounted for in the settings fingerprint.
- found: Fully-populated PipelineSettings fixture with every field set to a distinctive sentinel value across all format/tool sub-settings — matches prediction, but write_md5 is actually false (not enabled) despite the function's name emphasizing "flac_md5".
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `fingerprint_field_inventory_has_expected_size_and_no_duplicates`
- spec 3 · read at `8cb6744fa126` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T20:32:54Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: This test grabs the static list/const of field names or paths used to compute the settings fingerprint (e.g. FIELD_INVENTORY), asserts its length equals some expected constant, and separately asserts putting it into a HashSet yields the same length (i.e., no duplicate field entries).
- found: Asserts SETTINGS_FINGERPRINT_FIELD_COUNT equals 71, then sorts+dedups a copy of SETTINGS_FINGERPRINT_FIELD_PATHS and asserts the deduped length matches the original length (no duplicate paths) — same idea as guessed but via sort/dedup rather than a HashSet, and against a separate FIELD_COUNT constant rather than the path list's own original length.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sentinel_fingerprint_is_stable_and_exact`
- spec 3 · read at `1d1787290e3b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:00:11Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds the sentinel settings object (via sentinel_dsd_settings or similar), computes its fingerprint, and asserts the result equals a specific hardcoded hex digest string, pinning the exact stable output so any accidental fingerprint algorithm change is caught.
- found: Asserts settings_fingerprint(&flac_md5_sentinel()).to_hex() equals a specific hardcoded hex digest, pinning the exact output for the flac_md5 sentinel settings (not the DSD sentinel I guessed) rather than a generic sentinel.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `default_and_sentinel_have_different_fingerprints`
- spec 3 · read at `fab26009b020` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:49:25Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Computes the settings fingerprint for the default settings and for the sentinel settings (a deliberately all-non-default value set used elsewhere for coverage testing), and asserts the two fingerprints are not equal, confirming the fingerprint function is sensitive to at least one changed field.
- found: Exactly as predicted: asserts settings_fingerprint differs between PipelineSettings::default() and the flac_md5_sentinel() settings.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `every_conversion_affecting_field_changes_the_fingerprint`
- spec 3 · read at `3dc8a20ebedb` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:21:42Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Starts from a baseline settings value and, for each conversion-affecting field (per some maintained inventory), produces a modified copy with that one field set to a distinct sentinel value, computes the settings fingerprint for both, and asserts the fingerprints differ — a long enumeration of field mutations/assertions (hence the 300+ line length) that guards against a field silently being excluded from the fingerprint hash.
- found: Uses an assert_mutation_changes_fingerprint! macro invoked once per field (~70 fields across format/resampler/dsd/metadata/replay-gain settings), each mutating one field away from a sentinel baseline and checking the fingerprint changes while recording the field path into `covered`; at the end it sorts `covered` and compares against a separately maintained SETTINGS_FINGERPRINT_FIELD_PATHS constant to catch drift between the two lists.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `serde_recursive_field_count_matches_checked_inventory_for_known_shapes`
- spec 3 · read at `0fefb89e2fab` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:48:49Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Serializes one or more known settings values (e.g. default and sentinel settings) to a serde_json::Value, uses recursive_object_key_count to count all nested object keys, and asserts that count equals the size of the checked field inventory used elsewhere for fingerprinting — verifying the inventory list stays in sync with the actual serialized shape.
- found: Serializes PipelineSettings::default() and flac_md5_sentinel() to JSON and asserts recursive_object_key_count returns exact pinned literals (81 and 88) rather than comparing to a separately computed inventory size, pinning both the frozen flat-v1 default shape and the sentinel's legacy re-serialized shape.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Name says 'matches checked inventory' but the assertions are hardcoded literals (81, 88), not a comparison against another computed inventory value — the name is somewhat misleading about the mechanism.

### `recursive_object_key_count`
- spec 3 · read at `1fd582770b08` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:55:11Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks a serde_json::Value and counts the total number of object keys across all nested objects (and arrays), used by a test to verify that the settings fingerprint covers every field. For Value::Object it adds the number of entries plus recurses into each value; for Value::Array it recurses into each element; other variants contribute 0.
- found: Matches prediction exactly, explicit match arms for scalar variants all returning 0.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
