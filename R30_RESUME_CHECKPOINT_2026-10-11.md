# R30 interruption-safe engineering checkpoint — 2026-10-11

## Resume source of truth

1. This archive's `tonepoet/` directory is the complete corrected source tree,
   not a patch-only or subset package. `R30_FROM_R29C.patch` is a narrower,
   reviewable alternative against the original supplied R30 source snapshot.
2. The source came from the uploaded R30 package, SHA-256
   `50192aadb4b39479cf8356aa63b48ae9934c9a32d38e56c49f9b0a18b38ee330`.
   The uploaded R30 brief SHA-256 is
   `d3d36c7ae90e8d6a75fa2af65cc112e905380340bb25688f5aafb7b56d768ca8`.
3. Local scratch Git initially imported the full baseline at `f2c3891`.
   This is an ephemeral import marker, **not** downstream `572063d` history.
   `R30_FROM_R29C.patch` corresponds to the exact baseline snapshot.
4. The changed Rust files are enumerated in
   `R30_CHANGED_SHA256SUMS.txt`. The complete inventory and symlink link-text
   are in `R30_SOURCE_SHA256SUMS.txt`; generated delivery metadata are
   intentionally part of that tree and are not build-host qualification.

## Feature completion

- Two-decimal minimum + exact nanodecibel ceiling human log and Format row.
- Clickable PCM/DSD `custom` pill with matching hit-region registration.
- Enter/Esc/Tab and text input behavior; failed Enter cannot commit;
  blur/hidden row discards invalid draft without altering target or provenance.
- Shared command + editor parser: unsigned magnitudes mapped to negative
  headroom, negative explicit accepted, positive plus sign rejected, zero valid.
- Seven `missing_docs` sites annotated.
- Focused R30 tests added to the established source test modules.

## Independently checked here

- `git diff --check`: PASS.
- Lexical delimiter balance on all nine changed Rust files, using Pygments
  RustLexer: PASS. This is **not** a Rust parse or compilation check.
- Changes' source wiring: inspected renderer, button registration, mouse/key
  dispatch, parser, app state and existing command handling.
- Patch replay and byte-for-byte matching: see `R30_STATIC_QA.txt`.
- Archive/source checksum verification: see `R30_STATIC_QA.txt`.

## Unfinished hard gates; DO NOT certify R30 until passing

- `cargo check` / `cargo test --locked` (both targeted and full workspace):
  NOT RUN, no local Rust compiler/Cargo.
- `cargo fmt` / `rustfmt`: NOT RUN, no local toolchain. Manual source
  review is not a substitute for compilation or formatting verification.
- Reference v18 full requalification against new hashes of `stages.rs` and
  `tonepoet-pipeline/src/settings.rs`: NOT RUN, tools/real audio host missing.
- `reference_qualification_freshness`: expected to reject the original
  installed sidecars until requalified. These files were NOT modified here.
- Real SACD Reference conversion and positive/negative runtime black-box
  smokes: NOT RUN.
- Full performance and regression suite: NOT RUN.

## Immediate next steps

1. Move this full tree to the configured Rust 1.93+ build host.
2. Review formatting of edited fragments; if you change any source or compile
   fixes, update checkpoint/hashes and rerun all acceptance checks.
3. Execute `R30_ACCEPTANCE.sh` with a **real SACD ISO**; observe the mandated
   positive conversion, freshness, complete workspace, real-tool smokes.
4. Execute the negative Reference runtime-closure smoke and verify final
   human logs for `ceiling -0.10 dBTP`, as well as `-0.0001 dBTP`.
5. Only on green acceptance record the new sidecar hashes, builds and tests;
   repackage and mark as qualified. Preserve failures/diagnostics if interrupted.

**Candidate handoff is complete. Production/Reference qualification is HOLD.**
