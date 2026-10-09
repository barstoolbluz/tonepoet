# TonePoet R22 durable restart checkpoint — 2026-10-09

**Objective**: finish the three R22 corrections and the R22C test-only fix,
then deliver complete reproducible source + a test-only patch with truthful host gates.

**Inputs**:
- `BRIEF_LOGGING_R22_duplicate_log_debug_leakage_iso_preset_2026-10-09.md` (included).
- `TonePoet_LOGGING_R22_duplicate_log_debug_leakage_iso_preset_2026-10-09.tar.gz` (original complete R21 tree, user supplied).
- Claimed upstream reference baseline `origin/main=c08886d` per user brief;
  source snapshot contains no `.git` (so exact commit cannot be independently checked).

**Current modified source paths**:
- `src/convert/pipeline/stages.rs`: archives displaced log only, not first-run
  current log; previous-run timestamp + SHA collision suffix, exclusive creation,
  idempotent retry/backup cleanup, entire-album history carry; revised #65 tests
  and R22 preservation regression.
- `src/convert/pipeline/execution_evidence.rs`: human semantic evidence typed
  labels (no Rust Debug enum spelling), explicit encoder/input/output/owner/dither,
  a single projection for each fresh operation, direct and compound tests,
  simple typed unmet-facts text. R22C removed an unsupported double-projection
  equality assertion in one regression test; no runtime implementation changed.
- `src/tui/command.rs`: defer generic ISO preset until source probe facts known,
  probe-result guard, strict refusal remains for actual PCM; DSD64 successful,
  PCM strict-rejection and failed-probe/stale event tests.
- `scripts/smoke_r20_album_publication.sh`: assert first-run no history,
  second-run archive equals displaced prior log, N=1,2,3.

Other added deliverable files are the R22 brief, corrected handoff, checkpoint and
source-tree checksums (not a changed runtime dependency).

**Actual checks available**:
- `bash -n scripts/smoke_r20_album_publication.sh` and `bash -n scripts/requalify_reference_r20.sh` passed.
- Source file delta matches the four expected code/script files plus R22 docs.
- Python-based SHA manifest and archive/patch reproduction checks accompany deliverables.

- All 20 source-level static QA checks passed; details are in
  `R22_STATIC_QA_2026-10-09.txt` (lexical gates, not Rust compilation).

**NOT RUN / release blocking**:
- `cargo`, `rustc`, `rustfmt`, `nix` unavailable locally.
- Rust compilation, `cargo test --workspace`, black-box conversion, UI real ISO,
  real audio-tool execution, and Reference qualification require the host.
- `src/convert/pipeline/stages.rs` changed; installed v18 Reference qualification
  sidecars are now stale. DO NOT replace/update them by hand or ship as R22 qualified.

**Next build-host actions** (in order): `cargo fmt --all`, scoped review,
`cargo test --workspace`, release build, `scripts/smoke_r20_album_publication.sh`,
`scripts/requalify_reference_r20.sh /real/path/to/SACD.iso`, negative
runtime-mismatch smoke per preinstalled R20 handoff, real Browse preset
acceptance, then release. Full instructions in
`R22_ENGINEERING_HANDOFF_2026-10-09.md`.

**Scope discipline**: no extra dependencies, no rewrites, preserve strict preset
atomicity, never classify `.iso` as DSD from extension alone, never snapshot
intermediate same-run delivery updates. This is a conservative integration delta.
