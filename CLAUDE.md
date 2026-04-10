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
cargo test                   # Run all workspace tests
cargo test -p tonepoet-backend   # Backend tests only
cargo test -p tonepoet-features  # Features tests only
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

**Do not use system Rust** (1.82) — it cannot resolve some transitive dependencies. Always build inside `nix develop`.

## Workspace Structure

```
tonepoet/
├── Cargo.toml              # Workspace root + main tonepoet crate
├── flake.nix               # Nix dev shell + build package (includes libclang, ffmpeg)
├── src/
│   ├── main.rs             # Clap CLI: tui, convert, wizard, check-tools, config
│   ├── lib.rs              # pub mod convert; pub mod config; pub mod tui;
│   ├── config.rs           # TonepoetConfig (TOML at ~/.config/tonepoet/config.toml)
│   ├── convert/
│   │   ├── mod.rs           # Module root — re-exports, ConversionManager, ConversionConfig
│   │   ├── processor.rs     # ConversionProcessor — main orchestration (~4K LOC)
│   │   ├── queue.rs         # ConversionQueue, ConversionItem, ConversionStatus
│   │   ├── formats.rs       # AudioFormat, FileFormat, FormatDetector, ConversionOptions
│   │   ├── wizard_integration.rs  # Wizard state → ConversionOptions bridge
│   │   ├── simple_wizard.rs # ReplayGainMode, DitherType, NyquistTransition enums
│   │   ├── wizard.rs        # ConversionWizard (step-based, non-TUI)
│   │   ├── renaming.rs      # Tag-based file/folder renaming
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
├── crates/
│   ├── tonepoet-backend/    # FFmpeg/SoX command builders, pipeline, metadata I/O
│   ├── tonepoet-features/   # Log file writer, CUE sheet generator
│   └── tonepoet-wizard/     # Legacy ratatui TUI wizard (draw_wizard, events, presets)
```

## Crate Dependency Graph

```
tonepoet (main binary + lib)
├── tonepoet-backend     (no internal deps)
├── tonepoet-features    (depends on tonepoet-backend)
├── tonepoet-wizard      (no internal deps)
├── ffmpeg-next          (in-process audio probing via FFmpeg 7.1 bindings)
└── lofty                (audio metadata/tag reading)
```

## Key Types & Entry Points

**CLI subcommands** (src/main.rs):
- `tui` — launches the new TUI (default convert screen)
- `convert <PATHS>... [--format --output --workers --replaygain --preset --bitrate ...]`
- `wizard` — launches legacy TUI wizard
- `check-tools` — probes for ffmpeg, sox, ssrc, 7z, loudgain, opustags, etc.
- `config --show | --reset | --path`

**TUI architecture** (src/tui/):
- `AppState` holds all TUI state: `ConvertState`, `PresetState`, queue state, overlays
- `ConvertState` contains 4 pane states: `SourceState`, `MetadataState`, `FormatState`, `OutputOptionsState`
- `FormatState` uses `PillState<T>` for format, sample rate, bit depth, dither, ReplayGain pills
- `PillState<T>` is a generic pill selector with per-option enable/disable and format constraint cascading
- `AppScreen` enum: Convert (tab 1), Browse (2, placeholder), Library (3, placeholder), Queue (4), Config (5), Wizard (overlay)
- Two-pass rendering: draw first (immutable state), then register mouse buttons (mutable button_map)
- Vi command mode: `:` opens command input at bottom of screen, parsed by `command.rs`

**Audio probing** (src/tui/probe.rs):
- Uses `ffmpeg-next` Rust bindings (in-process, no subprocess spawning)
- `probe_audio(path)` → `SourceInfo` (format, codec, bit depth, sample rate, channels, duration, file size)
- `read_metadata(path)` → `SourceMetadata` (title, artist, album, genre, year) via `lofty`
- DSD rates displayed as DSD64-DSD1024 with MHz
- ffmpeg initialized once via `std::sync::Once`

