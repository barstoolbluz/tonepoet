# CLAUDE.md — tonepoet

## Project Overview

tonepoet is a standalone CLI + TUI audio conversion toolkit extracted from hexload-tui. It converts audio files between formats (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus, ALAC, and many more), extracts 7z archives, applies ReplayGain, preserves/rewrites metadata, renames files from tags, and generates CUE sheets and conversion logs.

## Build & Run

**Requires nix.** The project uses a nix flake to provide the Rust toolchain (latest stable via rust-overlay), all runtime audio tools, and ffmpeg development libraries for in-process probing via ffmpeg-next Rust bindings.

```bash
# Enter dev shell (Rust + all audio tools + ffmpeg headers + libclang for bindgen)
nix develop --extra-experimental-features 'nix-command flakes'

# Inside the dev shell:
cargo build                  # Debug build
cargo build --release        # Release build
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak   # THE GATE
                             # RUN IT IN THE BACKGROUND. It outruns the 600s Bash
                             # foreground timeout; a foreground run dies mid-suite
                             # and reports a misleadingly CLEAN partial result.
                             # plain `cargo test` tests only the root package;
                             # without --no-fail-fast, --workspace stops at the
                             # first failing binary; tonepoet-true-peak costs
                             # ~41 min and is gated separately. See ## Testing —
                             # the suite is 7348 tests; see ## Testing for the
                             # rare coordination-contention flake.
cargo test -p tonepoet-backend   # Backend tests only
cargo test -p tonepoet-features  # Features tests only
cargo test -p tonepoet-true-peak # True-peak + loudness core (~30 s on this AVX2 box;
                             #   ~41 min on the old pre-AVX2 Xeon — see ## The true-peak crate)
cargo check                  # Fast type check

# Run the binary
cargo run -- tui                             # Launch the TUI (main interface)
cargo run -- convert ./test.flac --format opus
cargo run -- check-tools
cargo run -- wizard                          # Legacy wizard (old TUI)
cargo run -- config

# Full nix package build (sandboxed)
nix build --extra-experimental-features 'nix-command flakes'
```

**Do not use the system Rust toolchain.** The root `Cargo.toml` declares `rust-version = "1.82"` but `crates/tonepoet-true-peak` declares `1.93`, so the effective workspace MSRV is **1.93** — above the system toolchain. Always build inside `nix develop`, which supplies the pinned toolchain.

## Workspace Structure

**This tree is ABRIDGED — roughly a third of the files.** It shows 9 of 44 in
`src/convert/pipeline/`, 22 of 73 in `src/tui/`, 11 of 22 in `src/convert/`, and omits
`tests/`, `assets/`, `docs/`, `examples/`, `fixtures/`, `scripts/`. Whole subsystems are
absent — `browse.rs`, `theme_builder.rs`, `musicbrainz.rs`, `scheduler.rs`,
`track_executor.rs`, `label_resolver.rs`, `memory_budget.rs`, and the `bluray_*` / `dvda_*`
families. **`ls` the directory; absence here does not mean absence on disk.**

