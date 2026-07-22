# Brief — Metadata-overlay QoL: right-click context menu + track/disc auto-numbering

Status: DRAFT for a reasoning-model implementation round. Author: Claude Code
(diagnosis + research). Target tree: tonepoet @ HEAD `fa6131e` (0.4.1).

**How to read this brief.** It states the OUTCOMES we want and points you at
the exact code that is implicated (file:line, verified against the tree at cut
time). It deliberately does NOT prescribe the implementation — choose the
cleanest approach that fits the existing architecture — with ONE exception:
the **UI/TUI layout** sections are concrete, because TUI design is where we most
want to constrain you. Reproduce the described look; don't reinvent it.

Deliver complete files (full-file contents for every file you touch), per the
project's delivery contract.

**On our diagnostics — verify, then trust yourself.** The root-cause analyses,
mechanisms, and file:line citations in this brief are our best evidence-backed
reading, assembled by a coding assistant that got its first pass at the
right-click bug WRONG before catching and correcting it. Treat everything here
about HOW the code behaves as *leads*, not gospel. You are the more capable
analyst: independently re-derive each mechanism from the actual source, and
**where your findings diverge from ours, trust yours** — do what the code proves
and note the divergence so we learn from it. Do not anchor on our explanations;
if a cleaner root cause or implementation exists, take it. What is NOT yours to
reinterpret is the desired user-facing OUTCOME and the UI/TUI direction (§4) —
those are product decisions. Everything about the technical HOW is yours to
determine and own.

---

## 0. Motivation — the real problem this solves

The user converts vinyl-rip albums whose track files are named by **side +
sequence**, e.g. Abbey Road:

```
A01 - Come Together.flac   A02 - Something.flac   ...  A06 - I Want You...flac
B01 - Here Comes The Sun.flac  ...  B11 - Her Majesty.flac
```

After conversion **every** output collapses to `01 - <title>.flac` — the track
number resolves to `1` for all 17 tracks, destroying order and identity.

**Root cause (verified, for your understanding — NOT something to fix in this
brief):**
- Each loose file is processed as an INDEPENDENT single-file job. With no
  `TRACKNUMBER` tag, `src/convert/pipeline/materializer_single.rs:72`
  (`metadata.track_number.unwrap_or(1).max(1)`) yields `1` for every file.
- The filename parser `strict_track_number_from_path_stem`
  (`src/convert/pipeline/stages.rs:34606`) reads leading ASCII digits only and
  breaks on the `A`/`B` prefix, so filename-derived numbers are `None` too.
- Output naming (`template_track_number`, `stages.rs:34427`; token map at
  `stages.rs:33241`) therefore falls back to `1`.

**Scope decision (user, 2026-07-22): MANUAL ONLY.** Do NOT change the automatic
conversion-path derivation (the shared `template_track_number` /
`strict_track_number_*` logic). We are giving the user manual tools in the
metadata editor to set numbering, plus a bounded pipeline change (raw-string
carriage, ~3 touch-points — see §5) so side-prefixed numbers reach filenames. The automatic derivation
of sides from filenames/cues is explicitly deferred to a later, separately
scoped round, and must be designed here only as an EXTENSIBILITY SEAM (§6),
because a future config option will let the user prefer sidecar vs embedded cue
sources.

---

## 1. Outcomes we want

1. **Right-click never dismisses the metadata overlay.** (Bug fix, §2.)
2. **Right-click a numbering field → a context submenu of auto-number schemes.**
   Applies to `TRACKNUMBER`, `DISCNUMBER`. (§3, §4.)
3. **Simple numeric schemes apply immediately** (`N`, `NN`, `N/NN`, `NN/NN`).
   A **`Custom…`** entry opens an overlay for side-label prefixes and advanced
   control. (§3, §4.)
4. **`TOTALTRACKS` / `TOTALDISCS` → "Auto populate"** (count of loaded
   tracks/discs). (§3.)
5. **Vi command-mode equivalents** for all of the above (`:autonumber …`,
   `:autopopulate …`). (§3.)
6. **Side schemes reach the output filename** — a `TRACKNUMBER` of `A01` yields
   an `A01 - Title.flac` output on convert, via bounded raw-string carriage
   through the pipeline. (§5.)
7. The side/number **derivation is source-ordered and extensible** toward a
   future sidecar-vs-embedded config; it never invents side data. (§6.)

---

## 2. Bug — right-click makes the metadata overlay "disappear"

