# Code task: Convert screen collapsible/scrollable pane redesign

## Repo

https://github.com/barstoolbluz/tonepoet.git
Branch: `main` at commit `e665357`

## Context

Read these files for background:
- `CLAUDE.md` — project overview, workspace structure, TUI architecture
- `src/tui/convert_screen.rs` — main layout: 12-slot vertical `Layout` with `Constraint::Length` for all panes
- `src/tui/draw_source.rs` — source pane rendering (amber border), `source_pane_height()` dynamic height calc
- `src/tui/draw_metadata.rs` — metadata pane rendering (purple border), 5 fixed rows
- `src/tui/draw_output.rs` — format pane rendering (green border), 10 fixed rows, PCM/DSD branching
- `src/tui/draw_output_options.rs` — output options pane rendering (cyan border), 7 fixed rows
- `src/tui/app.rs` — `ConvertState`, `ConvertFocus`, `SourceState`, `MetadataState`, `FormatState`, `OutputOptionsState`, `SourceMode`
- `src/tui/keybindings.rs` — `handle_convert_key()` (line 231), mouse dispatch (line 9613+)
- `src/tui/pill.rs` — `PillState<T>` generic pill selector
- `src/tui/button_map.rs` — `TuiButton` enum, `ButtonRenderMap`
- `src/tui/format_interactions.rs` — `handle_convert_format_row_step()`, `handle_convert_format_button()`, constraint cascading
- `src/tui/context_menu.rs` — `build_convert_menu()` (line 570)
- `src/tui/draw.rs` — `draw_ui()` top-level dispatch, `button_map.clear()` per frame
- `src/tui/draw_footer.rs` — `hint_groups_for()` context bar hints
- `src/tui/draw_queue.rs` — collapsible track sub-lines reference pattern (line 125+)

## What already exists

### Convert screen layout (`convert_screen.rs:29-45`)

The layout is a 12-slot vertical `Layout` with **all `Constraint::Length`** — completely rigid:

```
Slot  Constraint           Content
 0    Length(7)             ASCII art header
 1    Length(1)             blank
 2    Length(1)             preset bar
 3    Length(1)             blank
 4    Length(source_h)      Source pane (6-12 dynamic)
 5    Length(5)             Metadata pane
 6    Length(10)            Format pane
 7    Length(7)             Output options pane
 8    Length(1)             blank
 9    Length(1)             action bar (enqueue / enqueue+start)
10    Min(0)                absorb leftover space
11    Length(2)             footer (tabs + context)
```

Total minimum demand: **42 rows** (source=6). Maximum: **48 rows** (source=12). There is no height guard on the convert screen — if the terminal is shorter than 42 rows, ratatui silently clips content. The `Min(0)` absorber at slot 10 only catches surplus space; there is no mechanism to shrink panes.

### Two-pass rendering (`convert_screen.rs:47-93`)

**Pass 1 (draw):** Each pane draw function receives `(Frame, Rect, &State, bool)` with immutable state refs. They render using a `bordered_line()` helper that is identically duplicated across all 5 draw files (`draw_source.rs:487`, `draw_metadata.rs:111`, `draw_output.rs:340`, `draw_output_options.rs:211`, `draw_browse.rs:1446`).

**Pass 2 (register):** `register_buttons()` (line 97) writes to `app.button_map`. All button y-coordinates are **relative to pane rect origins** (e.g., `format_area.y + 2` for format pills, `+ 7` for replaygain). These adapt automatically when the pane Rect moves. Button registration does **not** guard against collapsed state — if a pane is collapsed to 1 row, registering pills at `y+2` through `y+7` would create phantom click targets in other panes.

### Button map lifecycle (`draw.rs:19`)

`button_map.clear()` is called once per frame at the start of `draw_ui()`. Renderers populate it during the same frame. Mouse handlers query it until the next frame clears it. Layout state changes take effect immediately on the next render — no invalidation needed.

### Pane focus navigation (`app.rs:90-108`)

`ConvertFocus` is a 4-variant enum (`Source`, `Metadata`, `Format`, `OutputOptions`) with `next()` and `prev()` methods that are **unconditional hardcoded match chains** — they always cycle through all 4 panes in order. Tab/BackTab in `handle_convert_key()` (line 234-239) calls these directly.

### Intra-pane navigation

**Format pane:** `FormatField` has `next_for(is_dsd)` / `prev_for(is_dsd)` (lines 601-611) cycling 6 visible rows. `FormatState::focused_pill_mut()` (line 1044) returns the active pill for Left/Right stepping. `format_interactions.rs` handles all side effects: auto-dither (`app.rs:853-889`), constraint cascade (`app.rs:952-1031`), DSD↔PCM defaults. This subsystem is **self-contained and height-agnostic**.

**Output Options:** `OutputOptionsField::next()` / `prev()` (lines 624-641) cycles 4 fields. Left/Right on merge pill.

**Source pane:** Up/Down/Space for multi-track cursor and selection (`keybindings.rs:242-287`). Already has its own scroll state in `SourceMode::MultiTrack { scroll, cursor, .. }`.

**Metadata pane:** No intra-pane navigation. Fields are clickable via `TuiButton::MetadataField(kind)` which opens a TextEdit overlay.

### `advanced_open` — dead flag

Every pane state carries `advanced_open: bool` (`app.rs:539, 657, 1131, 1142`). The `a` key and `AdvancedToggle` mouse click toggle it (`keybindings.rs:369-383, 9757-9773`). But **none of the four draw functions read this field**. It is fully wired into state and input handling with zero rendering effect. The toggle and click target remain visible on collapsed title bars and must behave correctly (see section 1.8).

### Collapsible queue sub-lines reference (`draw_queue.rs`, commit `4fa6e80`)

The queue screen's per-item track collapse provides the UX vocabulary for this redesign:

| Layer | Implementation |
|-------|---------------|
| State | `tracks_collapsed: bool` on `ConversionItem` (`queue.rs:184`), `#[serde(skip)]` |
| Toggle | `ConversionManager::toggle_track_collapse(id)` flips bool through RwLock |
| Keyboard | Tab key in queue context (`keybindings.rs:1243-1250`) |
| Mouse | Click 2-char `▼`/`▶` indicator at left edge → `QueueItemExpand` button (`draw_queue.rs:74-79`) |
| Context menu | Dynamic label "Expand/Collapse tracks" → `ContextAction::ToggleTrackCollapse` (`context_menu.rs:620-625`) |
| Expanded | `▼` indicator, up to 5 per-track sub-lines with progress, overflow "...and N more" |
| Collapsed | `▶` indicator, single summary line "N tracks converting…" |

### Existing scroll patterns

The codebase has **zero use of ratatui's `Scrollbar`, `ScrollbarState`, or `List` widgets**. All scrolling is hand-rolled in two patterns:

**Pattern A — simple offset** (most overlays): Single `scroll: usize` field, clamped in renderer via `scroll.min(total.saturating_sub(visible))`, rendered via `.skip(scroll).take(visible)`.

**Pattern B — vim-smooth cursor+scroll** (browse, queue, metadata editor, batch list): Dual state `(cursor: usize, scroll: usize)` with an `ensure_visible()` method that only shifts scroll when cursor exits the visible range.

### Convert screen context menu (`context_menu.rs:570-608`)

`build_convert_menu()` currently provides: Commit / Commit+start (if source loaded), Expand batch (if batch mode), Browse for source, Presets submenu by codec. No per-pane layout actions.

### State persistence

`ConvertState` persists across screen switches (Convert → Queue → Convert). Setting `app.current_screen` does not reset any state. The only reset path is Esc from the `:queue` batch flow when `previous_screen` is set (`keybindings.rs:146-155`). This reset path must also reset `layout` to `ConvertLayout::Default` and `pane_title_last_click` to `None` — otherwise a maximized layout and stale click state would persist after cancelling a batch review.

### Ratatui capabilities

The project uses **ratatui 0.26.3** (`Cargo.lock:1644`). `Constraint::Fill(u16)` is available — it distributes excess space proportionally among Fill elements after all other constraints are satisfied. Example: `[Length(5), Fill(1), Fill(2)]` gives Fill(1) one-third and Fill(2) two-thirds of remaining space. Currently no `Fill` or `Percentage` constraints are used on the convert screen.

### MetadataState (`app.rs:1125-1132`)

```rust
pub struct MetadataState {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub advanced_open: bool,
}
```

Five optional string fields, no file list, no cursor, no scroll. The metadata pane is a static display of the current file's tags. In batch mode, it shows the *cursor file's* metadata (populated via `AudioProbeComplete` message when the batch cursor moves).

### Source batch summary

`SourceMode::Batch` (`app.rs:235-247`) already carries precomputed summary data: `total_size: u64`, `album_count: usize`, `format_histogram: Vec<(AudioFormat, usize)>`. The source pane's `render_batch()` (`draw_source.rs:268-378`) already renders a 2-line summary (file count + albums + size, format histogram) followed by an inline file list. `source_pane_height()` (`draw_source.rs:32-72`) dynamically computes height from 6 (base) to 12 (max).

## Non-negotiable constraints

1. **Two-pass rendering pattern must be preserved.** Pass 1 draws with immutable state refs. Pass 2 registers buttons with mutable `button_map`. No draw function may mutate app state.

2. **`PillState<T>` must not be modified.** The generic pill selector is used across the format pane, output options pane merge mode, and (via button registration) the source pane. Its navigation (`select_next`, `select_prev`, `select_value`) and rendering (`render_pill_spans`) must remain intact.

3. **Format constraint cascade must continue to work.** `FormatState::apply_format_constraints()` (`app.rs:952-1031`), `after_user_selection()` (`app.rs:818-849`), auto-dither (`app.rs:853-889`), and `format_interactions.rs` are a self-contained subsystem. The redesign must not interfere with pill enable/disable logic, DSD↔PCM transitions, or the side-effect chain.

4. **Queue screen, progress pipeline, and conversion logic are off-limits.** This is purely a TUI layout change for the convert screen.

5. **Three interaction paths for maximize/restore.** The maximize toggle must have a keyboard path, a mouse click path, and a context menu path. This follows the queue sub-line precedent (`tracks_collapsed` has all three: Tab key, `QueueItemExpand` mouse target, and `ContextAction::ToggleTrackCollapse`).

6. **Colon commands for state-changing actions.** Maximize/restore and advanced toggle use colon commands (`:max`, `:adv`), not bare keys. This follows the project convention where bare keys are reserved for navigation (Tab, Up/Down, Left/Right, Space) and colon commands handle state changes (`:commit`, `:browse`, `:edit-tags`). The existing bare `a` key for advanced toggle is removed and replaced by `:adv`.

7. **No ratatui `Scrollbar`, `ScrollbarState`, or `List` widgets.** Follow the existing hand-rolled scroll patterns (Pattern A or B described above) for consistency.

## Design model: Default + Maximize

The convert screen has two layout modes:

**Default mode:** All 4 panes rendered at their standard fixed heights (the current layout). Every pane title bar shows a `◻` indicator meaning "click to maximize me."

**Maximized mode:** One pane is maximized — it receives `Fill` and gets the bulk of the screen. The other 3 panes collapse to a single title-bar line each (`╒ ◻ format ══════ advanced ╕`). The maximized pane's title bar shows `◼` meaning "click to restore to default."

