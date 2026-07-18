# Implementation Report: DSF Performance, Secrets, and Queue/Metadata UX Follow-up — P2 Corrective V6

## Delivery status and scope

Before inspection or modification, a fresh extraction of `tonepoet_full_tree_fa1c1f9.tar.gz` was verified against `docs/handoff_manifest.txt`. All **560 of 560** listed files matched their SHA-256 digests. The five offline review commits and full diffs in `docs/handoff_git_history.txt` were also reviewed.

This cumulative delivery retains the previously implemented **P0 and P1 work**, fixes the newly reported Source-policy regression, and implements **every P2 deliverable listed in `docs/followup_brief_dsf_perf_secrets_ux.md`**. No P2 item is deliberately cut in this version.

The explicitly out-of-scope items remain unchanged:

- companion-CUE product behavior;
- DFF tag writing;
- terminal-specific handling for genuinely ambiguous East Asian Width characters.

No Rust compiler, Cargo, rustfmt, Clippy, or network access was available. Compilation and tests were therefore not run, and this report does not claim that the acceptance commands are green.

## Corrective V6 changes

### GNUDB outside-click dismissal now follows the Esc lifecycle

The generic overlay mouse reducer no longer clears `GnudbReview` or `GnudbSelect` directly. A left click outside either popup synthesizes the same Esc action used by keyboard and footer cancellation while the overlay remains authoritative.

For `GnudbReview`, this runs the exact guarded restore path. A review reopened by `:gnudb-back` therefore restores only the metadata editor whose session ID, surface save generation, and editor generation match the review guard, and empties `pending_metadata_editor` on success.

For `GnudbSelect`, outside-click cancellation retires the current GNUDB operation. When that operation owns a parked metadata editor, the existing operation guard restores that exact editor and clears the pending slot. A stale operation retains the same fail-closed behavior as keyboard Esc rather than bypassing lifecycle checks.

Value-asserting mouse tests cover guarded review restoration, ordinary selection-operation retirement, and editor-owned selection restoration.

### Rejected output-pill clicks no longer dirty an active preset

`FormatState::select_row_index` now returns whether an enabled, in-range option was actually accepted. `handle_format_button` and `handle_convert_format_button` propagate that result to the screen-level mouse dispatcher. Disabled and invalid pill hits therefore return `false`, do not run user-policy side effects, do not run format-transition cascades, and do not call `PresetState::mark_modified`.

An enabled re-click still returns `true`: deliberately clicking the already-selected automatic value converts it to explicit policy and marks an active preset modified.

Dispatcher-level mouse tests prove that disabled rate and depth clicks and an invalid rate index preserve the selected value, override flag, source-derived provenance, and clean preset state. A companion test proves that an enabled re-click clears automatic provenance, sets the override flag, and dirties the active preset.

### Retained Corrective V5 behavior

The prior `:gnudb-back` guard refresh, legacy confirmation-path correction, and guarded GNUDB accept ownership remain unchanged. Disabled and invalid format-pill clicks still leave automatic source provenance untouched; V6 additionally propagates rejection to preset dirtiness and other caller-visible side effects.

## Corrected Source-policy behavior

`FormatState::clear_source_derived_defaults` now distinguishes automatic source-derived values from deliberate user policy.

- `rate=source` remains selected across failed, pending, mixed, removed, and otherwise unavailable source facts.
- `depth=source` remains selected for PCM targets.
- A DSD target with temporarily unknown source identity keeps `rate=source` selected but disabled. It no longer silently substitutes DSD64.
- Explicit scalar rate/depth choices survive later probes and source resets.
- Explicit dither, resampler, and Manual DSD-gain choices survive source-fact loss.
- Automatic dither, resampler, and non-Manual DSD gain reset when their source basis disappears.
- Scalar defaults installed by a source probe carry provenance. If constraints clamp an automatic 768 kHz/32-bit default to 384 kHz/24-bit, the clamped values remain automatic and clear when source facts disappear.
- Keyboard, accepted mouse selections, `:set`, and preset selections clear source-derived provenance and become explicit policy. Disabled or invalid pill hits return rejection, preserve provenance, and do not dirty an active preset.
- Async probe snapshots include the provenance and override flags, so a same-value explicit selection made while a probe is running is still recognized as a user change.

