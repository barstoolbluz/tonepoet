# Code task: Naming template expansion (folder + filename + custom variables)

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`

## Context

Read these files for background:
- `CLAUDE.md` -- project overview, workspace structure
- `src/convert/pipeline/types.rs` -- `NamingPolicy`, `PreparedTrack`, `TrackMetadata`, `AlbumMetadata`
- `src/convert/pipeline/stages.rs` -- `plan_outputs`, `render_track_template`, `validate_template`, `sanitize_component`
- `src/convert/pipeline/materializer_7z.rs` -- `read_track_metadata` (lofty tag reading)
- `src/tui/app.rs` -- `OutputOptionsState` (folder_template, filename_template)
- `src/tui/convert_actions.rs` -- `pills_to_options` (builds ConversionOptions from TUI state)

## What already exists

**Template engine** (in `stages.rs`):
- `validate_template()` accepts 9 hardcoded tokens: NN, N, TRACK, TITLE, ARTIST, ALBUM_ARTIST, ALBUM, DISC, FORMAT. Rejects unknown tokens with a hard error.
- `render_track_template()` does string `.replace()` for each token. Produces a `PathBuf` (supports `/` in templates for nested directories).
- `sanitize_component()` replaces filesystem-unsafe characters (`/\:*?"<>|` and control chars) with `_`. Designed for single path components -- it replaces `/` with `_`.

**TUI state** (in `OutputOptionsState` in `app.rs`):
- `folder_template`: default `"%ARTIST%/%ALBUM% (%YEAR%)"` -- editable in TUI but NEVER passed to the pipeline. Dead UI.
- `filename_template`: default `"%NN% - %TITLE%"` -- editable in TUI but NEVER passed to the pipeline. Dead UI.

**ConversionOptions** (in `formats.rs`):
- Has `naming_template: Option<String>` (never populated from TUI).
- Does NOT have `folder_template`.

**NamingPolicy** (in `types.rs`):
- Has `template: String` (filename template, hardcoded to `"%NN% - %TITLE%"` at every construction site).
- Has `per_album_subdir: bool` (always true).
- Does NOT have `folder_template`.

**Album directory construction** (in `plan_outputs()` in `stages.rs`):
- Hardcoded: `album_component = sanitize_component(album_name || container_stem || "Album")`.
- Ignores the folder template entirely. No artist, year, catalog, or other metadata in the directory name.

**Tag metadata** (in `read_track_metadata()` in `materializer_7z.rs`):
- Reads 13 hardcoded fields from lofty into `TrackMetadata` struct fields.
- Only puts `"album"` into the `extra` BTreeMap. Does NOT enumerate arbitrary tag fields.
- Lofty CAN enumerate all tag items via `tag.items()` (already used in `tui/probe.rs` `read_all_tags()` for the tag editor).

**PreparedTrack** (in `types.rs`):
- Has `sample_rate: u32`. Does NOT have `bit_depth`.

**Metadata available but not in templates:**
- `AlbumMetadata.date` / `TrackMetadata.date` -- year/date string
- `AlbumMetadata.genre` / `TrackMetadata.genre`
- `TrackMetadata.composer`, `TrackMetadata.isrc`
- `extra["catalog"]` (CUE materializer), `extra["sacd_album_catalog_number"]` (SACD materializer)
- `PreparedTrack.sample_rate` (no bit_depth yet)

## What this task delivers

### 1. Wire templates from TUI to pipeline

**Add `folder_template: Option<String>` to `ConversionOptions`** in `formats.rs`. Add it to the `Default` impl as `None`.

**Update `pills_to_options()`** in `convert_actions.rs` to pass both templates from the TUI state:
```rust
naming_template: Some(output_opts.filename_template.clone()),
folder_template: Some(output_opts.folder_template.clone()),
```

**Add `folder_template: Option<String>` to `NamingPolicy`** in `types.rs`.

**Update all NamingPolicy construction sites** to read from item options instead of hardcoding. There are 8 sites total:

| Function | File | Template source |
|----------|------|-----------------|
| `build_pipeline_request_template()` | `main.rs` | CLI `--naming` flag; add `--folder-naming` flag |
| `pipeline_request_for_cue_item()` | `processor.rs` | `item.options.naming_template` / `item.options.folder_template` |
| `pipeline_request_for_sacd_item()` | `processor.rs` | same |
| `run_sevenzip_pipeline_conversion_item()` | `processor.rs` | same |
| `process_item()` (7z branch) | `processor.rs` | same |
| `execute_commit()` (multi-track branch) | `command.rs` | `options.naming_template` / `options.folder_template` |
| `sample_request()` (test fixture) | `pipeline/mod.rs` | hardcoded default, just add `folder_template: None` |
| `mat_request()` (test fixture) | `pipeline/mod.rs` | hardcoded default, just add `folder_template: None` |

