# BRIEF — Chapters as first-class structure

**Date:** 2026-09-04
**Base:** `main` @ `d4d1d85`
**Sequence:** first of two. A second brief will cover *authoring* chapters — editing
boundaries, and generating them where none exist. This one is about making the chapter
structure that already exists in a file usable.

## What the user wants

**Chapters should be first-class export boundaries.** Where a source carries usable chapter
information, the user should be able to decompose it and export one file per chapter. Where
Tonepoet writes a chapter-capable container, it should be able to write that structure back.

Concretely, the case that prompted this: a 19h 43m audiobook, `.m4b`, 45 chapters with real
titles. Tonepoet cannot open it.

## Why this is being asked for

Two defects, one small and one structural.

**`.m4b` is not an accepted source.** `is_single_audio_extension`
(`src/convert/pipeline/stages.rs`) lists `m4a` and `mp4` but not `m4b`, so the file is rejected.
The refusal is also poor: it arrives *after* the item is queued and the scheduler has started,
and the message reads "Regular audio folders must be expanded into supported audio files
before queue processing" — which describes a folder problem for a single file, and never
mentions m4b or what would fix it.

**Chapters are invisible.** Nothing in the tree reads or writes embedded chapter information.
`ffprobe` sees the 45 chapters; Tonepoet has no path to them.

### Measured, not assumed

Adding `"m4b"` to that one list was tried directly. It is sufficient to make the file convert —
there is no second gate. But the result is bad: the conversion produced a **single track**,

```
converted/.track-0001.work/.001-01 - Pushing Ice.tonepoet-final.flac
```

with all 45 chapters discarded, having written 2.1 GB of staging in five minutes of a 19h 43m
program — extrapolating to roughly 8 GB of FLAC from a 538 MB source, as a lossless copy of
63 kbps lossy audio, in one unnavigable file.

**So the one-line fix must not ship on its own.** It converts a clear refusal into a silently
useless result, which is worse. `m4b` admission belongs in this delivery, with chapter handling,
not before it.

## What already exists

This is the most important section in the brief. Most of the machinery is present, and the
main risk in this work is building a parallel copy of something that already works.

**A boundary-carrying model.** `CueAlbumTrackSource` (`src/tui/app.rs`) carries
`index00_frames` and `index01_frames` — pregap and start, in CUE frames — beside
`original_track_number`, `file_ref` and ISRC. It already feeds the metadata editor's Album view,
and `app.rs:1384` derives start/end seconds from it.

**CUE authoring and regeneration.** `src/tui/cue_generate.rs` (1,732 lines) provides
`generate_single_image_cue` (one FILE, N TRACKs, cumulative `INDEX 01`),
`generate_multifile_cue`, `regenerate_cue_with_overrides`, `validate_cue_content`,
`frames_to_cue_timestamp`, and MusicBrainz fill. It emits `INDEX 00` from `index00_frames`
(line 331) and documents preserving both indices so timestamps do not drift through
regeneration.

**Sample-accurate cutting.** `materializer_cue.rs:3225` builds
`atrim=start_sample={start}:end_sample={end}`, deliberately in preference to `-ss`/`-t`
(asserted at line 5344). The cutter takes two sample numbers; it does not care where they came
from.

**Lossy-source segment handling.** `SegmentLengthPolicy` already distinguishes `Exact` (a
lossless image, staged length must match) from `LossyTail` (encoder delay/padding means the
decoded length becomes the fact, with a bounded shortfall guard). An m4b is lossy AAC and needs
exactly this.

**Chapter read and write in an existing dependency.** `ffmpeg-next` 7.1 — already a dependency,
already used by `src/tui/probe.rs` — exposes `Chapter::start()`, `end()`, `time_base()` and
`metadata()` for reading, and `format::context::Output::add_chapter(...)` for writing.

**Reading is proven.** It was run directly against the audiobook: 45 chapters, correct titles,
correct boundaries, last ending at 70,986.00 s.

**Writing is not.** `add_chapter` exists and validates its inputs, but it is a workaround rather
than a supported entry point: libav's `avpriv_new_chapter` is private, so the wrapper allocates
an `AVChapter` with `av_mallocz` and attaches it with `av_dynarray_add` inside `unsafe`. It has
not been executed here against any container. **Prove chapter writing early**, on a real file,
before the rest of the delivery is built on the assumption that it works — and if it does not,
say so rather than working around it silently. Reading, export and `m4b` admission all stand on
their own if writing turns out to need a different route.

```
id=0      0.00s ->     23.70s  Opening Credits
id=1     23.70s ->   1004.12s  Prologue
id=2   1004.12s ->   1845.34s  One
```

