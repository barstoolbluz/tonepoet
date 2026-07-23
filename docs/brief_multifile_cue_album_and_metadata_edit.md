# Brief: native multi-FILE cue albums (front-end) + cue-album metadata-edit fix

Date: 2026-07-23. For a fresh reasoning-model session. Baseline: branch
`working` at (see handoff — post-0.4.2), full workspace suite green
(`cargo test --workspace --no-fail-fast`, ~4740 tests, 0 failed). The sandbox
cannot compile; the applier (Claude Code) compiles, runs the full gate, and
validates on the real trees below. **Complete-file delivery contract**: return
every file you change in full, plus the manifests in the bundle.

---

## 0. READ THIS FIRST — scope discipline (hard, non-negotiable)

A previous round on the metadata write path returned ~15,000 lines of
unrequested transaction-authority rewrites that were **thrown away** — a wasted
multi-day cycle. That must not recur. This brief is deliberately narrow.

**The mindset for this round:**

- You are making a **tightly-scoped front-end recognition change** plus **one
  metadata-precedence fix**. You are **not** redesigning the cue/album
  architecture, the conversion engine, or the metadata write path.
- **Minimal diff is a hard success criterion.** A large or sprawling diff will
  be **rejected on sight**, regardless of correctness. Prefer reusing existing
  machinery over writing new machinery.
- **Do NOT "fix", refactor, "harden", or "improve" anything outside the
  explicit deliverables below** — even if some existing code looks problematic,
  smells bad, is inconsistent, or "could fail" in a situation you imagine. If
  you notice such a thing, write one line about it in an **"Observations (not
  acted on)"** section of your report and **move on**. Do not touch it. Do not
  "while I'm here" it. The instinct to fix adjacent code is exactly what
  produced the rejected round.
- **VERIFY BEFORE YOU BUILD.** Much of the infrastructure you need already
  exists (see §3). Your FIRST job for Deliverables 1–2 is to determine what
  *actually* fails today versus what already works, then fill **only** the real
  gaps. Do not reimplement working code.
- If you conclude a deliverable genuinely requires a change the guardrails
  forbid (e.g. a `src/convert/pipeline/` change), **STOP and document the
  reasoning in your report** instead of making the change. The applier will
  decide. A blocked deliverable with a clear explanation is a good outcome; an
  out-of-scope rewrite is not.

**Explicitly OUT OF SCOPE this round (do not touch):**

1. `src/convert/pipeline/**` — the conversion engine. `materializer_cue.rs`
   **already** materializes native multi-FILE cues correctly as one album (it is
   REFERENCE-ONLY here; see §3.1). Do not modify it. If Deliverable 3 seems to
   require an engine change, see the note there — it does not.
2. `sanitize_component` / the folder-name whitespace/slash sanitizer
   (`src/convert/pipeline/stages.rs`). A separate QoL fix already landed on
   `working`. Leave it entirely alone.
3. **Duplicate format-copy dedup** (a folder holding the same album as both a
   FLAC copy and a WavPack copy, each with its own cue — the "Foxy" two-cue
   enqueue). This is deferred to its own round. Do not attempt to detect or
   dedup it. (Future direction, for context only: tonepoet will *flag* the
   situation and let the user pick which copy to convert. You are not building
   that now — just do not make it harder to build later.)
4. Cue-less multi-file folders (N audio sides with **no** cue sheet). Every
   case this round has a cue. Requiring a cue is correct; do not invent a
   heuristic for the cue-less case.
5. Any config setting or UI for the future embedded-vs-sidecar preference (see
   Deliverable 4 — you build the *seam*, not the setting).
6. `db.rs`, crash-recovery journals, the metadata transaction machinery.

---

## 1. The real-world evidence (user-reproduced)

All three trees contain a single sidecar cue plus audio; the failures are in the
**metadata editor** and **`:tags-mb`**, not conversion.

**CASE A — Uriah Heep (1 cue → 2 files, multiple tracks per file):**
`~/torrents/Uriah Heep - Fallen Angel (…)/` contains `Side 1.flac`,
`Side 2.flac`, and `Uriah Heep - Fallen Angel.cue`. The one cue has multiple
tracks split across two `FILE` references (side 1 tracks under `Side 1.flac`,
side 2 tracks under `Side 2.flac`). User report: "tonepoet doesn't know what to
do with this. Metadata overlay separately picks up both `*.flac` files. Tagging
with MB tries to send the combined TOC from both `*.flac` files to MB and gets
nonsense matches."

**CASE B — 80's Movie Hits (1 cue → 4 files):**
`~/livetorrents/80's Movie Hits Collected (2022)/` contains
`80's Movie Hits Collected.cue` + `…_SideA.flac` / `_SideB` / `_SideC` /
`_SideD.flac` (a double-LP, four sides), multiple tracks per side. Same shape as
Case A with 4 parts. "This is a pattern we need to be able to handle."

