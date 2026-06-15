# DVD-Video command.rs Integration — Merge Prompt

## Context

A previous reasoning model pass produced the full DVD-Video integration
for tonepoet. It received all source files EXCEPT `src/tui/command.rs`
(5914 lines). Unable to modify the existing file, it wrote a standalone
811-line replacement containing only DVD-Video functions. That replacement
must now be merged into the real `command.rs`.

The existing `command.rs` is included in this bundle as the file to modify.
The reasoning model's standalone output is included as `command_dvdv_standalone.rs`
for reference — do NOT use it as the base. Use the existing `command.rs` as
the base and add the DVD-Video functionality into it.

## What to add

All functions from `command_dvdv_standalone.rs` that don't already exist
in `command.rs`. These are:

### New functions to add:

1. `dvdv_source_to_cd_sectors(path) -> Result<Vec<u32>, String>` — DVD-Video
   MusicBrainz TOC computation, parallel to `dvda_source_to_cd_sectors()`

2. `dvdv_presentation_to_cd_sectors(presentation) -> Result<Vec<u32>, String>` —
   helper for the above

3. `select_default_disc_presentation_index(contents) -> Option<usize>` — shared
   default presentation selection for disc browser and TOC lookup

4. `execute_commit_with_disc_selection_bridge(app, start, tx)` — bridges the
   Convert screen disc selection to the pipeline commit path

5. `apply_convert_source_disc_selection_to_source_options(mode, options)` — applies
   selected presentation to SourceOptions

6. `source_options_with_convert_source_disc_selection(mode, options) -> SourceOptions` —
   functional wrapper for the above

7. `apply_convert_source_disc_selection_to_pipeline_request(mode, request)` — same
   for PipelineRequest

8. `pipeline_request_with_convert_source_disc_selection(mode, request) -> PipelineRequest`

9. `commit_pipeline_request_with_convert_source_disc_selection(mode, request) -> PipelineRequest`

10. `dvdv_metadata_sidecar_path_for_source(source) -> Result<PathBuf, String>` — sidecar path

11. `save_dvdv_metadata_sidecar(...)` — write metadata sidecar for DVD-Video

12. `dvdv_metadata_sidecar_from_state(state) -> BTreeMap<...>` — build sidecar from editor

13. Helper functions: `write_dvdv_metadata_sidecar_atomic`, `atomic_replace_file`,
    `unique_sidecar_temp_path`, `dvdv_editor_key_to_sidecar_key`,
    `dvdv_is_album_level_sidecar_key`
    NOTE: `is_dir_writable` already exists in `keybindings.rs:6003`. Do NOT
    duplicate it — either call the existing one via `super::keybindings::is_dir_writable`
    (if it's pub(super)) or make it pub(super) and import it.

14. `durations_to_cd_sectors<I>(durations) -> Vec<u32>` — shared generic TOC helper

15. DVD-Video scoring helpers: `select_dvdv_toc_presentation`,
    `dvdv_default_presentation_score`, `dvdv_presentation_has_complete_positive_durations`,
    `dvdv_codec_rank`

### Existing function to modify:

16. `sacd_durations_to_sectors(durations)` — refactor to delegate to the shared
    `durations_to_cd_sectors()` generic helper

### New Command variant needed:

17. Add `CommitWithSourceOptionsTransform { start: bool, transform: Box<dyn FnOnce(SourceOptions) -> SourceOptions + Send> }`
    to the `Command` enum. Add a handler in `execute_command()` that builds
    the normal commit SourceOptions, applies the transform, then proceeds
    with the transformed options.

### Integration in existing `:tags-mb` flow:

18. In `try_dispatch_in_editor_tags_mb()`, add a DVD-Video branch alongside
    the existing SACD and DVD-Audio branches. When the editor has a DVD-Video
    source, call `dvdv_source_to_cd_sectors()` for TOC computation.

19. In the Browse right-click `Command::TagsFromMb` handler, add DVD-Video
    source detection alongside SACD and DVD-Audio detection. Open a DVD-Video
    metadata editor or directly compute TOC.

### Integration in commit path:

20. The existing `Command::Commit` handler needs to check if the source is a
    disc with a selected presentation. If so, route through the disc selection
    bridge to ensure the presentation ID reaches the materializer. The
    `CommitWithSourceOptionsTransform` variant handles this.

## What NOT to change

- Do not restructure the existing command parsing (`parse_command`, `Command` enum arms)
- Do not move or rename existing functions
- Do not change the existing DVD-Audio or SACD flows unless adding DVD-Video
  alongside them in the same match arms
- Keep all existing tests intact

## Files included

- `command.rs` — the EXISTING 5914-line file (BASE for modifications)
- `command_dvdv_standalone.rs` — the reasoning model's standalone output (REFERENCE only)

## What to produce

Modified `command.rs` with all DVD-Video functions merged in, the
`CommitWithSourceOptionsTransform` command variant added, and the
`:tags-mb` / commit paths extended for DVD-Video sources.
