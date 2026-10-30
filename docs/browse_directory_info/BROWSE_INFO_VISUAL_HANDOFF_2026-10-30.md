# TonePoet Browse Info: reference-layout correction

**Date:** October 30, 2026  
**Input:** `TonePoet_BROWSE_INFO_CORRECTIONS_SOURCE_2026-10-30.tar.gz`  
**Scope:** classified album, SACD, Blu-ray, DVD-Audio/DVD-Video and mixed-disc folder presentations; no new scan/probe/database layer.

## Disposition

This correction implements the three supplied PNGs as layout and interaction references. It preserves both fixes from the previous six-file delivery: ISO multi-disc names and truthful VOB/M2TS transport inventories. It is **source-level complete but not Rust-gate/visual accepted**. The integration host must compile and run the tests and capture actual TUI screenshots.

## Changes

Only `src/tui/draw_browse.rs` is modified relative to the supplied corrected source.

- Classified media folders render an **unlabelled, bold title**, **artist · year**, then **edition · medium/format**. An edition such as `Japan SHM SACD` does not redundantly repeat SACD.
- Rows use one consistent 10-column value alignment. The header and body have one empty row between them and no horizontal rule. Values and counts use the theme's cyan accent; labels/qualifiers use the theme's muted ink.
- FLAC albums show **Audio** (tracks plus summed duration when all tracks were already probed), **Format** (codec, bit depth, sample rate, channel layout), **Contents** (file/folder total plus audio/other breakdown), **Size**, **Modified**. Metadata not already cached is omitted or shown as unknown rather than fabricated.
- SACD shows **Audio**, **Format**, **Areas**, **Contents**, **Size**, **Modified**, with preferred stereo format and explicit stereo/multichannel area existence.
- Blu-ray shows **Audio**, **Mixes**, grouped **Streams** with first two higher-rate LPCM groups prioritized, remaining-stream count, **Protection**, **Contents**, **Size**, **Modified**. Standalone disc formats retain their underlying existing cached disc presentation; no probe work occurs in the drawing path.
- One-level `Contents` counts can be known while complete recursive `Size` is unknown. Completed measurements retain an active **Refresh** for media folders only; running measurements show spinning partial size and **Cancel**. Both actions and the existing **Streams…** control share one left-aligned row, with separately calculated hitboxes.
- Plain directories, C4 persistence, scanner, foreground/browse queue, conversion classification, and VOB/M2TS background probe data remain unchanged. Compact media views prioritize meaningful rows and controls, including transport-video rows; C4 plain-directory compaction remains unchanged.

## Precautions and data authority

Do not interpret the screenshots as an instruction to manufacture the exact examples when actual tags, disc tracks, folder counts or timestamps differ. All visual facts come from selected entry metadata, the existing folder classifier, cached audio probes, cached disc contents or directory-accounting result. Missing disc metadata displays a cache/probe status.

The screenshots were captured under a themed terminal. This change uses existing TonePoet semantic theme colors rather than hardcoding a new global color palette. Real screenshot comparison is still necessary under the intended theme/font/terminal geometry.

## Verification actually performed

- Read all three supplied PNGs and extracted their layout/label expectations.
- Corrected source archive checked against the intended previous six-file baseline.
- All untouched Rust sources match the corrected baseline by SHA-256.
- `git diff --no-index --check`: no whitespace errors.
- Idempotent patch applied on a fresh file extraction; second application returned a no-op; reverse patch check passed; mismatched baseline was refused.
- Target source hash: `373b62e47ac5b5262d4b326a80509ef06975119abe7087cca92057e39f7595b7`.
- Baseline source hash: `51970e0ab9b50874fb09559dfdcff1343564d9d45a031e5d73a748f6211186b9`.
- Patch hash: `565a1cf56a4bda020eb24fc8bf79edf34dae575e52e09f7ced2e2cef254e3cad`.
- **Not performed**: `cargo fmt --check`, Rust compilation, Rust tests, ratatui screenshot capture or physical-terminal interaction. There is no Rust compiler or Cargo on this host. Do not treat the added tests as passes.

## New Rust regression cases (not yet executed)

- FLAC album screenshot rows, header values, available known accounting counts and color styles.
- SACD edition line, format, areas, contents and adjacent actions.
- Blu-ray grouped LPCM ordering, mixes, protection, local-only contents with unknown recursive size.
- Unknown and running measurements with Refresh/Cancel.
- Exact painted button offsets and click hitboxes at 80×32, completed and scanning.
- Narrow row spans not overflowing at 11/13/14/18/28/78 content columns.
- Compact media arrangement at typical small terminal heights and preservation of plain-directory C4 behavior.

## How to resume and accept on a Rust host

1. Extract the **full source bundle**, or extract the **patch bundle** and run `sh apply.sh /path/to/tonepoet` on the exact corrected six-file baseline. Applying twice is safe; a different source SHA-256 fails closed.
2. Keep the PNGs in `references/`. Set the terminal font, theme and geometry used for the mockups. The mockups are intent/reference images, not automatic output; the mockups' album names, byte counts and dates are fixtures.
3. Run `cargo fmt --check` and `cargo test --workspace` in the project root. If formatting needs correction, apply `cargo fmt` to the affected Rust module and rerun tests, then update hashes/patch evidence. Re-run the existing full gate, not only the new tests.
4. In a TUI session, select a FLAC album, SACD ISO folder and Blu-ray ISO folder with populated caches. Capture Info at normal height and at 80×24. Compare the title/artist/year/edition hierarchy, row order, colors, spacing and exact action buttons against `references/`.
5. Also test empty/partial caches, one-level known counts with unknown recursive size, completed -> Refresh -> Cancel -> completed, the Streams overlay and both button hitboxes, a two-ISO MultiDisc and a VOB/M2TS video with 2 audio mixes and a video-only file.
6. Do not expand scanning, the persistent data model or conversion classification to close a rendering mismatch. If only visual differences remain, adjust `draw_browse.rs` and its focused tests.

## Rebase and checksum gates

- Original source archive SHA-256: `798d29a1e0a8775396d6c8bb940b95453664f7d4d39d450a57b53b8faa6a7a9a`
- Corrected source file SHA-256 after patch: `373b62e47ac5b5262d4b326a80509ef06975119abe7087cca92057e39f7595b7`
- Reference images: `evidence/verification.json`

**Next engineer:** compile, run gates and compare actual ratatui screenshots. The source and all fixtures are bundled so no earlier chat state is required.
