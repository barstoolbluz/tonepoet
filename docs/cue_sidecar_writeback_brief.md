# Brief: write metadata-editor corrections back to sidecar CUE files

Date: 2026-07-08

## Context — what already works

Single-image CUE albums (one audio image + one `.cue` per directory): the
metadata editor writes corrections to the **image file** — flat tags plus a
regenerated embedded CUESHEET tag (`regenerate_cuesheet_for_save`, referenced
around src/tui/probe.rs:2101; the per-track round-trip note is at
probe.rs:2130). As of the fix landing alongside this brief, conversion honors
those corrections too: sidecar CUE resolution upgrades to the referenced
image's embedded sheet when it exists and structurally matches
(`try_upgrade_sidecar_to_embedded_image_cue`,
src/convert/pipeline/materializer_cue.rs; upgrade is metadata-only — the
sidecar remains authoritative for structure, mismatches keep the sidecar and
log).

## The gap this brief addresses

The **sidecar `.cue` file itself stays stale**. The user corrects tags via
MusicBrainz lookup and `:fix-caps`, the overlay shows the corrections, the
conversion now applies them — but any other consumer of the library (players,
rippers, other tools that read the `.cue`) still sees the old titles. The user
explicitly noticed and dislikes this: after editing, "the cue files
themselves aren't changed."

Desired: when the metadata editor saves corrections for a single-image CUE
album, the sidecar `.cue` is rewritten to match — the same metadata the
embedded CUESHEET now carries.

## Why this is delicate (and why it gets a brief)

Rewriting user-owned CUE files can corrupt a library if done naively:

1. **Encoding.** Many sidecars are not UTF-8 — the parser does bounded
   SHIFT_JIS/GBK/BIG5/EUC_JP/Windows-1252 detection
   (`decode_cue_bytes_for_path`, src/convert/cue_parser.rs) precisely because
   the user's collection contains such files. Decide and document the
   write-back encoding policy. Reasonable candidates: (a) re-encode to the
   detected source encoding when the corrected text is representable,
   falling back to UTF-8 (with or without BOM?) when not; (b) always write
   UTF-8 and document the normalization. Whatever you choose must be
   deliberate, tested with non-UTF8 fixtures, and lossless for the metadata
   being written.
2. **Fidelity.** Only metadata fields (TITLE/PERFORMER at sheet and track
   scope; SONGWRITER if edited; REM GENRE/DATE etc. if the editor carries
   them) may change. FILE references, TRACK/INDEX structure, PREGAP, FLAGS,
   ISRC, CATALOG, and unrecognized/REM lines must survive byte-exact. A
   parse-mutate-serialize round trip through the existing `CueSheet` model
   would destroy formatting and unknown lines — a targeted line-level edit
   (or a serializer proven lossless on the user's real corpus) is required.
3. **Safety.** Atomic write (temp + rename) with the original preserved on
   any failure; never truncate-in-place. Respect read-only files gracefully
   (surface a status, do not fail the whole save). Multi-CUE directories:
   only rewrite the CUE actually associated with the edited image.
4. **Consistency.** The rewrite must happen from the same save path that
   regenerates the embedded CUESHEET, with the same values, so the three
   copies (flat tags, embedded sheet, sidecar) cannot diverge. If the
   sidecar rewrite fails, the save must still succeed for the image (today's
   behavior) and tell the user the sidecar was left stale.

## Where things live

- Editor save: `apply_audio_tag_changes_with_save_blocks` (src/tui/probe.rs:2120)
  and the CUESHEET regeneration machinery near it; the async save spawn is in
  src/tui/keybindings.rs (search `apply_audio_tag_changes_with_save_blocks`).
- Single-image detection: `detect_single_image` (src/tui/cue_parser.rs), which
  already locates the sidecar CUE and the image.
- CUE text decode: `decode_cue_bytes_for_path` and encoding tests
  (src/convert/cue_parser.rs).
- Conversion-side upgrade (context for consistency, do not change):
  `try_upgrade_sidecar_to_embedded_image_cue`
  (src/convert/pipeline/materializer_cue.rs).

## Hard constraints

- Zero behavior change when the user saves edits on non-CUE albums.
- The user's real corpus is the acceptance bar: CCR 24KT Gold collection
  (UTF-8 cues) must round-trip; add non-UTF8 fixtures (SHIFT_JIS at minimum)
  to tests.
- Deterministic: saving twice produces an identical file.
- Suite baseline: 2583 lib tests passing, 9 known pre-existing failures
  (docs/pre_existing_test_failures_triage_brief.md); zero warnings.
- The sandbox cannot compile; the applier fixes compile errors. Favor
  mechanically verifiable line-level edits over rewriting parsers.

## Acceptance

- Edit metadata on a single-image album (title + track titles), save: the
  sidecar `.cue` on disk reflects the corrections; FILE/INDEX/REM lines are
  byte-identical to before except the edited fields; re-saving is a no-op.
- A SHIFT_JIS-encoded fixture cue survives an edit with its encoding policy
  applied as documented.
- Conversion of the edited album produces the corrected tags (already true
  via the embedded upgrade — must remain true).
- Read-only sidecar: save succeeds for the image, user sees a clear status
  that the cue was not rewritten.