Value-asserting tests cover failed probes, unresolved pending probes, mixed/unavailable facts, source removal, DSD targets with unknown source identity, constrained automatic defaults, valid later probes, and preset round trips.

## P2 implementation

### P2-1: rollback restoration through symlinks with mode preservation

The database full-file recovery path resolves a symlink destination and restores through the resolved target instead of replacing the link. Recovery streams through a fixed 1 MiB buffer, preserves the target mode, fsyncs the restore temp, atomically publishes it, and fsyncs the parent directory. Tests assert that the symlink survives, the target bytes are restored, and the original mode remains intact.

### P2-2: DST normalization in the TUI probe

The TUI probe now delegates DSD/DST classification to the pipeline classifier. Codec `dst` is normalized as one-bit DSD, including conversion from byte-rate probe values to the true DSD sample rate. Ordinary PCM facts remain unchanged.

### P2-3: DFF mutation preflight before backup or payload work

All metadata and artwork mutation boundaries reject DFF before allocating a database/full-file backup. Batch artwork operations preflight all targets before reading image bytes or existing artwork metadata, so a later unsupported DFF cannot allow an earlier file to be mutated first. Defensive per-file checks remain at lower layers.

### P2-4: untouched multi-value metadata preservation

The joined scalar shown by the editor is documented as a presentation value, not a lossless serialization. Unedited keys emit no change and therefore retain their original distinct values. Tests assert that an unrelated FLAC title edit preserves two separate COMMENT items and an unrelated DSF title edit preserves two separate COMM frames.

### P2-5: GNUDB failure and lifecycle semantics

- Multi-disc workers retain per-disc query/read failures instead of converting transport failure into “no matches.”
- Total failure, partial failure, and true no-match states have distinct user-visible status.
- Review state carries the exact parked metadata-editor session guard, refreshed on each `:gnudb-back` invocation.
- Keyboard Esc, footer Esc, and outside-popup mouse cancellation share the same review/selection lifecycle reducers.
- Cancel and accept restore/consume only that matching session and never touch an unrelated editor.
- `GnudbSelect` outside-click cancellation retires the operation and restores an editor only when the operation owns its exact guard.
- GNUDB operation identity remains current even if a defensive MusicBrainz collision appears; overlay authority is denied, the GNUDB operation is retired, and the competing workflow’s parked editor remains untouched.
- Worker panic/cancellation containment and stale-operation checks remain operation-scoped.

### P2-6: canonical metadata keys shadow aliases

Presence of canonical `TRACKTOTAL` or `DISCTOTAL` now shadows `TOTALTRACKS` or `TOTALDISCS`, even when the canonical value is malformed. The alias is consulted only when the canonical key is absent. Tests assert both malformed-canonical and canonical-absent cases.

### P2-7: dead admission/keychain paths removed

The duplicate `commit_batch`, `is_active_commit_item`, and `try_keychain` functions remain removed. Live admission, queue, and secret-store paths are the only retained implementations.

### P2-8: durable queue sanitization

The JSON queue rewrite that removes legacy cleartext uses the durable atomic private-file publisher. It writes a same-directory temp, fsyncs it, renames it, and fsyncs the parent before secret references may be retired.

### P2-9: honest legacy WAV/AIFF carrier

Legacy WAV/AIFF `QualitySettings` now carry `Option<u16>` depth and `Option<u32>` rate. `None` represents Source policy; no hardcoded 24-bit replacement and no raw zero-rate sentinel escape into the legacy carrier. The unified request maps `None` to typed source-relative planner targets. Existing numeric serialized values remain representable as `Some(value)`.

### P2-10: parallel-safe sacd-rs allocation test

The test allocation counter is thread-local rather than global. Allocations from unrelated parallel tests no longer contaminate `dsdiff_dst_adapter_borrowed_path_has_no_per_frame_allocations`.