**Core conversion flow:**
1. CLI args → `ConversionOptions` (src/main.rs `run_convert()`)
2. Files scanned → `ConversionQueue` populated with `ConversionItem`s
3. `ConversionProcessor::process_queue_with_progress()` runs items through workers
4. Each item: extract archive (if 7z) → detect format → convert via backend → apply ReplayGain → rename → write log/CUE

**Config location:** `~/.config/tonepoet/config.toml`
**Preset location:** `~/.config/tonepoet/presets/` (TOML files)
**Queue persistence:** `~/.cache/tonepoet/conversion_queue.json`

## Audio Format Support

**Common output formats** (shown as pills in TUI): FLAC, Opus, AAC, MP3, ALAC, WAV, WavPack

**All output formats** (available in Advanced): above plus DSF, DFF, W64, RF64, LPCM, raw PCM, raw AAC, WebM/WEBA, MKV/MKA, AIFF

**Input-only formats**: ISO, CUE (image decomposition), SHN, APE, DTS, AC3

**Bit depths**: 16, 24, 32 integer, 32-bit float, 64-bit float (availability depends on format)

**Sample rates**: 44.1 kHz through 768 kHz (PCM); DSD64 through DSD1024 in Advanced

## Format Constraint Cascade

The `FormatState::apply_format_constraints()` method recalculates available options when the format changes:
- **Opus**: sample rate locked to 48 kHz, bit depth and dither disabled
- **AAC**: bit depth and dither disabled, sample rate capped at 192 kHz
- **MP3**: bit depth and dither disabled, sample rate capped at 48 kHz
- **FLAC/AIFF/ALAC**: float bit depths (32f, 64f) disabled
- **WAV/WavPack**: all options available including float

## Coding Conventions

- **Edition 2021**, workspace resolver v2
- **Async:** Tokio with `broadcast` channels for progress, `Arc<RwLock<>>` for shared queue state
- **Error handling:** `thiserror` for typed errors in library code, `anyhow` in main.rs
- **Logging:** `log` crate macros (`info!`, `warn!`, `error!`), initialized with `env_logger`
- **Serialization:** `serde` + `toml` for config/presets, `serde_json` for queue persistence
- **TUI:** `ratatui` 0.26 + `crossterm` 0.27; Tokyo Night theme in `theme.rs`
- **Audio probing:** `ffmpeg-next` 7.1 (in-process FFmpeg bindings, requires libclang for bindgen)
- **Metadata reading:** `lofty` 0.21 with `TaggedFileExt` and `Accessor` traits
- **External tool invocation:** `tokio::process::Command` (async) for ffmpeg/sox/7z/loudgain etc.
- Module-level re-exports in `mod.rs` files — consumers import from `tonepoet::convert::` not submodules directly
- TUI draw functions take immutable state refs; mouse buttons registered in a second pass via `ButtonRenderMap`

## Testing

```bash
cargo test                          # All workspace tests
cargo test -p tonepoet-backend     # 16 tests: ffmpeg builders, integration, channels
cargo test -p tonepoet-features    # 14 tests: log writer, CUE generator
```

Tests are in `crates/*/tests/` directories. Examples in `crates/*/examples/` serve as integration smoke tests. The main `tonepoet` crate does not have its own test suite yet.

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

## Important Notes

- **processor.rs is the largest file** (~4K LOC) — it orchestrates the entire conversion pipeline including 7z extraction, multi-format conversion, ReplayGain, metadata transfer, and file renaming
- The wizard crate has its own `main.rs` for standalone use but tonepoet's `main.rs` embeds the wizard directly
- The new TUI (`src/tui/`) is the primary interface; the wizard crate is kept as-is for legacy/preset access
- Archive passwords are configurable via `--archive-password` flag or `config.toml` — no hardcoded defaults
- The `items_mut()` method on `ConversionQueue` is private; use `add_item()` or `add_item_direct()` from outside the module
- `probe_audio()` uses unsafe FFI to access `bits_per_raw_sample` from codec parameters (standard ffmpeg-next pattern)
