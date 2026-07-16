# Brief: apply-round audit findings — Source-depth policy, float classification, persistence routes, MB escape hatches

**Baseline:** complete tree at commit `3c8992f` (workspace suite 4083/0, zero cold-build warnings, strict real-tool matrix green).
**Provenance:** a 4-way adversarial audit of the previous round's apply (f8009b6..3c8992f). Every finding below was mechanically verified against HEAD with file:line evidence. The previous round's architecture held up well — operation-ID allocation, reducer gating order, stop-generation races, the shared source-depth resolver, and the typed-carrier design were all cleared. What follows are the escapes.

**Line-number caveat:** all `file:line` references were taken at `3c8992f`. Verify context before editing; functions in `keybindings.rs`/`event_loop.rs`/`stages.rs` are large and line numbers drift fast.

---

## Non-negotiable invariants (unchanged)

1. A user-requested bit depth is honored exactly or the conversion fails closed with an actionable message. Never silently substituted — and never *claimed* from facts that don't support it.
2. The conversion log reports measurements as measurements and plans as plans.
3. A stale or duplicate async completion is a TOTAL no-op: no cache writes, no status, no overlay changes, no latch traffic, no authority release.
4. Track-scoped rows are never written as whole-file tags — and every accepted edit either persists or is refused with an explicit reason. Silent drops are forbidden.
5. Complete-file delivery: whole files only, plus `IMPLEMENTATION_REPORT.md`. Tests must compile and target the real pipeline (real encoded fixtures + `RealToolRunner` with per-tool skip guards; planned ffmpeg/sox commands bypass the `ToolRunner` seam).
6. Acceptance is `cargo test --workspace` (never plain `cargo test` — it silently skips every sub-crate).

---

## Tier 1 — merge-gating

### G1 — Fail-closed `Source` depth breaks the CLI's default flows (HIGH)

The new policy (`plan_bridge.rs:114-127`) errors for PCM-lossless targets whenever `resolve_source_pcm_depth(track)` is `None`. But:

- `PipelineSettings::default()` is `BitDepthTarget::Source` (`tonepoet-pipeline/src/settings.rs:67`); `ConversionOptions::default()` has `target_bit_depth: None` (`src/convert/formats.rs:658`) which maps to `Source` (`unified_request.rs:602-611`); **`src/main.rs` exposes no bit-depth CLI flag at all.**
- SACD tracks always carry `bit_depth: None` (`materializer_sacd.rs:104-111, 595`). Archives with lossy or DSF/DFF content likewise yield `None`/1-bit (`materializer_archive.rs:1706-1709`, `materializer_single.rs:139-142`).

Net: `tonepoet convert album.iso --area stereo --format flac` — a flagship flow — now fails every track with "choose an explicit bit depth", advice the CLI cannot express. The TUI is unaffected (`convert_actions.rs:309-316` always sends explicit `Pcm(...)`).

**Required design.** Keep the honesty invariant, restore the flows:

1. Add a `--bit-depth` CLI flag (`16 | 24 | 32 | 32f | 64f | source`) threaded through `ConversionOptions` → `unified_request.rs`.
2. Define the DEFAULT (no flag) as a **documented, logged decision**, not a hidden fallback: for sources with **no PCM representation at all** (DSD, lossy codecs), an unqualified conversion resolves to the format's conservative default (24-bit for FLAC/ALAC/WavPack/WAV/AIFF via `default_pcm_depth_for_format`) and the conversion log labels it `requested 24-bit (default for DSD source)` — a plan label, never a measurement. This is honest: "same as source" is *undefined* for these sources, so a default is not a substitution.
3. Fail closed ONLY where "same as source" is meaningful but unmeasurable: a PCM source whose depth probe genuinely failed. That case keeps the current error.
4. `BitDepthTarget::Source` given explicitly (flag `source` or a future UI choice) over a DSD/lossy source: also resolve to the documented default with the log label — do NOT error, because the CLI default must be expressible explicitly too. (Alternative acceptable design: explicit `source` over DSD errors while the *unset* default resolves — if you choose this, say so in the report and make the CLI error name the flag to use.)
5. 20-bit DVD-A (`dvda_lpcm.rs:91` supports 16/20/24) is currently rejected as "could not be measured" — false; it WAS measured, `PcmBitDepth` just has no `Int20` (`plan_bridge.rs:645-657` maps only 8/16/24/32/33/320/640). Resolve 20 → `Int24` with a log note (`20-bit source stored as 24-bit`), or add `Int20` if the encoder matrix genuinely supports it (it does not — prefer the 24 note).
6. Bridge vs planner: the bridge errors before `plan_conversion` ever runs, so the planner's documented passthrough allowance ("a proven passthrough may preserve an unknown-depth stream") is unreachable for pipeline callers (`plan_bridge.rs:120` fires before `plan.rs:583` passthrough short-circuit). Restructure so a same-format unknown-depth request can still reach the planner's passthrough decision; only fail when an encode is genuinely required. Update `API_SURFACE.md` if the contract changes.
7. Update the six fixtures that were patched with explicit depths during the apply (`plan_bridge.rs:1046,1245`; `track_executor.rs:1197,1236,1275,1321`) to match the final policy — at least one regression must pin the DEFAULT CLI shape end-to-end (unknown-depth source, no depth flag → succeeds at the documented default with the log label).