### P2-11: macOS path unification

The keychain compatibility file derives from `TonepoetConfig::config_path().with_file_name(...)`, matching the platform-native config directory rather than independently reconstructing `~/.config`.

## Retained P0 and P1 behavior

The cumulative files in this archive retain:

- bounded in-place DSF metadata-tail replacement, cancellable streamed journal creation, byte progress, full-rewrite fallback, target-bound recovery, and explicit legacy-marker resolution;
- structured commit-state uncertainty that never pairs a partial rollback with a retained COMMITTED journal;
- operation-scoped inline DSF cancellation, guarded progress, stale-result rejection, and visible durability warnings;
- Linux authenticated encrypted-file secrets, native macOS/Windows keychains, bounded lock initialization, lazy startup resolution, durable migration, and post-publication retirement;
- queue-history loading without eager secret resolution, dispatch-time authority, and transactional re-enqueue of terminal rows;
- plain-file/alien-CUE `:tags-mb` routing and fail-closed malformed local-CUE handling;
- source-aware ReplayGain provenance and recomputation after signal-altering conversion;
- tolerant DSF reads with strict mutation blocking and conversion-visible warnings;
- recognized DSD-rate labels;
- metadata-editor Tab/BackTab, Alt+A/Alt+O, detail navigation, shared keyboard/mouse commits, and operation guards;
- one display-column policy for wide/fullwidth glyphs, combining marks, rendering, clipping, cursor placement, and hit-testing.

## Tests added or strengthened

New or strengthened value assertions in this round cover:

- rejected rate/depth/invalid mouse hits preserving automatic provenance and active-preset cleanliness, with enabled re-clicks becoming explicit policy;
- Source sentinels surviving failed, pending, mixed, removal, and DSD-unknown paths;
- explicit scalar, dither, resampler, and Manual-gain policy surviving source resets;
- automatic source-derived and constraint-clamped scalar defaults clearing when source facts disappear;
- preset-applied Source/dither/resampler values becoming explicit policy;
- DST normalization to true one-bit DSD facts;
- DFF tag and artwork rejection before transaction, backup, image read, metadata read, or mutation;
- untouched FLAC and DSF multi-value fields remaining distinct;
- GNUDB total failure, partial degradation, refreshed back-navigation guards, exact keyboard/footer/outside-click restoration, selection-operation retirement, guarded accept ownership, and defensive MB collision;
- malformed canonical total values shadowing parseable aliases;
- source-relative WAV legacy and typed planner carriers;
- thread-local allocation counting.

No behavioral test was weakened or removed. One stale mixed-CUE assertion was corrected: automatic 96 kHz/24-bit values now reset to 44.1 kHz/16-bit when the source becomes unresolved, while deliberate Source or explicit scalar policy remains unchanged.

## I/O and memory cost statements

Symbols:

- `A`: existing DSF metadata-tail allocation, capped at 64 MiB for in-place replacement;
- `B`: bounded recovery-identity reads, at most 128 KiB;
- `P`: DSF audio-prefix bytes copied during a full rewrite;
- `T`: encoded replacement tag bytes;
- `S`: legacy full-file marker size;
- `M`: DSF ID3 metadata size read by tolerant metadata loading;
- `E` / `E'`: old/new encrypted-store size;
- `Q`: serialized config or JSON queue size;
- `N`: SQLite queue rows.

### In-place DSF success

- Reads: `B + A`.
- Writes: journal `A + 57`, target tail `A`, journal state byte `1`.
- Total payload moved: `B + 3A + 58`.
- Fsync calls: journal temp, journal parent after rename, target, committed journal, journal parent after removal: **5**.
- Peak memory: replacement tail `A`, one 1 MiB copy buffer, and at most 128 KiB identity samples. There is no second complete old-tail/journal buffer.
- First target-lock creation adds a small marker write and **2 fsyncs**.

### Commit-state durability uncertainty

