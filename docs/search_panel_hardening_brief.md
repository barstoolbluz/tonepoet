# Brief: Search Panel Hardening

## Overview

Four fixes to the Browse screen's search panel:

1. **False positive results from garbage fuzzy matches**: No minimum score threshold
2. **Column headers render above the search panel**: Confusing layout order
3. **Recursive toggle isolated from peer controls**: Hard to discover
4. **Mode and sort controls don't look clickable**: Plain text, no pill/button styling

## Bug 1: False positive fuzzy matches

### Root cause

`execute_search_over_entries()` at `src/tui/browse.rs:3569` and the async recursive worker `spawn_search_async()` at `src/tui/browse.rs:3710` both accept ANY non-None score from `SkimMatcherV2::fuzzy_match()`. There is no minimum score threshold.

Skim performs subsequence matching. When a query's characters are scattered across a very long string, the match is technically valid but semantically garbage. Verified with a unit test:

```
fuzzy_match("genesis - spot the pigeon ep (1977) [flac] {uk  virgin cdf 40} [nimbus]", "epping")
→ Some(52)
```

The subsequence e→p→p→i→n→g spans positions 3→12→18→19→23→45 across 70+ characters. Score 52 is extremely low but accepted because `if let Some(score) = best_score` has no floor.

For reference, a genuine match like `fuzzy_match("the battle of epping forest.flac", "epping")` would score ~150+.

All "Selling England by the Pound" folder names return `None` for "epping" — they only appear via recursive tag search (track title "The Battle of Epping Forest" matches in tags mode).

### Fix

Add a minimum score threshold. Reject matches below it. The threshold should scale with query length — a 3-character query needs a lower bar than a 10-character query because short queries produce higher scores per character. A reasonable heuristic:

```rust
let min_score = (query.len() as i64) * 8;
```

This gives: 3-char query → floor 24, 6-char query → floor 48, 10-char query → floor 80. The "epping" garbage match (score 52) would be borderline — tune the multiplier if needed (e.g., `* 10` would give floor 60, cleanly rejecting score 52).

Apply the threshold in three places:

1. `execute_search_over_entries()` (line 3614): add `if score >= min_score` guard
2. `spawn_search_async()` (line 3850): same guard
3. The async archive search worker (search for `best_score` in the archive search codepath)

### Verification test

Add a unit test that confirms:
- `fuzzy_match("the battle of epping forest.flac", "epping")` passes the threshold
- `fuzzy_match("genesis - spot the pigeon ep ... virgin ...", "epping")` (score 52) is rejected
- Short queries like "ab" still match "abacab" at reasonable scores

## Bug 2: Column headers above search panel

### Current layout

When the search panel is active, `draw_browse_list()` at `src/tui/draw_browse.rs:1268` renders rows in this order:

```
Line 0: ┌▾ browse ──────── search ✓ ┐  (title bar)
Line 1: │ name     size  date  type │  (column headers — line 1337)
Line 2: │ / [search input]  recursive│  (search row 1 — line 1344)
Line 3: │ mode: filename sort: ▲ audio│ (search row 2 — line 1386)
Line 4+: │ <results>                 │  (file entries)
└────────────────────────────────────┘
```

The column headers (name/size/date/type) sit above the search panel. This is confusing — the column headers describe the file list below, not the search controls. The search input and its controls should be visually grouped together, and the column headers should anchor to the results they describe.

### Required layout

```
Line 0: ┌▾ browse ──────── search ✓ ┐  (title bar)
Line 1: │ / [search input]          │  (search row 1)
Line 2: │ recursive  mode  sort audio│ (search row 2 — all controls)
Line 3: │ name     size  date  type │  (column headers — anchored to results)
Line 4+: │ <results>                 │  (file entries)
└────────────────────────────────────┘
```

### Fix

In `draw_browse_list()`, swap the render order: emit search rows first (lines 1344-1422), then the column header row (line 1337). The `reserved` height calculation (line 1321) stays the same — total reserved rows don't change, just their order.

## Bug 3: Recursive toggle isolated from peer controls

### Problem

The recursive toggle is on search row 1 (right edge, same row as the search input, line 1348). The mode, sort, and audio controls are on search row 2 (line 1386). The recursive toggle is visually separated from its functional peers, making it easy to miss.

