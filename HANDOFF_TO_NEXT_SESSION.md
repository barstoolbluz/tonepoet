# R19 HDCD Decode — authoritative continuation checkpoint (2026-10-14)

## What to use

- Governing brief: `BRIEF_R19_convert_hdcd_decode_planning_FINAL_R2_2026-10-03.md`.
- Current-tree addendum: `ADDENDUM_HDCD_BRIEF_current_tree_2026-10-13.md`.
- Input SHA-256: `8f6cb3bfe5565193e8b39483e31f6ef32e8bcae895edc88478d91d0f34fa3904` (`TonePoet_LOGGING_R37_HDCD_decode_2026-10-13.tar.gz`).
- R37 baseline Git commit `f5a9747`. Inspect `git log` for work commits and `git diff f5a9747` for exact changes. Source tree is supplied complete with its local Git history.
- Delivery and qualification caveats: `DELIVERY_NOTES_R19_HDCD_2026-10-14.md`.
- Integrity: `sha256sum -c R19_SOURCE_SHA256SUMS.txt` in this directory; the archive `SHA256SUMS` is external to this tree.

## Work done

1. Structured detector/capability model and identity-valid analysis-cache storage for HDCD; strict 16/44.1 eligibility.
2. Bounded async, identity/generation-safe per-file and sample-exact CUE member inventory, dynamic Convert HDCD row with notice/control/defaults/warnings and preset/source provenance.
3. Guarded per-item request binding and source-first typed/planned FFmpeg HDCD lowering, widened S32 and existing Float64 carrier, selected SSRC/SoX/FFmpeg continuation; effect graph and CUE ordinal specialization.
4. Typed fingerprints, fail-closed Int16 and true-peak combinations, source identity pre/post decoding, physical frame/format validation.
5. Focused Rust tests (not run) plus standalone executable physical FFmpeg regression test; corrected Int24 preset/source-swap persistence.

## Resume from here

1. Verify `git status --short` is clean; read both brief and delivery notes. A prior session's WIP status descriptions are superseded by this handoff.
2. The current container has no Rust/Cargo/Nix. **Compile and run the focused tests on the pinned build host**. The original 7,459 passing tests are not results for these source edits.
3. Exercise the mandatory paths named in the delivery notes (SSRC, SoX-ng, FFmpeg/SoXR, lossless same rate, lossy, PE + de-emphasis, single-image CUE mixed content, source mutation, presets); fix only evidenced defects and rerun affected tests.
4. The three modified source-locked files invalidate the previously installed Reference certificate. Requalify the current bytes and full required runtime closure using `scripts/requalify_reference_r18.sh`, not a hash-only edit; test freshness and production refusal for mismatched source.
5. Once qualification and release gates pass, repackage the **post-qualification** checkout with newly generated source and archive SHA-256 manifests; do not promote this unqualified source archive as a release build.

## Invariants to preserve

- No HDCD scan for unknown/ineligible PCM; no whole-album inference from track one.
- Detected TF-only/passive is visible, not actionable; explicit On applies only to PE/LLE members.
- Original 16-bit PCM enters HDCD before de-emphasis and resampling; precision survives as S32/Float64 and lossless Int16 is refused.
- Single-image CUE decode is sample-range- and source-ordinal-specific; no image-wide decoder flag applied to non-HDCD tracks.
- User's selected resampler/terminal/dither contracts remain unchanged; certified true-peak+HDCD is refused until qualified.
- Manual overrides and preset provenance must not be overwritten by late scans, source swaps or automatic Int24 promotion.
- Do not bypass/reference-override the source lock or claim a green Rust suite absent an actual build.

## Corrective continuation checkpoint (2026-10-14)

The last R19 original-archive commit was `d7ef9a3`. The next commits are:

- `8bf1ebb` — add missing exhaustive HDCD evidence match; preserve source-derived preset bit-depth provenance across serialization; require size and nanosecond mtime on HDCD cache writes.
- `05924b0` — exercise the actual FFmpeg detector on a synthetic Peak Extend packet after the first second.
- `485c308` — prove FLAC/CUE per-member scans independently classify a sample-bounded mixed track pair.
- `19ef3b1` — add a Rust unit asserting distinct completed HDCD execution evidence.

The original FFmpeg `/dev/null` concern was a manual test invocation mistake: source already passes `-y`. No detector fix is necessary. Physical scanner and sample-exact decoder tests pass on the supplied FFmpeg 7.1.5. Re-run `python3 tests/hdcd_source_first_ffmpeg_smoke.py` on FFmpeg 7.1.3 in the pinned build environment. Run `cargo test --locked -p tonepoet-pipeline hdcd`, `cargo test --locked --lib hdcd`, the complete workspace build/test and the mandatory Reference requalification before release. No Rust/Cargo was available in this environment, so test assertions in the Rust source remain unverified.

For artifact validation, first run `sha256sum -c R19_SOURCE_SHA256SUMS.txt` in the extracted checkout; verify `git log --oneline -8`, `git status --short` and the separately supplied whole-archive digest. Continue from the highest checkpoint commit. Do not reapply any of the prior corrective commits to an earlier baseline.

## R3 follow-up correction — completed-scan hot path and analysis-cache persistence

**Use the R3 archive, not the R2 archive.** Follow-up finding 1: the completed Convert HDCD scan was rechecking every source on each UI tick; the generation-matched return in `src/tui/app.rs` now makes that path constant-time, with identity checks retained at completion/commit/execution. Follow-up finding 2: `store_analysis` used to drop typed HDCD columns, and Details completion did not persist its typed detector result; those paths are corrected in `src/db.rs` and `src/tui/event_loop.rs`.

The code patch was checkpointed in Git as `b367fc9`. The smoke test `tests/verify_hdcd_analysis_cache_sql.py` reads and executes the actual Rust SQL; the new Rust tests exercise the completed generation, reload/replacement, Details cache handoff, and stale source rejection. Read `git log` for the final documentation/manifest commit. In an interrupted session, start from this tree's `HEAD`; do not reapply R2 or R3 edits.

This runtime has **no Cargo, rustc, or pinned FFmpeg 7.1.3**, so the actual Rust tests, full workspace build, end-to-end Convert execution and Reference requalification are not claimed. The existing modified Reference-locked files still require proper requalification on the pinned build host. After verification, regenerate the source manifest and complete archive rather than distributing a locally rebuilt binary under the old Reference certificate.