**Symptom (observed):** Right-clicking (seemingly anywhere) in the metadata
overlay makes the big editor vanish and the Browse pane behind it reappear;
pressing Esc brings the editor back.

**Our reading — verify it, and trust your own conclusion.** We traced this to a
RENDERING gap, not a close/dismiss (this is the diagnosis we got wrong on the
first pass, so re-derive it yourself):
- Right-clicking a metadata row / detail / the `+ Add field` gutter opens a
  context menu, which PARKS the editor into `app.pending_metadata_editor` and
  sets `ActiveOverlay::ContextMenu` (`keybindings.rs:21715 / :21735 / :21751`).
- `pending_metadata_editor` (`app.rs:10170`) is referenced only in
  `context_menu.rs` (park/restore) — it is NEVER read by any `draw*.rs`. The
  top-level draw paints the base screen first (`draw.rs:27`) then the overlay
  (`draw.rs:72`), and `draw_overlay` renders ONLY the menu for `ContextMenu`
  (`draw_overlays.rs:415`). So the parked editor is not drawn and the full
  Browse base shows behind the small menu — the editor "disappears."
- Esc closes the menu via `close_context_menu_restoring_parked` (its doc-comment:
  "every ContextMenu close path"), which restores the parked editor — that is
  why Esc brings it back.
- SEPARATELY, a right-click OUTSIDE the rows hits the catch-all `_ =>` arm
  (`keybindings.rs:21757`) → `request_metadata_editor_close` (`:7324`) → when the
  surface is clean it goes to `ActiveOverlay::None` with no parking (so Esc
  cannot restore it). This is a second, real defect.

We DISPROVED an earlier hypothesis (auto-populate marks the editor dirty on open
→ right-click routes into a discard confirmation): for this album
`parse_title_from_filename("A01 …")` (`probe.rs:5804`) yields no leading-digit
track number, and the files already carry TITLE tags, so
`did_auto_populate = false` and the surface is not dirty. Flagged so you don't
chase it — but confirm independently and correct us if you find otherwise.

**Outcome wanted:**
1. A right-click inside the metadata overlay must NEVER close or dismiss it;
   outside the actionable rows it is a harmless no-op.
2. When a context menu (or the §4 Custom overlay) is open over the metadata
   editor, the editor must REMAIN VISIBLE (dimmed) behind it — opening a menu
   must not reveal the Browse pane. Close the parked-editor-not-rendered gap
   (draw the parked editor as the backdrop, or keep the editor as the live base
   and layer the menu over it — your design call).
3. Preserve the existing row/detail context menus and their Esc-restore.

This matters beyond the bug: every new right-click menu we add (§3) inherits the
same parking path, so the rendering fix is a prerequisite for the whole feature.

---

## 3. Feature — auto-numbering context menu + Custom overlay + commands

### 3.1 Right-click submenus (reuse the existing context-menu system)

There is a full context-menu framework already; reuse it — do NOT build a new
one:
- `src/tui/context_menu.rs` (~2884 lines): `MenuLevel`,
  `ContextMenuEntry::{Item, Separator, Submenu{label, children}}`,
  `ContextMenuItem{label, action, shortcut, enabled}`, `ContextAction` enum,
  `MAX_CONTEXT_MENU_DEPTH = 4`.
- Overlay state `ActiveOverlay::ContextMenu{levels, origin}`
  (`src/tui/app.rs:5463`); overlay-on-overlay handled by parking the editor in
  `app.pending_metadata_editor` (`app.rs:10170`) and restoring via
  `run_context_action_restoring_parked` (`keybindings.rs:6073`).
- The row context menu you extend: `build_metadata_row_context_menu`
  (`keybindings.rs:19697`), already wired at the row right-click arm
  (`keybindings.rs:21726`).

**You will need to ADD (reuse, but these are genuinely new):** new
`ContextAction` variants for the numeric schemes and for opening the Custom
overlay, with dispatch in `execute_context_action`; and a distinct **new
`ActiveOverlay` variant** for the §4 Custom overlay (its own draw fn + key/mouse
handlers + parking/restore). The Custom overlay is NOT a `ContextMenu` — do not
force it into `ActiveOverlay::ContextMenu`. Reuse the editor-parking pattern for
it, but note it depends on the §2 rendering fix (a parked editor must draw behind
its overlay, or the Custom overlay will itself "disappear" the editor).