**CASE C — Foxy (metadata edit not applied):**
`~/torrents/Foxy/1978 - Get Off (…)/` contains a FLAC image with an embedded
cuesheet plus a sidecar `.cue`. User opened the metadata overlay, set
`ALBUM = "Get Off (Japan CBS-Sony 25AP 1115 Promo LP / 32-192)"`, pressed
ALT+O, and the ALBUM change **did not apply** to the converted output. (The
folder also contains a second WavPack copy with its own `WV.cue`; the
two-copy dedup is OUT OF SCOPE per §0.3 — you are fixing only the lost-edit bug.)

The user's proposed heuristic (Cases A/B), which is correct and is the model to
implement: **a single sidecar cue that references N ≥ 2 audio files with
multiple tracks per file is ONE album** (concatenated sides, continuous track
numbering).

---

## 2. What already works (verified) — do not rebuild

- **Conversion.** A single multi-FILE sidecar cue is admitted as ONE queue item
  (`cue_queue_decision_for_path` → `admit_split_cue_member().contributes_synthetic_album_part()` is
  true because ≥1 file owns ≥2 tracks → `CueQueueDecision::SplitSource`,
  `src/convert/queue_expansion.rs:1744`). `materializer_cue.rs` then materializes
  it as one album: per-track boundaries are resolved per-FILE with INDEX times
  local to each file (`compute_track_boundaries_for_layout`), track numbers are
  continuous 1..N, `disc_number = None`. Tests already cover this
  (`synthetic_multifile_cue_materializes_as_one_prepared_album_source`,
  `multifile_pregap_file_switch_materializes_index01_from_new_file` in
  `materializer_cue.rs`). **Conversion of Cases A/B via the cue already produces
  one correct album. Do not change it.**
- **The metadata "surface" struct already supports multi-FILE cues.**
  `MetadataCueSurface` (`src/tui/keybindings.rs` ~10330) carries
  `audio_paths: Vec<PathBuf>` (distinct member images) and
  `track_audio_paths: Vec<PathBuf>` ("Track-position-aligned image ownership;
  supports multi-FILE CUEs"). `metadata_cue_surface_from_admitted_member`
  populates both. A multi-FILE cue *is* admitted as a surface
  (`collect_metadata_cue_admission`, keybindings.rs ~10407, filters on
  `contributes_synthetic_album_part`).
- **Split-cue album grouping + unified surface** exists from prior rounds
  (N single-file cues → one merged album view + concatenated synthetic sheet;
  see `docs/unified_synthetic_cue_album_brief.md`). A native multi-FILE cue is
  essentially a *pre-merged* group — reuse this machinery; do not fork it.
- **CueSidecarPolicy** already exists (`materializer_cue.rs` `resolve_cue_input`,
  default `PreferSidecar`) — the seam Deliverable 4 builds on.

---

## 3. Deliverables

### Deliverable 1 — a native multi-FILE cue opens as ONE album in the metadata editor

**Outcome:** opening Case A or Case B (select the folder, or the cue, or an
image) opens the metadata editor as ONE album: a single continuous track list
(all N tracks, numbered 1..N, one album header), each row internally mapped to
its owning image via the existing `track_audio_paths`. No per-file rows, no
"only side 1 visible", no tabs.

**Verify first, then fill the gap.** The surface struct and admission already
support multi-FILE (§2). The likely real gaps, to confirm and fix minimally:

- **Editor entry routing.** When the user opens the *folder* (not the `.cue`
  file), `open_metadata_editor_impl` (`src/tui/keybindings.rs` ~15786) currently
  routes non-cue-selection input through `expand_audio_paths_for_metadata`
  (`src/tui/command.rs` ~93 → `expand_paths_to_all_audio`), which returns each
  `.flac` as a separate file → the per-track surfacing at keybindings.rs ~16071
  only fires for `paths.len() == 1`. Ensure a folder (or image) whose contents
  resolve to a single multi-FILE cue surface is routed to the **surface** path,
  the same way a folder of split single-file cues is. Look at the region
  ~15900–16080 ("Multi-part single-image CUE albums must stay CUE-shaped") and
  the CUE-selection branch; extend the routing to recognize the
  single-multi-FILE-cue case. Reuse `collect_metadata_cue_surfaces` /
  `collect_metadata_cue_admission`.
- **Per-track rendering for a multi-FILE surface.** Confirm the unified/per-track
  editor renders a multi-FILE surface's tracks using `track_audio_paths` for
  row→image mapping. If the rendering assumes a single image
  (`apply_embedded_cuesheet_per_track`, `inject_sidecar_cuesheet_if_present` are
  gated on `paths.len() == 1`, keybindings.rs ~16071), make the minimal change so
  a multi-FILE surface uses the cue's own track list. Reuse the existing unified
  surface path if it already handles this for merged groups.

Do **not** add a new editor mode or widget. This is about routing the existing
data (which already models multi-FILE) into the existing surface editor.

### Deliverable 2 — MB/GNUDB TOC for a multi-FILE cue (fixes the "nonsense matches")

**Root cause:** `detect_single_image_cue` / `single_image_info_for_cue`
(`src/tui/cue_parser.rs` ~150–206) returns `None` when the cue references
multiple FILEs (the `all_same_file` guard, ~162–169). So `:tags-mb` and the
in-editor `:tags-mb` fall through to a **naive whole-file concatenation** of
each file's full duration (`src/tui/command.rs` ~5012–5031, and the editor path
~12129–12141, via `accuraterip::collect_sample_counts`), producing a TOC that
matches no real release.