### Fix

Move the recursive toggle to search row 2, as the first control before mode/sort/audio:

```
Row 1: │ / [search input ........................] │
Row 2: │ recursive  mode: filename  sort: ▲  audio │
```

This groups all search option controls on one row and gives the search input the full width of its row.

## Bug 4: Mode and sort controls don't look clickable

### Problem

The mode label (`mode: filename`) and sort label (`sort: relevance ▲`) are rendered as plain colored text:

- Mode: `Style::default().fg(theme.cyan)` (line 1414)
- Sort: `Style::default().fg(theme.amber)` (line 1416)

These have no background color, no border, no visual affordance indicating they're clickable toggles. Compare to `recursive` and `audio` which use proper pill styling: `fg(theme.pill_active_fg).bg(theme.green)` when active, `fg(theme.text_dim).bg(theme.surface)` when inactive.

### Fix

Style mode, sort, recursive, and audio uniformly as pills with background color:

**Active state** (toggle is on, or the control has a non-default value):
```rust
Style::default()
    .fg(theme.pill_active_fg)
    .bg(theme.green)
    .add_modifier(Modifier::BOLD)
```

**Inactive/default state**:
```rust
Style::default()
    .fg(theme.text_dim)
    .bg(theme.surface)
```

For mode and sort, the "active" concept means they always show their current value — so use a neutral pill style that's clearly a button but doesn't imply on/off:

```rust
// Mode and sort: always visible, always clickable
Style::default()
    .fg(theme.text_bright)
    .bg(theme.surface)
```

This gives all four controls a visible background (`.bg(theme.surface)`) that reads as "this is a clickable element." The recursive and audio toggles additionally turn green when active.

## Current code locations

- `execute_search_over_entries()`: `src/tui/browse.rs:3569`
- `spawn_search_async()`: `src/tui/browse.rs:3710`
- Async archive search: `src/tui/browse.rs:5535` (search `SkimMatcherV2`)
- `draw_browse_list()`: `src/tui/draw_browse.rs:1268`
- Column header render: `src/tui/draw_browse.rs:1337` (`render_header_row`)
- Search row 1 (input + recursive): `src/tui/draw_browse.rs:1344`
- Search row 2 (mode + sort + audio): `src/tui/draw_browse.rs:1386`
- Mode label style: `src/tui/draw_browse.rs:1414`
- Sort label style: `src/tui/draw_browse.rs:1416`
- Recursive pill style (active): `src/tui/draw_browse.rs:1350`
- Recursive pill style (inactive): `src/tui/draw_browse.rs:1358`
- Audio pill style (active): `src/tui/draw_browse.rs:1394`
- Audio pill style (inactive): `src/tui/draw_browse.rs:1402`
- Button registration for search controls: `src/tui/draw_browse.rs` (search `register_browse_buttons`)
- Search control button hit targets: `src/tui/draw_browse.rs` (search `BrowseSearchRecursive`, `BrowseSearchMode`, `BrowseSearchSort`, `BrowseSearchAudio`)

## Files to modify

1. **`src/tui/browse.rs`** — Add minimum score threshold to `execute_search_over_entries()`, `spawn_search_async()`, and async archive search worker
2. **`src/tui/draw_browse.rs`** — Swap column header / search panel render order, move recursive to row 2, style mode/sort as pills with background

## Exit criteria

- Searching "epping" in a Genesis folder does NOT return "Spot the Pigeon EP" (score 52 < threshold)
- Genuine matches (e.g., recursive tag search finding "The Battle of Epping Forest") still return
- Short queries (2-3 chars) still produce useful results
- Search panel layout: search input on row 1 (full width), all controls on row 2, column headers on row 3 (anchored to results)
- Recursive toggle on the same row as mode/sort/audio (first position)
- All four controls (recursive/mode/sort/audio) styled as pills with visible background
- Recursive and audio use green bg when active, surface bg when inactive
- Mode and sort use surface bg always (neutral pill, always clickable)
- Button hit targets updated for new positions
- `cargo check` — zero errors, zero warnings
- `cargo test --no-run` — zero errors, zero warnings