```
tonepoet/
├── Cargo.toml              # Workspace root + main tonepoet crate
├── flake.nix               # Nix dev shell + build package (includes libclang, ffmpeg)
├── src/
│   ├── main.rs             # Clap CLI (15 subcommands; see `enum Commands`)
│   ├── lib.rs              # 10 pub mods: concurrency config convert ctdb_rs db
│                           #   disc dsf_tags metadata_persistence secret_store tui
│   ├── config.rs           # TonepoetConfig (TOML at ~/.config/tonepoet/config.toml)
│   ├── convert/
│   │   ├── mod.rs           # Module root — re-exports, ConversionManager, ConversionConfig
│   │   ├── processor.rs     # ConversionProcessor — main orchestration
│   │   ├── queue.rs         # ConversionQueue, ConversionItem, ConversionStatus
│   │   ├── formats.rs       # AudioFormat, FileFormat, FormatDetector, ConversionOptions
│   │   ├── pipeline/        # Staged conversion pipeline (PRs 1-9)
│   │   │   ├── mod.rs           # Module root, #![deny(unsafe_code)], re-exports, tests
│   │   │   ├── types.rs         # PipelineRequest, PreparedSource, ArtifactSet, AlbumOutcome, etc.
│   │   │   ├── errors.rs        # All pipeline error types (14 enums)
│   │   │   ├── tool.rs          # ToolBinary, ToolCommand, ToolRunner trait, RealToolRunner, StubToolRunner
│   │   │   ├── reporter.rs      # PipelineEvent, PipelineReporter, RecordingReporter
│   │   │   ├── stages.rs        # Stage functions, orchestrator (run_pipeline_item), publish
│   │   │   ├── materializer_archive.rs # ArchiveMaterializer (7z/zip/rar/…)
│   │   │   ├── materializer_cue.rs  # CueImageMaterializer
│   │   │   └── materializer_sacd.rs # SacdIsoMaterializer
│   │   ├── wizard_integration.rs  # Wizard state → ConversionOptions bridge
│   │   ├── simple_wizard.rs # ReplayGainMode, DitherType, NyquistTransition enums
│   │   ├── wizard.rs        # ConversionWizard (step-based, non-TUI)
│   │   ├── renaming.rs      # Tag-based file/folder renaming (dormant, pending preset system)
│   │   ├── labels.rs        # Label/pressing info detection
│   │   └── metadata.rs      # FLAC metadata extraction
│   └── tui/
│       ├── mod.rs            # Module declarations
│       ├── app.rs            # AppState, ConvertState, FormatState, OutputOptionsState, enums
│       ├── theme.rs          # Tokyo Night color constants
│       ├── pill.rs           # PillState<T> generic pill selector widget
│       ├── probe.rs          # Audio probing via ffmpeg-next + metadata via lofty
│       ├── command.rs        # Vi-style command mode (:q, :e, :set, :preset, etc.)
│       ├── convert_screen.rs # Main convert view layout + mouse button registration
│       ├── draw.rs           # Top-level draw dispatch by screen
│       ├── draw_header.rs    # ASCII art TONEPOET banner
│       ├── draw_preset_bar.rs # Active preset indicator
│       ├── draw_source.rs    # Source pane (amber border)
│       ├── draw_metadata.rs  # Metadata pane (purple border)
│       ├── draw_output.rs    # Format pane (green border) — format/rate/depth/dither/RG pills
│       ├── draw_output_options.rs # Output options pane (cyan border) — dest/templates/merge
│       ├── draw_footer.rs    # 5-tab bar + context keybinding bar
│       ├── draw_queue.rs     # Queue screen (tab 4)
│       ├── draw_status.rs    # Legacy header/status for queue screen
│       ├── draw_overlays.rs  # Modal dialogs + command input line
│       ├── keybindings.rs    # Key + mouse event dispatch
│       ├── button_map.rs     # TuiButton enum + ButtonRenderMap for mouse clicks
│       ├── event_loop.rs     # Async event loop (crossterm + mpsc messages)
│       └── message.rs        # AppMessage enum for async communication
├── tonepoet-pipeline/      # NOTE: at the repo root, NOT under crates/
│                           # Pipeline settings, enums, planning, fingerprint
└── crates/
    ├── tonepoet-backend/   # FFmpeg/SoX command builders, pipeline, metadata I/O
    ├── tonepoet-features/  # Log file writer, CUE sheet generator
    ├── tonepoet-wizard/    # Legacy ratatui TUI wizard (draw_wizard, events, presets)
    ├── tonepoet-true-peak/ # BS.1770 true-peak certification + loudness core
    ├── sacd-rs/            # SACD ISO parsing
    ├── dvda-demuxer/       # DVD-Audio demuxing
    ├── dvdvideo/           # DVD-Video parsing
    └── tui-file-picker/    # File/directory picker widget
```

## Crate Dependency Graph

