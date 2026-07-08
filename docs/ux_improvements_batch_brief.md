# UX Improvements Batch

## Archive Password Prompt on Extraction Failure

When archive extraction fails with an encrypted/password error, the metadata editing and convert paths should open a password input dialog (like the archive listing path already does at `event_loop.rs:3410-3415`) instead of just showing a status message. Pattern: detect "password"/"encrypted" in the error, open `TextEditTarget::ArchivePassword`, retry extraction with the entered password.

Currently: `keybindings.rs:9488` spawns extraction, errors surface as status messages only.
Reference: `event_loop.rs:3410-3415` — the archive listing path already has this dialog.

---

## Browse Issues

### 0. Sort order should persist for the session

When the user sets a sort order (e.g., by date, by size), descends into a directory, does their work, and ascends back, the sort order is reset to the default. It should persist for the entire session.

**Root cause:** `navigate_to()` at `browse.rs:4059` calls `reset_sort_to_default()` on every directory change.

**Fix:** Remove `reset_sort_to_default()` from `navigate_to()`. The sort persists in `self.sort_by` / `self.sort_dir` — just stop resetting it. The user's explicit "set as default" UI action (via `set_default_sort`) is separate and should remain unchanged.

### 1. Clipboard operations in browse view

The browse view needs `Ctrl+V` paste support for inline editing (rename, etc.). The user reports `Ctrl+X` and possibly `Ctrl+C` already work, but `Ctrl+V` does not.

---

## Metadata Overlay Issues

### 1. Save button visibility across all tabs

When the user makes metadata changes on one tab, switches to another tab (e.g., ReplayGain), and presses Escape, they get an "unsaved changes" prompt — but they're on a tab that has no save button. The save button should be visible on ALL tabs, and it should only appear when there are unsaved changes.

### 2. OK / Apply keybindings (foobar2000 style)

Add two keybindings:
- **Alt+O** — commit changes and close the metadata overlay ("OK")
- **Alt+A** — apply/save changes but keep the overlay open ("Apply")

Add an `OK` button to the button row alongside the existing save/apply controls. Rename `Save` to `Apply` to match the foobar2000 convention.

### 3. Tab key behavior change

Currently: `Tab` / `Shift+Tab` cycle between overlay tabs (Metadata, Artwork, ReplayGain).

Change to:
- **Shift+Tab** — forward-cycle between overlay tabs
- **Ctrl+Shift+Tab** — reverse-cycle between overlay tabs (if the terminal supports this — check crossterm capability; if not, find a suitable alternative)
- **Tab** — cycle between fields within the current tab

### 4. Text selection and clipboard in metadata fields

- **Double-click** on a field should highlight/select its contents
- **Shift+Left/Right** — select character by character
- **Ctrl+Shift+Left/Right** — select word by word
- **Ctrl+C** — copy selection
- **Ctrl+X** — cut selection
- **Ctrl+V** — paste (replacing selection if any)

Note: mouse highlighting doesn't work in TUI. Keyboard selection is the only way to select partial text within a field.

---

## Convert View Issues

### 1. Force re-encode option

Add a "Force re-encode" toggle in the below-the-fold section of the Output pane. Currently `force_encode` exists in `ConversionOptions` (hardcoded to `false` at `convert_actions.rs:362`) but is not surfaced in the UI.

When the user converts from FLAC to FLAC (same format), this toggle controls whether to re-encode or passthrough. Should be inherited by presets.

### 2. Lossy codec presets in the Format pane

For lossy codecs (MP3, AAC, Opus), surface common quality presets in the above-the-fold portion of the Format pane. Dynamically replace the `bit-depth` row (which is meaningless for lossy) with a `preset` row showing common presets by name and bitrate, ending with `custom` for manual settings.

Examples:
- MP3: `V0 (245kbps)`, `V2 (190kbps)`, `320 CBR`, `custom`
- AAC: `256 VBR`, `192 VBR`, `128 VBR`, `custom`
- Opus: `128`, `96`, `64`, `custom`

### 3. %SAMPLERATE% and %BITDEPTH% should reflect format pane selection

Template variables `%SAMPLERATE%` and `%BITDEPTH%` in the Output pane should use the values selected in the Format pane, not the source file's original values — unless the format pane preserves the original (source passthrough).

This means capturing the effective sample rate and bit depth even if they're not set as tags in the file.

### 4. Conditional template blocks with `{...}`

Allow `{...}` delimiters in folder/file name templates. Content inside braces is included only if ALL template variables within can be resolved to non-empty values. If any variable is empty/missing, the entire `{...}` block (including the braces and any literal text) is dropped.

Example: `%ARTIST% - %ALBUM%{ (%TITLE_EXTRA%  %BITDEPTH%-%SAMPLERATE%)}` — if `%TITLE_EXTRA%` is empty, the whole `{ (...)}` block is omitted.

### 5. Better %TITLE_EXTRA% heuristics

Current `extract_title_extra` at `stages.rs:15400` parses parenthesized suffixes. It needs richer heuristics to detect:

- Catalog numbers: `US CTI 6015 LP`, `SME JSACD SRGS-4504`
- Format indicators: `Blu-ray`, `DVD-A`, `DVD-V`, `SACD`, `SHM-CD`
- Regional indicators: `Japan`, `US`, `UK`, `EU`
- Combined: `(US CTI 6015 LP / 24-96)`, `(Japan SHM SACD ISO)`

The key heuristic: parenthesized content that looks like metadata (catalog numbers, format labels, regional codes, bitrate/resolution specs) rather than song/album title subtitles.

### 6. Deduplication of rate/depth info between %TITLE_EXTRA% and %BITDEPTH%/%SAMPLERATE%

When `%TITLE_EXTRA%` already contains bitrate/resolution info (e.g., `24-96` in `(US CTI 6015 LP / 24-96)`), and the template also uses `%BITDEPTH%-%SAMPLERATE%`, the output would be redundant. Detect and strip the resolution portion from `%TITLE_EXTRA%` when the template also uses the explicit variables, to avoid duplication like `(US CTI 6015 LP / 24-96) 24-96`.

### 7. Strip "ISO" from %TITLE_EXTRA% when converting from ISO

When `%TITLE_EXTRA%` contains "ISO" (e.g., `Japan SHM SACD ISO`, `DVD-A ISO`), strip "ISO" (and surrounding whitespace) from the output since the converted files are no longer in ISO format. Keep provenance indicators like `SACD`, `DVD-A`, `DVD-V`, `Japan`, `SHM`.

Examples:
- `(Japan  SHM SACD ISO)` → `(Japan  SHM SACD)`
- `(DVD-A ISO)` → `(DVD-A)`
- `(DVD-V ISO)` → `(DVD-V)`

### 8. Multi-disc subfolder option

For multi-disc sets, add a below-the-fold option in the Output pane to create disc-specific subfolders (e.g., `Disc 01`, `Disc 02`). Detect multi-disc sets from tags using heuristics (DISCNUMBER, DISCTOTAL, disc-number patterns in track metadata).

---

## Priority Guidance

These are listed in rough priority order within each section, but the reasoning model should use its judgment on implementation order based on complexity and dependencies. The metadata overlay text selection (#4) and the template variable improvements (#4-7) are the most complex items.