**State model:** A single enum on `ConvertState`, not per-pane booleans:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConvertLayout {
    Default,                    // all panes at standard heights
    Maximized(ConvertFocus),    // one pane expanded, other 3 title-bar-only
}
```

**Transitions:**

| From | Action | To |
|------|--------|----|
| Default | Click `◻` on any pane (or `:max`) | Maximized(that pane) |
| Maximized(X) | Click `◼` on pane X (or `:max` while focused on X) | Default |
| Maximized(X) | Click `◻` on collapsed pane Y (or Tab to Y, `:max`) | Maximized(Y) |

There is no "all collapsed" state. Clicking `◼` on the maximized pane always returns to Default. No separate reset button is needed.

## What this task delivers

### Feature 1: Maximize / restore layout

#### 1.1 State changes

**File: `src/tui/app.rs`**

Add the `ConvertLayout` enum (near `ConvertFocus`, after line 108):

```rust
/// Convert screen layout mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConvertLayout {
    /// All 4 panes at their standard fixed heights.
    Default,
    /// One pane maximized (gets Fill), other 3 collapsed to title bars.
    Maximized(ConvertFocus),
}
```

Add a `layout` field to `ConvertState` (line 1165):

```rust
pub struct ConvertState {
    pub source: SourceState,
    pub metadata: MetadataState,
    pub format: FormatState,
    pub output_options: OutputOptionsState,
    pub focus: ConvertFocus,
    pub layout: ConvertLayout,  // NEW
}
```

Default to `ConvertLayout::Default` in `ConvertState::new()`.

Add convenience methods to `ConvertState`:

```rust
/// Whether a specific pane is currently collapsed to its title bar.
pub fn is_collapsed(&self, pane: ConvertFocus) -> bool {
    match self.layout {
        ConvertLayout::Default => false,
        ConvertLayout::Maximized(maximized) => pane != maximized,
    }
}

/// Whether a specific pane is currently maximized.
pub fn is_maximized(&self, pane: ConvertFocus) -> bool {
    self.layout == ConvertLayout::Maximized(pane)
}