For each production site, the pattern is:
```rust
naming: NamingPolicy {
    template: item.options.naming_template.clone()
        .unwrap_or_else(|| "%NN% - %TITLE%".to_string()),
    folder_template: item.options.folder_template.clone(),
    per_album_subdir: true,
    collision_policy: NamingCollisionPolicy::Fail,
},
```

### 2. Folder template rendering

Add a new function `render_folder_template()` in `stages.rs`. This is album-level (not per-track), so it takes `PreparedSource` + format, not `PreparedTrack`.

```rust
fn render_folder_template(
    template: &str,
    source: &PreparedSource,
    format: AudioFormat,
) -> PathBuf
```

**Variable resolution** (all values sanitized individually via `sanitize_component`):
- `%ARTIST%` -- `album_metadata.album_artist` || first track artist || `"Unknown Artist"`
- `%ALBUM_ARTIST%` -- same as `%ARTIST%` for folder context
- `%ALBUM%` -- `album_metadata.album` || container file_stem || `"Album"`
- `%YEAR%` -- extract 4-digit year from `album_metadata.date` || `""`
- `%GENRE%` -- `album_metadata.genre` || `""`
- `%CATALOG%` -- `album_metadata.extra.get("catalog")` || `album_metadata.extra.get("sacd_album_catalog_number")` || `""`
- `%FORMAT%` -- target format extension
- `%SAMPLERATE%` -- first track's `sample_rate` (formatted: `"44.1kHz"`, `"96kHz"`, `"192kHz"`)
- `%BITDEPTH%` -- first track's `bit_depth` || `""`
- Any unknown `%FOO%` -- `album_metadata.extra.get("foo")` || `""` (lowercase lookup)

**Critical: multi-component path handling.** The template `%ARTIST%/%ALBUM% (%YEAR%)` produces a string with `/`. Handle as follows:
1. Render all `%VAR%` substitutions first (each value individually sanitized via `sanitize_component`, which replaces `/` in values with `_`)
2. The rendered string may contain literal `/` from the template itself (NOT from values)
3. Split the rendered string on `/`
4. Trim each component (remove leading/trailing whitespace and dots)
5. Filter out empty components
6. Rejoin as a `PathBuf`

**Example:** Template `%ARTIST%/%ALBUM% (%YEAR%)` with artist="Miles Davis", album="A Tribute to Jack Johnson", year="1971":
- After substitution: `"Miles Davis/A Tribute to Jack Johnson (1971)"`
- Split + rejoin: `PathBuf("Miles Davis/A Tribute to Jack Johnson (1971)")`

**Empty variable:** Same template with year missing:
- Result: `PathBuf("Miles Davis/A Tribute to Jack Johnson ()")` -- parentheses stay, no magic cleanup

**Update `plan_outputs()`** in `stages.rs`: replace the hardcoded `album_component` logic with:
```rust
let album_dir = if req.naming.per_album_subdir {
    match &req.naming.folder_template {
        Some(tmpl) => {
            let rendered = render_folder_template(tmpl, source, req.target_format);
            output_root.join(rendered)
        }
        None => {
            // Existing fallback: album name || container stem || "Album"
            let album_component = sanitize_component(
                source.album_metadata.album.as_deref()
                    .or_else(|| source.container.file_stem().and_then(|s| s.to_str()))
                    .unwrap_or("Album"),
            );
            output_root.join(album_component)
        }
    }
} else {
    output_root.clone()
};
```

### 3. Expand filename template variables

In `render_track_template()` in `stages.rs`, add these built-in substitutions after the existing 9:

- `%YEAR%` -- extract 4-digit year from `source.album_metadata.date` || `""`
- `%GENRE%` -- `track.metadata.genre` || `source.album_metadata.genre` || `""`
- `%COMPOSER%` -- `track.metadata.composer` || `""`
- `%CATALOG%` -- `source.album_metadata.extra.get("catalog")` || `source.album_metadata.extra.get("sacd_album_catalog_number")` || `""`
- `%SAMPLERATE%` -- formatted from `track.sample_rate`
- `%BITDEPTH%` -- `track.bit_depth` as string || `""`
- `%ISRC%` -- `track.metadata.isrc` || `""`