**Per-field submenu content** (choose the entries based on the clicked row's
`display_key`):
- **`TRACKNUMBER`** → `Auto number ▸`:
  - `N` — 1, 2, 3, … (unpadded) — applies immediately
  - `NN` — 01, 02, 03, … (2-pad) — applies immediately
  - `N/NN` — 1/17, 2/17, … (number / total) — applies immediately
  - `NN/NN` — 01/17, 02/17, … (both 2-pad) — applies immediately
  - `Custom…` — opens the Auto-Number overlay (§4) for side-label prefixes and
    advanced control
- **`DISCNUMBER`** → the SAME `Auto number ▸` submenu, plus `Auto populate`.
- **`TOTALTRACKS`** → `Auto populate` (value = count of loaded tracks).
- **`TOTALDISCS`** → `Auto populate` (value = count of distinct discs).

Numbering is assigned in the editor's existing track order (already sorted by
`(disc, track, filename)` — see §7), written into the field's
`TagEntry.per_file_values`, and the surface marked dirty. The user still saves
explicitly (existing save flow). Do not auto-save.

### 3.2 Numbering semantics (exact)

`n` = the track's 1-based position within its numbering group; `T` = total in
that group (loaded-track count, or per-side count for side schemes).

| Scheme | Value for track n | Notes |
|--------|-------------------|-------|
| `N`    | `n`               | unpadded: …8, 9, 10, 11 |
| `NN`   | `n` 2-padded      | …08, 09, 10, 11 (min width 2; widen if T ≥ 100) |
| `N/NN` | `n`/`T`           | total 2-padded (widen ≥100), number unpadded |
| `NN/NN`| `n`/`T` both 2-pad | |
| `SN`   | `<prefix><n>`     | per-side reset: A1, A2 … / B1, B2 …; unpadded |
| `SNN`  | `<prefix><nn>`    | per-side reset: A01, A02 … / B01, B02 …; 2-pad |

Side schemes (`SN`/`SNN`) are only reachable via `Custom…` / the overlay (they
need a prefix), never applied blind from the menu.

### 3.3 Auto populate

- `TOTALTRACKS` = number of loaded tracks. `DISCNUMBER` auto-populate =
  best-effort per-track disc from the derivation sources (§6); if none, leave
  unchanged (never invent). `TOTALDISCS` = number of distinct disc numbers among
  loaded tracks — a natural analog we suggest but that the user did not
  explicitly request; include it if it falls out cheaply, skip if awkward.

### 3.4 Vi command-mode equivalents

Follow the existing command pattern: `Command` enum (`command.rs:2185`),
`parse_command` (`command.rs:2579`), `execute_command` (`command.rs:3033`), and
the editor accessors `with_editor_state` / `with_editor_state_and_tx`
(`command.rs:2950` / `:2981`). Model after `MetaAdd`/`MetaDelete`
(`command.rs:4269`).

- `:autonumber N|NN|N/NN|NN/NN` — apply a numeric scheme to `TRACKNUMBER`.
- `:autonumber SN|SNN [PREFIX]` — side scheme; if PREFIX omitted and none
  derivable, default `A` (or open the overlay — your call, but a headless
  command should be scriptable, so accept the prefix arg).
- `:autonumber disc N|NN|…` — same for `DISCNUMBER`.
- `:autopopulate totaltracks|totaldiscs|discnumber`.

Command mode from the editor already parks/restores the editor
(`keybindings.rs:9392`); reuse it.

---

## 4. The `Custom…` / side-prefix overlay — UI reference

> **Reasoning model: this section is prescriptive about LOOK, because TUI design
> is your weak spot. Model the new overlay on our EXISTING multi-track editing
> popup (documented in §4.1) and adapt it into §4.2 — reuse the same visual
> language (border colors, header, per-track rows, footer pills). Do NOT invent
> a new look.**

### 4.1 The existing multi-track popup to model on — the "detail" view

We already have a per-track editing popup: the metadata editor's `DetailEdit`
phase, drawn by `draw_metadata_detail()` (`src/tui/draw_overlays.rs:5657`). It
edits ONE field's value across every loaded track, one row per track. Its state
lives on the editor model: `detail_field_idx` (which field), `detail_cursor`
(which track row), `detail_scroll`, `detail_edit: Option<TextInputState>` (inline
edit buffer when editing). Keys: Up/Down/`k`/`j` move rows, Tab/Shift-Tab skip to
next editable row, Enter edits, Esc backs out; scroll wheel moves the cursor
(`keybindings.rs` DetailEdit arm ~`:9702`). This is the layout to reuse:

```
╭─ Metadata Editor: 17 files ─────────────────────────────────────╮
│ ┌─ Metadata ─┐  Details   ReplayGain   Artwork                  │
│                                                                  │
│   TITLE                              ← header: field name (cyan, bold)
│                                                                  │
│   01     Come Together               ← label col (from file_labels)
│   02     Something                        + value col; cursor row
│   03     Maxwell's Silver Hammer          label = amber/bold,
│   ...                                     changed value = green
│                                                                  │
│            Enter edit    Esc back                ← centered footer pills
╰──────────────────────────────────────────────────────────────────╯
```

Details that matter for the adaptation:
- Popup is the standard editor box: **~85% of the terminal, centered**, single
  border, **cyan when clean / amber when dirty**.
- **Header:** two spaces + the field `display_key` in **cyan bold**, then a blank
  line.
- **Row = label column + value column.** Label is right/left-padded to a dynamic
  width (min 10, capped at 1/3 popup width) and comes from
  `file_labels[i]` (the per-track label — a track number like `01` or a filename
  stem); when there are more per-track values than files it synthesizes
  `Track NN`. Cursor row label is **amber bold**; other rows **muted**. Value is
  **bright**, or **green** when it differs from the on-disk original.
- **Footer:** centered "pills" (rounded chips) of context help, e.g.
  `Enter edit`, `Esc back`; while editing they become `Enter confirm`,
  `Esc cancel`.

### 4.2 The proposed `Custom…` / side-prefix overlay (adapt §4.1)

`Custom…` (from the `Auto number ▸` submenu) opens an overlay built from the
§4.1 skeleton, adding: a **scheme selector**, an **editable side-prefix field**,
a **live preview column** (`now → new`), and **in-overlay multi-select** so the
user can assign a prefix to a range of tracks. The main editor has no multi-
select — keep it CONTAINED in this overlay (its own selection state), exactly as
the user requested.

Target layout (adapt colors/pills from §4.1; `▸` = cursor, `‣`/highlight =
selected range):

```
╭─ Auto-number: TRACKNUMBER ──────────────────────────────────────╮
│  Scheme:  ( ) N   ( ) NN   ( ) N/NN   (•) SNN                    │
│  Side prefix for selection:  [ B ]     Sides from: filename      │
│                                                                  │
│    track (file)                     now    →   new               │
│                                                                  │
│    A01 - Come Together.flac          01    →   A01               │
│    A02 - Something.flac              01    →   A02               │
│    A06 - I Want You...flac           01    →   A06               │
│  ‣ B01 - Here Comes The Sun.flac     01    →   B01   ┐ selected   │
│  ▸ B02 - Because.flac                01    →   B02   │ range set   │
│  ‣ B11 - Her Majesty.flac            01    →   B11   ┘ prefix "B"  │
│                                                                  │
│   ↑↓ move · Space select · p set prefix · Tab scheme            │
│              Enter apply     Esc cancel                          │
╰──────────────────────────────────────────────────────────────────╯
```

