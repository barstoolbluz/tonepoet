# DVD-Audio Pipeline — Two Remaining Issues

## Issue 1: ALBUM tag missing from output

### Symptom

Converted FLAC has `TITLE=So Far Away`, `ARTIST=Dire Straits`,
`DATE=1985`, etc. But no `ALBUM=` tag. The metabase ALBUM value
appears only as a provenance tag:

```
TONEPOET_TRACK_DVDA_METABASE_ALBUM=Brothers in Arms (DVD-A) [Multichannel ISO]
```

The folder name is "Dire Straits - ALBUM (1985)" — using the ISO
file stem instead of the metabase album title.

### Root cause (suspected)

`overlay_album_metadata` in `materializer_dvda_metabase.rs` stores
the metabase ALBUM as a `TONEPOET_*` extra tag instead of setting
`AlbumMetadata.album`. Or `overlay_track_metadata` doesn't set the
`album` field on `TrackMetadata`.

### Expected behavior

The output FLAC should have `ALBUM=Brothers in Arms (DVD-A) [Multichannel ISO]`
as a standard Vorbis comment, and the folder name should use this
album title instead of the file stem.

---

## Issue 2: Wrong group converted

### Symptom

User selected the stereo group (Group 3: "Stereo (derived from 5.1)")
in the Convert screen's Stream pill. The TUI correctly showed the
stereo presentation. But the conversion produced 5.1 multichannel
output — group 1 was extracted instead of group 3.

### Root cause (suspected)

The `PresentationId` → `SourceOptions.dvda_group_selection` bridge
isn't working. When the user selects a presentation via the Stream
pill, the `selected_presentation_id` is set on `SourceMode::MultiTrack`,
but when the pipeline request is built for conversion, it doesn't
read this ID and map it to `DvdaGroupSelection::Group(n)`.

The pipeline request construction likely uses the default group
selection (`DvdaGroupSelection::Default` → group 1) instead of
the user's chosen presentation.

### Where to look

The bridge from TUI selection to pipeline request happens when the
user commits the conversion (`:commit` or enqueue button). Trace:

1. `SourceMode::MultiTrack.selected_presentation_id` — set by the
   Stream pill or disc browser
2. Pipeline request construction — where does `SourceOptions` get
   built? Does it read `selected_presentation_id`?
3. `SourceOptions.dvda_group_selection` — is it set to the selected
   group number, or left as `Default`?

Check `src/tui/command.rs` around the `:commit` / `:queue` handler,
and `src/convert/pipeline/unified_request.rs` for how `SourceOptions`
is populated from the Convert screen state.

---

## Evidence

```
metaflac output on converted FLAC (group 1, multichannel — WRONG group):
  TITLE=So Far Away
  ARTIST=Dire Straits
  ALBUMARTIST=Dire Straits
  DATE=1985
  (no ALBUM tag)
  TONEPOET_TRACK_DVDA_METABASE_ALBUM=Brothers in Arms (DVD-A) [Multichannel ISO]
  TONEPOET_TRACK_DVDA_GROUP=1          ← should be group 3 if stereo was selected
  TONEPOET_TRACK_DVDA_CHANNEL_COUNT=6  ← 5.1, not stereo
```

---

## What the reasoning model should produce

1. Fix `overlay_album_metadata` / `overlay_track_metadata` to set
   the standard `ALBUM` field from metabase, not just provenance
2. Fix the PresentationId → DvdaGroupSelection bridge so the selected
   group is actually converted
3. Verify the stereo downmix policy also activates when group 3 is
   selected (the auto-detection should still work)
