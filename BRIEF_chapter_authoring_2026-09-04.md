# BRIEF — Chapter authoring: creating and changing division points

**Date:** 2026-09-04
**Base:** `feat/chapters-as-structure` @ `146f42f`
**Sequence:** second of two. The first delivery (`146f42f`) made chapter structure *readable*,
splittable and writable. This one lets a user *create and change* it.

**Accompanying material in this bundle:**

- `SPEC_chapter_authoring_ui_moving_parts_2026-09-04.md` — the domain, and every moving part the
  interface has to surface.
- `tonepoet_chapter_editor_grid_plain_buttons.html`
- `tonepoet_chapter_save_dialog_plain_buttons.html`

**The two HTML files are the operator's own design direction**, not this brief's invention.
Treat them as intent about the shape of the thing. Where they and this document disagree about
appearance, they win.

## What the user wants

Three things, described in their words:

> A custom chapter-authoring workflow for long-form audio and other chapter-capable media …
> The core feature is a general-purpose chapter authoring and re-authoring interface. CUE
> creation is one representation of that structure; embedded chapter metadata in chapter-capable
> containers is another.

Users should be able to invoke it when no chapter structure exists; when an existing CUE or
embedded chapter map is broken or unusable; when Tonepoet cannot repair the existing structure
satisfactorily; when the structure is technically valid but produces the wrong chapters; or when
they simply want to discard it and build a new map from scratch.

They should be able to define structure by specifying boundaries manually, by specifying pregaps
or interstitial audio, by splitting into fixed-duration chapters, by dividing into uniformly
sized chunks, and — later, not in this round — by having Tonepoet determine boundaries
automatically.

For generated chapters they should be able to give a base title and a numbering format. After
generation, the resulting structure should open in the existing metadata editing interface so
individual titles can be adjusted or bulk-pasted.

## What already exists

Reckoning with this is the most important constraint in the brief, because nearly every piece is
already present and the failure mode is building a second copy of one.

**Boundaries are already modelled, carried and rendered.** `CueAlbumTrackSource` (`src/tui/app.rs`)
holds `index00_frames` and `index01_frames` — pregap and start, in CUE frames — beside track
number, file reference and ISRC. It already feeds the metadata editor's Album view, which
already presents one audio image as N logical tracks.

**CUE authoring already exists.** `src/tui/cue_generate.rs` (1,732 lines) provides
`generate_single_image_cue`, `generate_multifile_cue`, `regenerate_cue_with_overrides`,
`validate_cue_content`, `frames_to_cue_timestamp`, and MusicBrainz fill. It emits `INDEX 00`
from `index00_frames` and documents preserving both indices so timestamps do not drift through
regeneration.

**The editor can already write structure back, and create it where none exists.**
`cue_sidecar_writeback_plan_for_state` writes an edited album to a sidecar CUE, and
`pending_sidecar_cue_creation` (set by `stage_cueless_untaggable_album_surface`) covers the case
where no sidecar exists yet.

**Numbering already exists.** `NumberingScheme` (`src/tui/metadata_autonumber.rs`) implements
`N`, `NN`, `N/NN` and `NN/NN`, with width derived from the total. Its module documentation
states that context menus, command mode and the preview overlay all dispatch through the same
functions "so their behavior cannot drift". Note the request said "n of nn"; the existing
formatter renders a slash. Worth resolving rather than assuming.

**Bulk title paste already exists** — clipboard lines mapped onto positional slots.

**Delivery one added the chapter side.** `src/convert/chapter_structure.rs` provides
`RawEmbeddedChapter`, `ProgramTrackBoundary` (`start_sample`, `samples`, `is_program_tail`) and
`EmbeddedChapterTrack` (`ordinal`, `title`, `boundary`), plus `read_embedded_chapters` and
`normalize_embedded_chapters`. `src/convert/pipeline/chapter_write.rs` writes chapters back to
MP4-family containers and verifies by reading them back.

**Sample-accurate cutting already exists**, and is boundary-source agnostic:
`src/convert/pipeline/materializer_cue.rs` builds `atrim=start_sample={start}:end_sample={end}`.

## What does not exist

Verified by enumerating every production write of `index00_frames` / `index01_frames` in the
tree. All of them parse a value from a file, copy it between models, or synthesise a literal
zero. **Not one is a user changing a boundary.** Specifically:

- **No boundary is editable.** The fields exist; nothing writes a user's intent into them.
- **No way to create N boundaries within one file.** `stage_cueless_untaggable_album_surface`
  seeds one track *per file*, each at `index01_frames: Some(0)`.
- **No add or remove of a row.** The editor's only "add" is `add_key_input`, which adds a
  metadata *field*. The single `add_track` in the tree is in `src/tui/preemphasis/corpus.rs` and is
  unrelated.
- **The editor and chapters are unconnected.** `chapter_structure` is referenced nowhere in
  `src/tui/`.

## Rules the interface must respect

`normalize_embedded_chapters` rejects — as hard errors that fail the whole conversion —
non-monotonic starts, zero-length entries, gaps or overlaps greater than one sample, leading
audio outside the structure, and non-positive durations.

These are facts about the pipeline, not preferences. A structure that violates them cannot be
converted, so a user must learn about a violation while editing rather than at conversion time.

A boundary can legitimately be invalid mid-edit — while moving one point past another, for
instance. How that is surfaced, and whether saving is blocked or the structure is adjusted, is a
design decision.

## Where structure can be stored

Availability is format-dependent in a way that is easy to get wrong:

- **sidecar CUE** — available for anything;
- **inside the audio file** — MP4-family containers hold real chapter entries; **FLAC cannot
  hold chapter entries but can carry an entire CUE sheet as an embedded tag**, which Tonepoet
  already reads (`src/tui/app.rs`, "Read an embedded CUESHEET tag from a FLAC file") and
  writes — `src/config.rs` exposes `AggregateMetadataTarget::EmbeddedCue`, "Rewrite a CUESHEET
  tag embedded in an audio image", as a user-facing metadata destination; WAV has no
  embedded-structure support here;
- **split output** — one file per chapter at conversion time.

## Also in this round

Two items that are not chapter authoring but belong with it.

### A chapterless `.m4b` is refused, and should not be

`processor.rs:3780` fails outright when an `.m4b` carries no embedded chapters. Verified against
a purpose-built 8-second chapterless `.m4b`: it is rejected with

> "M4B source '…' contains no embedded chapters. Tonepoet admits .m4b as a structured audiobook
> source and will not silently convert it as one undifferentiated track."

Such a file is an ordinary AAC stream in an MP4 container and should convert as a single track.

**This traces to an invented requirement in the previous brief.** That brief said "a source that
genuinely has no chapters is refused clearly, early". The user never asked for it; it came from
observing a bad refusal *message* and wrongly generalising it into a refusal *policy*. The same
brief contradicted itself, correctly scoping the guarantee elsewhere to files that *carry*
chapters. The delivery implemented the stricter line, faithfully.

The adjacent failure at `processor.rs:3794`, when chapter inspection errors, is the previous
delivery's own caution rather than an invented requirement — it refuses because proceeding
"could silently discard chapters". Whether that should also soften is a genuine question, not a
correction. Note it interacts with the strict validation above: a file whose chapter map is
slightly malformed currently has no path forward at all, which is precisely one of the
situations this feature is meant to rescue.

### `tests/chunk2_orchestrator_contract.rs` asserts on source text

That file makes **57 `contains()` assertions across 25 `include_str!` reads of 10 source files**.
It is the only file in `tests/` written this way. The operator has asked for it to be replaced
with tests of behaviour.

Two documented failures motivate this:

- During the previous delivery, a test required `build_single_file_work` to appear within 500
  characters of `Some(SourceKind::SingleFile)`. Legitimate chapter-detection code pushed the
  distance to 2,195 and the test failed, though the contract it protects was untouched. It was
  repaired to an ordering assertion as an interim measure.
- Earlier in this project a source-text test passed while the behaviour it named was broken,
  recorded at the time as: a test asserting on source text is not a test of behaviour.

Convert the file, not one test — one behavioural test beside ten textual ones leaves it
inconsistent. Where a contract genuinely cannot be expressed behaviourally, say so rather than
leaving a text assertion unexplained.

## Outcomes wanted

- A user can create division points where none exist, change existing ones, and remove them.
- Structure can be generated by fixed duration or by uniform count, titled from a base and a
  numbering format, and then corrected by hand.
- Pregaps can be authored, not merely preserved.
- A file whose existing structure is invalid can be repaired through this interface rather than
  being a dead end.
- Authored structure can be saved to whichever destinations the file supports, and the interface
  does not offer one that would silently do nothing.
- A chapterless `.m4b` converts as a single track.
- `tests/chunk2_orchestrator_contract.rs` tests behaviour.

## Scope

**In scope:** authoring and re-authoring division points; generation by duration or count;
titling and numbering; saving to the available destinations; the `.m4b` refusal; the contract
tests.

**Out of scope:**

- **Automatic boundary detection** by silence analysis. A different kind of problem, with no
  foundation in the tree, and separable.
- **The Convert-screen control** for sidecar-versus-embedded CUE output. The operator has
  deferred it; the previous delivery deliberately left room for it by not reinterpreting the
  legacy `generate_cue` flag.
- Consolidating the scattered source-format lists.

## Working constraints

- The implementation container has no Rust toolchain, no Nix, and none of the audio tools.
  Running the gate is the operator's job; no delivery should assume it has been run.
- **Do not add a parallel representation of a division point.** `CueAlbumTrackSource`,
  `EmbeddedChapterTrack` and `src/tui/cue_generate.rs` already exist. Subsume or bridge them by
  a stated rule, and say which was chosen. Note the tree already demonstrates the pattern:
  `src/tui/cue_parser.rs` opens `pub use crate::convert::cue_parser::*;` and adds only layout on
  top, with no parser of its own.
- **Colours come from the theme**, and dim when unfocused, as every other surface does. Tonepoet
  has a user-facing theme builder; a hardcoded colour is correct only in the default palette.
- `tonepoet-pipeline` is a pure planner: no `std::process`, no `tokio::process`, no I/O, no
  interactive behaviour.
- `crates/tonepoet-true-peak` must not be touched.
- No emoji or decorative unicode in user-visible text. No F-keys. Plain letters in Browse remain
  reserved for type-ahead.
- Tests that mutate process-global state have caused repeated flakes in this project.
- Two tests are known low-rate flakes and are not this work's responsibility:
  `cancel_abandons_a_wedged_helper_without_waiting_for_it` (#20) and
  `empty_dead_queue_scope_is_reclaimed_but_live_empty_scope_is_preserved` (#31).

## A note on this brief

The previous brief in this sequence contained a requirement the operator never asked for, and it
shipped as behaviour that blocks a legitimate conversion. This document tries to describe the
situation — what exists, what the rules are, what the user asked for — and to leave the design
to you. Where something here reads as a decision that should have been the implementer's, treat
it as an error in the brief rather than a constraint to satisfy, and say so.