Behavior (outcomes; you own the mechanics):
- **Pre-fill from derivation (§6).** On open, run the source resolver. If sides
  are derivable (Abbey Road's filenames give A/B), pre-assign each track's prefix
  and per-side sequence so the `new` column already reads `A01…A06`, `B01…B11` —
  the user can just press **Enter apply**.
- **When nothing is derivable,** default every track's prefix to `A`, continuous
  `A01…`; the user multi-selects a range (Space/Shift-move) and presses `p` (or
  edits the prefix field) to stamp a prefix onto the selection, which re-numbers
  that side from 1. Never fabricate B/C/D on your own.
- **Scheme selector** toggles padding/total exactly as §3.2 (`N`, `NN`, `N/NN`,
  `NN/NN`, and the side forms `SN`/`SNN`). The `new` column is a **live preview**
  (green, like a changed value).
- **Enter apply** writes the previewed values into the `TRACKNUMBER`
  `per_file_values` and marks the surface dirty (no save). **Esc cancel** discards
  and returns to the editor. Reuse the parking pattern (§3.1) so the editor is
  restored afterward.
- The `Custom…` overlay is also where `DISCNUMBER` side/advanced numbering is
  authored (same overlay, different target field + title).

This overlay is the single home for anything needing a user-supplied prefix; the
plain numeric schemes never open it (they apply straight from the menu).

---

## 5. Side-prefix → output filename (raw-string carriage)

**Outcome:** After setting a side-prefixed `TRACKNUMBER` like `A01` (via §4) and
saving, converting the album produces `A01 - Come Together.flac`,
`A02 - Something.flac`, …, `B11 - Her Majesty.flac` — the side-prefixed value
reaches the filename's track-number position.

**What we found (verify, then choose your mechanism).** The pipeline currently
models the track number as numeric end-to-end, so `A01` cannot survive as-is —
this is bigger than a template tweak, so plan for it:
- The source tag is read via numeric parse (`vorbis.track()` `metadata.rs:107`;
  `tag.track()` `probe.rs:6499`) into `track_number: Option<u32>`;
  `materializer_single.rs:72` then defaults a failed parse to `1`. So `"A01"` is
  discarded at read time.
- Every naming token (`NN` / `TRACKNN` / `N` / `TRACKN` / `TRACK`) is built from
  that `u32` (`stages.rs:33242`) via `template_track_number` (`stages.rs:34427`);
  there is **no raw-string track token**. The output tag is also written numeric
  (`push_tag_value("TRACKNUMBER", &n.to_string())`, `stages.rs:4446`).

Delivering `A01` filenames therefore means carrying a RAW track-number STRING
alongside the numeric one: capture it before the numeric parse discards it
(`metadata.rs:107`), thread it onto `PreparedTrack`, and have the naming token
prefer the raw string when it is non-numeric. Bounded (≈ metadata read + one
struct field + the token), but genuinely a ~3-touch change, and it is the ONE
conversion-path touch this brief authorizes.

**Constraints:** Do not change the numeric *derivation* itself
(`template_track_number`, `strict_track_number_*`) beyond adding the raw-string
preference; the numeric schemes must behave exactly as today (verified: saving
`01…17` round-trips correctly through `materializer_single:72`). Whether the
OUTPUT file's `TRACKNUMBER` tag should also carry `A01` (a 4th touch at
`stages.rs:4446`) or stay numeric is your call — the user cares about the
filename; pick the cleaner option and say which. Cover it with tests. If, on
your own analysis, a fundamentally better route to the outcome exists (e.g. a
dedicated side concept rather than overloading `TRACKNUMBER`), propose it — the
outcome is fixed, the mechanism is yours.

Non-goal: making arbitrary conversions auto-detect sides. That's deferred (§6).

---

## 6. Extensibility seam — derivation sources (design, don't fully build)