**Implementation constraints (verified against HEAD — ignore at your peril):**

- The fail-closed logic lives in **TWO places**: the bridge (`plan_bridge.rs:114-127`) AND the public planner's `resolve_target_bit_depth` (`plan.rs:~1538`, error "requires an authoritative source PCM representation"). Change both coherently or the CLI flow still fails at the planner.
- The DSD/lossy/unmeasured-PCM tri-state has **no data source today**: `resolve_source_pcm_depth` returns a bare Option (None for all three), and `SourceAudioCoding { Pcm, Dsd, DvdaUnknown, Unknown }` has no `Lossy` variant — `source_audio_coding_from_codec_name` (`materializer_archive.rs:1723-1737`) buckets mp3/aac/vorbis/opus into `Unknown`, the same bucket as a failed probe, and the CUE/single materializers set `coding` inconsistently. Add a `Lossy` arm (or equivalent typed class), classify from `codec_name` in EVERY materializer (share the helper with G2/T8), and implement the decision table over `{Pcm, Dsd, Lossy, Unknown}`: Dsd/Lossy → default+label; Pcm with None depth → fail closed; Unknown → fail closed (conservative).
- Resolve the default into `settings.target_bit_depth`, NEVER into DSD `SourceInfo.bit_depth` — `SourceInfo::validate()` rejects PCM depth facts on DSD sources, pinned by `dsd_source_rejects_pcm_bit_depth_fact` and `dsd_source_rejects_bit_depth_even_without_sample_kind` (`planning.rs:559-585, 755-775`).
- **Pinned tests your policy flips or grazes — handle each explicitly:**
  - `dsd_to_pcm_source_depth_is_undefined_and_rejected` (`planning.rs:223-252`): FLIPPED — rewrite to pin the new default+label behavior.
  - `pcm_lossless_source_target_requires_authoritative_source_depth` (`planning.rs:152-166`): SURVIVES as the item-3 pin (unmeasured PCM keeps the error) — do not delete.
  - `unknown_source_representation_fails_closed_for_pcm_lossless_target` (`plan_bridge.rs:1472-1497`, asserts the exact error strings): restate per the final classification — an unknown-coding staged WAV is `Unknown` → keeps failing closed.
- The log label needs its OWN summary slot: the "[output depth unverified]" suffix (`stages.rs:14268-14289`) keys on `verified_output_bit_depth.is_none()`, and a DSD→FLAC default conversion normally VERIFIES its output depth — do not piggyback the unverified suffix.
- CLI mapping: `unified_request.rs::bit_depth_target` maps `Some(320)→Float32` but has **no 640 arm** (`Some(640)` falls into the `Some(_)→Source` catch-all) — extend it, reject unmappable numerics with an error (never a silent fallback), and update the `settings_sentinel.rs` projection tests that pin options→settings propagation. Wire the clap flag with help text.

### G2 — Lossy CUE images misclassified as authoritative Float32 sources (HIGH)

