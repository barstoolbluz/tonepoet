# TonePoet R21 resume checkpoint — 2026-10-09

Task: update eight stale fixtures and two non-failing unused-import warnings per the superseding BRIEF_LOGGING_R21_r20_accepted_eight_fixtures_2026-10-09.md (SHA-256 `ebfafb26ba6532075d43b17ce1d52af1bca1e3424d533cae3df8d7eb09dc4d71`) based on TonePoet_LOGGING_R21_r20_accepted_eight_fixtures_2026-10-09.tar.gz (input SHA-256 `743c326029e83398e333081e751ea865cc4b774da826927d6ff2a070eff3578c`).

## Current source state

- Implementation and test edits have been applied to exactly five existing files. See `R21_ENGINEERING_HANDOFF_2026-10-09.md` for the change-by-change ledger.
- Actions row preserved at pane heights 20 and 21 (offset 18) by dropping only its separator and heading in the compact layout; at height >=22, existing layout remains (offset 20).
- The third #65 strict Reference inventory test uses the same single-snapshot proof as the other inventories.
- ReplayGain smoke fixture is 1.0 s. The legacy manifest test checks preservation of pre-existing bytes and no new publication.
- New superseding requirement: removed unused `SsrcPdfType` production import and six unused test-module manifest imports. A single test-only `SsrcPdfType` use now has an explicit `tonepoet_pipeline::` qualifier to preserve test compilation.
- Source code modifications are complete; no conversion implementation code was changed beyond the UI visibility/positioning fix.

## Verification and unresolved release gate

Static checks, original-source and previous-R21 patch reproducibility, full-source comparison, and SHA-256 are described in the external superseding R21 verification report. Rust/Cargo/Nix and a configured real SACD toolchain were unavailable here; none of the 7 affected Rust tests have been executed in this environment. The real-audio smoke has not been executed here.

**Never claim Reference qualification from this archive:** `src/convert/pipeline/stages.rs` belongs to Reference's full-file locked source set. The original four sidecars were preserved and therefore are stale. Finish on a qualified host with `./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso` (runs gate and smokes), then perform the separate negative mismatch refusal smoke. Do not forge or directly edit qualification JSON, and do not use `TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE=1`.

## Resume instructions

1. Use the superseding R21 full-source archive, the full patch against the exact original R20C2 source archive named above, or the incremental patch against the previous R21 corrective bundle. Both patch applications are reproduced on clean source extractions.
2. Run focused unit tests and the host requalification runner from `R21_ENGINEERING_HANDOFF_2026-10-09.md`.
3. After passing on-host tests and qualification, publish the host-generated four qualification sidecars along with this unchanged code. If further locked source changes occur, regenerate the sidecars again.
4. Keep the delivery SHA-256 checksums and build-host test logs with the release record.