All values sanitized via `sanitize_component`.

**Helper functions needed:**

`extract_year_from_date(date: &str) -> Option<String>` -- extract first 4-digit sequence from date strings like `"1971"`, `"1971-03-15"`, `"March 1971"`.

`format_sample_rate(hz: u32) -> String` -- `44100` -> `"44.1kHz"`, `48000` -> `"48kHz"`, `96000` -> `"96kHz"`, `2822400` -> `"DSD64"`, etc.

### 4. Custom variable fallthrough (extra map lookup)

After all built-in `.replace()` calls in both `render_track_template` and `render_folder_template`, scan for any remaining `%TOKEN%` patterns and resolve them from the `extra` maps.

Add a function `resolve_extra_tokens()`:
1. Scan for `%...%` patterns in the rendered string
2. For each token, lowercase it: `"CATALOGNUMBER"` -> `"catalognumber"`
3. Look up in `track.metadata.extra` first (for track template), then `source.album_metadata.extra`
4. Replace with the value if found, empty string if not
5. Sanitize the value via `sanitize_component`

This enables any tag field to be used in templates: `%CATALOGNUMBER%`, `%BARCODE%`, `%MUSICBRAINZ_ALBUMID%`, `%RELEASECOUNTRY%`, etc.

### 5. Update validate_template

Change `validate_template()` in `stages.rs` from whitelist-reject to structural validation only:

```rust
fn validate_template(template: &str) -> Result<(), String> {
    // Only validate structure (balanced % delimiters, non-empty tokens).
    // Unknown tokens are resolved from extra maps at render time.
    let mut rest = template;
    while let Some(start) = rest.find('%') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('%') else {
            return Err("unclosed % token".to_string());
        };
        let token = &rest[..end];
        if token.is_empty() {
            return Err("empty token %%".to_string());
        }
        rest = &rest[end + 1..];
    }
    Ok(())
}
```

### 6. Validation and downstream album_dir awareness

**`validate_request()`** in `stages.rs` currently validates `req.naming.template` by calling `validate_template()`. When `folder_template` is added to `NamingPolicy`, `validate_request()` must also validate it (same structural check — balanced `%` delimiters, non-empty tokens).

**`run_features()`** in `stages.rs` derives `album_dir` from the first audio artifact's `final_path` parent — NOT from the pipeline request. This is correct and must NOT be changed. When folder templates produce nested paths (e.g., `Artist/Album (Year)`), the artifact `final_path` values set by `plan_outputs()` will already reflect this nesting. `run_features()` will pick up the right directory from those artifact paths. No changes needed here, but be aware of the dependency.

**`infer_publish_album_dir()`** in `stages.rs` validates that all publish entries share the same first-level directory component under `output_root`. With folder templates that produce nested paths, this function's validation still works because all tracks and sidecars share the same album directory (set by `plan_outputs`). No changes needed, but do not break its invariant.

### 7. Add bit_depth to PreparedTrack (NEW FIELD)

**`PreparedTrack`** in `types.rs`: add `pub bit_depth: Option<u32>`.

**`materializer_7z.rs`**: in the ffprobe invocation (the function that builds the ffprobe command and parses JSON), extend `-show_entries` to include `bits_per_raw_sample`. Parse it from the JSON response. Set `bit_depth` on each `PreparedTrack`.

**`materializer_cue.rs`**: same -- extend ffprobe to capture `bits_per_raw_sample` from the image file probe.

**`materializer_sacd.rs`**: set `bit_depth: None` on `PreparedTrack` construction (SACD is DSD; output bit depth comes from encode options, not source).

### 8. Enumerate all lofty tag items into extra

In `read_track_metadata()` in `materializer_7z.rs`, after the existing hardcoded field extraction, enumerate all tag items and insert text values into `extra`:

```rust
// Enumerate all tag fields into extra for custom template variables
let tag_type = tag.tag_type();
for item in tag.items() {
    if let lofty::tag::ItemValue::Text(text) = item.value() {
        let key = item_key_to_extra_key(item.key(), tag_type);
        if !key.is_empty() {
            extra.entry(key).or_insert_with(|| text.clone());
        }
    }
}
```

Add a helper `item_key_to_extra_key()`:
```rust
fn item_key_to_extra_key(key: &lofty::tag::ItemKey, tag_type: lofty::tag::TagType) -> String {
    // Use lofty's format-specific key mapping (gives "ARTIST", "ALBUM" etc for Vorbis)
    if let Some(s) = key.map_key(tag_type, true) {
        return s.to_lowercase();
    }
    // Fallback for Unknown keys: extract the inner string
    match key {
        lofty::tag::ItemKey::Unknown(s) => s.to_lowercase(),
        _ => format!("{:?}", key).to_lowercase(),
    }
}
```