`id3` 1.17 supports CHAP frames if MP3 chapters ever matter. No new dependency is needed.

## What does not exist

- Any read or write of embedded chapters (`chpl`, `ChapterAtom`, lofty chapters,
  `-show_chapters`: all absent).
- Any way to cut at boundaries that did not come from an existing CUE. Every split path derives
  from parsed structure.
- `m4b` as an accepted source.

## Outcomes wanted

- A source carrying usable chapter information can be decomposed and exported as one file per
  chapter, with chapter titles becoming track titles.
- `.m4b` is an accepted conversion source, and never lands as a single undifferentiated track
  when the file carries chapters.
- Where Tonepoet writes a chapter-capable container, chapter structure survives the conversion
  rather than being silently dropped.
- Chapter information from a container and track structure from a CUE are the same kind of fact
  to the rest of Tonepoet, so downstream code does not care which one it came from.
- A source that genuinely has no chapters is refused clearly, early, and in terms that name the
  actual problem.

## Chunking

Offered as a reasonable decomposition, not a required one:

1. **The model.** One representation of ordered, titled time boundaries over a program, and its
   relationship to what already exists (see the constraint below).
2. **Reading.** Chapters out of chapter-capable containers into that model.
3. **Export.** One file per chapter, through the existing cutter and the existing lossy-tail
   policy.
4. **Writing.** The model back into a chapter-capable container on output.
5. **`m4b` admission**, plus the misleading refusal message, landing with the above.

## The constraint that matters most

Six modules in this tree handle CUE data: `src/convert/cue_parser.rs`,
`src/tui/cue_parser.rs`, `src/tui/cue_generate.rs`, `src/convert/split_cue_album.rs`,
`src/convert/pipeline/materializer_cue.rs`, and `crates/tonepoet-features/src/cue_generator.rs`.

They are not, however, six competing implementations, and the existing arrangement is worth
copying rather than merely avoiding. **Parsing has exactly one core.**
`src/convert/cue_parser.rs` owns every parse entry point; `src/tui/cue_parser.rs` opens with
`pub use crate::convert::cue_parser::*;` and adds only layout and file-reference resolution on
top, with no parser of its own. That is the shape to follow: one owner of the data, thin layers
that re-export and extend it.

**Do not add a parallel representation.** The chapter model must explicitly reckon with
`CueAlbumTrackSource` and `src/tui/cue_generate.rs` — subsuming them, or bridging to them by a
stated rule — and the delivery should say which it chose and why. A chapter type that
duplicates `index00_frames`/`index01_frames` under new names, with its own formatter, is the
most likely way this work goes wrong.

## Scope

**In scope:** the chapter model; reading embedded chapters; export one file per chapter; writing
chapters to chapter-capable output containers; `m4b` admission and its refusal message.

**Out of scope, and deliberately:**

- **Authoring** — editing boundaries, generating them where none exist, numbering schemes,
  pregap authoring. That is the second brief. This delivery reads, exports and writes structure
  that already exists.
- **Automatic boundary detection** (silence analysis). A separate problem with no foundation
  here and no dependency on this work.
- The scattered source-format lists noted below.

## Notes

- **MKV chapter writing is unverified.** MP4 chapter read was tested directly; MKV was not
  tested at all. Treat Matroska support as conditional on it being genuinely cheap, and say so
  rather than promising it.
- **The audiobook case is lossy and low-rate** — 22.05 kHz, 63 kbps AAC. Re-encoding it to a
  lossless target is a legitimate user choice but an expensive one; nothing here should assume
  the target is lossless.
- `.m4b` is the third format-list mismatch found in two days, after an AAC rate cap and a
  CLI/TUI format disagreement. Source-format knowledge is spread across
  `src/convert/classify.rs` (which already maps `m4b` to AAC), `is_single_audio_extension`,
  `src/tui/probe.rs` and the CLI's own list, with no single authority. Consolidating that is out
  of scope here, but the delivery should avoid making it worse.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the audio tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- `tonepoet-pipeline` is a pure planner: no `std::process`, no `tokio::process`, no I/O, no
  interactive behaviour.
- `crates/tonepoet-true-peak` must not be touched and must keep its empty `[dependencies]`.
- Plain letters in Browse remain reserved for type-ahead. No F-keys. No emoji or decorative
  unicode in UI text.
- Tests that mutate process-global state have caused repeated flakes in this project.
- Two tests are known low-rate flakes and are not this work's responsibility:
  `cancel_abandons_a_wedged_helper_without_waiting_for_it` (#20) and
  `empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved` (#31).