/// Toggle layout: if the pane is maximized, restore to Default.
/// If Default or another pane is maximized, maximize this pane.
pub fn toggle_maximize(&mut self, pane: ConvertFocus) {
    self.layout = match self.layout {
        ConvertLayout::Maximized(current) if current == pane => ConvertLayout::Default,
        _ => ConvertLayout::Maximized(pane),
    };
}
```

#### 1.2 Focus navigation

**File: `src/tui/app.rs`** — `ConvertFocus` impl (lines 90-108)

Tab/BackTab cycle between all 4 panes **unconditionally** — the existing `next()` / `prev()` methods are unchanged. In Maximized mode, Tab moves focus to collapsed title bars. The user can then type `:max` on a collapsed pane to switch the maximize target, or use intra-pane keys (which are no-ops on collapsed panes). This keeps navigation predictable.

**No changes to `ConvertFocus::next()` / `prev()`.**

**File: `src/tui/keybindings.rs`** — `handle_convert_key()` Tab/BackTab (lines 233-239)

Unchanged — Tab still calls `app.convert.focus.next()`.

#### 1.3 Toggle input handling

**Keyboard** — **File: `src/tui/command.rs`**

Add a new `Command::Maximize` variant. Register `:maximize` and `:max` as aliases in the command parser.

```rust
Command::Maximize => {
    app.convert.toggle_maximize(app.convert.focus);
}
```

This handles all three transitions from the state table:
- Default + `:max` on Source → Maximized(Source)
- Maximized(Source) + `:max` on Source → Default
- Maximized(Source) + Tab to Format + `:max` → Maximized(Format)

No bare key — the user types `:max` (or `:maximize`) in command mode.

**Mouse** — Add a new `TuiButton` variant for the `◻`/`◼` indicator.

**File: `src/tui/button_map.rs`** — Add to `TuiButton` enum (after line 49):

```rust
/// Collapse/maximize toggle indicator (◻/◼) in pane title bars.
MaximizeToggle(ConvertFocus),
```

Add to the `screen()` match arm alongside the other convert screen buttons.

**File: `src/tui/convert_screen.rs`** — `register_buttons()`. Add a `register_maximize_toggle()` function analogous to `register_advanced_toggle()` (line 322). Register the clickable `◻`/`◼` indicator area in the pane title bar, to the right of the `╒` corner. Register it for both expanded and collapsed panes (both have the indicator).

**File: `src/tui/keybindings.rs`** — mouse dispatch (after line 9773). Add handlers:

```rust
// Single click on ◻/◼ indicator: toggle maximize/restore.
TuiButton::MaximizeToggle(pane) => {
    app.convert.toggle_maximize(pane);
    if app.convert.is_maximized(pane) {
        app.convert.focus = pane;
    }
}
```

**Double-click on pane title bar** — **File: `src/tui/app.rs`**

Add a double-click timestamp to `ConvertState`:

```rust
/// Last pane title bar click: (which pane, when). Used for double-click
/// detection on title bars to toggle maximize/restore.
pub pane_title_last_click: Option<(ConvertFocus, std::time::Instant)>,
```

Default to `None` in `ConvertState::new()`.

**File: `src/tui/keybindings.rs`** — in the existing `TuiButton::Pane(focus)` handler (line 9614), add double-click detection:

```rust
TuiButton::Pane(focus) => {
    let now = std::time::Instant::now();
    let is_double = app.convert.pane_title_last_click
        .map(|(prev_focus, prev_time)| {
            prev_focus == focus && now.duration_since(prev_time).as_millis() < 300
        })
        .unwrap_or(false);

    if is_double {
        app.convert.toggle_maximize(focus);
        app.convert.pane_title_last_click = None;
    } else {
        app.convert.focus = focus;
        app.convert.pane_title_last_click = Some((focus, now));
    }
    app.current_screen = AppScreen::Convert;
}
```

Single click focuses the pane (existing behavior). Double click within 300ms on the same pane title toggles maximize/restore (like double-clicking a desktop window title bar). This works on both expanded and collapsed title bars since `TuiButton::Pane(focus)` is registered for both.

**Click target summary for pane title bars:**

| Target | Single click | Double click |
|--------|-------------|-------------|
| `◻`/`◼` box | Toggle maximize/restore | — |
| Title text area | Focus this pane | Toggle maximize/restore |
| "advanced" text | Toggle advanced (compound if collapsed) | — |

**Context menu** — **File: `src/tui/context_menu.rs`**

Add a new `ContextAction` variant:

```rust
TogglePaneMaximize(ConvertFocus),
```

In `build_convert_menu()` (line 570), add per-pane entries with dynamic labels:

```rust
let pane_items = [
    (ConvertFocus::Source, "Source"),
    (ConvertFocus::Metadata, "Metadata"),
    (ConvertFocus::Format, "Format"),
    (ConvertFocus::OutputOptions, "Output Options"),
];
items.push(separator());
for (focus, name) in &pane_items {
    let label = if app.convert.is_maximized(*focus) {
        format!("Restore {}", name)
    } else {
        format!("Maximize {}", name)
    };
    items.push(item(&label, ContextAction::TogglePaneMaximize(*focus)));
}
```

Note: `item()` takes `&str` and calls `.to_string()` internally (`context_menu.rs:234-236`), so dynamic `format!()` labels work. `ContextAction` derives `Clone, Debug` (`context_menu.rs:65`). `ConvertFocus` derives `Clone, Copy, Debug, PartialEq` — all compatible.

In the action dispatch section. Update focus for consistency with the mouse handler:

```rust
ContextAction::TogglePaneMaximize(pane) => {
    app.convert.toggle_maximize(pane);
    if app.convert.is_maximized(pane) {
        app.convert.focus = pane;
    }
}
```

#### 1.4 Collapsed guard on intra-pane handlers

**File: `src/tui/keybindings.rs`** — existing bare-key handlers in `handle_convert_key()`

All intra-pane navigation handlers must add a guard so they're no-ops when their pane is collapsed. Use `!app.convert.is_collapsed(ConvertFocus::X)`:

| Handler | Lines | Add guard |
|---------|-------|-----------|
| Source Up/Down (multi-track) | 242-274 | `&& !app.convert.is_collapsed(ConvertFocus::Source)` |
| Source Space (toggle selection) | 275-287 | same |
| Source Enter/e (edit/expand) | 349-366 | same |
| Format Up/Down (field focus) | 290-298 | `&& !app.convert.is_collapsed(ConvertFocus::Format)` |
| Format Left/Right (pill step) | 302-313 | same |
| Output Options Up/Down | 316-324 | `&& !app.convert.is_collapsed(ConvertFocus::OutputOptions)` |
| Output Options Left/Right (merge) | 328-341 | same |

When a pane is collapsed to its title bar, its navigation keys are no-ops. The bare `a` key handler (lines 369-383) is **removed entirely** — replaced by the `:adv` colon command (section 1.8).

#### 1.5 Layout changes

**File: `src/tui/convert_screen.rs`** — `draw_convert_screen()` (lines 24-93)

Replace the fixed constraints for the 4 panes with layout-dependent constraints:

```rust
pub fn draw_convert_screen(f: &mut Frame, area: Rect, app: &mut AppState) {
    let layout = app.convert.layout;

    let pane_constraint = |pane: ConvertFocus, default_height: u16| -> Constraint {
        match layout {
            ConvertLayout::Default => Constraint::Length(default_height),
            ConvertLayout::Maximized(maximized) if maximized == pane => {
                Constraint::Fill(1) // this pane gets all remaining space
            }
            ConvertLayout::Maximized(_) => {
                Constraint::Length(1) // collapsed to title bar
            }
        }
    };

    let source_h = super::draw_source::source_pane_height(
        &app.convert.source.mode, area.width,
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),                          // header
            Constraint::Length(1),                          // blank
            Constraint::Length(1),                          // preset bar
            Constraint::Length(1),                          // blank
            pane_constraint(ConvertFocus::Source, source_h),      // source
            pane_constraint(ConvertFocus::Metadata, 5),           // metadata
            pane_constraint(ConvertFocus::Format, 10),            // format
            pane_constraint(ConvertFocus::OutputOptions, 7),      // output options
            Constraint::Length(1),                          // blank
            Constraint::Length(1),                          // action bar
            Constraint::Min(0),                             // absorb
            Constraint::Length(2),                          // footer
        ])
        .split(area);

    // ... draw and register calls follow ...
}
```

**Height examples (48-row terminal):**

| State | Pane heights (S / M / F / O) | Notes |
|-------|------------------------------|-------|
| Default, source=6 | 6 / 5 / 10 / 7 = 28 | Same as today. 6 surplus rows to absorber. |
| Default, source=12 | 12 / 5 / 10 / 7 = 34 | Same as today. 0 surplus. |
| Maximized(Format) | 1 / 1 / **Fill→31** / 1 | 14 overhead + 3 title bars = 17. Format gets 31 rows. |
| Maximized(Metadata), batch | 1 / **Fill→31** / 1 / 1 | Metadata file list gets 31 rows — ~29 visible files. |
| Maximized(Source), batch | **Fill→31** / 1 / 1 / 1 | Source file list fully visible even for large batches. |

#### 1.6 Collapsed title bar rendering

When a pane is collapsed (not the maximized pane), it renders as a **single title-bar line** — the top border of the pane with the `◻` indicator, title, and "advanced" toggle, but no content and no bottom border:

```
╒ ◻ format ══════════════════════════════════════════════ advanced ╕
```

This is the existing top border format from each draw file, with `◻` inserted after the left corner, the corner characters changed from `┌`/`┐` to `╒`/`╕`, the fill character changed from `─` (light horizontal) to `═` (double horizontal), and the content/bottom rows omitted.

**Title bar fill character: `═` (U+2550, double horizontal).** The double-line fill evokes classic Macintosh title bar stripes and visually distinguishes the title bar as interactive chrome (clickable, double-clickable) from structural `─` borders used elsewhere in the TUI. All pane title bars — both expanded and collapsed — use `═` for the fill between the title text and "advanced."

**Title bar corner characters: `╒` (U+2552) and `╕` (U+2555).** These are the box-drawing corners designed for single-vertical + double-horizontal junctions. They connect cleanly with `═` to their sides and `│` below. The existing `┌`/`┐` corners (single-horizontal) would create a visible gap at the junction with `═`. The bottom border `└───┘` remains unchanged — it uses `─` (single horizontal) matching the `│` side borders.

**File: `src/tui/draw_source.rs`** — Add `pub fn draw_source_title_bar(f, area, state, focused)`
**File: `src/tui/draw_metadata.rs`** — Add `pub fn draw_metadata_title_bar(f, area, state, focused)`
**File: `src/tui/draw_output.rs`** — Add `pub fn draw_format_title_bar(f, area, state, focused)`
**File: `src/tui/draw_output_options.rs`** — Add `pub fn draw_output_options_title_bar(f, area, state, focused)`

Each renders a single line:
```
╒ ◻ <title> ══════════════════════════════════ advanced ╕
```

Title bar functions **always render `◻`** — they are only called for collapsed panes, which are never maximized. No `maximized` parameter needed.

Use the pane's border color (amber/purple/green/cyan) when focused, `TEXT_DIM` when unfocused. The `◻` is the `MaximizeToggle` click target. The "advanced" text is the `AdvancedToggle` click target (see section 1.8).

#### 1.7 Expanded pane indicator

When a pane is maximized (the one big pane), show `◼` in its title bar **inside the border, immediately after `╒`**:

```
╒ ◼ source ══════════════════════════════════════════════ advanced ╕
```

When in Default mode (all panes at standard heights), all panes show `◻`:

```
╒ ◻ source ══════════════════════════════════════════════ advanced ╕
```

The `◻` in Default mode means "click to maximize this pane." The `◼` on the maximized pane means "click to restore to default." The `◻` on a collapsed title bar means "click to maximize this pane instead."

Modify the top border rendering in all 4 draw files to accept a `maximized: bool` parameter and render `◼` or `◻` accordingly.

#### 1.8 Advanced toggle on collapsed title bars

The "advanced" click target remains visible on collapsed title bars. Both the mouse click and the `:adv` colon command use the same compound logic:

When advanced is toggled on a **collapsed** pane:
1. **Maximize the pane** — switch to `Maximized(that pane)`.
2. **Toggle `advanced_open`** — open the advanced section.

This is a compound action. The intent is unambiguous: toggling "advanced" on a pane you can't see the basic content of means "show me everything about this pane."

When advanced is toggled on a pane that is **already visible** (Default mode or the maximized pane), only `advanced_open` toggles — no layout change.

**File: `src/tui/command.rs`** — Add a new `Command::Advanced` variant. Register `:advanced` and `:adv` as aliases in the command parser.

```rust
Command::Advanced => {
    let focus = app.convert.focus;
    // If this pane is collapsed, maximize it first.
    if app.convert.is_collapsed(focus) {
        app.convert.layout = ConvertLayout::Maximized(focus);
    }
    // Toggle advanced on the focused pane.
    match focus {
        ConvertFocus::Source => {
            app.convert.source.advanced_open = !app.convert.source.advanced_open;
        }
        ConvertFocus::Metadata => {
            app.convert.metadata.advanced_open = !app.convert.metadata.advanced_open;
        }
        ConvertFocus::Format => {
            app.convert.format.advanced_open = !app.convert.format.advanced_open;
        }
        ConvertFocus::OutputOptions => {
            app.convert.output_options.advanced_open =
                !app.convert.output_options.advanced_open;
        }
    }
}
```

**File: `src/tui/keybindings.rs`** — modify `AdvancedToggle` mouse handler (lines 9757-9773) to use the same compound logic:

```rust
TuiButton::AdvancedToggle(focus) => {
    if app.convert.is_collapsed(focus) {
        app.convert.layout = ConvertLayout::Maximized(focus);
    }
    app.convert.focus = focus;
    match focus {
        ConvertFocus::Source => {
            app.convert.source.advanced_open = !app.convert.source.advanced_open;
        }
        // ... same for other 3 panes ...
    }
}
```

**Remove the existing bare `a` key handler** (lines 369-383 in `handle_convert_key()`). The `:adv` colon command replaces it entirely.

#### 1.9 Draw dispatch

**File: `src/tui/convert_screen.rs`** — In `draw_convert_screen()`, dispatch to title-bar or full draw functions based on layout:

```rust
let source_collapsed = app.convert.is_collapsed(ConvertFocus::Source);
let metadata_collapsed = app.convert.is_collapsed(ConvertFocus::Metadata);
let format_collapsed = app.convert.is_collapsed(ConvertFocus::Format);
let output_collapsed = app.convert.is_collapsed(ConvertFocus::OutputOptions);