```
tonepoet (main binary + lib)
├── tonepoet-backend     (workspace: FFmpeg/SoX command builders, metadata I/O)
├── tonepoet-features    (workspace: log file writer, CUE sheet generator; depends on tonepoet-backend)
├── tonepoet-pipeline    (workspace: pipeline settings, enums, planning)
├── tonepoet-true-peak   (workspace: BS.1770 true-peak certification + loudness core)
├── tonepoet-wizard      (workspace: legacy ratatui TUI wizard)
├── sacd-rs              (workspace: SACD ISO parsing)
├── dvda-demuxer         (workspace: DVD-Audio demuxing)
├── dvdvideo             (workspace: DVD-Video parsing)
├── tui-file-picker      (workspace: file/directory picker widget)
│
│ Notable external dependencies:
├── ffmpeg-next          (in-process audio probing via FFmpeg 7.1 bindings)
├── lofty                (audio metadata/tag reading)
└── libbluray-sys        (FFI bindings for libbluray)
```

## The true-peak crate (`crates/tonepoet-true-peak`)

**160 passed / 0 failed** (was 136 before the portable-Reference dispatch-tier work added
24). Runtime is hardware-dependent and the spread is enormous: **~30 s on this
32-core/64-thread AVX2 host** (measured 2026-10-03: 160/0 in 29 s, the heaviest
constant-carrier test 1.15 s in isolation), against the **~41 min** recorded on the
previous 14-year-old pre-AVX2 Xeon. The crate carries AVX dispatch, so a host without it
pays the old price. It is still excluded from the routine workspace gate — that exclusion
is worth revisiting on AVX2 hardware, where including it would cost seconds.

- **Its public API is frozen.** Additions are allowed, removals and signature changes are
  not.
- `PeakTier { Reference, Standard, Fast }`. Gain is always computed from
  `PeakCertificate::finite_interval.upper_linear`, a certified upper bound guaranteed not
  to under-read — so the tier chooses speed vs tightness, never safety.
- As of 2026-09-14 it carries a BS.1770 loudness core (integrated LUFS + LRA) and loudgain
  compatibility under three `pub mod`s: `loudness`, `replaygain`, `compat`. These are
  tested against frozen libebur128 1.2.6 references but **nothing in tonepoet consumes
  them yet**.
- Those are the crate's first `pub mod` declarations, so an API-freeze audit can no longer
  stop at `lib.rs` — it must descend into the modules.
- Two constant-carrier reference tests accounted for ~40 of the ~41 minutes on the old
  Xeon. On AVX2 they are about a second each. If this crate ever takes tens of minutes
  again, suspect the host's SIMD support rather than a hang.

## Key Types & Entry Points

**CLI subcommands** (src/main.rs, `enum Commands` — 15 variants; the user-facing ones):
- `tui` — launches the new TUI (default convert screen)
- `convert <PATHS>... [--format --output --workers --replaygain --preset --bitrate --track --track-range --area --no-cue --partial --overwrite --naming --no-metadata --no-features ...]`
- `wizard` — launches legacy TUI wizard
- `check-tools` — probes for ffmpeg, sox, ssrc, 7z, loudgain, opustags, etc.
- `config --show | --reset | --path`
- `tags-mb` — MusicBrainz tagging
- `disc-info`, `dvda-info`, `dsf-recover` — disc/format inspection and repair
- `legacy-file-recovery-list`, `legacy-file-recovery-adopt` — recover interrupted file ops
- Four hidden helper-subprocess variants (`__action-script-supervisor`,
  `__execution-item-supervisor`, `__action-script-launcher`, `__file-task-worker`) —
  double-underscore prefixed and `hide = true`; not user-facing

