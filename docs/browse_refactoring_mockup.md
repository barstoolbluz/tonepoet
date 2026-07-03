# Browse Screen Refactoring — Layout Mockup

## Default layout: all three panes open

```
╭─ TONEPOET ──────────────────────────────────────────────────────────────────────────────────────╮
│                                                                                                  │
│  ▀▀█▀▀ █▀▀█ █▀▀▄ █▀▀▀ █▀▀█ █▀▀█ █▀▀▀ ▀▀█▀▀      v0.2.9                                       │
│    █   █  █ █  █ █▀▀▀ █▀▀▀ █  █ █▀▀▀   █                                                       │
│    ▀   ▀▀▀▀ ▀  ▀ ▀▀▀▀ ▀    ▀▀▀▀ ▀▀▀▀   ▀                                                       │
│                                                                                                  │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
 ‹ Back  › Fwd  ↑ Up  Refresh  Options ▾  Search                    Show hidden: ○
 path: ~/kairos/pbthal/downloads                                                          [Go]
┌▾ explore ──────┬─▾ browse ──────────────────────────────────────────────┬─▾ info ──────────────┐
│ ▾ daedalus      │  name ▲              size      date       type        │  name                 │
│   ▸ .cache      │  ..                                                   │  06 - Bitch.flac      │
│   ▸ .config     │  Atlanta Rhythm S…   882 MB    2021-12-01  7z         │                       │
│   ▸ dev         │  Deep Purple - Fi…   924 MB    2021-12-01  7z         │  format   FLAC        │
│   ▾ kairos      │▸ Led Zeppelin - T…   3.8 GB    2017-06-01  7z         │  codec    FLAC 16-bit │
│     ▾ pbthal    │  Pink Floyd - Ato…   1.0 GB    2020-12-01  7z         │  rate     44.1 kHz    │
│       ▾ downl…  │  Rolling Stones -…   687 MB    2021-12-01  7z         │  channels stereo      │
│       ▸ compl…  │  The Clash - Sand…   3.1 GB    2024-12-01  7z         │  duration 03:38       │
│       ▸ vinyl…  │  The Who - Who's …   1.2 GB    2022-06-01  7z         │  size     21.1 MB     │
│   ▸ library     │                                                       │                       │
│   ▸ temp        │                                                       │  replaygain (t+a)     │
│   ▸ preemph-d…  │                                                       │  tk gain  +0.36 dB    │
│                 │                                                       │  tk peak  0.816581    │
│                 │                                                       │  al gain  -0.53 dB    │
│                 │                                                       │  al peak  0.847365    │
│                 │                                                       │                       │
│                 │                                                       │       [ analyze ]     │
│                 │                                                       │                       │
│                 │                                                       │  title   Bitch        │
│                 │                                                       │  artist  The Rollin…  │
│                 │                                                       │  album   Sticky Fin…  │
│                 │                                                       │  genre   Rock         │
│                 │                                                       │  year    1971         │
│                 │                                                       │                       │
│                 │                                                       │       [ edit tags ]   │
└─────────────────┴───────────────────────────────────────────────────────┴───────────────────────┘
 Browse    Library    Convert    Queue    Config                        q quit  ? help  : command
```

**Split ratios (default):** Explorer 20% · Browse 50% · Info 30%

## Explorer collapsed

```
┌─┬─▾ browse ──────────────────────────────────────────────────────────────┬─▾ info ──────────────┐
│▸│  name ▲              size      date       type                        │  name                 │
│ │  ..                                                                   │  06 - Bitch.flac      │
│e│  Atlanta Rhythm S…   882 MB    2021-12-01  7z                         │                       │
│x│  Deep Purple - Fi…   924 MB    2021-12-01  7z                         │  format   FLAC        │
│p│▸ Led Zeppelin - T…   3.8 GB    2017-06-01  7z                         │  codec    FLAC 16-bit │
│l│  Pink Floyd - Ato…   1.0 GB    2020-12-01  7z                         │  rate     44.1 kHz    │
│o│  Rolling Stones -…   687 MB    2021-12-01  7z                         │  channels stereo      │
│r│  The Clash - Sand…   3.1 GB    2024-12-01  7z                         │  duration 03:38       │
│e│  The Who - Who's …   1.2 GB    2022-06-01  7z                         │  size     21.1 MB     │
│r│                                                                       │                       │
│ │                                                                       │  title   Bitch        │
│ │                                                                       │  artist  The Rollin…  │
│ │                                                                       │  album   Sticky Fin…  │
│ │                                                                       │  genre   Rock         │
│ │                                                                       │  year    1971         │
│ │                                                                       │                       │
│ │                                                                       │       [ edit tags ]   │
└─┴───────────────────────────────────────────────────────────────────────┴───────────────────────┘
```

