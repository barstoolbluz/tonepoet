# tonepoet — Sidecar-CUE metadata for untaggable-carrier CONVERSION (2026-08-07)

You are starting **fresh**; everything you need is in this bundle. Outcomes + guardrails;
diagnosis is evidence, not prescription — you choose HOW.

**Project:** tonepoet (ratatui TUI + clap CLI, tokio, edition 2021), version 0.4.6 — do not
bump. Gate `cargo test --workspace --no-fail-fast` green; must stay green.

## The problem

Converting a folder of **untaggable carriers + a one-to-one sidecar `.cue`** discards the
cue's metadata entirely. Real case: `~/torrents/Michael Jackson - Thriller. 1984 Japan/` —
nine `.dff` carriers + `Michael Jackson - Thriller.cue` (per-track TITLE, PERFORMER, ISRC;
album CATALOG). Output (observed in `~/temp/external/temp/`):
`Unknown Artist - 01 - Wanna Be Startin' Somethin' () [FLAC]`, track title `Track 04`, empty
year, empty output FLAC tags — and the cue is copied alongside as a companion but never
consulted. The user's point: the ARTIST/TITLE **are** in the cue; nothing propagates.

This is the CONVERSION pipeline (`src/convert/`), a different subsystem from the metadata
EDITOR (where we just fixed the analogous untaggable-carrier + sidecar-cue authority). The
two should now be consistent: a valid sidecar cue is the metadata source for carriers that
cannot hold their own tags.

## Diagnosis (mapped; verify at will)

The failure is not "the cue is unknown" — it is **recognized and then suppressed**:

1. Folder scan → each `.dff` becomes an independent `SourceKind::SingleFile` request
   ("folder album batch"); queue_expansion.rs (~1997-2040) / stages.rs (~628-722).
2. The cue is classified `CueQueueDecision::MetadataArtifact` (one-to-one with the audio,
   or single-image) — queue_expansion.rs `cue_queue_decision_for_path` (~2162-2192) — which
   SUPPRESSES the cue from queueing AND, at src/convert/mod.rs (~777-780), stamps each audio
   file with `CueSidecarPolicy::EmbeddedOnly` — i.e. "ignore sidecar cues". The cue's
   metadata is never transferred to the audio requests before dispatch.
3. Per-track metadata read: `SingleFileMaterializer` →
   `read_track_metadata_with_warnings` (materializer_single.rs ~331-390): DSF gets native
   ID3, else lofty; **.dff fails lofty** → returns `TrackMetadata::default()` +
   "Tag read: FAILED (...) - converted without metadata". **No sibling-cue fallback.**
4. Planning metadata: `dispatch_track_metadata_for_output_planning` (processor.rs) —
   `SingleFile` branch (~756-758) probes only the file; the `CueImage` branch (~759-788)
   ALREADY extracts album/artist/date/title from the cue (`dispatch_metadata_sheet_for_
   sidecar_cue` → `parse_cue_file`). The template + machinery to read cue metadata for
   conversion EXISTS — it is simply not wired for the SingleFile-carrier shape.
5. Naming/tags fall back to defaults: `Unknown Artist`
   (src/convert/renaming.rs ~106/~266), `Track {nn}` (stages.rs ~3725/~19152), empty year;
   output tags come from the empty `TrackMetadata` (ffmpeg `-map_metadata` from the
   untaggable source carries nothing). Planning entry: `dispatch_track_metadata_for_output_
   planning` (processor.rs:748; SingleFile vs CueImage branches within).