- Before warning: at most `B + 3A + 58` bytes moved and **4 fsyncs attempted**.
- No rollback write and no journal removal occurs after a state-byte write whose durability is uncertain.
- PREPARED recovery later moves `B + 2A + 57` and performs **2 fsyncs**.
- COMMITTED recovery reads `B + 57`, writes no target payload, and performs **1 parent fsync**.

### Other DSF cancellation/error paths

- Cancellation during journal creation moves at most `B + 2A + 57`; target bytes remain unchanged.
- Failure before state-byte mutation plus complete rollback moves at most `B + 5A + 57` and performs **5 fsyncs**.
- Rollback is deliberately non-cancellable once restoration begins. A failed rollback retains the PREPARED journal.

### DSF full rewrite

- Reads `P`; writes `P + T` plus two 8-byte header-field overwrites.
- Core payload movement: `2P + T + 16`.
- Fsync calls: temp and parent after atomic replacement: **2**.
- Peak copy memory: one 1 MiB buffer plus encoded tag storage. No independent full-file backup is made.

### Tail-journal recovery

- PREPARED: reads `B + A + 57`, writes `A`; total `B + 2A + 57`; **2 fsyncs**.
- COMMITTED: reads `B + 57`, writes no target payload; **1 parent fsync**.
- Target-attributable orphan cleanup reads no payload and fsyncs the parent once after removals.

### Legacy marker inspection and resolution

- Different lengths: stat work only; no payload reads/writes/fsyncs.
- Equal-length comparison: reads `2S` through two 1 MiB buffers; peak comparison memory **2 MiB**.
- Identical-marker retirement or `keep-current`: no payload writes; unlink plus **1 parent fsync**.
- `restore-backup`: reads `S`, writes `S`, fixed 1 MiB memory; total `2S` moved and **3 fsyncs**.

### Database full-file rollback restoration

- Reads `S` from the marker and writes `S` to a same-directory temp: `2S` bytes moved.
- Peak copy memory: **1 MiB**.
- Fsync calls: restore temp, target parent after rename, marker parent after retirement: **3**.
- Resolving a symlink and reading/preserving mode add metadata operations but no full-payload pass.

### Tolerant DSF reads

- Reads the 28-byte DSF header, required fixed-size chunk headers, and at most `M` ID3 bytes; audio payload is not copied.
- Writes/fsyncs: **0**.
- Container inspection is constant-memory; existing ID3 decoding is `O(M)`.

### ReplayGain recomputation

When inherited tags are invalid, the existing loudgain stage adds one full decode/read pass over selected output artifacts and writes the resulting tags. Exact physical bytes and fsyncs depend on external tools and output formats and cannot be derived honestly from this tree. Proven signal-equivalent output adds no extra I/O.

### Linux encrypted store

- Existing get: reads the 32-byte key plus `E`; **0 fsyncs**.
- Existing set/delete: reads key plus `E`, writes `E'`; **2 fsyncs**.
- First key/store/lock publication performs **6 fsyncs** across file/parent pairs.
- Batch retirement decrypts once and performs at most one store rewrite.

### Config and queue persistence

- Atomic config/JSON publication writes `Q` once and performs temp plus parent **2 fsyncs**.
- A secret-publication journal adds one small JSON write and **2 fsyncs**; retirement adds one parent fsync.
- SQLite persistence remains one transaction with `N` logical row inserts. Physical bytes/fsyncs depend on SQLite VFS, page, journal/WAL, and cache configuration.

### P2-specific paths

- DFF rejection at public metadata/artwork boundaries moves **0 target bytes**, performs **0 target fsyncs**, and for DFF-containing artwork batches occurs before image or artwork-metadata payload reads.
- Untouched multi-value preservation adds no extra read/write pass; it omits mutation for the unedited key inside the existing carrier write.
- Source provenance, rejection propagation/preset dirtiness, DST classification, canonical-shadow policy, DSD labels, editor/display behavior, legacy-carrier typing, GNUDB guard refresh/ownership/outside-click lifecycle, and allocation-counter isolation are in-memory changes with **0 file payload I/O and 0 fsyncs**.
- GNUDB performs the same query/read attempts as before; the change retains structured failures rather than discarding them.