// Title bar functions always render ◻ (collapsed panes are never maximized).
// Full draw functions receive a `maximized: bool` to choose ◻ vs ◼ in their title.
if source_collapsed {
    draw_source_title_bar(f, chunks[4], &app.convert.source, focused_source);
} else {
    let maximized = app.convert.is_maximized(ConvertFocus::Source);
    draw_source_pane(f, chunks[4], &app.convert.source, focused_source, maximized);
}

// Metadata has an additional source_mode parameter for file-list rendering.
if metadata_collapsed {
    draw_metadata_title_bar(f, chunks[5], &app.convert.metadata, focused_metadata);
} else {
    let maximized = app.convert.is_maximized(ConvertFocus::Metadata);
    draw_metadata_pane(
        f, chunks[5], &app.convert.metadata, &app.convert.source.mode,
        focused_metadata, maximized,
    );
}

// Format and output options follow the source pattern (maximized: bool only).
// ... same as source for chunks[6] (format) and chunks[7] (output options)
```

The maximized pane's draw function receives a potentially much larger `Rect` than usual (e.g., 31 rows instead of 10 for format). All four draw functions already have minimum height guards (`if area.height < N { return; }`) and use the provided `area` for layout — they will adapt. The source and metadata panes already handle variable heights dynamically.

**Format and output options panes need modification:** Their existing draw functions generate a fixed number of lines (10 and 7 respectively) and stop. When maximized, the extra rows below the last content row would be unrendered (showing background). Modify both `draw_format_pane()` and `draw_output_options_pane()` to extend the box when `area.height` exceeds the standard content line count: push the bottom border (`└───┘`) down to the last allocated row, and fill the gap between the last content row and the bottom border with blank bordered lines (`│` + padding + `│`). This keeps the box-drawing structure intact — all content is inside the box, the bottom border is always the last row.

#### 1.10 Button registration guards

**File: `src/tui/convert_screen.rs`** — `register_buttons()` (line 97)

Wrap each pane's button registration block in a layout guard. When collapsed:
- Register `TuiButton::Pane(focus)` for the 1-line title bar area (click to focus).
- Register `TuiButton::MaximizeToggle(focus)` for the `◻` indicator.
- Register `TuiButton::AdvancedToggle(focus)` for the "advanced" text.
- Do NOT register any pill buttons, text field buttons, or metadata field buttons.

When expanded (Default or Maximized):
- Register everything as before.
- Register `TuiButton::MaximizeToggle(focus)` for the `◻`/`◼` indicator.

### Feature 2: Scroll support within maximized panes

When a pane is maximized and receives more space than its standard height, the extra rows are available for content. This primarily benefits the source pane (file lists) and the metadata pane (Feature 3's file list).

#### 2.1 Source pane

Both `render_batch()` and `render_multi_track()` already accept `pane_height: u16` and compute `track_area` dynamically. When maximized with 31 rows instead of 6-12, the source pane will show more files/tracks automatically. `source_pane_height()` is only used in Default mode to compute the ideal `Length`. **No changes needed** — the existing rendering adapts to whatever height it receives.

#### 2.2 Metadata pane (delivered by Feature 3)

See Feature 3 below.

#### 2.3 Format and output options panes

These panes have fixed internal layouts (10 rows and 7 rows respectively). When maximized, they receive more space than needed. The extra rows below the last content row should render as empty bordered lines (matching the existing blank rows within these panes). No scroll support needed — there's nothing to scroll. If `advanced_open` is later implemented to add content rows, scroll can be added at that time following Pattern A.

### Feature 3: Metadata pane as scrollable file list

Transform the metadata pane from a static 5-field display into a scrollable file list when in batch or multi-track mode. Click a file to view/edit its metadata. Single-file mode retains the current layout.

#### 3.1 State changes — cursor reuse, scroll addition

The metadata file list cursor **must be the same logical cursor** as the source pane's cursor to avoid desync. `SourceMode::Batch` already has `cursor: usize` and `SourceMode::MultiTrack` already has `cursor: usize`. Moving files in either pane moves the same cursor. This means:
- Source pane shows audio properties (format, sample rate, duration) for the cursor file.
- Metadata pane shows tags (title, artist, album) for the same cursor file.
- Up/Down in the metadata pane moves `SourceMode::Batch.cursor` / `MultiTrack.cursor` — the same field the source pane reads.

**No new `file_cursor` field on `MetadataState`.** The cursor lives on `SourceMode` where it already exists.

**File: `src/tui/app.rs`** — Add only a scroll offset to `MetadataState` (line 1125):

```rust
pub struct MetadataState {
    // Existing fields — unchanged
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub advanced_open: bool,