**TUI architecture** (src/tui/):
- `AppState` holds all TUI state: `ConvertState`, `PresetState`, queue state, overlays
- `ConvertState` contains 4 pane states: `SourceState`, `MetadataState`, `FormatState`, `OutputOptionsState`
- `FormatState` uses `PillState<T>` for format, sample rate, bit depth, dither, ReplayGain pills
- `PillState<T>` is a generic pill selector with per-option enable/disable and format constraint cascading
- `AppScreen` enum: Browse (tab 1, default home), Library (2, placeholder), Convert (3, conversion settings/staging), Queue (4), Config (5), Wizard (overlay). Default screen is configurable via `[ui] default_screen` in config.toml; new users open on Browse.
- Two-pass rendering: draw first (immutable state), then register mouse buttons (mutable button_map)
- Vi command mode: `:` opens command input at bottom of screen, parsed by `command.rs`

**Audio probing** (src/tui/probe.rs):
- Uses `ffmpeg-next` Rust bindings (in-process, no subprocess spawning)
- `probe_audio(path)` → `SourceInfo` (format, codec, bit depth, sample rate, channels, duration, file size)
- `read_metadata(path)` → `SourceMetadata` (title, artist, album, genre, year) via `lofty`
- DSD rates displayed as DSD64-DSD1024 with MHz
- ffmpeg initialized once via `std::sync::Once`

**Core conversion flow:**
1. CLI args → `ConversionOptions` + optional `PipelineRequest` (src/main.rs `run_convert()`)
2. Files scanned → `ConversionQueue` populated with `ConversionItem`s (with `pipeline_request` if pipeline flags set)
3. `ConversionProcessor::process_queue_with_progress()` runs items through workers
4. Multi-track sources (7z/CUE/SACD) route through `run_pipeline_item()`: materialize → plan → convert → merge? → metadata → RG → features → publish → log → terminal
5. Single audio files use the legacy single-file path via the backend crate

**Config location:** `~/.config/tonepoet/config.toml`
**Preset location:** `~/.config/tonepoet/presets/` (TOML files)
**Queue + core persistence:** SQLite at `~/.local/share/tonepoet/tonepoet.db` (XDG_DATA_HOME, WAL mode, schema versioned via PRAGMA) — see `src/db.rs`. `~/.cache/tonepoet/conversion_queue.json` is the **legacy** path: import-only since schema v23, read once by `load_legacy_queue_for_import()` and then deleted by `remove_legacy_queue_file_after_import()` (`src/convert/mod.rs:2979-3040`). Nothing writes it. To inspect queue state, open the database.

## Audio Format Support

**Common output formats** (shown as pills in TUI): FLAC, Opus, AAC, MP3, ALAC, WAV, WavPack

**All output formats** (available in Advanced): above plus DSF, DFF, W64, RF64, LPCM, raw PCM, raw AAC, WebM/WEBA, MKV/MKA, AIFF

**Decode-only formats** (accepted as sources and CUE backing images, never output targets): APE, Musepack, Shorten, OGG, TTA (`AudioFormat::input_decodable` vs `output_encodable`, `formats.rs:133`). ISO and CUE are container/image sources handled by materializers.

**Not accepted as sources at all**: DTS, AC3, SHN — these are *output* formats (`advanced_output()` = `[Dts, Ac3, Lpcm]`) and are absent from the `is_single_audio_extension` allowlist (`stages.rs:619`), so they are rejected as inputs. Adding them as sources is open backlog work.

**Bit depths**: 16, 24, 32 integer, 32-bit float, 64-bit float (availability depends on format)

**Sample rates**: 44.1 kHz through 768 kHz (PCM); DSD64 through DSD1024 in Advanced

## Format Constraint Cascade

