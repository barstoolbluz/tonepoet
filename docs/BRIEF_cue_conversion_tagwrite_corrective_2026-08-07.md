# tonepoet — CUE-conversion tag-write CORRECTIVE (2026-08-07)

You are starting **fresh**; everything you need is in this bundle. Outcomes + guardrails;
diagnosis is evidence, not prescription — you choose HOW.

**Project:** tonepoet (Rust CLI + TUI, tokio, edition 2021), version 0.4.6 — do not bump.
Gate `cargo test --workspace --no-fail-fast` must be green ×2.

**This is a CORRECTIVE that builds ON TOP of your just-delivered sidecar-CUE conversion
metadata change** (already applied in this bundle's tree). That change correctly transfers
cue metadata into output NAMING and LOGGING — but it does NOT get the metadata into the
output file's embedded TAGS, and two of its own new tests fail. Finish it.

## Symptom (verified on the real album)

Converting `~/torrents/Michael Jackson - Thriller. 1984 Japan/` (nine `.dff` untaggable
carriers + one sidecar `.cue`) to FLAC now produces correctly NAMED files
(`01 - Wanna Be Startin' Somethin'.flac`) whose conversion.log says
`Metadata source: Sidecar CUE: …(track 1)` — but the output FLAC vorbis comments contain
**only `ENCODER` + `REPLAYGAIN_*`**. No TITLE, ARTIST, ALBUM, DATE, GENRE, ISRC. The
cue metadata drives naming but never becomes embedded tags. The log line
`Metadata: Skipped (already satisfied by the output planner)` is the tell.

## Root cause (consensus-verified by two independent audits)

The orchestrator metadata-write stage is SKIPPED for this path:

1. `planner_metadata_already_satisfied` (src/convert/pipeline/stages.rs ~4005; consulted at
   the orchestration skip ~26173) returns true, so the Metadata stage never runs — and that
   stage (`apply_metadata` → `authoritative_metadata_tags`, stages.rs ~4223/4536) is the
   ONLY writer of cue-derived vorbis comments. ReplayGain is a separate later stage, which
   is why only RG survives.
2. It returns true because `metadata_obligations_for_request` →
   `source_needs_authoritative_metadata` (src/convert/pipeline/plan_bridge.rs ~697) gates a
   `SourceKind::SingleFile` on `album_metadata.extra` containing
   `FALLBACK_RECOVERED_METADATA_EXTRA_KEY`.
3. The untaggable+cue materialization path never sets that key: the cue path carries
   `metadata_recovered_by_fallback = false`, and `apply_recovered_album_totals_from_metadata`
   (materializer_single.rs ~329-360) early-returns without inserting the key when that flag
   is false (guard at ~334). (The taggable/APE-fallback path DOES set it, which is why
   taggable conversions embed tags correctly.)
4. So the planner "already satisfied" test passes even though ffmpeg's `-map_metadata`
   pulled from the empty intermediate WAV and the untaggable `.dff` (both tagless) — nothing
   was actually embedded.

**The fix template already exists.** DVD-Audio has exactly this carve-out:
`dvd_audio_artifact_has_authoritative_metadata` (stages.rs ~4049, called inside
planner_metadata_already_satisfied at ~4031) FORCES the metadata stage to run because DVD-A
tags are "materializer-authored … resolved from a sidecar, so the planner cannot satisfy
them while encoding." Sidecar-CUE-sourced tags on an untaggable carrier are the same shape.
There is currently NO cue equivalent.

**Test-coverage gap that let this ship:** the delivery's conversion tag test (e.g.
`untaggable_dts_sidecar_cue_drives_real_flac_naming_and_tags_idempotently`) calls
`apply_metadata()` DIRECTLY (stages.rs ~8597), bypassing the skip gate — so it proves the
stage CAN write cue tags but never exercises the pipeline decision that SKIPS it.

## Outcomes

