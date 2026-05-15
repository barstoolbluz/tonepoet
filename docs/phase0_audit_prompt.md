# Phase 0 — Pipeline audit

## What we need

A code-quality audit of three functions in the `tonepoet` repo, with a
verdict for each (ship-as-is / refactor / rebuild) and concrete reasons.

## What we'll give you

Repo: `https://github.com/barstoolbluz/tonepoet.git`
Commit: `644ac50` (main branch, pin for reproducibility)

Functions in scope:
1. `src/convert/processor.rs::process_item`
2. `src/convert/processor.rs::extract_and_convert_7z`
3. `src/tui/cue_parser.rs::extract_single_image_tracks`

Read their dependencies as needed (`ConversionItem`, `ConversionError`,
queue orchestration, the `tonepoet-backend` crate, etc.). Don't audit
unrelated parts of the codebase unless they materially affect the
three functions above.

## Spec

**Use case:** personal archival workflow over 40,000+ albums (CD rips,
SACD rips, LP rips, single-image CUE+FLAC pairs). Processed over weeks
of intermittent batch runs. Roughly 400,000+ per-track conversions
total over the workflow's lifetime.

**Why this audit exists:** we're about to build a multi-track-source
pipeline (SACD ISO → N tracks; CUE+FLAC → N tracks). The new code
will mirror, extend, or replace the three functions above. Before
writing it, we want to know whether those foundations are sound at
this scale, or whether they need work first.

**Success looks like:**
- Each function gets a verdict + concrete reasons grounded in
  specific code locations
- The recommendation is actionable (clear PR boundaries)
- Real strengths get preserved if you recommend refactor
- The use case (40K-album reliability) drives the analysis, not
  generic best-practices

**Failure looks like:**
- Vague advice ("improve error handling")
- Manufactured problems where the code is actually fine
- Speculation when source wasn't fetched
- A wall of style nits that misses the load-bearing issues

## If you propose code

Output full file contents I can save to disk:

```
=== FILE: <relative path from repo root> ===
<entire file, line 1 to EOF>
=== END FILE ===
```

Full files only. No diffs, snippets, or pseudo-code.
If you recommend audit-only / no code, skip this entirely.

## Reference

The `crates/sacd-rs/` crate in the same repo recently passed an
audit-driven port + validation cycle and is the current quality
high-water mark in the repo. Worth a glance to calibrate.