`materializer_cue.rs:1580-1586` sets the float descriptor from `sample_fmt.starts_with("flt")` — but MP3/AAC/Vorbis/Opus **decoders** all report `flt`/`fltp`. An MP3+CUE album becomes an "authoritative Float32 source":

- target FLAC/ALAC + `Source`: fails closed with an error claiming the MP3's "source PCM representation" is float;
- target WAV/WavPack + `Source`: silently emits Float32 output logged as a source-derived fact — the exact honesty violation this program exists to prevent.

**Fix (precise, verified against the fields actually parsed at `materializer_cue.rs:1543-1586` — there are no separate "stream flags" available there):** classify float **iff** `codec.starts_with("pcm_f32")` / `("pcm_f64")`, **or** `codec == "wavpack"` AND `sample_fmt` starts with `flt`/`dbl` — for the wavpack decoder specifically, `fltp` is emitted only for genuinely float streams (int WavPack decodes as `s32p`; empirically verified, and `measured_pcm_depth` at `stages.rs:~1892` documents the same asymmetry — use it as prior art). Every other codec: integer bits only; lossy codecs → `None` depth + the G1 `Lossy` class. Do NOT bolt wvunpack into source probing. Regressions: MP3+CUE → FLAC (succeeds at default), MP3+CUE → WAV (integer default output, not float), float-WavPack+CUE → WAV (float class PRESERVED).

### G3 — Multi-FILE single-CUE surfaces accept per-track edits then silently never persist them (HIGH)

The new shared admission accepts a shape the old editor rejected: ONE cue referencing ≥2 images (`keybindings.rs:9578-9589` admits any `SyntheticAlbumPart`). `build_metadata_editor_for_cue_surfaces` takes the `sorted.len() == 1` branch (`keybindings.rs:10518-10557`): paths = all images, Track-scoped rows created, **no synthetic sheet installed**. On save:

- `metadata_editor_unpersistable_per_track_reason` (`keybindings.rs:6077+`) returns `None` (its checks require `paths.len()==1` or a synthetic sheet), so the edit is allowed;
- `regenerate_cuesheet_for_save` early-returns `Ok(false)` at `keybindings.rs:9276-9277` (`n_paths != 1`) BEFORE the per-track-dirty refusal at ~9336;
- the whole-file writer correctly skips Track rows (`probe.rs:6650-6656`).

Net: user edits track titles + ALBUM, saves; ALBUM persists, titles vanish, status says success, tab stays dirty forever.

**Preferred fix (reuses proven machinery — do NOT invent a new route):** the `sorted.len()==1` branch of `build_metadata_editor_for_cue_surfaces` simply omits installing a `CueAlbumSyntheticSheet`; the multi-surface branch installs one and everything downstream already handles this shape — `regenerate_unified_cue_album_cuesheet_for_save` (`keybindings.rs:9161-9248`) consumes the sheet, `cue_album_generate_synthetic_cuesheet` (`:10185+`) already emits multi-FILE sheets, and the writer replicates the regenerated CUESHEET to every member's embedded tag (`probe.rs:6659-6660`). Install the sheet for the 1-cue-N-images shape (binding tracks to member images from the cue's own FILE structure). Do NOT attempt a sidecar route — `cue_sidecar_writeback_plan_for_state` is single-image-only and `regenerate_cue_with_overrides` is single-FILE-only. Fail-closed refusal (the single-path error at ~9336) remains the fallback if the sheet cannot bind tracks to members. Silent drop is not an option. Regression: 1 cue × 2 images × 4 tracks, edit a track title, save, re-open, title persisted (or refusal surfaced).

### G4 — Two MB-lifecycle escape hatches destroy live operations (HIGH)