**Outcome:** for a multi-FILE cue, build the TOC from the cue's **per-track**
INDEX boundaries across all its FILEs (INDEX times are local to each file; a
track's sample count runs to the next track *in the same file*, or that file's
end for the last track in it), then hand the existing lookup a per-track
concatenated CD-sector TOC — exactly as `compute_track_boundaries_for_layout`
(materializer, REFERENCE) computes it, and exactly as
`concat_single_image_cue_infos_to_cd_sectors` (`src/tui/command.rs` ~1112)
concatenates per-track boundaries for the N-single-file-cue case. The per-track
image path is the track's owning file (`track_audio_paths`).

**Suggested minimal shape** (you own the exact code): add a multi-FILE-aware
boundary computation — either generalize `single_image_info_for_cue` to a
`multi_file_info_for_cue` sibling that returns `(start_sample, sample_count)`
per track using each track's own file's probe, or compute boundaries directly in
the surface→TOC bridge (`metadata_cue_surfaces_to_single_image_infos`,
keybindings.rs ~10453, which today calls the single-image detector per surface).
Route a single multi-FILE surface through this new path; keep the existing
single-image and N-cue paths byte-for-byte unchanged. **Never emit the naive
whole-file concatenation for a multi-FILE cue** — if per-track boundaries can't
be computed, fall back to the album text search (as the split-cue path does),
never to a bare TOC miss.

Same treatment for the GNUDB dispatch where it mirrors the MB TOC path.

### Deliverable 3 — a saved album-level metadata edit reaches conversion for a cue album (Foxy)

**Root cause (verified):** the editor saves an ALBUM edit to (a) the flat
Vorbis `ALBUM` tag and (b) a regenerated embedded CUESHEET
(`regenerate_cuesheet_for_save`, keybindings.rs ~10064). But at conversion,
`CueSidecarPolicy::PreferSidecar` (the default) reads the **sidecar** cue
(`materializer_cue.rs` `read_sidecar_cue` ~364), and
`cue_album_metadata` takes `sheet.title.or_else(|| image.album)`
(`materializer_cue.rs` ~2986) — **the cue's title always wins over the flat
ALBUM tag.** `try_upgrade_sidecar_to_embedded_image_cue` (~439–512) only lets the
freshly-regenerated embedded sheet win if it structurally matches the sidecar
(track count + every INDEX 01). If sidecar write-back was skipped and the
embedded upgrade doesn't match, conversion uses the **stale sidecar album** and
the edit is lost.

**Outcome:** an album-level edit (ALBUM, and by the same mechanism
ARTIST/DATE/GENRE/CATALOG) saved from the metadata editor for a cue-image album
**must** be reflected in the converted output.

**This does NOT require a `src/convert/pipeline/` change.** The engine already
prefers the sidecar and already honors the embedded sheet when it matches. The
correct, in-scope fix is on the **TUI/save side**: ensure the edit is persisted
to the source conversion actually reads. The existing sidecar write-back
machinery (`docs/cue_sidecar_writeback_policy.md`; the cue's top-level `TITLE`
line *is* the album title, which is within write-back scope) should fire when
album-level fields change. Investigate the write-back gate
`metadata_editor_audio_tag_changes_required_for_sidecar_writeback`
(keybindings.rs ~6861) — an album-only edit likely does not currently trip it,
so the sidecar keeps its stale `TITLE`. Make the minimal change so an
album-level edit triggers sidecar `TITLE`/`REM` write-back (and confirm the
already-regenerated embedded sheet path stays consistent). If you find the only
correct fix genuinely lives in the engine's precedence, STOP and document it per
§0 — do not change the engine.

Pin with a regression test reproducing the Foxy shape: a FLAC image with an
embedded cuesheet + a sidecar `.cue`, edit ALBUM in the editor, save, then assert
the value conversion would read (sidecar and/or embedded, per the write path) is
the edited album — not the stale one.

### Deliverable 4 — forward-compat seam for the future embedded-vs-sidecar config

