# DVD-Audio Group Selection Not Reaching Materializer

## Problem

User selects Group 3 (stereo) via the Convert screen's Stream pill.
The TUI correctly shows the stereo presentation. But conversion
extracts Group 1 (5.1 multichannel) instead.

## Evidence

From the conversion log (`~/.cache/tonepoet/tonepoet.log`):
```
group=1, ats=Some(1), decoded_channels=6, dvda_downmix_policy=none
```

Expected: `group=3` with `dvda_downmix_policy` set for stereo.

## Prior fix attempt

The previous bundle added code to the commit path in `command.rs`
that reads `SourceMode::MultiTrack.selected_presentation_id` and
maps `PresentationId::DvdAudioGroup(n)` to
`SourceOptions.dvda_group_selection = DvdaGroupSelection::Group(n)`.

A unit test confirms `apply_presentation_to_source_options` correctly
maps `PresentationId::DvdAudioGroup(3)` to `DvdaGroupSelection::Group(3)`.

But the conversion still uses group 1. The bridge code either:
1. Isn't reached during the actual commit flow
2. Sets the SourceOptions on a request that gets replaced
3. The materializer ignores the group selection

## What to investigate

### The commit flow

When the user presses the enqueue button or runs `:commit`:

1. `command.rs` handles `Command::Commit` or `Command::Queue`
2. A `ConversionItem` is created from the Convert screen state
3. A `PipelineRequest` is built (either from the template or
   from `build_pipeline_request_from_settings`)
4. The item is added to the conversion queue
5. The processor picks it up and calls the materializer

Trace: where does `selected_presentation_id` get read? Does the
`PipelineRequest` that reaches the materializer have
`dvda_group_selection = Group(3)`?

### Key code paths

- `src/tui/command.rs` — the `:commit` / `:queue` handler. Where
  does it read `selected_presentation_id` from `SourceMode::MultiTrack`?
  Does it call `apply_presentation_to_source_options`?
- `src/convert/pipeline/unified_request.rs` — `build_pipeline_request`.
  Does it preserve the `dvda_group_selection` from the template?
- `src/convert/processor.rs` — `process_queue_with_progress`. Does
  it rebuild the PipelineRequest and potentially reset group selection?
- `src/convert/pipeline/materializer_dvda.rs` — `materialize()`.
  Does it read `req.source.dvda_group_selection`?

### The previous fix location

The fix added `apply_presentation_to_source_options` call in the
commit path, specifically when `MultiTrack` has `selected_presentation_id`.
But the commit path may have multiple branches — one for "has deselected
tracks" (builds a custom PipelineRequest) and one for "all tracks selected"
(uses the default/template request). The previous fix claims to handle
both, but the log shows it didn't work.

## What to produce

1. Find exactly where the group selection is lost
2. Fix it so `DvdaGroupSelection::Group(3)` reaches the materializer
3. Verify the stereo downmix policy also activates (auto-detection
   should still work since group 3 is a cross-ATS stereo presentation)