1. **UNASSIGNED-picker accept kills the blocking operation** (`event_loop.rs:5886-5898`): accepting an `:mb-back` picker while any operation is active correctly refuses via `begin_tags_mb_apply_operation` — but the `Err` arm calls `restore_parked_editor(app)`, the UNCONDITIONAL terminator (`finish_metadata_editor_tags_mb_operation` at 5564-5568/5343-5346 clears `active_tags_mb_operation` and the latch). The operation that caused the refusal is destroyed, while the status text says "cancel it before selecting again". **Fix:** `restore_parked_editor` internally calls the unconditional `finish_metadata_editor_tags_mb_operation` as its first act — there is no restore-only primitive. Add `restore_parked_editor_without_finishing(app)` (park-slot → overlay only) and use it on the refusal arm (`event_loop.rs:~5893`) and any other foreign-authority path. Scoped precedent: `finish_tags_mb_operation_if_current` / `cancel_mb_select_operation` (`event_loop.rs:~5350/~5503`). Note: the post-input reconciler is already safe after a plain restore (it early-returns for non-picker-owned foreign ops and honors `pending_mb_select`) — do not "fix" the reconciler.
2. **GNUDB never got the identity treatment** (`context_menu.rs:1682-1704`; `event_loop.rs:4137-4142, 4165, 4188`; `command.rs:4178-4215`): gnudb grouping/query/read/multi-disc completions mutate the overlay unconditionally — a stale completion replaces a live MB picker or destroys an unparked dirty editor, and the reconciler then cancels the live MB operation. `:gnudb-back` (`command.rs:4212`) drops a possibly-latched editor. **Fix:** either give the gnudb flow the same operation-identity + phase gating (it can share `TagsMbOperationId`), or — since GNUDB is already hidden from the context menu — explicitly gate its completions on "no MB operation active AND the overlay is still gnudb-owned" and park (never drop) editors. State the choice in the report.
3. Same family, smaller: right-click outside a Selecting picker calls the unconditional terminator (`keybindings.rs:20494-20497`) where every other cancel path uses scoped `cancel_mb_select_operation` — route it through the scoped cancel. And `run_context_action_restoring_parked` (`keybindings.rs:5776-5790`) drops a parked editor when the action leaves a *Selecting* picker (the stale-picker accept path) — preserve the parked editor there.

---

## Tier 2 — should fix in this round