Do **not** add a config setting or UI. Just ensure Deliverables 1–3 route
cue-source selection through the existing `CueSidecarPolicy` seam
(`materializer_cue.rs` `resolve_cue_input`) rather than hardcoding
"sidecar always" or "embedded always" assumptions, so a later round can add a
folder-level `[cue] prefer = "embedded" | "sidecar"` config that flips the
default. In your report, note any place your implementation assumes a fixed
precedence that the future config would need to override.

---

## 4. Robustness requirements

1. **N parts, not two.** Everything works for 2..N FILEs in one cue (test with
   3 and 4). Differing track counts per file. Cue TRACK numbers are 2-digit;
   a merged track count > 99 must fail closed with a clear status message, never
   an invalid TOC or panic.
2. **INDEX times are LOCAL to each file.** A track's INDEX is relative to its
   own image; do not accumulate absolute times across files (that is the
   materializer's proven model — mirror it).
3. **Hostile input tolerance.** Member FILEs may differ in codec (the WavPack vs
   FLAC case), and cues vary in CRLF/BOM/casing/REM/quoting/INDEX 00 pregaps.
   None may break TOC building or surfacing; degrade to album text search, never
   crash, never `unwrap`/`expect` on user-file-derived data.
4. **No naive fallback for multi-FILE cues.** If per-track boundaries are
   unavailable, use the album text-search fallback (existing
   `dispatch_split_cue_musicbrainz_text_fallback*` path), never the whole-file
   concatenation and never a bare "no release matched".
5. **Byte-stable / no-op-safe.** Existing single-image and N-single-file-cue
   paths must remain byte-for-byte unchanged (pin with the existing tests).
6. **No panics, status-line errors only.** Production paths use the status-line
   error conventions.

---

## 5. Tests (prove the behavior, not the happy path)

- Multi-FILE cue TOC: a 2-file and a 4-file cue fixture → assert the per-track
  concatenated sector TOC equals the hand-computed expectation (per-file-local
  INDEX), and that `build_mb_toc` accepts it. Assert a WavPack+FLAC-mixed or
  differing-per-file-track-count fixture still produces a valid TOC.
- Metadata surface: a multi-FILE cue fixture → `collect_metadata_cue_surfaces`
  yields ONE surface whose track list is the concatenation (rows 1..N) with
  correct row→image mapping via `track_audio_paths`.
- Editor entry routing: opening the folder (not the cue) resolves to the single
  multi-FILE surface (one album), not per-file rows. Pin the routing.
- Foxy: embedded-cue + sidecar fixture, edit ALBUM, save → the source conversion
  reads carries the edited album. (Fixtures follow the suite's patterns — real
  ffmpeg-encoded images where the exercised path probes; placeholder bytes
  where it does not. Cue write-back tests can use byte fixtures.)
- Regression pins: existing single-image `:tags-mb`, N-single-file-cue concat
  TOC, and materializer multi-FILE tests all still pass unchanged.

---

## 6. Constraints & gate

- Full workspace suite (`cargo test --workspace --no-fail-fast`) stays green,
  0 failed, no regressions; zero new cold-build warnings.
- Tag writes go through the existing lofty / native-FLAC / sidecar-write-back
  machinery only. No new tag backends.
- MB etiquette unchanged: `mb_acquire` rate limiting, cache-first; tests use
  pre-fetched cache bodies, never live network.
- TUI conventions: two-pass draw + `ButtonRenderMap`; coeval mouse/keyboard.
- The conversion-actions UI gate and its tests stay untouched.
- Report your seams (§0): a final section enumerating every behavioral decision
  where this brief left freedom, every precedence rule as implemented, every
  limitation knowingly accepted, and the "Observations (not acted on)" list.

---

## 7. Files in this bundle

Complete manifests included (Cargo.toml, Cargo.lock, src/lib.rs, CLAUDE.md).
Core (edit as needed within scope): `src/tui/command.rs`,
`src/tui/keybindings.rs`, `src/tui/cue_parser.rs`, `src/tui/musicbrainz.rs`,
`src/tui/gnudb.rs`, `src/tui/accuraterip.rs`, `src/tui/event_loop.rs`,
`src/tui/app.rs`, `src/tui/message.rs`, `src/tui/probe.rs`,
`src/tui/context_menu.rs`, `src/tui/draw_overlays.rs`, `src/tui/help.rs`,
`src/tui/external_editor.rs`, `src/tui/mod.rs`; `src/convert/classify.rs`,
`src/convert/queue_expansion.rs`, `src/convert/split_cue_album.rs`,
`src/convert/source_admission.rs`, `src/convert/cue_parser.rs`,
`src/convert/mod.rs`. **REFERENCE-ONLY (do not modify):**
`src/convert/pipeline/mod.rs`, `src/convert/pipeline/materializer_cue.rs`,
`src/convert/pipeline/stages.rs`.