The `FormatState::apply_format_constraints()` method recalculates available options when the format changes:
- **Opus**: sample rate locked to 48 kHz, bit depth and dither disabled
- **AAC**: bit depth and dither disabled; sample rate limited to the encoder's direct-rate table, which tops out at **96 kHz** (`AAC_DIRECT_SAMPLE_RATES_HZ`, `tonepoet-pipeline/src/mapping.rs:609`) — 176.4/192 kHz clamp down to 96 kHz
- **MP3**: bit depth and dither disabled, sample rate capped at 48 kHz
- **FLAC/ALAC**: float bit depths (32f, 64f) disabled, and sample rate capped at 384 kHz. ALAC additionally disables Int32
- **WavPack**: lossless output supports explicit **Float32**; Float64 is unsupported, hybrid remains integer-only, and `Source` retains the round-5 float-to-Int32 policy
- **WAV/AIFF/LPCM**: full range, including float32 and float64
- **APE/Musepack/Shorten/TTA**: float disabled (lossless but not encodable; same shape as FLAC)

All of the above are match arms in `FormatState::apply_format_constraints()` (`src/tui/app.rs:5610-5635`) — read them there rather than trusting this summary.

## Coding Conventions

- **Edition 2021**, workspace resolver v2
- **Async:** Tokio with `broadcast` channels for progress, `Arc<RwLock<>>` for shared queue state
- **Error handling:** `thiserror` for typed errors in library code, `anyhow` in main.rs
- **Logging:** `log` crate macros (`info!`, `warn!`, `error!`), initialized with `env_logger`
- **Serialization:** `serde` + `toml` for config/presets, `serde_json` for queue persistence
- **TUI:** `ratatui` 0.26 + `crossterm` 0.27; Tokyo Night theme in `theme.rs`
- **Audio probing:** `ffmpeg-next` 7.1 (in-process FFmpeg bindings, requires libclang for bindgen)
- **Metadata reading:** `lofty` 0.21 with `TaggedFileExt` and `Accessor` traits
- **External tool invocation:** `tokio::process::Command` (async) for ffmpeg/sox/7z/loudgain etc. All subprocesses use `Stdio::null()` for stdin (tool.rs) to prevent interactive prompts from blocking the TUI.
- Module-level re-exports in `mod.rs` files — consumers import from `tonepoet::convert::` not submodules directly
- TUI draw functions take immutable state refs; mouse buttons registered in a second pass via `ButtonRenderMap`

## Testing

```bash
# THE GATE. Run it exactly like this, IN THE BACKGROUND: it runs well past the Bash
# foreground timeout of 600s, and a foreground run dies mid-suite reporting a
# misleadingly clean partial result. (~10 min of test execution plus build time.)
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak

# --no-fail-fast: without it, --workspace STOPS at the first failing test binary and
#   every later target silently never runs.
#
# TRAP, hit twice on 2026-10-02: `cargo test --lib <bare_fn_name> -- --exact`
#   matches NOTHING. --exact requires the full module path. Cargo then prints
#   "test result: ok. 0 passed; 0 failed; N filtered out", which reads as a pass
#   and is not. Always pass the full path
#   (convert::processor::tests::the_name) and check the run says "1 passed",
#   not "0 passed". A triage built on bare-name filters concluded two real
#   regressions were flakes.
# --exclude tonepoet-true-peak: that crate costs ~41 min on its own. Gate it separately
#   at crate-delivery time:  cargo test -p tonepoet-true-peak --no-fail-fast

cargo test -p tonepoet --lib       # root-crate unit tests only (~5,826; ~31s after build)
                                   #   the usual home of a real failure; the gate's
                                   #   biggest target by far
cargo test -p tonepoet-backend     # ffmpeg builders, integration, channels
cargo test -p tonepoet-features    # log writer, CUE generator
cargo test -p tonepoet-true-peak   # BS.1770 true-peak + loudness core (~41 min)
```

Tests are in `crates/*/tests/` directories, `src/` (inline `#[cfg(test)]` modules), and `tests/` (integration/contract/sentinel tests). The workspace suite is ~7,210 tests across 57 targets, plus 160 in `tonepoet-true-peak`.
NEVER truncate failure output.

**The suite is 7340 tests across 63 targets.** The count fell from 7363 when the
logging/evidence redesign removed tests that asserted log content inferred from
argv rather than from execution receipts.

