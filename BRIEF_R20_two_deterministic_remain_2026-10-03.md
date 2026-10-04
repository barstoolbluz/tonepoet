# Brief R20 — eight of ten cleared, two deterministic failures remain

Date: 2026-10-03
Base: the supplied `tonepoet-src.tar.gz` is the `auto-deemphasis` branch with
the R19 corrective and CUE `FLAGS PRE` evidence R1–R3 snapshot applied
unchanged. It compiles with zero warnings; no edits were needed.

The gate went from 7267 passed / 10 failed to 7291 passed / 4 failed.

## Correction to the previous brief

R19 said "nine" deterministic failures. It enumerated eight, and eight is the
count: ten total, less the Reference qualification mismatch and the known
concurrency flake.

## Expected

`installed_reference_qualification_binds_the_current_locked_sources` still
fails. The snapshot changes three Reference-locked files: `stages.rs`,
`plan.rs`, `semantic_plan.rs`. Requalification runs on this host.

## Passes in isolation

`convert::pipeline::materializer_single::tests::single_file_writer_round_trips_ordered_performer_and_arranger_lists`

```
native FLAC metadata-region tag write refused … cannot start metadata write
```

## The two that remain

Both fail in isolation as well as in the suite.

### `tonepoet-pipeline/src/semantic_plan.rs:8286`

`one_track_album_keeps_group_binding_with_track_equivalent_policy_and_terminal_proof`

```
left:  …route=ffmpeg-direct:flac:176400:24:dither=none
right: …route=ffmpeg-direct:flac:source:24:dither=none
```

This test was passing before the R19 corrective.

### `src/tui/context_menu.rs:5207`

`context_menu_convert_last_used_audio_directory_commits_after_async_expansion`

```
expected ConvertDeemphasisBatchPreflightComplete,
got ProbeResult { generation: 2, path: "…/album/01 - One.flac", … }
```

This test was failing before the R19 corrective with a different assertion:
`left: Convert  right: Browse`.

## The outcome we want

The gate is green, apart from the Reference requalification which runs here.

## State

7291 passed / 4 failed across 63 targets, zero warnings.
`tonepoet-true-peak` is gated separately and was not run; CLAUDE.md records it
at 160/0.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so.
