# Known issues at push of the G-round stack (audited 2026-07-16, HEAD 7eb466e)

Findings from the 4-way adversarial audit of 5a0870c..7eb466e, pushed with the
stack deliberately (fixes deferred to the next reasoning-model brief). Every
item was mechanically verified with file:line evidence at 7eb466e.

## HIGH — user-visible breakage

1. **CLI `tags-mb` cannot save single-image sources.** The S9 alignment gate
   in `apply_audio_tag_changes_with_save_blocks_and_progress`
   (`src/tui/probe.rs:~6612`) skips EVERY path when ANY row's value count
   differs from the path count — but the CLI path expands per-track
   TITLE/ARTIST/ISRC rows to n-tracks values over one image
   (`src/main.rs:2318 → keybindings.rs:11044`), so the whole save (including
   aligned CUESHEET + album rows) is refused; exit code 3. TUI unaffected
   (explicit RowScope API).
2. **Source-depth fail-closed regressed lossy targets.** The new
   `resolve_target_bit_depth` (`tonepoet-pipeline/src/plan.rs:~1543`) errors
   for unmeasured-PCM and Unknown sources on EVERY target format; the old
   code defaulted for non-PCM-lossless targets (a lossy encode makes no depth
   promise). `convert x.shn --format mp3` (also APE/TrueHD/DTS-HD → lossy,
   any unclassified codec → anything) now fails at planning, with an error
   naming "PCM-lossless" for a lossy target — and the TUI disables the depth
   pill for lossy formats, so the remedy is unreachable there.

## MEDIUM

3. Classifier table misses real ffprobe codec_name spellings:
   `wmalossless`, `mp4als`, `ralf`, and `dst` (DST-compressed DFF classifies
   Unknown, not Dsd) → measurable sources fail closed under `Source` depth
   (`src/convert/pipeline/types.rs:~1670`). `wmalossy` is not a real codec
   name; `pcm_alaw/mulaw` land Pcm.
4. GNUDB completions gate identity but still REPLACE whatever overlay is open
   (`event_loop.rs:4186/4210/4240`) — a slow query completing over a dirty
   editor destroys it. Latent (menu entry disabled) but the round's purpose
   was to make this flow safe. Related: gnudb workers hold authority but have
   no panic wrapper and no cancel command (wedge until a fresh `:tags-mb`
   silently retires); a gnudb read failure strands the parked dirty editor
   invisibly while the quit gate blocks on it (`event_loop.rs:4222`).
5. D1-class escape in the invalid-selection arm of picker accept
   (`event_loop.rs:~6082`): an UNASSIGNED picker with out-of-range selection
   calls the unconditional `restore_parked_editor`, killing a foreign live
   operation's authority/latch. Sibling of the fixed refusal arm.
6. `MetadataEditorSplitCueAlbumGroupingComplete` is identityless and installs
   its editor unconditionally (even on Err), which can replace a live picker
   and cause the reconciler to cancel a live MB operation
   (`message.rs:606`, `keybindings.rs:~10921`). Same class:
   `CuePreviewComplete`/`CueMbComplete`/`CueFillComplete`.
7. Unified FILE-ref resolver drops direct DTS/AC3 refs the queue previously
   accepted (`classify.rs` has no dts/ac3 arms; `split_cue_album.rs:548`),
   and reports an existing non-audio ref as "was not found" (no
   exists-but-non-audio variant in `SplitCueReferenceResolution`); the
   pipeline materializer's own resolver accepts any existing file — two
   resolvers, divergent semantics. Also: a non-audio direct target with an
   audio same-stem sibling silently rebinds to the sibling.

## LOW

8. `TONEPOET_REQUIRE_TOOLS` semantics split: `"1"`-exact in
   `depth_format_matrix.rs:66`/`keybindings.rs:34591` vs any-value in
   `unified_synthetic_cue_output_boundary.rs` — CI with `=true` silently
   skips the depth matrix.
9. Log-label defects: the default-policy label REPLACES the verified output
   description instead of accompanying it (`stages.rs:~14306`); the 20-bit
   note fires for lossy targets and contradicts the mapped source width in
   the same line (`stages.rs:~14335`).
10. Preflight timing: the wvunpack preflight runs after materialization and
    pre-actions, contradicting its doc ("before expensive work"); serial
    path has no test.
11. Lossy-CUE residuals (local commit 7eb466e): shortfall floor (8192) makes
    the truncation guard vacuous for tails under ~186 ms; boundary ADMISSION
    still trusts header duration (an understating Xing-less VBR header can
    reject a legitimate final track before LossyTail measures); interior
    truncation errors name the .tmp path, not the image.
12. Weakened/self-satisfying sentinels: `gnudb_back` pin is variant-only;
    `custom_format_sentinel` no longer carries `AudioFormat::Custom`; four
    command.rs self-scan sentinels have EOF-unbounded windows satisfied by
    their own literals (pre-existing).
13. Nits: duplicated fixture line `plan_bridge.rs:1439/1441`; dead
    `Unspecified` arm `plan.rs:1561`; stale "line 26" marker comment
    `plan_bridge.rs:146`; prefetch dead on the compat `ConfirmAction::MbBack`
    picker; `tonepoet-pipeline/README.md` 20-bit claim lives in the wrong
    crate's README.

## Cleared by the same audit (for the record)

Decision-table coherence (PCM-lossless), passthrough-first incl. force_encode,
carrier-first depth args, dither plan/log parity across all three gates,
GNUDB↔MB mutual exclusion, G3 binding fail-closed + FILE-order-correct,
T3 directives in both emitters, T9 determinism with a REAL final constraint
check, S10 stop ownership, S12/S13 mechanics, the lossy-CUE commit's
concurrency/naming/backfill, CLI flag parsing incl. 640, sentinel fingerprint
fallout, and API_SURFACE.md accuracy (14/15 claims).