## Assumptions and design decisions

- Source-derived provenance belongs to a selected scalar, not merely its original source value. Constraint-clamped values remain automatic until an explicit user or preset selection clears that provenance.
- An explicit enabled selection of the already-selected scalar still becomes policy and may dirty an active preset; disabled and invalid hits are rejected before policy/provenance mutation and before preset dirtiness. Async probe snapshots therefore include override flags, not only selected values.
- Outside-click is semantically equivalent to Esc for GNUDB overlays. In review edit mode this cancels only the active inline edit, exactly as keyboard Esc does; in navigation mode it closes the review and performs guarded restoration.
- Manual DSD gain is treated as explicit policy because no automatic path selects Manual.
- DFF preflight is extension-based at the TUI mutation boundary, matching the supported `.dff` format surface. DFF writing remains out of scope.
- Joined multi-value text is presentation-only. Editing that row is intentionally lossy, but unrelated edits preserve the original separate stored values.
- A defensive simultaneous GNUDB/MusicBrainz state denies GNUDB overlay authority and preserves the competing workflow rather than attempting ownership transfer.
- `unicode-width` remains the shared terminal cell-width policy; ambiguous-width terminal configuration is not inferred.

## Files touched

- `Cargo.lock` — cumulative dependency lock update for the direct encrypted-store dependency.
- `Cargo.toml` — direct `ring 0.17` dependency for the isolated Linux encrypted-file backend.
- `crates/sacd-rs/src/lib.rs` — thread-local allocation counter for parallel-safe zero-allocation testing.
- `crates/tui-file-picker/src/display_width.rs` — shared display-cell measurement, fitting, truncation, and invariants.
- `crates/tui-file-picker/src/lib.rs` — exports the display-width module.
- `crates/tui-file-picker/src/progress.rs` — display-cell-safe progress rendering.
- `crates/tui-file-picker/src/render.rs` — shared display-width rendering and cursor placement.
- `src/config.rs` — bounded store-file locking, initialization-under-lock, and durable secret reconciliation.
- `src/convert/formats.rs` — honest optional WAV/AIFF legacy rate/depth carrier.
- `src/convert/metadata.rs` — canonical total-key shadowing and tests.
- `src/convert/mod.rs` — transactional re-enqueue, durable queue sanitization, artifact rollback, and secret retirement.
- `src/convert/pipeline/materializer_archive.rs` — tolerant DSF metadata reads and visible degradation for archive materialization.
- `src/convert/pipeline/materializer_single.rs` — tolerant DSF reads and warning assertions for single materialization.
- `src/convert/pipeline/stages.rs` — ReplayGain provenance policy and production source plumbing.
- `src/convert/pipeline/unified_request.rs` — maps optional legacy rate/depth to typed Source targets.
- `src/convert/processor.rs` — dispatch-time secret resolution.
- `src/convert/queue.rs` — lazy secret authority, persisted-secret migration, replacement ownership, and GC.
- `src/convert/wizard.rs` — honest display of source-relative legacy WAV/AIFF settings.
- `src/convert/wizard_integration.rs` — preserves absent/zero wizard rate/depth as Source rather than guessed scalars.
- `src/db.rs` — fixed-memory, symlink-/mode-correct rollback restoration and SQLite sanitization.
- `src/dsf_tags.rs` — DSF journal/rewrite/recovery, tolerant reads, explicit marker recovery, multi-value preservation test, and cost-bounded streaming.
- `src/main.rs` — `dsf-recover` CLI and lazy CLI secret behavior.
- `src/secret_store.rs` — typed backend seam, Linux encrypted-file store, native backend routing, migration, and retirement.
- `src/tui/app.rs` — Source-policy provenance/override state, acceptance-returning row selection, value-asserting disabled/invalid click tests, inline-write operation state, and GNUDB review session guard.
- `src/tui/bookmarks_overlay.rs` — display-cell-safe bookmark rows.
- `src/tui/command.rs` — `:set rate/depth` through normal selection cascades, Source formatting, and fresh `:gnudb-back` guard binding to the editor being parked.
- `src/tui/context_menu.rs` — per-disc GNUDB failure aggregation.
- `src/tui/conversion_actions_ui.rs` — shared display-width fitting.
- `src/tui/convert_actions.rs` — honest Source legacy carrier and dead admission-fork removal.
- `src/tui/display_width.rs` — main-TUI re-export of the shared width policy.
- `src/tui/draw.rs` — cumulative state/render integration and removal of eager secret access.
- `src/tui/draw_browse.rs` — display-cell-safe browse rows, symmetric overflow handling, and width invariants.
- `src/tui/draw_metadata.rs` — shared display-width metadata rendering.
- `src/tui/draw_output_options.rs` — source-rate estimation refusal and shared width logic.
- `src/tui/draw_overlays.rs` — operation progress, inline/editor rendering, multiline display/byte mapping, and tests.
- `src/tui/draw_source.rs` — shared display-width source labels.
- `src/tui/event_loop.rs` — guarded probe/write completions, GNUDB operation/error/session semantics, and exact guarded take/restore helpers for parked editors.
- `src/tui/format_interactions.rs` — propagates enabled/in-range pill acceptance through convert-screen mouse handling so rejected hits cannot trigger cascades or preset dirtiness.
- `src/tui/gnudb.rs` — initializes review session ownership explicitly.
- `src/tui/inline_edit.rs` — display-cell-safe clipping and cursor placement.
- `src/tui/keybindings.rs` — operation cancellation/progress, GNUDB outside-click routing through Esc lifecycle, shared editor commits/navigation, exact-session cancel/accept ownership, rejection-aware preset dirtiness, and mouse regressions.
- `src/tui/keychain.rs` — platform-config path unification and dead compatibility API removal.
- `src/tui/message.rs` — operation-scoped write messages and structured GNUDB multi-disc results.
- `src/tui/metadata_view_models.rs` — DSD-rate names in Details rows.
- `src/tui/mod.rs` — registers the shared width module.
- `src/tui/presets.rs` — preset rate/depth/dither/resampler values become explicit policy; round-trip assertions.
- `src/tui/probe.rs` — controlled metadata writers, DST normalization, DFF preflight, multi-value preservation tests, typed DSF read issues, and recovery integration.
- `src/tui/recent_overlay.rs` — display-cell-safe recent-path rows.
- `src/tui/text_input.rs` — display-cell-safe cursor movement, clipping, multiline navigation, and hit-testing support.
- `IMPLEMENTATION_REPORT_followup_dsf_perf_secrets_ux_p2_corrective_v6.md` — this report.