So: cue detected → cue suppressed → per-track read fails → defaults. The fix belongs at the
transfer point (give the audio requests the cue's per-track + album metadata), reusing the
existing cue-metadata extraction rather than inventing a second one.

## Governing spec
`docs/metadata_source_selection_heuristic.md` (LODESTAR, bundled). This is source-selection
territory and has regressed repeatedly — respect it. The principle the editor now enforces:
a valid cue is authoritative for carriers that cannot hold embedded tags; for taggable
carriers a cue is a *preference among viable sources*, not a silent override.

## Outcomes

**Q1 — Cue metadata reaches conversion for untaggable one-to-one albums.** Converting a
folder of untaggable carriers (DFF/SHN/DTS/AC3 — define by the untaggable classification,
not an extension list) with a valid one-to-one sidecar cue populates each track's
conversion metadata from the cue: per-track TITLE and ARTIST (cue PERFORMER) and ISRC;
album-level ALBUM (cue TITLE header), ALBUMARTIST (cue header PERFORMER), DATE (REM DATE),
GENRE (REM GENRE), CATALOGNUMBER (CATALOG) where the cue provides them. Naming templates
(`%ARTIST%`, `%TITLE%`, `%ALBUM%`, `%YEAR%`, `%TRACKNN%`) and the output file tags reflect
these values. The Thriller folder must produce per-track artist "Michael Jackson", real
track titles, and the catalog/ISRC where present — never `Unknown Artist` / `Track NN` when
the cue supplies the field. Where the cue omits a field (e.g. a headerless cue with no
album TITLE), that field stays empty — do not invent it.

**Q2 — Reuse the existing cue-metadata path.** The `CueImage` planning branch already reads
cue metadata for conversion; extend/generalize that mechanism to the untaggable one-to-one
SingleFile shape rather than adding a parallel reader. One cue-metadata-for-conversion
source of truth. The `parse_cue_file` / sheet-to-metadata mapping is bundled — reuse it.

**Q3 — Suppression becomes transfer.** The point that stamps `CueSidecarPolicy::EmbeddedOnly`
on metadata-artifact audio (mod.rs ~777-780) must not leave the audio metadata-less: either
carry the cue's per-track metadata onto the requests at classification/dispatch time, or
give the SingleFile materialization/planning path a sibling-cue fallback keyed to the
already-detected metadata-artifact cue. Your choice; state it and why. The cue must map to
tracks by the same FILE/track correspondence the classifier already established (do not
re-guess the mapping).

**Q4 — Authority is lodestar-correct.** For untaggable carriers the cue is authoritative
(the carrier holds nothing). For TAGGABLE carriers that happen to have a one-to-one cue, do
NOT silently override the file's own tags — follow the existing source-selection
preference (fill only what the file lacks, or honor the configured cue policy). If unsure
where the taggable line sits, keep taggable behavior byte-identical to today and scope the
change to untaggable carriers; say so.

**Q5 — Honest logging.** The conversion log must stop implying "converted without metadata"
when the cue in fact supplied it; it should record that per-track metadata came from the
sidecar cue (source attribution), consistent with how other metadata sources are logged.

## Guardrails
- Do NOT disturb: split-source cue decomposition (single big image + cue → per-track
  splitting), single-image FLAC+cue conversion, taggable single-file conversion where file
  tags are authoritative, or the companion-cue copy-to-output behavior.
- Lodestar-governed; full-gate ×2 posture. Do not change queue classification semantics
  (split-source vs metadata-artifact) — only what happens to the metadata-artifact cue's
  DATA.
- Writeback/tag-application stays on the existing authoritative metadata writer; no second
  tag writer. No new dependencies. Version 0.4.6.
- Consistency check (not a rewrite): the values a track gets in CONVERSION from a given cue
  should match what the metadata EDITOR shows for the same album (same cue → same
  per-track ARTIST/TITLE), since both now treat the cue as the untaggable carrier's source.
- Tests, minimum (drive the REAL conversion path — planning + materialization + naming +
  output tags, asserting on produced folder/file names AND written FLAC tags): (a) nine-file
  untaggable dff + one-to-one cue (the ground-truth shape) → per-track artist/title/ISRC and
  album catalog land; (b) a non-DFF untaggable variant (SHN/DTS); (c) a headerless cue
  (per-track fields present, album TITLE absent) → per-track fields land, ALBUM stays empty,
  no "Unknown"; (d) regression: taggable single-file folder (no cue, or cue + real file
  tags) behaves exactly as today; (e) regression: split-source image+cue decomposition
  unchanged. Existing "converted without metadata" expectations that assumed the old
  behavior must be updated to the cue-sourced reality.

## Deliverables
Complete replacement files or unambiguous patches; a WHY summary (transfer-vs-fallback
choice, where authority is decided, the taggable line); test list; honest
unverifiable-in-your-environment note (no real .dff fixtures unless you synthesize headers —
cue parsing and metadata mapping are testable without real DSD audio).

## Bundle manifest
- This brief; docs/metadata_source_selection_heuristic.md (LODESTAR).
- Complete `src/` tree (conversion pipeline: convert/queue_expansion.rs, convert/mod.rs,
  convert/processor.rs, convert/pipeline/{stages,materializer_single,materializer_cue,
  track_executor,types}.rs, convert/cue_parser.rs, plus naming/template code; and the
  editor's cue-authority code for the consistency reference).
- `crates/tui-file-picker/`, `crates/tonepoet-backend/`, `crates/tonepoet-features/`
  (cue generation / metadata I/O). Root `Cargo.toml`, `CLAUDE.md`.
NOT included: other workspace crates, target/, other docs. If anything is missing, say so
rather than guessing.