**Key normalization rule: all extra keys are lowercase.** This is consistent with the existing SACD and CUE materializers (which already use lowercase keys -- do NOT change their key casing). The template engine lowercases tokens before lookup.

**Note:** Standard fields (title, artist, etc.) will appear in BOTH the dedicated `TrackMetadata` struct fields AND in `extra`. This is intentional -- struct fields are used by built-in template variables (direct access), while `extra` is the fallthrough for custom variables. `extra.entry(key).or_insert_with(...)` ensures the explicit struct-level extraction wins if there's a conflict.

## Locked contracts (do not change)

- `PipelineEvent` enum
- `PipelineReporter` trait
- `ProgressUpdate` struct
- `ConversionStatus` enum
- `AlbumMetadata` struct fields (only ADD extra entries, don't rename/remove existing fields)
- `TrackMetadata` struct fields (same)
- Existing `extra` key strings in `materializer_sacd.rs` and `materializer_cue.rs` (all lowercase, do not change)

## Files modified

| File | Changes |
|------|---------|
| `src/convert/formats.rs` | Add `folder_template: Option<String>` to `ConversionOptions` + default |
| `src/convert/pipeline/types.rs` | Add `folder_template: Option<String>` to `NamingPolicy`; add `bit_depth: Option<u32>` to `PreparedTrack` |
| `src/convert/pipeline/stages.rs` | Update `plan_outputs`, `validate_template`, `render_track_template`; add `render_folder_template`, `resolve_extra_tokens`, `extract_year_from_date`, `format_sample_rate` |
| `src/convert/pipeline/materializer_7z.rs` | Enumerate all tag items into extra; add bit_depth to ffprobe + PreparedTrack |
| `src/convert/pipeline/materializer_cue.rs` | Add bit_depth to ffprobe + PreparedTrack |
| `src/convert/pipeline/materializer_sacd.rs` | Set `bit_depth: None` on PreparedTrack construction |
| `src/convert/processor.rs` | Read `naming_template` + `folder_template` from `item.options` at 4 NamingPolicy construction sites |
| `src/tui/convert_actions.rs` | Pass `naming_template` + `folder_template` in `pills_to_options` |
| `src/tui/command.rs` | Read `naming_template` + `folder_template` from options at `execute_commit()` NamingPolicy site |
| `src/main.rs` | Add `--folder-naming` CLI flag; pass to NamingPolicy |
| `src/convert/pipeline/mod.rs` | Add `folder_template: None` to 2 test fixture NamingPolicy constructions |

## Tests required

**Template engine:**
- `render_folder_template` with `%ARTIST%/%ALBUM% (%YEAR%)` produces correct nested path
- `render_folder_template` with empty year produces `Artist/Album ()`
- `render_folder_template` with no folder_template falls back to album name
- `render_track_template` with new built-in variables (`%YEAR%`, `%GENRE%`, `%CATALOG%`, `%SAMPLERATE%`, `%BITDEPTH%`) produces correct values
- `render_track_template` with custom variable `%CATALOGNUMBER%` resolves from extra
- `render_track_template` with unknown variable `%NONEXISTENT%` produces empty string
- `validate_template` accepts unknown tokens (no longer rejects them)
- `validate_template` still rejects unclosed `%` and empty `%%`
- Folder template: `/` in artist name becomes `_` (via sanitize_component), but `/` from template structure is preserved as path separator

**Extra enumeration:**
- `read_track_metadata` populates extra with all tag fields (CATALOGNUMBER, BARCODE, etc.)
- Extra keys are lowercase
- Standard fields still populate dedicated struct fields

**Bit depth:**
- `PreparedTrack.bit_depth` populated from ffprobe for FLAC (24), WAV (16), etc.
- `PreparedTrack.bit_depth` is None for SACD

**Integration:**
- Full pipeline run with folder_template set produces correct directory structure
- Existing tests pass with `folder_template: None` (backward compatible)

## `#![forbid(unsafe_code)]`

All pipeline modules are under `#![forbid(unsafe_code)]`. The bit_depth extraction via ffprobe JSON parsing does NOT use unsafe (unlike the TUI probe path which uses unsafe FFI).

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## Deliverable

Production-ready code changes to all files listed above. Must compile and pass `cargo test --lib` (currently 668 tests, must not regress).