## Deliberately cut

No in-scope P0, P1, or P2 item is deliberately cut in this cumulative delivery.

The brief’s explicit exclusions remain excluded: companion-CUE product behavior, DFF tag writing, and ambiguous-EAW terminal handling.

## Verification performed and limitations

Performed offline:

- handoff manifest verification: **560/560 OK**;
- review of the five supplied commit diffs;
- cumulative complete-file diff inventory against the verified original tree;
- whitespace/error check equivalent to `git diff --check`;
- delimiter/string/comment-aware structural scan over every changed Rust file;
- call-site and constructor sweeps for changed `QualitySettings`, GNUDB messages/review state, and Source-provenance fields;
- exact dead-function declaration scan;
- targeted audits of DFF mutation ordering, disabled/invalid-pill acceptance and preset dirtiness, GNUDB keyboard/footer/outside-click lifecycle and accept ownership, queue durability, database restoration, keychain path derivation, and display-column use;
- archive extraction, exact path inventory, and byte-for-byte comparison against all shipped files.

Not performed: compilation, rustfmt, Clippy, or Rust tests. The applying side must run:

```text
cargo test --workspace
cargo build --workspace --all-targets
TONEPOET_REQUIRE_TOOLS=1 cargo test --test depth_format_matrix
```

The direct `ring 0.17` calls remain isolated behind the encrypted-file module and marked **NEEDS-VERIFICATION** for external API signature verification at apply time.
