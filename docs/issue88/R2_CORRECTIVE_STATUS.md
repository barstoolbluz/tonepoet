# Issue #88 R2 corrective source checkpoint

**Status: corrective source complete, Rust build and real-disc acceptance NOT verified.**

Base: `02303b22cfe7a188d806ccb308d242787985af22` (R1 corrective checkpoint). The attached `notes/R2_AUDIT.md` is the authoritative R2 scope. This correction changes native extraction and its existing TUI bridges. It does not redesign Convert, publication or recovery.

| Audit finding | Focused source correction | Qualification |
| --- | --- | --- |
| R2-01 P1 Continuous SACD | Native continuous DSF join streams DSD bytes through the existing readers/writer. DST-preserving DFF join re-emits verified encoded DST frames through the existing DFF writer. Selected tracks retain authored order; staging and cancellation remain in the current worker. | Synthetic 2/5/6-channel, nonadjacent-selection, DST-frame, cancellation tests added, **not executed**. SACD+CUE is expressly unsupported: CUE `FILE WAVE` cannot describe DSF/DFF. No real SACD fixture run. |
| R2-02 P2 Source precedence | SACD mapper retains distinct authored and sidecar album/track fields, then resolves missing values per selected field according to the Tags source cycle when the extraction job is built. | Per-field and selected-job regressions added, not executed. Other disc formats retain their existing metadata sources where distinct text is unavailable. |
| R2-03 P2 Artwork/Edit tags | More/Edit tags reuses the existing SACD, DVD-Audio, DVD-Video and Blu-ray editors; after explicit saved edits, the affected disc cache is invalidated. Artwork resolves from prepared source keys or an existing shallow cover-name policy. No cover sidecar is published by extraction. | Editor integration, proper image embedding and real-media tag persistence need runtime verification. **Existing editors save their source sidecar immediately on explicit Save**; this does not fully implement edit-until-extraction staging in the R2 audit, and must be reviewed before approval. |
| R2-04 P1 Probe facts | Existing async probe completion carries observed rate, depth and channel count, applies values to the matching open presentation, then recomputes current estimate/blockers. Existing probe generation/path guards are unchanged. | Unknown-depth and delayed/closed-overlay real-media tests not executed. |
| R2-05 P1 Continuous storage | Estimates continuous output plus two retained full-program buffers and largest-item scratch allowance. Checks destination-volume free space because private fallback staging lives there even with separate configured scratch. | Synthetic 20-chapter preflight test added, not executed. Approximate estimates still require tests with real output sizes and storage configurations. |
| R2-06 P1 Blu-ray masks | Rejects ambiguous count-derived `7.1` as speaker-layout proof; carrier masks win, otherwise only explicitly identified side/wide channel layouts map to masks. | Focused mask test added; channel-impulse and authored 7.1 fixture tests unrun. |
| R2-07 P2 CUE indexing | Rounds only index positions to nearest 1/75 s with wide integer arithmetic; writes a precision note and never changes source PCM samples. | 48,001-sample and aligned 48 kHz tests added, not executed; parseable cue and native sample comparisons require build-host tests. |
| R2-08 P2 SACD header | Displays available catalog, publisher, date, disc n of m and hybrid TOC facts on the second header row, omitting missing fields. | Source inspection only; requested 150x40, 96x30, 64x20 terminal checks not executed. |

## Preserved from R1

- Existing Convert source routing and normal job queue/cancellation integration; no parallel worker lifecycle.
- Separate-track extraction, native integer PCM and source masks, bounded private working storage and atomic completed-file publication.
- Earlier D-01..D-15 fixes were not intentionally reopened.
- SACD omits a protection-blocker row; unresolved DVD/BD protection behavior still requires authentic media to qualify.

## Verification boundary

**Performed here:** static source/interface checks, `git diff --check`, Git object/provenance checks, patch replay and independently generated RIFF/RF64/Wave64 reference files inspected by FFprobe. These checks do **not** compile or run the changed Rust code.

**Not performed:** `cargo fmt`, `cargo check`, Rust unit tests, real-disc native extraction, original-vs-extracted byte-for-byte DSD/PCM comparisons, image-tag validation, long-file capacity tests, or interactive terminal/mouse tests. Rust/Cargo are not installed in this environment. The build-host script in `verification/verify_on_build_host.sh` must run on an appropriately provisioned machine.

**Acceptance:** No claim of production approval until the Rust build, focused tests and representative native-disc validation have passed, and the editor's save/staging semantics are resolved against R2-03.