- **T1. Duplicate TOC-zero-match completion spawns a second text search** (`event_loop.rs:4999-5002`): the fallback dispatch does not transition the phase, so a duplicated completion passes the Lookup gate and fires a second HTTP search. Add a `Lookup`-substate or a consumed flag so the fallback dispatch is once-only.
- **T2. Latch leak on worker panic** (`command.rs:11503-11512`, `event_loop.rs:5190-5209`): a panicking lookup worker never sends a completion; the operation and editor latch stay active forever (reconciler only covers picker-owned phases, `event_loop.rs:5533-5534`). Wrap workers so panic/JoinError produces an `Err` completion, or extend the reconciler with a liveness/timeout check for Discovery/Grouping/Lookup phases.
- **T3. Unified-album saves strip `FLAGS PRE`/track REM directives** (M-series): the parser retains `CueTrack.directives` (`cue_parser.rs:49-58`) and single-image regeneration re-emits them (`cue_generate.rs:274-282`), but `cue_album_generate_synthetic_cuesheet` (`keybindings.rs:10185+`) and the queue's merged-CUE builder (`queue_expansion.rs:~1125-1150`) emit none — any unified save destroys pre-emphasis flags on every member image. Plumb directives through `CueAlbumTrackSource` and the synthetic builders. (Pre-existing loss, but the data is now in hand and the E-series claim says round-trip.)
- **T4. `:cuesheet-edit` staging creates per-track rows as `RowScope::File`** (`keybindings.rs:11681-11717`, line 11704): rescued today only by the length fallback the RowScope marker was built to eliminate. Set `Track` explicitly on both create and update branches (mirror `cue_album_upsert_per_track_entry`).
- **T5. Log-vs-plan dither disagreement** (both directions): log predicate `stages.rs:~14349` (`is_float() || target < source`) vs planner gate `pcm_conversion_reduces_depth` (defined `plugins.rs:1686`, **three call sites**: SSRC `:359`, ffmpeg/soxr `:1322`, sox `:1547` — fix all three). Float32→Int32 logs "TPDF dither" that never ran; 16-bit CUE image→16-bit target with dither enabled APPLIES dither to bit-identical content (carrier Int32→Int16 "reduces depth") while the log says none. Unify: the planner's dither decision should use the resolved TRUE source class/width, and the log must reflect what the plan actually contains. **HARD CONSTRAINT: leave `SourceInfo.bit_depth` carrier-first — do not touch it** (`plan_bridge.rs:549-554` comment; pinned by `cue_wav_target_reencodes_from_s32_carrier_to_original_source_depth` — a prior regression was caught exactly here: a 16-bit target from an s32 carrier still needs explicit depth args). Thread the true class through a NEW channel (prefer a `SourceInfo` sibling field like `true_source_depth: Option<PcmBitDepth>`); if you grow `PipelineSettings` instead, you MUST update the settings-sentinel + fingerprint suites (`tests/settings_sentinel.rs`, `tonepoet-pipeline/tests/settings_fingerprint.rs`).
- **T6. FLAC/ALAC float targets fail late with raw tool errors**: extend `reject_unsupported_resolved_depth` (`plan.rs:1518-1535`) and `PipelineSettings::validate` (`settings.rs:120-132`) to reject `(Flac|Alac, Float32|Float64)` with the same actionable message style as WavPack-float. (Interacts with G2: once lossy sources stop resolving float, the common trigger disappears, but explicit float targets remain expressible.) **WARNING:** the sentinel fixtures currently pair Float32 with FLAC as a VALID settings shape — `settings_fingerprint.rs:~82` and the `tests/settings_sentinel.rs` valid sentinels. Re-home their Float32 coverage onto WAV (keeping every-field-non-default coverage intact) or the suites fail.
- **T7. WavPack wvunpack pre-flight**: fail-closed measurement is right, but a wvunpack-less machine now encodes-then-fails every track. Add a cheap once-per-album wvunpack availability probe before the convert stage when `target_format == WavPack` (clear error before any encode work).
- **T8. Archive/single-file materializers lack the 320/640 float convention** (`materializer_single.rs:139-142`, `materializer_archive.rs:1706-1709`): a real float32 WAV in a 7z is misread as Int32 — plans integer output under `Source` and can hard-fail D5 class-strict. Apply the same codec-gated classification as G2's fixed CUE path (shared helper; do not duplicate a third copy).
- **T9. Preset refusal semantics are pre-apply-state-dependent** (M4): `select_or_already` treats "equals current selection though disabled" as applied, so the SAME preset can load cleanly or hard-fail (`:queue` flow rolls back, `command.rs:6256-6287`) depending on what the pills happened to show before. Make refusal a function of the preset alone: only refuse a field when its value is (a) parseable, (b) not selectable under the preset's OWN format after constraints, and (c) semantically meaningful for that format (skip DSD-only pills for PCM formats and vice versa). Also cover the tail fields (`merge`/`force_encode`/`disc_subfolders`/`write_log`) consistently, and record a refusal if the final `apply_format_constraints()` re-run snaps a previously-accepted selection.

---

## Tier 3 — smaller, fix if touching the file anyway