**C1 — Cue tags are embedded.** Converting an untaggable-carrier album with a valid sidecar
cue writes the cue-resolved per-track and album fields into the output file's tags
(TITLE/ARTIST/ALBUM/ALBUMARTIST/DATE/GENRE/CATALOGNUMBER/ISRC/TRACKNUMBER as the cue
supplies them), for FLAC and the other taggable output formats. The orchestrator metadata
stage must run for this path instead of being skipped. Model the carve-out on the DVD-Audio
precedent: a sidecar-cue-sourced (materializer-authored) track whose `authoritative_metadata_tags`
are non-empty forces the stage. Naming and logging (already correct) must not regress.

**C2 — Scope the fix; don't break the correct skip.** For a taggable source (FLAC→FLAC)
where ffmpeg `-map_metadata` genuinely copied the source's own tags, skipping the
orchestrator stage is CORRECT and must stay. The new force-run must trigger only when the
authoritative tags came from the sidecar cue (materializer-authored), not from the carrier —
i.e. keyed on the cue-metadata provenance (`sidecar_cue_track_metadata` / the same signal
that drove naming), analogous to how DVD-A keys on its source kind.

**C3 — Full-pipeline proof, not direct-stage.** Add a test that drives the REAL orchestrated
pipeline (the path that consults `planner_metadata_already_satisfied`) end-to-end for an
untaggable-carrier + cue album and asserts the OUTPUT FILE's embedded tags contain the cue
title/artist/album — the assertion that would have caught this. The existing direct-
`apply_metadata` tests may stay but are insufficient alone.

**C4 — Fix the two failing tests this delivery shipped red.** Currently
`nine_dff_metadata_sidecar_album_drives_real_conversion_naming_and_flac_tags` and
`untaggable_dts_sidecar_cue_drives_real_flac_naming_and_tags_idempotently` FAIL. Diagnosed
layers (verified while integrating):
  - Both set `naming.folder_template` but inherit `per_album_subdir = false` from
    `request_for_case`, so `album_dir` never expands — enabling `per_album_subdir = true`
    (the author's evident intent, since they assert the subfolders) advances them past the
    album_dir assertion. Fix in the tests.
  - DFF, then: the DSD→PCM `sox` command is killed (~8ms, SIGKILL) on the ~0.01s synthetic
    fixture — the command timeout collapses to sub-process-startup size for a near-zero-
    duration fixture (production static timeouts are 30s/60s/6h, so this is a test-fixture/
    timeout-derivation mismatch, deterministic on any host). Resolve so real sox can run
    (longer fixture, a timeout floor, or a test knob — your call; document it).
  - DTS, then: a `PRE_EMPHASIS` managed-key is asserted-exactly-once but appears zero times
    while all cue tags write correctly. Decide whether cue `FLAGS PRE` should propagate to a
    `PRE_EMPHASIS` output tag (real behavior) or the assertion over-specifies, and reconcile.
  These must end green; do not delete the tests to pass.

## Guardrails
- Preserve everything the delivery got right: cue→naming, cue→logging (Metadata source:
  Sidecar CUE), `aggregate_metadata_target_priority` honoring, the one-source-of-truth cue
  mapping, split-source and taggable behavior.
- Lodestar-governed (docs/metadata_source_selection_heuristic.md, bundled). Full gate ×2.
  No regressions in the ~5,650 other tests. No new deps. Version 0.4.6.
- The established authoritative metadata writer stays the only tag writer; no second writer.

## Deliverables
Complete replacement files or unambiguous patches; a WHY summary (the carve-out you added
and how it's scoped; the timeout/PRE_EMPHASIS resolutions); the test list including the new
full-pipeline embedded-tag test; honest unverifiable-in-your-environment note.

## Bundle manifest
- This brief; docs/metadata_source_selection_heuristic.md (LODESTAR).
- Complete `src/` tree (delivery ALREADY APPLIED — this is a corrective on top) +
  `crates/tui-file-picker`, `crates/tonepoet-backend`, `crates/tonepoet-features`; root
  `Cargo.toml`, `CLAUDE.md`.
NOT included: other workspace crates, target/, other docs. If anything is missing, say so
rather than guessing.