**Split ratios:** Explorer 3 cols (vertical title) · Browse ~55% · Info ~45%

## Info collapsed

```
┌▾ explore ──────┬─▾ browse ──────────────────────────────────────────────────────────────────┬──┐
│ ▾ daedalus      │  name ▲              size      date       type                             │▸ │
│   ▸ .cache      │  ..                                                                       │  │
│   ▸ .config     │  Atlanta Rhythm S…   882 MB    2021-12-01  7z                              │i │
│   ▸ dev         │  Deep Purple - Fi…   924 MB    2021-12-01  7z                              │n │
│   ▾ kairos      │▸ Led Zeppelin - T…   3.8 GB    2017-06-01  7z                              │f │
│     ▾ pbthal    │  Pink Floyd - Ato…   1.0 GB    2020-12-01  7z                              │o │
│       ▾ downl…  │  Rolling Stones -…   687 MB    2021-12-01  7z                              │  │
│       ▸ compl…  │  The Clash - Sand…   3.1 GB    2024-12-01  7z                              │  │
│       ▸ vinyl…  │  The Who - Who's …   1.2 GB    2022-06-01  7z                              │  │
│   ▸ library     │                                                                            │  │
│   ▸ temp        │                                                                            │  │
│                 │                                                                            │  │
└─────────────────┴──────────────────────────────────────────────────────────────────────────── ┴──┘
```

**Split ratios:** Explorer 20% · Browse ~77% · Info 3 cols (vertical title)

## Both collapsed — browse maximized

```
┌─┬─▾ browse ──────────────────────────────────────────────────────────────────────────────────┬──┐
│▸│  name ▲              size      date       type                                             │▸ │
│ │  ..                                                                                       │  │
│e│  Atlanta Rhythm Section - Champagne Jam (WLP RL)-dec-2021.7z    882 MB    2021-12-01  7z   │i │
│x│  Deep Purple - Fireball (2018 Alchemy Reissue)-dec-2021.7z      924 MB    2021-12-01  7z   │n │
│p│  Led Zeppelin - The Complete BBC Sessions (2016)-jun-2017.7z    3.8 GB    2017-06-01  7z   │f │
│l│  Pink Floyd - Atom Heart Mother (UK)-dec-2020.7z                1.0 GB    2020-12-01  7z   │o │
│o│  Rolling Stones - Sticky Fingers (Japan)-dec-2021.7z            687 MB    2021-12-01  7z   │  │
│r│  The Clash - Sandinista! (UK)-dec-2024.7z                       3.1 GB    2024-12-01  7z   │  │
│e│  The Who - Who's Next (UK)-jun-2022.7z                          1.2 GB    2022-06-01  7z   │  │
│r│                                                                                            │  │
│ │                                                                                            │  │
│ │                                                                                            │  │
└─┴────────────────────────────────────────────────────────────────────────────────────────────┴──┘
```

**Split ratios:** Explorer 3 cols · Browse ~94% · Info 3 cols. Note file names now have room to display fully.

## Toolbar detail

```
 ‹ Back  › Fwd  ↑ Up  Refresh  Options ▾  Search                    Show hidden: ○
```

- **‹ Back / › Fwd** — directory history navigation (same as file picker)
- **↑ Up** — go to parent directory
- **Refresh** — reload current directory (also `:refresh` command)
- **Options ▾** — dropdown menu:
  - Show hidden files (toggle)
  - Sort by (name/size/date/type)
  - Filter (audio only / all files)
  - Archive listing mode (auto/always/never)