- **S1.** Quit gate: `defer_quit_for_browse_archive_metadata` (`event_loop.rs:347-399`) only defers for archive-owned parked editors; programmatic quit-resume sites (`event_loop.rs:1976, 2215, 2347, 2376`) can exit over a dirty parked editor from the mb-back/tags-mb flows. Centralize: any dirty parked or open editor blocks quit at the single gate.
- **S2.** `MbDetailPrefetchComplete` (`event_loop.rs:4474-4485`) stamps whichever picker is open. The message (`message.rs:634-637`) carries only `release_id` — it CANNOT be gated without a new identity field. Add one (the prefetch-generation snapshot already captured at spawn in `spawn_mb_detail_prefetch`, `event_loop.rs:~5791`, or the picker's `operation_id`) and gate the in-memory `state.prefetch.insert` on it; the SQLite write stays unconditional.
- **S3.** `complete_tags_mb_apply_operation` mismatch branch (`event_loop.rs:6117-6133`) discards a completed verification with only `log::debug!` — add a status message so an accepted selection can't vanish silently. Align its picker-location check with the reconciler's `pending_mb_select` clause.
- **S4.** `:mb-back` while an operation is in flight (`command.rs:4150-4176`): either refuse with a status ("cancel the running lookup first") or make `open_mb_select_picker`'s `editor_park` branch (`event_loop.rs:5259-5282`) check the current overlay before overwriting (the non-park branch already does).
- **S5.** Matrix temp-dir leak on panic (`tests/depth_format_matrix.rs`, `unique_root()`): use a drop guard; and let one failing cell not abort the loop (collect failures, assert at end).
- **S6.** Legacy log writer renders 640 as "640bit" (`crates/tonepoet-features/src/log_writer.rs:895-901, 1060-1078`) — add the Float64 arm next to the existing 320/33 handling.
- **S7.** Two-image MB save regression: pre-tag fixture files with a real whole-file TITLE and assert it SURVIVES the save (the current non-empty check is blind to empty-value deletes of legitimate tags).
- **S8.** `open_metadata_editor_impl` sibling-poisoning (`keybindings.rs:~14939-14975`): if atomic folder admission is intended, name the offending sibling cue in the status; otherwise open the clicked valid pair.
- **S9.** Legacy write wrapper (`probe.rs:6593+`) re-derives scope by length for CLI callers — thread real scopes or document the API boundary as File-only.
- **S10.** `stop_all_conversions` fallback-of-fallback (inside `stop_all_conversions`, `mod.rs:1961+`; the `blocking_write()` fallback ~70 lines in): `blocking_write()` on the calling thread can self-deadlock when spawn fails; log-and-skip like the empty-snapshot branch. Also clear `active_conversion_items` on normal run completion and snapshot Queued items enqueued mid-run.
- **S12.** `process_id_is_live` returns `false` for every foreign PID on non-unix (`queue_expansion.rs:1470-1485`), so the startup edit-buffer scavenger would delete LIVE sessions' buffers on a non-unix build. Moot on Linux; either `cfg`-gate the scavenger to unix or make the non-unix arm return `true` (fail toward leaking, not deleting).
- **S13.** FILE-ref resolution is duplicated: `resolve_split_cue_file_reference` (rejects symlinks) vs `resolve_cue_file_reference_for_queue` (follows them; still used at `queue_expansion.rs:840, 938, 1050, 1139`). Divergences currently fail closed at admission, but this is the same drift shape P1-P4 killed for membership — unify on one resolver with one symlink policy.
- **S11.** Failed-mandatory-refresh dirty flag is not sticky (`mark_saved_surface_refresh_failed`, `app.rs:7529+`): `reduce_saved_slots` already cleared the diffs, so the next dirty recompute erases the "unresolved" state. Persist an explicit `refresh_failed` flag on the tab that survives recomputes and is cleared only by a successful re-read.

---

## Explicitly deferred (do NOT implement)

- **E10 archive-password persistence** — decision still pending with the user. The Design A/B writeup from the previous report stands; touch nothing.

## Test-authoring rules

- Real fixtures + `RealToolRunner` for anything that executes planned commands (the stub seam does not carry planned ffmpeg/sox invocations). Guard per-tool with skip messages; `TONEPOET_REQUIRE_TOOLS=1` converts skips to failures.
- New async TUI reducer tests that touch tokio channels/spawns need `#[tokio::test]` (five of last round's shipped as sync `#[test]` and failed on the reactor).
- `read_all_tags_merged` synthesizes empty placeholder rows for standard keys — assert on VALUES, not row existence.
- Every new fail-closed path gets both directions pinned: the refusal fires for the bad shape AND the adjacent good shape still succeeds.

## Acceptance sequence (applier will run)

1. `cargo test --workspace` — green, zero failures.
2. `touch src/lib.rs && cargo build --workspace` — zero warnings.
3. `TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix -- --nocapture` — green.
4. G1: `tonepoet convert <sacd.iso> --format flac` (no depth flag) plans and converts at the documented default; the log carries the default label. With `--bit-depth 24`, identical output, no label.
5. G2: MP3+CUE fixture → FLAC succeeds (integer default); → WAV emits integer, not float.
6. G3: multi-FILE single-CUE edit/save/reopen round-trip (or explicit refusal).
7. G4: mb-back accept during live lookup leaves the live operation intact; stale gnudb completion is a no-op over a live MB picker.

## Delivery contract

Complete files only — never snippets or diffs. Include `IMPLEMENTATION_REPORT.md`: what was implemented, what was deliberately deferred and why, every file touched, and any policy choice made where this brief offered alternatives (G1 items 2/4, G4 item 2, S8).
