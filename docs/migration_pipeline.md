# Pipeline migration notes

## Overview

tonepoet's conversion pipeline was rebuilt across PRs 1-9. Multi-track sources (7z archives, CUE+image pairs, SACD ISOs) now flow through a staged pipeline instead of the legacy monolithic `extract_and_convert_7z` function.

## Stage order

```
materialize → plan-outputs → convert → merge? → metadata → replaygain → features → publish → durable-log → terminal-event
```

- **materialize**: Parses/unpacks the source into a track manifest. Does not decode audio.
- **plan-outputs**: Assigns final output paths from the naming template.
- **convert**: Encodes each track via the backend (ffmpeg/sox).
- **merge** (optional): Concatenates tracks into a single file (`-c copy`).
- **metadata**: Tags output files (metaflac/opustags/wvtag/ffmpeg).
- **replaygain**: Applies loudgain (album mode for tracks, single-file for merged).
- **features**: Generates conversion log and CUE sheet sidecars.
- **publish**: Atomically moves staged output to final paths with advisory locking.
- **durable-log**: Writes structured JSON report.
- **terminal-event**: Updates queue status.

## Source routing

| Source type | Detection | Materializer |
|---|---|---|
| 7z archive | `.7z` extension | `SevenZipMaterializer` |
| CUE+image | CUE sidecar or embedded CUESHEET + single-image layout | `CueImageMaterializer` |
| SACD ISO | `.iso` extension + SACD Master TOC magic | `SacdIsoMaterializer` |
| Single audio file | Everything else | Legacy `determine_output_path` path (no pipeline) |

## Failure policy

- **Default** (`FailAlbumOnAnyTrackFailure`): Any track failure blocks the entire album. No output is published.
- **Partial** (`AllowPartialAlbum`, `--partial` flag): Failed tracks are dropped. Successful tracks are published. Queue status is `Partial`.

## Stage requirements

Stages are binary: `Enabled` or `Disabled`. An enabled stage that fails blocks the album. A disabled stage is skipped.

- `--replaygain off` → ReplayGain disabled
- `--no-metadata` → Metadata tagging disabled
- `--no-features` → Feature generation (log/CUE sidecars) disabled

## Crash-resume model

- `PipelineRequest` is the persisted job input.
- On restart, the pipeline deletes orphaned staging directories and re-runs from `materialize`.
- The pipeline never trusts a half-finished staging tree.
- `PreparedSource` is re-derivable diagnostic data, not job state.

## Secret redaction

- Archive passwords flow as `SecretString` — `Debug`/`Display` always print `<redacted>`.
- Durable logs serialize `RedactedPipelineRequest`, never the raw request.
- Tool command records redact secret args by index.

## Durable log location

Logs are written to `{output_root}/.tonepoet-logs/` as JSON files named `{job_id}-{item_id}.json`.

## Durable log schema example

```json
{
  "request": {
    "job_id": "job-abc123",
    "item_id": "abc123",
    "container": "/path/to/album.7z",
    "source": {
      "archive_password": "<redacted>",
      "sacd_area": null,
      "cue_sidecar": "PreferSidecar",
      "track_selection": "All"
    },
    "target_format": "Flac",
    "encode": {
      "backend": "Auto",
      "bitrate": null,
      "compression_level": 8,
      "dither": "Auto"
    },
    "merge": false,
    "output_root": "/output/dir",
    "naming": {
      "template": "%NN% - %TITLE%",
      "per_album_subdir": true,
      "collision_policy": "Fail"
    },
    "publish": {
      "overwrite": "FailIfExists",
      "same_filesystem_required": false
    },
    "log": {
      "root": "/output/dir/.tonepoet-logs",
      "write_for_blocked": true
    },
    "stages": {
      "metadata": "Enabled",
      "replaygain": "Enabled",
      "features": "Disabled"
    },
    "failure_policy": "FailAlbumOnAnyTrackFailure"
  },
  "source": {
    "container": "/path/to/album.7z",
    "kind": "SevenZip",
    "tracks": [ ... ],
    "album_metadata": { ... },
    "provenance": {
      "source_kind": "SevenZip",
      "source_sha256": null,
      "tool_versions": {},
      "extracted_at": "2026-05-17T12:00:00Z"
    }
  },
  "plan": {
    "album_dir": "/output/dir/Album",
    "entries": [ ... ]
  },
  "artifacts": { ... },
  "published": { ... },
  "outcome": {
    "Complete": {
      "tracks": [ ... ],
      "stages": [
        { "stage": "Materialize", "outcome": "Ok" },
        { "stage": "PlanOutputs", "outcome": "Ok" },
        { "stage": "Convert", "outcome": "Ok" },
        { "stage": "Merge", "outcome": "Skipped" },
        { "stage": "Metadata", "outcome": "Ok" },
        { "stage": "ReplayGain", "outcome": "Ok" },
        { "stage": "Features", "outcome": "Skipped" },
        { "stage": "Publish", "outcome": "Ok" },
        { "stage": "DurableLog", "outcome": "Ok" }
      ]
    }
  },
  "durable_log": "/output/dir/.tonepoet-logs/job-abc123-abc123.json"
}
```

## New CLI flags (PR 10)

| Flag | Effect |
|---|---|
| `--track N` | Select single track (1-based) |
| `--track-range A-B` | Select track range (inclusive) |
| `--area stereo\|multichannel` | SACD area selection |
| `--no-cue` | Ignore CUE sheets |
| `--partial` | Allow partial album output |
| `--overwrite` | Overwrite existing output (with backup) |
| `--naming TEMPLATE` | Output naming template |
| `--no-metadata` | Disable metadata tagging |
| `--no-features` | Disable feature generation |