- **Search** — opens recursive search (type-ahead already exists; this surfaces it as a button)
- **Show hidden: ○/●** — quick toggle, always visible (most-used option)

All buttons styled with `theme.button` background, same as file picker toolbar.

## Inline editing improvements

### Selection and clipboard

| Key | Action |
|-----|--------|
| Double-click | Select entire field text |
| Ctrl+A | Select all text in field |
| Ctrl+C | Copy selection to clipboard |
| Ctrl+X | Cut selection to clipboard |
| Ctrl+V | Paste from clipboard |
| Ctrl+Left | Skip word left |
| Ctrl+Right | Skip word right |
| Ctrl+Home | Jump to beginning of text |
| Ctrl+End | Jump to end of text |
| Home | Jump to beginning of text |
| End | Jump to end of text |

### Tab behavior while inline editing

| Key | Context | Action |
|-----|---------|--------|
| Tab | Editing in browse view | Commit edit, move to next file/folder |
| Shift+Tab | Editing in browse view | Commit edit, move to previous file/folder |
| Tab | Editing path field | Filesystem tab completion (already works) |
| Tab | Editing template field | %VARIABLE% completion (already works) |

### Clipboard implementation

Add to `TextInputState`:

```rust
pub struct TextInputState {
    pub text: String,
    pub cursor: usize,
    pub select_all: bool,
    // NEW:
    pub selection_start: Option<usize>,  // byte offset of selection anchor
    pub clipboard: String,               // internal clipboard (not OS clipboard)
}
```

Or use the system clipboard via `arboard` crate for OS integration. Internal clipboard is simpler and doesn't require a new dependency.

## Pane collapse/expand interaction

| Action | Result |
|--------|--------|
| Click `▾` on any pane title | Collapse that pane to vertical title bar |
| Click `▸` on collapsed vertical title | Expand that pane back to its default size |
| Double-click browse pane title | Maximize browse (collapse both explore + info) |
| Double-click again | Restore all to defaults |
| `:max` command | Same as double-click maximize |

The `▾`/`▸` triangle on pane title bars is a clickable toggle. The collapsed vertical title bar is also a click target that expands the pane.

## Implementation notes

### Reusing file picker's tree component

The file picker's `TreeNode` and tree rendering code (`crates/tui-file-picker/src/tree.rs`) can be reused for the explore pane. The browse screen would maintain its own `Vec<TreeNode>` synced with the filesystem. When the user clicks a tree node, it navigates the browse list to that directory.

The tree state (expanded/collapsed nodes) persists across the session. The root defaults to the user's home directory.

### Layout engine

The three-pane horizontal split uses ratatui's `Layout` with dynamic constraints:

```rust
let constraints = match (explore_collapsed, info_collapsed) {
    (false, false) => vec![
        Constraint::Percentage(20),  // explore
        Constraint::Min(40),         // browse (at least 50% effective)
        Constraint::Percentage(30),  // info
    ],
    (true, false) => vec![
        Constraint::Length(3),       // collapsed explore (vertical title)
        Constraint::Min(40),         // browse expands
        Constraint::Percentage(45),  // info gets more room
    ],
    (false, true) => vec![
        Constraint::Percentage(20),  // explore
        Constraint::Min(40),         // browse expands
        Constraint::Length(3),       // collapsed info (vertical title)
    ],
    (true, true) => vec![
        Constraint::Length(3),       // collapsed explore
        Constraint::Min(40),         // browse gets almost everything
        Constraint::Length(3),       // collapsed info
    ],
};
```

### Collapsed vertical title bar rendering

```rust
fn draw_collapsed_pane_title(f: &mut Frame, area: Rect, title: &str, theme: Theme) {
    // area.width == 3: border + char + border
    // Render title vertically: one char per row
    // Top: ▸ (click to expand)
    // Then each character of title vertically
}
```

The vertical title uses the pane's border color and renders the title one character per row within a 3-column-wide bordered area.