When we need a side letter and/or sequence for a track (for the `Custom…`
overlay's pre-fill, and later for auto-derivation), resolve it from an **ordered
list of sources**, first hit wins:

```
1. embedded cue sheet   (per-track index/title carrying side info)
2. standalone (sidecar) cue sheet
3. existing tags        (TRACKNUMBER / a side field if present)
4. filename prefix      (leading [A-Z] before the digits, e.g. "A01" → side A, n 1)
```

Design this as a small resolver (enum/trait of sources + a fixed default order)
so that a FUTURE config option (`sidecar` vs `embedded` preference — not built
here) can reorder sources 1↔2. In THIS brief only the filename source needs to
actually produce data (for the Abbey Road case); the cue/tag sources may be
stubs that return `None` for now, but the ordered-resolver shape must exist so
the later config work slots in without a redesign.

**Hard rule — never invent side data.** If no source yields a side letter, the
overlay defaults every track to prefix `A` and lets the user type/assign
prefixes to a selection (§4). Numbering never fabricates B/C/D. Do NOT segment
sides by `DISCNUMBER` (explicit user decision).

---

## 7. Implicated code map (verified file:line — WHERE, not HOW)

**Right-click bug (rendering gap + close-arm)**
- Right-click arm `keybindings.rs:21690`; working row-menu arm `:21726`; the
  offending catch-all `_ =>` close `:21757`. Mouse entry `handle_mouse` `:29389`
  → `handle_metadata_editor_mouse` `:21414` → `_in_area` (fn def) `:21428`.
- Parking: `pending_metadata_editor` field `app.rs:10170`, parked at
  `:21715/:21735/:21751`; restored by `close_context_menu_restoring_parked`
  ("every ContextMenu close path", incl. `KeyCode::Esc`). NEVER read by any
  draw code (grep-confirmed).
- Render path: base screen `draw.rs:27` then overlay `draw.rs:72`;
  `draw_overlay` draws only the menu for `ContextMenu` `draw_overlays.rs:415`.
- Close gate `request_metadata_editor_close` `keybindings.rs:7324` (dirty → park
  + `Confirmation` `:7368`; clean → `ActiveOverlay::None` `:7399`).
- Not-dirty proof: `parse_title_from_filename` `probe.rs:5804`; auto-populate
  `keybindings.rs:15291` returns `did_auto_populate`, caller sets
  `dirty = did_auto_populate` (false for this album).

**Context-menu framework**
- `context_menu.rs`: `MenuLevel` `:32`, `ContextMenuEntry` `:60`, `ContextAction`
  `:75`; existing metadata builders `build_metadata_row_context_menu`
  `keybindings.rs:19697`, `build_metadata_detail_context_menu` `:19847`.
- Overlay state `ActiveOverlay::ContextMenu` `app.rs:5463`; park field
  `app.rs:10170`; action dispatch + restore `run_context_action_restoring_parked`
  `keybindings.rs:6073`; cancel/Esc restore `close_context_menu_restoring_parked`.
- Menu draw `draw_overlays.rs:530` / `:591`; geometry/hit-test
  `keybindings.rs:22850` / `:23000` / click `:23096` / hover `:23043`.

**Editor data model**
- `MetadataEditorState{ model: MetadataEditorModel }` `app.rs:7491`;
  `PresentationTab{ paths, entries, … }` `app.rs:7103`;
  `TagEntry{ display_key, item_key, value, per_file_values, row_scope }`
  `probe.rs:5206`. `per_file_values[i] ↔ paths[i]`.
- Track ordering: `expand_paths_to_all_audio` `queue_expansion.rs:167` (alpha
  sort) then `sort_paths_entries_metadata_and_errors_by_track` (def
  `probe.rs:5876`, called `keybindings.rs:15913`), key `(disc, track, filename)`
  `probe.rs:5924`; per-file vectors permuted in lockstep `probe.rs:5884`.
  Per-track display labels: the surface's `file_labels` field.

**Command mode**
- `Command` `command.rs:2185`; `parse_command` `:2579`; `execute_command`
  `:3033`; `with_editor_state`/`_and_tx` `:2950`/`:2981`; exemplar handlers
  `:4269` (MetaAdd) `:4274` (MetaDelete). Editor command-input entry
  `keybindings.rs:9392`; draw `draw_overlays.rs:3037`.

**Conversion naming (side pass-through only)**
- `template_track_number` `stages.rs:34427`; token map `stages.rs:33241`; raw
  metadata read `metadata.rs:107` / `probe.rs:6499`; output tag write
  `stages.rs:4446`; filename parser (leave alone)
  `strict_track_number_from_path_stem` `stages.rs:34606`. Root-cause default
  `materializer_single.rs:72`.

---

## 8. Constraints & non-goals

- **Manual only.** No change to automatic conversion-path derivation except the
  bounded side→filename raw-string carriage (§5).
- **Never invent side data;** no disc-number side segmentation.
- **Reuse** the existing context-menu framework and the existing multi-track
  popup look — no new menu/overlay frameworks.
- Editor edits **tags**; it does not rename files. Filenames change on the next
  conversion (that's why §5 matters). Do not add a file-rename side effect here.
- Version discipline: no version bumps in your bundle (the maintainer bumps
  patch-only as the last commit of a merge set).
- Keep the existing save/dirty/confirmation flow intact; auto-number only
  mutates `per_file_values` + dirty, never saves.

---

## 9. Acceptance (Abbey Road as the fixture)

Given the 17 side-prefixed files with no `TRACKNUMBER` tags, loaded in the
metadata editor:
1. Right-clicking anywhere in the editor never *dismisses* it: outside the rows
   is a no-op; on a row/gutter it opens a menu that renders **over the still-
   visible (dimmed) editor** — the Browse pane never shows through, and Esc
   returns cleanly to the editor.
2. Right-click `TRACKNUMBER` → `Auto number ▸ NN` → the grid shows
   `01, 02, … 17` in track order; save; convert → `01 - Come Together.flac` …
   `17 - Her Majesty.flac`.
3. Right-click `TRACKNUMBER` → `Auto number ▸ Custom…` → overlay pre-fills side
   `A` for A0x and `B` for B0x from the filename prefix, `SNN` per-side reset →
   `A01…A06`, `B01…B11`; save; convert → `A01 - Come Together.flac` …
   `B11 - Her Majesty.flac`.
4. A folder with NO derivable sides → `Custom…` defaults all to prefix `A`; user
   selects a range and types `B` → those become `B01…`; nothing fabricated.
5. `TOTALTRACKS` → `Auto populate` → `17`. `:autonumber NN` and
   `:autopopulate totaltracks` produce identical results to the menu.
6. Full workspace suite stays green (`cargo test --workspace --no-fail-fast`,
   untruncated, 0 failed); zero cold warnings.
