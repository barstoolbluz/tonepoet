# DVD-Audio Metabase Tags Not Reaching Converted Output — Investigation Brief

## Problem

DVD-Audio metabase sidecar XML contains per-track TITLE, ARTIST,
ALBUM, DATE, GENRE tags. The TUI Convert screen and `disc-info`
display these correctly. But converted output FLAC files have NO
tags — only ReplayGain and encoder metadata.

### Reproduction

1. Tag a DVD-Audio ISO via "Get tags from MusicBrainz" in the TUI
2. Verify the sidecar XML has tags (it does — TITLE, ARTIST, ALBUM
   per track)
3. Convert the disc (any group)
4. Check the output FLACs: no TITLE, ARTIST, or ALBUM tags

### Evidence from Brothers in Arms DVD-Audio

Sidecar at `9388657AE3239B071FA78EB9A0C940E2.xml` has:
```xml
<track id="2.1.1">
  <meta name="ALBUM" value="Brothers in Arms (DVD-A) [ISO]"/>
  <meta name="ARTIST" value="Dire Straits"/>
  <meta name="TITLE" value="So Far Away"/>
  ...
</track>
```

Output FLAC `01 - So Far Away.flac` tags:
```
ENCODER=Lavf61.7.100
REPLAYGAIN_ALBUM_GAIN=-8.45 dB
REPLAYGAIN_ALBUM_PEAK=1.076520
REPLAYGAIN_TRACK_GAIN=-7.64 dB
REPLAYGAIN_TRACK_PEAK=1.046078
WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0x3f
```

No TITLE, ARTIST, ALBUM, DATE, or GENRE. The file is named correctly
("So Far Away") so the naming template resolved the title, but the
tags were not written to the FLAC metadata.

---

## What exists

### Materializer metabase integration

The materializer (`materializer_dvda.rs`) loads the metabase at
line ~160 and passes it through to track/album construction:

```
load_for_materializer(volume, &req.container) → LoadedDvdaMetabase
overlay_track_metadata(base, metabase, keys) → merges metabase tags
overlay_album_metadata(base, metabase, loaded) → merges album tags
```

Both `overlay_track_metadata` (line 1909) and `overlay_album_metadata`
(line 1832) are called. But the tags don't appear in the output.

### The metadata write stage

After the materializer produces `PreparedSource` with
`Vec<PreparedTrack>`, the pipeline orchestrator runs stages:
1. Materialize → PreparedSource with TrackMetadata
2. Convert (ffmpeg decode + encode)
3. **Metadata stage** — writes tags from TrackMetadata to output files
4. ReplayGain
5. Features (log, CUE)
6. Publish

The metadata stage uses the pipeline's metadata writer to apply
`TrackMetadata` fields to the output audio files. If `TrackMetadata`
fields are empty or the stage is disabled, no tags are written.

---

## What the reasoning model should investigate

1. **Trace `TrackMetadata` population**: After `overlay_track_metadata`
   runs, does the resulting `PreparedTrack.metadata` actually contain
   TITLE, ARTIST, ALBUM? Or is the overlay failing silently?

2. **Trace the metadata stage**: In `stages.rs`, how does the metadata
   stage consume `TrackMetadata`? What function writes tags to the
   output FLAC? Is the metadata stage enabled for DVD-Audio conversions?

3. **Check `StageRequirement` for metadata**: Is the metadata stage
   set to `Enabled` or `Disabled` in the pipeline request for DVD-Audio
   conversions? Check `PipelineRequest.stages.metadata`.

4. **Check the metadata writer**: What module actually writes Vorbis
   comments to FLAC files? Does it read from `TrackMetadata.title`,
   `.artist`, `.album`? Or does it use a different field path?

5. **Check if the overlay functions produce the right field names**:
   `overlay_track_metadata` in `materializer_dvda_metabase.rs` maps
   metabase keys to `TrackMetadata` fields. Are the field mappings
   correct? Does `TrackMetadata` have `.title`, `.artist`, `.album`
   as `Option<String>`?

6. **Root cause**: Is the issue that:
   a. `TrackMetadata` is populated but the metadata writer doesn't
      read it?
   b. `TrackMetadata` is empty because the overlay failed?
   c. The metadata stage is disabled?
   d. The metadata stage runs but targets the wrong file?
   e. Something else?

---

## Code to read

```
Materializer + metabase overlay:
  src/convert/pipeline/materializer_dvda.rs      — materialize(), metabase loading
  src/convert/pipeline/materializer_dvda_metabase.rs — overlay functions
  src/convert/pipeline/types.rs                  — TrackMetadata, PreparedTrack, StagePolicy

Pipeline orchestration:
  src/convert/pipeline/stages.rs                 — run_pipeline_item, metadata stage

Metadata writer:
  (search for the function that writes tags to output audio files —
   likely in the pipeline or features crate)

Pipeline request construction:
  src/main.rs                                    — build_pipeline_request_template
  src/convert/pipeline/unified_request.rs        — build_pipeline_request_from_settings
```

---

## What the reasoning model should produce

1. Diagnosis: exact point where metabase tags are lost
2. Fix: ensure metabase tags flow from PreparedTrack.metadata through
   the metadata stage to the output FLAC tags
3. Verification: converted output should have TITLE, ARTIST, ALBUM,
   DATE, GENRE tags matching the metabase sidecar