**Conversion log evidence.** As of 2026-10-07 the conversion log is projected from
`src/convert/pipeline/execution_evidence.rs` using typed execution receipts. It does
not parse argv to infer what happened. Failure paths retain typed semantic evidence;
quantization ownership and dither ownership are distinct facts; compound terminals
link every physical invocation that realized them. Multi-value metadata renders as a
JSON array rather than silently keeping the first value. As of 2026-10-06, after R31-R35, the
long-standing coordination flake appears resolved: **12/12 consecutive unserialized
`-p tonepoet --lib` runs clean**, and a full gate at 7363/0. Measured per-run failure
rates fell across the series: 0.40 (R31), 0.30 (R32), 0.25 (R33), 0.17 (R34), 0.00
over twelve runs (R35).

Root causes, all test-only, none a production defect:
- tests asserted immediate observation of state driven by a descriptor closing, which
  an unrelated worker's fork could falsify by holding an inherited copy until exec;
- some tests observed a lock their own probe had created;
- tests wrote an executable at runtime and immediately exec'd it, racing to ETXTBSY;
- a process-visible coordination root under a TempDir could be removed while another
  worker still held the path, giving ENOENT on lease staging.

If a single failure reappears, rerun and check whether it passes in isolation before
treating it as a regression. Do not serialize the harness. As of 2026-10-05 three consecutive
runs each came back 7347 / 1 with a *different* contention flake, every one passing
3/3 in isolation — `tui::probe::id3_numbering_alias_conflicts…`,
`tui::keybindings::permanent_delete_is_blocked_by_recovery_reserved_claim`,
`concurrency::same_process_recovery_coholds_exact_descriptor…`. A clean 7348/0 was
not observed on that tree; earlier trees did reach 0** (was 7212
on 2026-09-29; R13/R14 added 16, the Reference freshness gate 2, and R15/R16 and
R12's SACD multi-value work the rest, through 2026-10-02).
Reference was requalified on 2026-09-29 against the Nix rooting, and the SSRC
true-peak terminal registry was commissioned on 2026-09-30, so the stale-evidence
and uncommissioned refusals that previously failed are gone.

A coordination-contention flake remains, and it wanders. Across roughly thirty gate runs
it has hit at least sixteen different tests, usually none to two per run and once four,
every one passing in isolation. It can move between two runs of the same tree.

**Back-to-back runs are much flakier than a cold gate.** Measured 2026-10-02: three
consecutive `cargo test -p tonepoet --lib` runs on a clean `main` produced 3, 3 and 2
failures across five distinct tests — yet full gates on the same commit came back 0. If
you are re-running the suite repeatedly, expect two or three contention failures per run
and judge a change by *which* tests fail, not how many. A per-test tally across equal
numbers of runs on both trees is the only reliable way to separate a real regression from
this: R15 was cleared that way (main 8 failures / 5 tests vs R15 10 / 6, with only two
tests separating 0/3 against 3/3 — those two were real and became R16).
Observed messages:

```
scanner probe must acquire the now-ownerless durable descriptor inode:
  Os { code: 11, kind: WouldBlock }
retire test recovery reservation: "persistent lease is live-owned: .../…lease"
assertion `left == right` failed   left: Live   right: RecoveryReserved
lazy reclamation must remove the retired exported descriptor
```

All are lock/ownership contention in the coordination-lease area, a different family from
the descriptor-size flake ("exceeds 1048576 bytes") that the schema-3 encoding fixed. Do
not expect it on a specific test; expect it somewhere in `concurrency`,
`tui::keybindings`, `tui::probe` or `tui::rename_plan`.

A failure that survives running the test alone is real. A failure that disappears in
isolation, with one of the messages above, is this known flake — rerun, and do not
"fix" it by serializing the suite or relaxing a lock.

`cargo test -p tonepoet-true-peak` is separate and IS at 160/0.

Superseded 2026-09-29: the three deferred hard-ceiling failures now pass; the
"persistent lease descriptor exceeds 1048576 bytes" flake was fixed by the schema-3
descriptor encoding; and the two stale-evidence qualification refusals were cleared by
requalification.


### Reference qualification goes stale when locked sources change

The Reference runtime closure fingerprint binds the exact bytes of the 24 files in
`REFERENCE_COMMON_SOURCE_PATHS` (`reference_source_lock.rs`). Touch one — `w64.rs`,
`track_executor.rs`, `stages.rs`, `semantic_plan.rs`, `Cargo.lock`, … — and the installed
qualification stops binding the running build, so **every DSD source fails at runtime**
with "Reference production promotion is inactive". That is the certification machinery
working, not a bug.

`tests/reference_qualification_freshness.rs` catches this in the ordinary gate and names
the drifted files. It needs no audio tools and runs in milliseconds. If it fails,
requalify — do not edit the sidecar by hand:

```bash
unset TONEPOET_REFERENCE_RUNTIME_CLOSURE_ROOT TONEPOET_REFERENCE_RUNTIME_CLOSURE_MANIFEST_PATH
export TONEPOET_REQUIRE_TOOLS=1
cargo test --release --test dsd_reference_qualification \
  complete_p0_reference_qualification_report -- --nocapture   # ~13 min
cp target/dsd_reference_common_v18_{report,certification,evidence,source_lock}.json \
   tonepoet-pipeline/qualification/
```

The qualification run writes the sidecar itself, so requalify-then-copy keeps it correct.
This was added after R4–R11 silently invalidated the 2026-09-29 qualification and the
breakage surfaced only when a field SACD conversion failed eight commits later.

## External Tool Dependencies

All provided by the nix flake dev shell. The binary wraps these in PATH at build time:

| Tool | Purpose |
|------|---------|
| ffmpeg (7-full, unfree) | Primary conversion backend + in-process probing via ffmpeg-next |
| sox (sox_ng) | Alternative backend, Gesemann dithering, DSD support |
| ssrc | Brick-wall resampling |
| flac, metaflac | FLAC encode/decode and metadata |
| lame | MP3 encoding |
| opusenc, opustags | Opus encoding and metadata |
| wavpack, wvtag | WavPack encoding and metadata |
| loudgain | ReplayGain analysis |
| AtomicParsley | AAC/M4A metadata |
| ffprobe | Audio stream analysis (also available in-process via ffmpeg-next) |
| 7z (p7zip) | Archive extraction |

## Nix Build Notes

The flake provides:
- `llvmPackages.libclang` + `clang` — required by `bindgen` for `ffmpeg-sys-next` FFI generation
- `LIBCLANG_PATH` — environment variable pointing to libclang.so
- `BINDGEN_EXTRA_CLANG_ARGS` — C header include paths for sandboxed `nix build`
- `ffmpeg_7-full` in `buildInputs` — provides libavformat/libavcodec/libavutil for ffmpeg-next linking

## Reference Data (assets/ = code-embedded; docs/ = prose briefs)

Compile-time embedded reference data lives under `assets/` (`include_str!`/`include_bytes!` targets — never delete without updating the embedding source). Prose/design briefs live under `docs/`.


- `assets/reference/hexload_labels_reference.rs` — 370+ label/pressing/mastering-engineer/pressing-plant mappings vendored from hexload-tui. `include_str!`'d by `label_resolver.rs` as the data source for the `DictionaryLabelResolver` implementation. Includes audiophile labels, country-specific labels (UK Harvest vs Japan Harvest), mastering engineers (RL, Sterling, KG, BG, Wally, etc.), and pressing plants (RTI, QRP, Pallas, Monarch, TML, etc.).
- `assets/reference/canonical_artists_reference.txt` — 2,437 canonical artist names for case normalization. One name per line. `include_str!`'d by `label_resolver.rs`; used by `ArtistCanonicalizer` for case-insensitive exact matching (match → canonical casing, no match → pass through unchanged).
- `assets/reference/cds-with-preemphasis-shf.csv` — authoritative pre-emphasis CD list. `include_str!`'d by `src/tui/preemphasis/catalog.rs`.
- `assets/dsd_reference/` — 6 DSD guidance/brief docs `include_bytes!`'d by `track_executor.rs` (`validate_embedded_qualification_report`): `tonepoet_dsd_to_pcm_guidance_evidence_based_v9.md`, `sox_ng_dsd_decimation_test_report_v5.md`, and 4 `brief_dsd_reference_p0_*` files.
- `docs/hexload_log_writer_reference.rs` — Vendored log writer from hexload-tui predecessor project. Reference for conversion log structure and field coverage. **NOT created by a reasoning model** — use as inspiration for WHAT to include, not HOW to implement. Tonepoet's pipeline has richer data structures.

## Important Notes

- **The giant files** (re-measured 2026-10-01 after R13/R14; they grow steadily, so re-measure rather than trusting these): `src/tui/keybindings.rs` 113K lines / 4.5 MB, `tonepoet-pipeline/src/ssrc_true_peak_terminal_commissioned.rs` 87K / 4.4 MB, `src/convert/pipeline/stages.rs` 80K / 3.1 MB, `src/tui/probe.rs` 32K / 1.3 MB, `src/tui/command.rs` 24K / 960K, `src/tui/app.rs` 23K / 908K, `src/tui/event_loop.rs` 23K / 936K, `src/tui/browse.rs` 22K / 836K, `src/convert/processor.rs` 13K / 500K, `src/tui/context_menu.rs` 8.5K / 340K, `src/convert/cue_parser.rs` 7.2K / 272K. stages.rs holds the pipeline stage functions, publish logic, template rendering, and conversion log assembly. **Search these; never read one whole.** The commissioned registry is `@generated` by `promote_ssrc_true_peak_terminal.py` — never hand-edit it; re-run qualification and promotion instead. For scale, the tracked Rust tree is ~946K lines across 393 files, of which that one generated file is ~9%.
- **CUE text encoding is guessed, and path resolution is the deciding signal.** `decode_cue_bytes_for_path` (`src/convert/cue_parser.rs`) is the only decoder production uses; the path-blind `decode_cue_bytes` is test-only. It tries UTF-8/BOM first, then scores legacy candidates — CP932/Shift-JIS, EUC-JP, GBK, Big5, Windows-1251, Windows-1252 — and sorts by score descending, then by priority ascending (Windows-1252 is priority 0, the Western default on a pure tie). Resolving a decoded `FILE` reference is worth up to +5,000 and dominates everything else; the +10 CJK/kana bonus applies *only* when that same candidate actually resolved a file (R14). Consequence worth knowing before touching the scorer: when no candidate resolves, the decision comes down to the tiebreak and the -500-per-control-character penalty — see issue #60.
- The wizard crate has its own `main.rs` for standalone use but tonepoet's `main.rs` embeds the wizard directly
- The new TUI (`src/tui/`) is the primary interface; the wizard crate is kept as-is for legacy/preset access
- Archive passwords are configurable via `--archive-password` flag or `config.toml` — no hardcoded defaults
- `ConversionQueue::items_mut()` is `pub(crate)` (`queue.rs:863`), not private — it is reachable from anywhere in the `tonepoet` crate and is used that way (`wizard_integration.rs`). Prefer `add_item()` / `add_item_direct()` unless you specifically need raw deque access
- `probe_audio()` uses unsafe FFI to access `bits_per_raw_sample` from codec parameters (standard ffmpeg-next pattern)
- The theme builder uses a `BuilderTab` + `BuilderOverlay` model (Edit/Preview/Derived tabs + floating gallery/menu overlays)
- Pipeline staging can optionally use tmpfs via `scratch_directory` config, with a memory budget system (`memory_budget.rs`) that gates admission and falls back to disk
- Template rendering supports conditional `{...}` blocks that are dropped when variables inside are empty
- `trash` crate was removed — file deletion is permanent with safety guards (rejects root paths, dot components, empty paths)
