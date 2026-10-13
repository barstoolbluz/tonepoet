# ADDENDUM — `BRIEF_R19_convert_hdcd_decode_planning_FINAL_R2_2026-10-03.md`

The brief is unchanged and still authoritative. It was written on 2026-10-03;
the tree has moved through R29C–R36 since. Three notes.

**The `ⓘ` pattern has been generalized.** The brief says to reuse the
de-emphasis information affordance. As of R32 the same mechanism —
a two-cell hitbox beside the row label opening `ActiveOverlay::Notice` — also
serves the PCM and DSD true-peak rows through one shared
`TuiButton::TruePeakInfo` and one shared message. That two-row case is closer
to what HDCD needs than the single-row de-emphasis one.

**The analysis cache moved.** `ANALYSIS_ALGO_VERSION` went 27 → 28 in R31, and
the metadata-only `analysis_cache` UPSERT no longer carries a stale
`clipping_count` across a version change. Relevant to section 10 and
section 24.

**Six of the files named in section 26 are source-locked:**
`semantic_plan.rs`, `plan.rs`, `plugins.rs`, `fingerprint.rs`,
`plan_bridge.rs`, `stages.rs`. Section 26 lists them as files to inspect;
inspecting costs nothing, but editing any of them unbinds the installed
Reference qualification and requires a full requalification on acceptance. The
UI-side files it names are not locked.

## State of the tree

Gate: **7459 passed / 0 failed**. Reference qualification was reinstalled on
2026-10-13 (`f3286fc`) and binds this build; the freshness suite is 3 of 3.

Verified against this tree for the brief's own premises: `HdcdResult`,
`detect_hdcd` and `parse_hdcd_output` exist in `src/tui/analyze.rs`; the
detector shells `ffmpeg -af hdcd -f s24le /dev/null` and parses an info line
already carrying `peak_extend`, `max_gain_adj` and `transient_filter`
separately; the pinned FFmpeg is 7.1.3 and provides the `hdcd` filter.
