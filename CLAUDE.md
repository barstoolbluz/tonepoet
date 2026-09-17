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
                             # a clean gate is 6858/4, NOT zero failures.
cargo test -p tonepoet-backend   # Backend tests only
cargo test -p tonepoet-features  # Features tests only
cargo test -p tonepoet-true-peak # True-peak + loudness core (~41 min)
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

Separately gated, ~41 min, **136 passed / 0 failed**. Excluded from the routine workspace
gate; run it explicitly whenever the crate itself changes.

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
- Two constant-carrier reference tests account for ~40 of the ~41 minutes. Slow is expected
  there, not a hang.

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
# --exclude tonepoet-true-peak: that crate costs ~41 min on its own. Gate it separately
#   at crate-delivery time:  cargo test -p tonepoet-true-peak --no-fail-fast

cargo test -p tonepoet-backend     # ffmpeg builders, integration, channels
cargo test -p tonepoet-features    # log writer, CUE generator
cargo test -p tonepoet-true-peak   # BS.1770 true-peak + loudness core (~41 min)
```

Tests are in `crates/*/tests/` directories, `src/` (inline `#[cfg(test)]` modules), and `tests/` (integration/contract/sentinel tests). The workspace suite is ~6,860 tests across 57 targets, plus 136 in `tonepoet-true-peak`.
NEVER truncate failure output.

**A clean gate on `main` is 6858 passed / 4 FAILED, not zero failures.** Three are the
deferred hard-ceiling tests awaiting a decision on which quantity governs the output
ceiling; the fourth is a recurrent contention flake
(`cue_matrix_validates_real_outputs_when_external_tools_are_available`, "persistent lease
descriptor exceeds 1048576 bytes") that passes in isolation. The correct check is that the
failure set equals exactly those four — a fifth failure, or a different one, is real.
`cargo test -p tonepoet-true-peak` is separate and IS at 136/0.

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

- **The giant files** (measured 2026-09-14; they grow steadily, so re-measure rather than trusting these): `src/tui/keybindings.rs` ~108K lines, `src/convert/pipeline/stages.rs` ~72K lines / 2.8 MB, `src/tui/app.rs` ~22K lines, `src/convert/processor.rs` ~9.7K lines. stages.rs holds the pipeline stage functions, publish logic, template rendering, and conversion log assembly. **Search these; never read one whole.**
- The wizard crate has its own `main.rs` for standalone use but tonepoet's `main.rs` embeds the wizard directly
- The new TUI (`src/tui/`) is the primary interface; the wizard crate is kept as-is for legacy/preset access
- Archive passwords are configurable via `--archive-password` flag or `config.toml` — no hardcoded defaults
- `ConversionQueue::items_mut()` is `pub(crate)` (`queue.rs:863`), not private — it is reachable from anywhere in the `tonepoet` crate and is used that way (`wizard_integration.rs`). Prefer `add_item()` / `add_item_direct()` unless you specifically need raw deque access
- `probe_audio()` uses unsafe FFI to access `bits_per_raw_sample` from codec parameters (standard ffmpeg-next pattern)
- The theme builder uses a `BuilderTab` + `BuilderOverlay` model (Edit/Preview/Derived tabs + floating gallery/menu overlays)
- Pipeline staging can optionally use tmpfs via `scratch_directory` config, with a memory budget system (`memory_budget.rs`) that gates admission and falls back to disk
- Template rendering supports conditional `{...}` blocks that are dropped when variables inside are empty
- `trash` crate was removed — file deletion is permanent with safety guards (rejects root paths, dot components, empty paths)
