# Round-7 Brief — Transfer Tags: carrier semantics, both directions

**Branch:** `hardening` @ 02b8822. **Baseline suite:** 5,265 passed / 0 failed
(56 targets). **Version stays 0.4.4.**

All mechanisms are research-verified with citations; do not re-derive them.
Standing: rigor-vs-usability directive; NO function keys; NO emoji; Ctrl+Q
stays quit; scoped keybindings; never regress `:messages`, the verification
split, round-5 ID3-prefix FLAC support, or round-6 tag machinery.

## 0. The governing design stake (user, verbatim intent)

The user has placed a stake: tonepoet's "default means of tagging/managing
files" will eventually be a Config setting with cascading fallthrough
priorities (individual-files-first / sidecar-cue-first / embedded-cue-first)
defined at the DIRECTORY level — and later at the ALBUM level in the
future `library` ("the directory abstracts at the same level as the
'album'"). **THIS ROUND does NOT build the setting.** This round makes
Transfer tags operate through the EXISTING de-facto heuristic as the
documented default method, in BOTH directions, with the future setting's
plug-in point left clean. The plug-in point already exists and is
documented in code: `DEFAULT_FRONTEND_CUE_POLICY =
CueSidecarPolicy::PreferSidecar` (src/tui/cue_parser.rs:74-79 — its comment
says "A future folder-level preference replaces the value supplied at those
entry points without changing their behavior"). TWO-LAYER TRUTH
(audit-corrected): CUE ADMISSION is policy-free structural discovery
(`collect_metadata_cue_admission` keybindings.rs:14262 /
`_with_selections` :14269 — the second parameter is CUE-selection
overrides, NOT a policy); the POLICY chooses sidecar-vs-embedded for an
already-admitted pair one layer down (`resolve_metadata_cue_source`
:14064, `apply_policy_selected_metadata_cue_source` :14122,
`open_metadata_editor_impl`'s `cue_policy` parameter :20051). Thread
`DEFAULT_FRONTEND_CUE_POLICY` at THAT layer, mirroring the editor. Do not
add a policy parameter to admission (nothing consumes it) and do not
hardcode the constant at resolution sites.

**The de-facto heuristic, verbatim for this round** (research-verified from
the editor's open/save routing, keybindings.rs:20051+ / 9762+ / 14064+):
folder → admit CUEs (PreferSidecar); viable single-image CUE pairs operate
at TRACK scope with truth in the CUESHEET (sidecar writeback when identity
= Sidecar; embedded CUESHEET tag when identity = Embedded); anything
without a viable CUE is per-file lofty tags at FILE scope. (The machinery
also handles native multi-FILE CUE albums — one CUE, N files; see the §7
fence: REFUSED for transfer this round.) Disc images (SACD/DVD-A/DVD-V/BD)
are OUT OF SCOPE for transfer this round (refuse with an honest status).
PreferSidecar nuance the reads must mirror (keybindings.rs:14069-14081):
when a valid embedded sheet STRUCTURALLY MATCHES the sidecar, resolution
uses the embedded TEXT under Sidecar IDENTITY — transfer-FROM reads must
reproduce this, exactly like the editor.

## 1. File picker: multi-path completion + contextual confirm

Verified current state: completion is single-path everywhere —
`FilePickerAction::Selected(PathBuf)` (crates/tui-file-picker/src/
state.rs:112-118), six constructor functions across seven emission sites
(state.rs:2065/2274/2283+2290/2620/2629, input.rs:618), one tonepoet funnel
`send_file_picker_completion` (keybindings.rs:11876-11901), message
`FilePickerComplete { path: Option<PathBuf> }` (message.rs:567-571),
consumer `reduce_file_picker_complete` (event_loop.rs:869-1274) with 10
match arms covering 12 purpose variants (CopyTo|MoveTo and
SelectFile|SelectDirectory are OR-arms). Multi-select machinery EXISTS
ungated (`multi_selected` state.rs:1023, Ctrl+Space/Ctrl+A/Ctrl+click,
input.rs:530-537/877-881) but feeds only file operations; NOTHING consults
it for completion. AUDIT-CRITICAL FACT: marks are NOT files-only — the
files pane lists directories and both Ctrl+Space and Ctrl+A mark them
(toggle :1199-1213 has no is_dir guard; select_all_visible :1215-1217
marks everything visible). Marks are pruned to the visible directory on
navigation (state.rs:1934-1935) — multi-file returns are inherently
same-directory; keep that. `multi_selected` preserves MARK order for
toggles but sorted order for select-all — ordering is gesture-dependent.
No range selection exists; do not add one this round.

Required:
1. **Multi-path completion**: add `SelectedMany(Vec<PathBuf>)` to
   `FilePickerAction` (do not widen `Selected` — the save/address/search
   constructors stay single-path). Emit it ONLY from the confirm path
   (below) when file-marks exist. MARK RULES (audit-forced — marks can
   contain directories): at confirm time, marked DIRECTORIES are filtered
   out; `SelectedMany` carries only marked FILES, emitted in VISIBLE
   (sorted) order regardless of mark gesture; if any directories were
   filtered, the completion status appends "(N directories ignored)"; if
   marks contain ONLY directories, treat as no-marks (cursor rules apply).
   Widen the funnel and the message (recommend: add `paths: Vec<PathBuf>`
   alongside the existing `path` field, with `path = paths.first()` for
   compatibility — the 8 non-transfer arms / 10 purpose variants keep
   reading `path` untouched; ONLY the two transfer arms read `paths`).
   The transfer arms already take `Vec<PathBuf>` roots (`start_tag_transfer`
   defined at context_menu.rs:1776-1804; editor-side arm
   event_loop.rs:985-1114 wraps the single path as `&[selected]`) —
   slice-widening, verified.
2. **Contextual confirm button**: extend the single existing toolbar
   confirm (render.rs:252-254, today `"Select Folder"` gated to
   `Directories` mode) to also render in `FilesOrDirectories` and `Files`
   modes with a context-computed label: cursor on a directory + no
   file-marks → `Select Folder` (FilesOrDirectories only — in `Files` mode
   directories are not acceptable (`accepts_entry` state.rs:33-38), so no
   confirm renders for a dir cursor there); file-marks present →
   `Select N Files` (N = marked FILES after directory filtering); cursor
   on a file, no marks → `Select File`. Routes through the existing
   `ToolbarAction::AcceptSelection` hit-testing (render.rs:277-283,
   input.rs:941/1119) — geometry free. VISIBILITY (audit-found hazard):
   the toolbar renders buttons in order with hard clip-and-drop at
   `toolbar_right` (render.rs:284-287) and AcceptSelection is appended
   LAST — in the embedded editor picker the confirm could be entirely
   DROPPED. Required: give the confirm button PRIORITY placement (render
   it before overflow-prone buttons) or reserve its width; there is no
   label-truncation mechanism today, so build one or guarantee space.
   Pin a test that the confirm is visible at the embedded picker's
   minimum width.
3. **Semantics unchanged elsewhere**: Enter/double-click on a directory
   DESCENDS (state.rs:2270-2272) — unchanged, every picker relies on it.
   The confirm button (and Space where it already accepts) is the explicit
   select path; Space IS forwarded in the embedded editor host (verified,
   keybindings.rs:11859-11874 forwards all keys). The Directories-mode
   Space asymmetry (accepts current_dir, state.rs:2282-2283) is
   contract-tested (state.rs:7090-7113, app.rs:17110-17134) — LEAVE IT.
4. **Filter**: transfer pickers must admit `.cue` files alongside audio —
   the transfer launch sites (context_menu.rs:1748-1749,
   keybindings.rs:10804) use `FilePickerFilter::Audio`, which does NOT
   include cue (filter.rs:25-31); swap those sites to a Custom set that
   adds cue (do NOT change the global Audio filter; other pickers rely on
   it). Pin with a test.
5. `accept_current_selection` ALREADY accepts a highlighted directory in
   FilesOrDirectories mode (`accepts_entry` → true for all entries,
   state.rs:33-38; accept at :2289-2290) — audit-verified, NO change
   needed there; do not write dead code. The ONLY new behavior in this
   function: when file-marks exist, return `SelectedMany` per the §1.1
   mark rules.

## 2. Carrier classification at the transfer boundary (BOTH directions)

New function (natural home: tag_interchange.rs or a sibling), applied to
EACH picked/fixed side. SEAMS (audit-corrected): the editor-side blind
expansion is in the reducer arm (event_loop.rs:985-1114, both To/From call
`expand_audio_paths_for_metadata_limited`); the BROWSE-side blind expansion
is NOT in the reducer arm (:1149-1173 only routes roots) — it happens in
the WORKER inside `launch_tag_transfer` (context_menu.rs:~1877-1893).
Classification must replace BOTH expansion sites. Note the expansion fn
admits only `EntryKind::AudioFile` (command.rs:129-134) — a picked `.cue`
is silently DROPPED today.

| Selection | Carrier | Read (FROM) | Write (TO) |
|---|---|---|---|
| Directory | admission (`collect_metadata_cue_admission_with_selections`, keybindings.rs:14269 — policy-free) then policy resolution (`resolve_metadata_cue_source` :14064 with `DEFAULT_FRONTEND_CUE_POLICY`): exactly one viable single-image CUE scope → that CUE carrier per the resolved identity; else → individual files (bounded expansion, existing caps) | per resolution | per resolution |
| `.cue` file | sidecar CUE (explicit user pick) | parse + track rows (§3) | structured sidecar rewrite (§4) |
| audio image whose tags carry CUESHEET | **CARRIER CONSISTENCY RULE (audit-forced — the biggest guess-forcer):** selecting the image runs FOLDER admission + policy for its parent, exactly like the editor (open_metadata_editor_impl re-admits the parent, keybindings.rs:20232-20288) — under PreferSidecar an image WITH a viable sidecar resolves to the SIDECAR carrier, same as picking the directory or the .cue. Only an image with embedded CUESHEET and NO viable sidecar resolves to the EMBEDDED carrier (validated via `embedded_cue_candidate_for_metadata`, keybindings.rs:14021). The same album must get the SAME carrier regardless of selection gesture. | per resolution (§3) | per resolution (§4) |
| individual file(s) (multi-select or single non-image) | per-file tags | merged read (existing) | classified per-file writer (existing) |

Refusals, all honest statuses: disc images/dirs; archives; a `.cue` whose
FILE references are missing; a `.cue` with fewer than 2 tracks (mirrors
the editor's admission floor, keybindings.rs:17508); a directory whose
admission yields MULTIPLE viable CUE scopes (multi-disc — refuse with
"multiple CUE albums in this folder; select a specific .cue or image";
future config/library rounds revisit); a native multi-FILE CUE album
(one CUE referencing N files — §7 fence); a multi-select mixing a `.cue`
with audio files ("selection mixes a CUE with audio files; select one
carrier"); an image without embedded CUE picked as a cue carrier (it's
just a file — treat as individual file); empty directories.

## 3. Transfer FROM (read side per carrier)

- Individual files / file-resolved directory: existing
  `read_transfer_source_entries` (tag_interchange.rs:1041) — unchanged.
- Sidecar CUE: `parse_cue_file` (src/convert/cue_parser.rs:59; encoding/
  BOM-aware, :~130-185) → build Track-scoped TagEntry rows using the
  established overlay pattern (`reload_from_sidecar_cue` /
  `overlay_per_track_values`, keybindings.rs:17484/17411): per-track
  TITLE/PERFORMER(→ARTIST)/ISRC + album fields
  (ALBUM/album-PERFORMER/DATE/GENRE/CATALOGNUMBER) from `CueSheet`
  (cue_parser.rs:16-56).
- Embedded CUE: read the image's CUESHEET tag text (raw text is in
  per_file_values — cue_summary_string is display-only), `parse_cue`, same
  row construction.
- KEY UNBLOCK (round-6 planner): `transfer_entry_selected`
  (tag_interchange.rs:927-944) excludes track-scoped and binary rows —
  the new track-dimension path must NOT route through that exclusion;
  CUESHEET itself (is_binary) still never transfers as a field.

## 4. Transfer TO (write side per carrier)

- Individual files: existing classified per-file writer — unchanged.
- Sidecar CUE: **new thin composer, existing engine**: compose a
  replacement CUESHEET text (the target's own parsed sheet as template
  with planned values applied) and call
  `rewrite_cue_sidecar_metadata_from_cuesheet(cue_path, replacement_text)`
  (src/convert/cue_parser.rs:479) — audit-verified STRUCTURED byte-span
  rewrite: the engine RE-READS the original from disk itself, extracts
  only editable metadata from the replacement, preserves untouched lines /
  encoding / BOM / structural commands, atomic tmp+rename,
  Unchanged/Rewritten/Utf8Fallback outcomes; call-site precedent is the
  editor save (keybindings.rs:10178-10183 lifts the CUESHEET entry text →
  :10252 calls the engine). Engine facts the composer must honor:
  (a) replacement audio-track count must equal the original's or the
  engine errs (cue_parser.rs:684-690) — the composer refuses at PLAN time
  with a better status, before any write; (b) fields ABSENT from the
  replacement leave the original lines untouched — the engine can never
  CLEAR a CUE field; transfer therefore never deletes CUE fields (matches
  round-6 posture; disclose it). WRITE-TIME RE-ADMISSION (audit-forced,
  TOCTOU): immediately before the rewrite, the worker re-runs the
  `admit_split_cue_member` validation mirroring the editor save's
  preflight (keybindings.rs:10088-10108 — sidecar still references
  exactly the expected image) and refuses "left unchanged" on mismatch;
  classification-time checks alone are insufficient for a background
  write. Do not write CUE text any other way.
- Embedded CUE: regenerate the CUESHEET text the same way (target's own
  parsed sheet as template, same plan-time track-count refusal) and write
  the `ItemKey::Unknown("CUESHEET")` tag through the classified writer.
  FORMAT SCOPE (audit-found risk): the write→read round-trip is pinned
  ONLY for FLAC Vorbis comments (`lofty_cuesheet_vorbis_comment_round_
  trips_multiline`, probe.rs:16659). Non-FLAC embedded targets (WAV/APE/
  WavPack images carrying CUESHEET) are NOT proven — either add a
  per-format round-trip pin for each format admitted, or FAIL CLOSED with
  an honest status ("embedded CUE write is supported for FLAC images this
  round"). Do not assume the FLAC pin generalizes.
- Directory: resolve per §2 and route accordingly.
- **Field capping for CUE targets** (both sidecar and embedded), with the
  KEY MAP defined (audit-forced): per-track TITLE↔TITLE and
  ARTIST↔PERFORMER (both directions — the read side already maps
  PERFORMER→ARTIST, mirror it on write); album ALBUM↔TITLE,
  ALBUMARTIST↔PERFORMER, DATE↔REM DATE, GENRE↔REM GENRE,
  CATALOGNUMBER↔CATALOG (DATE/GENRE are REM directives — the engine
  handles them via its dedicated REM path with quote rejection).
  SONGWRITER is EXCLUDED this round entirely (audit-found asymmetry: the
  parser does not surface it, so cue→cue would silently drop it; exclude
  with a report disclosure rather than write-only support). TRACKNUMBER
  is structural (TRACK NN) — never written. ISRC read-only. Source fields
  outside the cap are SKIPPED with the existing `SkippedField`-style
  disclosure (tag_interchange.rs:45, TagTransferReport :874).

## 5. Track-dimension planning (new)

`plan_transfer_values` (tag_interchange.rs:970) is file-positional. Add a
track-dimension plan for CUE-involved transfers:
- Track-carrier → track-carrier (cue→cue, cue→embedded, etc.): N tracks →
  N tracks positional by track number; N→M hard-fails (same posture as
  round 6).
- Track-carrier → N individual files: N tracks → N files positional
  (track order → traversal order); mismatch hard-fails.
- N files → track-carrier: same, reversed.
- Track-carrier or N files → SINGLE image file's per-file tags (the
  user-stake rule): multi-value fields collapse to the FIRST track's value
  (title, number, etc.), disclosed in the report ("wrote first-track
  values to single image"). This is the stake's single-image constraint —
  implement it exactly.
- 1-source broadcast rules carry over from round 6 (scalars only, never
  numbering keys) where the target is file-dimensional.
- 1 source file → track-carrier target (audit-forced rule): album-level
  fields apply; per-track fields (TITLE etc.) are SKIPPED with disclosure
  — NOT a hard fail (there is no meaningful 1→N per-track broadcast).
  No other broadcast INTO a track carrier exists.
- Track pairing is PURE POSITIONAL after sorting tracks by TRACK number
  (audit-forced: gapped or non-1-based numbering is tolerated — position
  in the sorted order pairs, not the literal number).
- The single-image per-track sensibility test mirrors the gnudb model
  (single_image && has_cuesheet, gnudb.rs:434-445).

## 6. Direction symmetry, statuses, tests

- BOTH popup/menu directions (Transfer from / Transfer to, browse and
  editor) gain the full carrier matrix. Transfer-from still lands in the
  OPEN EDITOR when invoked editor-side (reviewable, unchanged posture).
  BROWSE-SIDE CONFIRMATION (audit-forced — verified reality: the round-6
  dirty-editor blocking confirm is EDITOR-SIDE ONLY; browse-side
  transfer-to currently writes with NO confirmation, and round 7 turns one
  click into whole-directory or CUE-carrier writes): ALL browse-side
  transfer-to writes now get a BLOCKING confirmation overlay naming the
  carrier and count before the worker writes ("Write 4 fields to sidecar
  CUE (12 tracks)?" / "Write 4 fields to 12 files?"). Editor-side postures
  unchanged (dirty-editor confirm as in round 6).
- Statuses name the carrier: "read 12 tracks from sidecar CUE",
  "wrote 4 fields to embedded CUE in <image>", "wrote first-track values
  to single image", plus skipped-field disclosures. `:messages` retains
  failures.
- Tests, minimum per item: picker SelectedMany + contextual labels (all
  label states incl. Files-mode dir-cursor suppression; directory-mark
  filtering + "(N directories ignored)"; sorted emission order;
  confirm-button VISIBILITY at the embedded picker's minimum width);
  compatibility pins that all 8 non-transfer arms / 10 purpose variants
  still work via `path`; the `.cue` filter admitted at both transfer
  launch sites; carrier classification for all selection types incl.
  EVERY refusal (multi-scope directory, <2-track cue, mixed cue+audio
  marks, multi-FILE CUE album, disc, archive) AND the carrier-consistency
  rule (image-with-viable-sidecar resolves to sidecar — same carrier for
  all three gestures); CUE read → Track rows (encoding fixtures reuse the
  parser's existing BOM/CP932 corpus) incl. the matched-sheets
  embedded-text-under-sidecar-identity nuance; sidecar rewrite via the
  structured engine (untouched-line preservation + encoding + atomicity +
  the plan-time track-count refusal + write-time re-admission refusal);
  embedded CUESHEET write round-trip (FLAC) + the non-FLAC fail-closed
  status; track-dimension N→N/N→M/first-track-collapse/1-file→track-
  carrier-skip plans + gapped-track-number pairing; field-cap skip and
  SONGWRITER-exclusion disclosures; browse-side blocking confirm (accept
  and decline paths); the directory resolution honoring
  `DEFAULT_FRONTEND_CUE_POLICY` threaded at the resolution layer (pin
  that the constant is consulted, not hardcoded — the future config
  setting must be able to swap it per its documented comment).

## 7. Scope fences

- The Config cascade setting: NOT this round (plug-in point stays clean).
- Native multi-FILE CUE albums (one CUE referencing N audio files):
  transfer REFUSES them honestly this round ("multi-file CUE albums are
  not yet transfer targets/sources"); the editor continues to handle them
  for manual edits (keybindings.rs:10115-10176). Queued for a follow-up.
- Library/album abstraction: NOT this round.
- Disc-image transfer targets/sources: refused honestly.
- ISRC writeback, range selection in the picker, cross-directory
  multi-select: NOT this round.
- Custom builder + Paste tags: the round AFTER this (user-ordered).

Deliverables: overlay bundle + preimage manifest; engineering report with
per-item pinning tests, the carrier matrix as implemented, disclosed
limitations, and deviations with rationale.