    // New: scroll offset for file list rendering (Pattern B — vim-smooth).
    // The cursor is SourceMode::Batch.cursor / MultiTrack.cursor.
    pub file_scroll: usize,
}
```

Default `file_scroll` to `0` in `MetadataState::default()`.

**Reset on source change:** Wherever `app.convert.source.mode` is reassigned (e.g., `:e` command, `:queue` command, `AudioProbeComplete` for SACD/CUE), also reset `app.convert.metadata.file_scroll = 0`. The existing code already resets `MetadataState` fields at these sites — add the scroll reset alongside.

#### 3.2 Rendering changes

**File: `src/tui/draw_metadata.rs`** — Modify `draw_metadata_pane()`

Add a `source_mode: &SourceMode` parameter. The call site in `convert_screen.rs` (line 60) must thread `&app.convert.source.mode`.

**Single-file / empty mode:** Render exactly as today — title, artist+album, genre+year. If the pane receives more than 5 rows (possible when maximized), pad with empty bordered lines.

**Batch mode:** Render a scrollable file list. Each row shows a filename (truncated) from `SourceMode::Batch.paths` with the cursor file's metadata summary (artist · album) on the right. The cursor row is highlighted in purple.

**Multi-track mode:** Same concept — list `MultiTrackEntry` items (track number + title).

Layout within the expanded metadata pane (batch mode, height H):
```
Row 0:     ╒ ◼ metadata ═════════ advanced ╕   (top border, ◼ if maximized)
Row 1..H-2: File list (scrollable, vim-smooth)
Row H-1:   └──────────────────────────────┘    (bottom border)
```

Each file list row:
```
│  1. song_name.flac              Artist · Album │
```

Cursor row highlighted in purple (the metadata pane's focus color).

#### 3.3 Input handling

**File: `src/tui/keybindings.rs`** — in `handle_convert_key()`, add a metadata pane section (after the Source pane handlers):

```rust
// Within Metadata pane + batch mode (not collapsed): Up/Down moves the
// shared batch cursor (same cursor the source pane reads).
(KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE)
    if app.convert.focus == ConvertFocus::Metadata
        && !app.convert.is_collapsed(ConvertFocus::Metadata)
        && app.convert.source.mode.is_batch() =>
{
    if let SourceMode::Batch { cursor, .. } = &mut app.convert.source.mode {
        if *cursor > 0 {
            *cursor -= 1;
            // ensure_visible: adjust metadata.file_scroll
        }
    }
    // Trigger debounced probe for the newly selected file (reuse
    // existing batch_probe_debounce on SourceState).
}
```

Similar for Down (with bounds check against `paths.len()`), and for `MultiTrack` mode (moving `MultiTrack.cursor`).

**Enter on a file row:** Opens the metadata editor overlay for the cursor file. Reuse the existing tag editor opening mechanism — the `:edit-tags` command (`command.rs`) and `BrowseInfoEditTags` button already construct a `MetadataEditorState` from a file path via `read_all_tags()` in `probe.rs`. The metadata pane's Enter handler should follow the same code path: read tags from the cursor file, construct `MetadataEditorState`, set `app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state))`.

**Mouse:** Register `TuiButton::MetadataFileRow(usize)` for each visible file row. Click sets the shared source cursor and updates metadata display. Double-click opens the editor overlay.

**Scroll wheel:** When hovering over the metadata pane in batch/multi-track mode, scroll wheel moves the shared source cursor by ±3, which triggers `ensure_visible` on `metadata.file_scroll`. Do NOT directly adjust `file_scroll` — that could push the cursor out of the visible range. This matches the overlay scroll wheel convention (e.g., browse screen's `scroll_viewport`).

**Collapsed guard:** All metadata pane handlers use `!app.convert.is_collapsed(ConvertFocus::Metadata)` in their guard. This correctly evaluates against the `ConvertLayout` enum — no per-pane boolean needed.

#### 3.4 Metadata update on cursor change

Since the metadata file list shares the source cursor (`SourceMode::Batch.cursor`), moving the cursor in the metadata pane triggers the **same probe path** that already exists for source pane batch navigation. The flow is:

1. Cursor moves (Up/Down in metadata pane or source pane — same field).
2. `batch_probe_debounce` fires after 150ms of no further cursor movement (`SourceState`, line 546).
3. Event loop sends async probe request.
4. `AudioProbeComplete` message arrives, updating `cursor_info` (source pane) and `cursor_metadata` (which populates `MetadataState` fields).

**No new message types or probe paths needed.** The existing batch cursor probe already reads both audio properties and metadata for the cursor file. The metadata pane simply reads the same `MetadataState` fields that the probe populates.

The only new work is triggering the debounce when the cursor moves from the metadata pane (currently only triggered from source pane key handlers). Move the debounce-set logic into a shared helper that both panes call.

#### 3.5 Button registration

**File: `src/tui/convert_screen.rs`** — in `register_buttons()`, replace the metadata field buttons block (lines 283-316) with layout- and mode-dependent registration:

- **Collapsed (title bar only):** Register `TuiButton::Pane(ConvertFocus::Metadata)`, `MaximizeToggle`, and `AdvancedToggle` only.
- **Expanded, Single/Empty:** Register `MetadataField` buttons as before.
- **Expanded, Batch/MultiTrack:** Register `MetadataFileRow(index)` buttons for each visible file row (index is absolute, accounting for `metadata.file_scroll`). Do NOT register the old field buttons (they don't exist in this view).

### Feature 4: Source pane batch summary

The source pane in batch mode already renders a 2-line summary (file count + size + format histogram) followed by an inline file list (`draw_source.rs:268-378`). No changes needed for the default layout. When maximized, the source pane receives more rows and `render_batch()` automatically shows more files (the existing `pane_height` parameter drives this).

## File-level change summary

| File | Changes |
|------|---------|
| `src/tui/app.rs` | New `ConvertLayout` enum. Add `layout: ConvertLayout` and `pane_title_last_click: Option<(ConvertFocus, Instant)>` to `ConvertState`. Add `is_collapsed()`, `is_maximized()`, `toggle_maximize()` methods. Add `file_scroll: usize` to `MetadataState` (cursor reuses `SourceMode` cursor). |
| `src/tui/convert_screen.rs` | Layout-dependent constraint computation via `pane_constraint()`. Conditional draw dispatch (title-bar vs full). Conditional button registration per layout state. New `register_maximize_toggle()`. Thread `&app.convert.source.mode` to `draw_metadata_pane()` call (new parameter). |
| `src/tui/draw_source.rs` | New `draw_source_title_bar()`. Top border: insert `◻`/`◼` indicator, change corners `┌`/`┐` → `╒`/`╕`, change fill `─` → `═` (new `maximized: bool` parameter). |
| `src/tui/draw_metadata.rs` | New `draw_metadata_title_bar()`. Top border: same corner/fill/indicator changes. Mode-dependent rendering (single-file vs file-list). New file-list rendering for batch/multi-track. Add `source_mode: &SourceMode` parameter. |
| `src/tui/draw_output.rs` | New `draw_format_title_bar()`. Top border: same corner/fill/indicator changes. Handle extra rows when maximized (empty bordered lines). |
| `src/tui/draw_output_options.rs` | New `draw_output_options_title_bar()`. Top border: same corner/fill/indicator changes. Handle extra rows when maximized. |
| `src/tui/button_map.rs` | New `TuiButton::MaximizeToggle(ConvertFocus)` variant. New `TuiButton::MetadataFileRow(usize)` variant. Update `screen()` match. |
| `src/tui/command.rs` | New `Command::Maximize` (`:maximize` / `:max`). New `Command::Advanced` (`:advanced` / `:adv`) with compound maximize-if-collapsed logic. Register both in the command parser. |
| `src/tui/keybindings.rs` | **Remove** bare `a` key handler (lines 369-383). Add `!is_collapsed()` guards to all existing intra-pane navigation handlers (format, output options, source). Modify `AdvancedToggle` mouse handler to use compound maximize-if-collapsed logic. Add double-click detection to `TuiButton::Pane(focus)` handler for title bar maximize/restore. New metadata pane Up/Down/Enter handlers for file list navigation (moves shared source cursor). Mouse handler for `MaximizeToggle` and `MetadataFileRow`. |
| `src/tui/context_menu.rs` | New `ContextAction::TogglePaneMaximize(ConvertFocus)`. Per-pane entries in `build_convert_menu()`. Dispatch handler. |
| `src/tui/draw_footer.rs` | Add `:max` hint to convert screen hint group at **priority 2** (droppable on narrow terminals). |

## What NOT to change

- **`src/tui/pill.rs`** — `PillState<T>` and `render_pill_spans` are untouched.
- **`src/tui/format_interactions.rs`** — Constraint cascade, auto-dither, DSD transitions untouched.
- **`src/tui/draw_queue.rs`** — Queue screen rendering untouched.
- **`src/convert/`** — All conversion logic, pipeline, processor untouched.
- **`src/tui/draw_header.rs`, `draw_preset_bar.rs`** — Untouched.
- **`src/tui/event_loop.rs`, `message.rs`** — No changes to either file. Maximize/restore is synchronous state (set an enum, re-render next frame). No new message types. The metadata file list cursor reuses the existing source batch cursor and its `batch_probe_debounce` mechanism. The debounce timer check in `event_loop.rs` is already generic — it fires for any pending debounce regardless of which key handler set it. The new metadata pane handlers in `keybindings.rs` set the same `batch_probe_debounce` field, so the event loop picks it up automatically.
- **`bordered_line()` duplication** — Do not extract to a shared utility in this task. That's a separate cleanup.

## Test and verification

### Manual verification checklist

1. **Default layout** — verify all 4 panes render at their standard fixed heights, identical to the current layout. Each title bar shows `◻` indicator.

2. **Maximize each pane** — type `:max` on each focused pane in Default mode. Verify:
   - That pane expands to fill available space, indicator changes to `◼`
   - Other 3 panes collapse to title-bar lines with `◻` indicators
   - All pills, text fields, and buttons work in the maximized pane
   - Intra-pane keys (Up/Down/Left/Right) work in the maximized pane

3. **Restore to default** — type `:max` on the maximized pane. Verify:
   - All 4 panes return to standard heights
   - All indicators return to `◻`
   - No state loss (pill selections, metadata fields, source file preserved)

4. **Switch maximize target** — while pane X is maximized, Tab to collapsed pane Y, type `:max`. Verify:
   - Layout switches to Maximized(Y)
   - Pane X collapses to title bar, pane Y expands

5. **Mouse click on `◻`/`◼` indicators** — click `◻` to maximize, click `◼` to restore. Verify all three transitions work via single click on the indicator box.

6. **Double-click on pane title bar** — double-click the title text area of a pane. Verify:
   - In Default mode: pane maximizes
   - On maximized pane: restores to Default
   - On collapsed title bar: maximizes that pane (switches target)
   - Single click only focuses (no maximize)

7. **Right-click context menu** — verify "Maximize/Restore [pane]" entries appear with correct dynamic labels.

8. **Advanced on collapsed title bar (mouse)** — click "advanced" on a collapsed pane. Verify:
   - Pane maximizes AND `advanced_open` toggles to true (compound action)
   - Click "advanced" again (pane now maximized): only toggles `advanced_open`, no layout change

9. **Advanced on collapsed pane (keyboard)** — Tab to a collapsed title bar, type `:adv`. Verify same compound behavior as mouse click.

10. **Intra-pane keys on collapsed panes** — Tab to a collapsed title bar, press Up/Down/Left/Right. Verify they are no-ops (no invisible state mutation).

11. **Format pane maximize/restore** — after maximizing and restoring:
    - Pill selections preserved
    - Constraint cascade still works (change format, verify rate/depth/dither update)
    - DSD↔PCM transition still switches visible rows

12. **Metadata file list (batch mode)** — load a batch of files, maximize metadata pane with `:max`:
    - File list visible with cursor
    - Up/Down navigates files (moves the shared source cursor)
    - Source pane updates in sync when restored to Default
    - Enter opens metadata editor for the selected file
    - Scroll wheel works when file list exceeds visible area

13. **Metadata pane in single-file mode** — verify original 5-field layout still works, no file list.

14. **Screen switching** — switch to Queue and back to Convert:
    - Layout state preserved (Maximized/Default)
    - Focus preserved
    - Expanded pane content unchanged

15. **Small terminal** — resize to ~25 rows:
    - Default mode may clip (same as today)
    - Maximized mode works well: 3 title bars + 1 expanded pane fits in ~20 rows

16. **Metadata file list in Default mode** — load a batch, stay in Default layout:
    - Metadata pane at Length(5) shows ~3 files in the list
    - Up/Down and scroll wheel navigate within the small view
    - Maximizing metadata pane (`:max`) expands to show full list

17. **Esc batch cancel resets layout** — enter batch review via `:queue`, maximize a pane, press Esc:
    - Layout resets to Default (not stuck in Maximized)
    - Source and metadata state cleared as before

### Automated testing

No existing test infrastructure for TUI rendering. Manual verification only for this task.
