# Outstanding work — edge cases batch (2026-08-07)

User-reported batch. Anchors recon'd against `hardening` @ eaf12ff. Grouped for briefs.
The **CUE scope/authority cluster (4,5,7,8,9)** is the recurring lodestar area and must be
fixed at the ROOT this time, not leaf-patched again.

---

## Group I — CUE scope / authority (ROOT-CAUSE brief; lodestar-governed)

The unified-CUE album metadata model assigns each row a scope (File = album-level, Track =
per-track). We have repeatedly INFERRED scope from vector dimensions, then switched to
DECLARED scope + a strict save-time validator (Thriller corrective). Both are fragile: they
FAIL LOUD when a row's dimension doesn't match the target album shape instead of
NORMALIZING album fields to that shape. Every item below is a symptom of that one weakness.

### 7. "cannot persist per-track ALBUM on a multi-image CUE album" on a SINGLE-image cue
Repro: `~/torrents/Michael Jackson – Thriller(MS)-1982-JP` — a single `.flac` + one cue,
9 tracks (NOT multi-image). Editing any album field errors. The cue has **malformed/
duplicate headers** (`REM GENRE Pop` AND `REM GENRE R&B`; junk `REM "Epic" Of CBS Inc`;
`REM (MASTER SOUND 30-3P-431)`), which likely give ALBUM multiple values → mis-scoped Track.
Edit-time guard: `metadata_editor_unpersistable_per_track_reason` — keybindings.rs ~9879
(`unified_cue_entry_is_track_scoped(entry) && per_file_values.len() == track_dim` →
"cannot persist per-track {}"). Not reliably reproducible by the user but confirmed real.

### 8. "save aborted: unified CUE invariant violated: ALBUM is Track-scoped but must be File-scoped"
Repro: transfer tags FROM a multi-file album TO a single-cue+single-image album, then save.
The multi-file source gives ALBUM a Track-dimension (N-value) vector; the target is
single-image (File dim 1). The save validator `cue_album_surface_shape_error`
(keybindings.rs:19412) rejects the mismatch instead of collapsing ALBUM to one File value.
**User's point: transfer must not care about source vs target shape — it should just work.**

### 9. Transfer privileges embedded cue over the user's authority preference
Repro: transfer from a single-image FLAC that has BOTH an embedded cuesheet AND a sidecar
cue; user preference is `[IndividualFiles, SidecarCue, EmbeddedCue]` but transfer uses the
embedded cue (slow). The transfer classifier does not honor
`aggregate_metadata_target_priority` for source-authority selection (conversion now does;
transfer does not). Fix: transfer must consult the same preference. (tag_interchange /
classify_tag_transfer_roots authority selection.)

### 4. DFF per-track TITLE shows "<unsupported: failed to read '…dff'>" despite a valid cue
Double-clicking a `<multiple values>` field (e.g. TITLE) in the metadata editor shows each
track as `<unsupported: failed to read '….dff': No format…>` (from probe.rs:7923). Edits
"stick" (cue is save-authority) but the per-track VALUE DISPLAY still reads the untaggable
carrier and shows the failure. The untaggable-sidecar authority we shipped covers save but
NOT the detail-edit value display. Fix: when a valid sidecar cue is authority, the per-track
display sources values from the cue, not from a doomed carrier read.

### 5. Auto-create a sidecar cue when transferring/copying tags to untaggable formats
Untaggable carriers (DFF/SHN/DTS/AC3) can't hold tags, so transfer/copy tags should
MATERIALIZE a sidecar cue automatically (as ISO/disc sources already produce xml/toml
sidecars). Today B5 materialization exists for the editor save path; extend the same to the
transfer/copy-tags path so "tag an untaggable album" always lands somewhere durable.

**Group I brief posture:** establish ONE authoritative album-shape/scope model, classified
once at construction; NORMALIZE album-level fields to File-scope and per-track fields to
Track-scope for the target shape on edit/transfer/import (collapse, don't reject); make the
edit guard + save validator consult that one model; extend untaggable-cue authority to the
value DISPLAY and to transfer/copy (incl. auto-materialization); honor
`aggregate_metadata_target_priority` in transfer source selection; tolerate malformed/
duplicate cue headers. Lodestar-governed (docs/metadata_source_selection_heuristic.md);
full-gate ×2; every symptom above becomes an acceptance test.

---

## Group II — Browse/TUI UX (separate briefs)

### 1. Progress feedback when opening/preparing folders (UX)
Large folders or sleeping drives take several seconds to enumerate with no feedback. Want a
status-bar message (e.g. "Reading <folder>…" / a spinner in the footer) that appears when a
folder open exceeds a short threshold and clears when entries arrive. The Browse open/refresh
path already runs async (refresh_with_search / bounded traversal) — hook a "pending open"
indicator into the footer/status. Design: cheap threshold-triggered status line + optional
count-so-far; keep it non-blocking.

### 2. Inline rename editor corrupts multi-byte chars (e.g. `•`) + duplicates on Enter
`•` (U+2022, 3-byte UTF-8) garbles the inline file/folder rename display, and on Enter the
value sometimes DUPLICATES (whole name reinserted mid-string). The metadata OVERLAY editor
handles this correctly; the inline Browse rename editor does not — likely the inline RENDER
(draw_browse) uses byte-slicing instead of `display_width`, and/or mouse-click cursor
positioning computes a wrong byte offset across a multi-byte char. `TextInputState` itself
has char-boundary handling (text_input.rs) and `display_width::width` (:841) — mirror the
overlay's correct render/cursor path in the inline editor. The duplication-on-commit is the
worst part and must be root-caused (stale/doubled value on commit vs. bad insert offset).

### 3. Alt+Space opens the context menu (keybinding)
Add Alt+Space as a keybinding to open the Browse context menu (currently opened via
open_context_menu / right-click; open_editor_context_menu keybindings.rs:9393). Byobu-safe:
Alt+Space is acceptable (not an F-key; not the sole path — right-click remains). Confirm no
existing Alt+Space binding / terminal conflict.

---

## Group III — GNUDB (quick fix)

### 6. GNUDB "500 Unknown developer email for tonepoet 0.1"
`HELLO = "tonepoet+localhost+tonepoet+0.1"` (gnudb.rs:128). gnudb rejects unregistered
clients — the CDDB HELLO `username+hostname+client+version` must carry a valid developer
email / registered client. Fix: set a valid contact email + real version in the HELLO
(decide the email; possibly register tonepoet with gnudb). Small code change (gnudb.rs:128),
one decision (which email).

---

## Suggested sequencing
- **Group I** is the big one and the source of the week's frustration — one careful
  root-cause brief, lodestar-governed, all symptoms as tests. Highest priority.
- **Group II** items are independent Browse UX briefs (1 UX, 2 bug, 3 tiny).
- **Group III** (#6) is a near-trivial fix pending an email decision.
