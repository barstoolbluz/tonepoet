# CLAUDE.md — tonepoet

## Project Overview

tonepoet is a standalone CLI + TUI audio conversion toolkit extracted from hexload-tui. It converts audio files between formats (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus), extracts 7z archives, applies ReplayGain, preserves/rewrites metadata, renames files from tags, and generates CUE sheets and conversion logs.

## Build & Run

**Requires nix.** The project uses a nix flake to provide both the Rust toolchain (latest stable via rust-overlay) and all runtime audio tools (ffmpeg, sox, ssrc, loudgain, opustags, flac, lame, wavpack, p7zip, AtomicParsley, etc.).

```bash
# Enter dev shell (Rust + all audio tools)
nix develop --extra-experimental-features 'nix-command flakes'

# Inside the dev shell:
cargo build                  # Debug build
cargo build --release        # Release build
cargo test                   # Run all workspace tests
cargo test -p tonepoet-backend   # Backend tests only
cargo test -p tonepoet-features  # Features tests only
cargo check                  # Fast type check

# Run the binary
cargo run -- convert ./test.flac --format opus
cargo run -- check-tools
cargo run -- wizard
cargo run -- config
```

**Do not use system Rust** (1.82) — it cannot resolve some transitive dependencies. Always build inside `nix develop`.

## Workspace Structure

```
tonepoet/
├── Cargo.toml              # Workspace root + main tonepoet crate
├── flake.nix               # Nix dev shell + build package
├── src/
│   ├── main.rs             # Clap CLI: convert, wizard, check-tools, config
│   ├── lib.rs              # pub mod convert; pub mod config;
│   ├── config.rs           # TonepoetConfig (TOML at ~/.config/tonepoet/config.toml)
│   └── convert/
│       ├── mod.rs           # Module root — re-exports, ConversionManager, ConversionConfig
│       ├── processor.rs     # ConversionProcessor — main orchestration (~4K LOC)
│       ├── queue.rs         # ConversionQueue, ConversionItem, ConversionStatus
│       ├── formats.rs       # AudioFormat, FileFormat, FormatDetector, ConversionOptions
│       ├── wizard_integration.rs  # Wizard state → ConversionOptions bridge
│       ├── simple_wizard.rs # ReplayGainMode, DitherType, NyquistTransition enums
│       ├── wizard.rs        # ConversionWizard (step-based, non-TUI)
│       ├── renaming.rs      # Tag-based file/folder renaming
│       ├── labels.rs        # Label/pressing info detection
│       └── metadata.rs      # FLAC metadata extraction
├── crates/
│   ├── tonepoet-backend/    # FFmpeg/SoX command builders, pipeline, metadata I/O
│   ├── tonepoet-features/   # Log file writer, CUE sheet generator
│   └── tonepoet-wizard/     # Ratatui TUI wizard (draw_wizard, events, presets)
```

## Crate Dependency Graph

```
tonepoet (main binary + lib)
├── tonepoet-backend     (no internal deps)
├── tonepoet-features    (depends on tonepoet-backend)
└── tonepoet-wizard      (no internal deps)
```

The `src/convert/` module imports from all three workspace crates:
- `processor.rs` uses `tonepoet_backend::` and `tonepoet_features::`
- `wizard_integration.rs` uses `tonepoet_wizard::` and `tonepoet_backend::`
- `formats.rs` uses `tonepoet_backend::Backend` and `tonepoet_backend::types::ConversionSettings`

## Key Types & Entry Points

**CLI subcommands** (src/main.rs):
- `convert <PATHS>... [--format --output --workers --replaygain --preset --bitrate ...]`
- `wizard` — launches TUI with ratatui/crossterm
- `check-tools` — probes for ffmpeg, sox, ssrc, 7z, loudgain, opustags, etc.
- `config --show | --reset | --path`

**Core conversion flow:**
1. CLI args → `ConversionOptions` (src/main.rs `run_convert()`)
2. Files scanned → `ConversionQueue` populated with `ConversionItem`s
3. `ConversionProcessor::process_queue_with_progress()` runs items through workers
4. Each item: extract archive (if 7z) → detect format → convert via backend → apply ReplayGain → rename → write log/CUE

**Config location:** `~/.config/tonepoet/config.toml`
**Preset location:** `~/.config/tonepoet/presets/` (TOML files)
**Queue persistence:** `~/.cache/tonepoet/conversion_queue.json`

## Coding Conventions

- **Edition 2021**, workspace resolver v2
- **Async:** Tokio with `broadcast` channels for progress, `Arc<RwLock<>>` for shared queue state
- **Error handling:** `thiserror` for typed errors in library code, `anyhow` in main.rs
- **Logging:** `log` crate macros (`info!`, `warn!`, `error!`), initialized with `env_logger`
- **Serialization:** `serde` + `toml` for config/presets, `serde_json` for queue persistence
- **TUI:** `ratatui` 0.26 + `crossterm` 0.27; wizard uses `draw_wizard(f, &wizard) -> MouseAreas` pattern
- **External tool invocation:** `tokio::process::Command` (async) for ffmpeg/sox/7z/loudgain etc.
- Module-level re-exports in `mod.rs` files — consumers import from `tonepoet::convert::` not submodules directly

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
| ffmpeg (7-full, unfree) | Primary conversion backend |
| sox (sox_ng) | Alternative backend, Gesemann dithering |
| ssrc | Brick-wall resampling |
| flac, metaflac | FLAC encode/decode and metadata |
| lame | MP3 encoding |
| opusenc, opustags | Opus encoding and metadata |
| wavpack, wvtag | WavPack encoding and metadata |
| loudgain | ReplayGain analysis |
| AtomicParsley | AAC/M4A metadata |
| ffprobe | Audio stream analysis |
| 7z (p7zip) | Archive extraction |

## Important Notes

- **processor.rs is the largest file** (~4K LOC) — it orchestrates the entire conversion pipeline including 7z extraction, multi-format conversion, ReplayGain, metadata transfer, and file renaming
- The wizard crate has its own `main.rs` for standalone use but tonepoet's `main.rs` embeds the wizard directly
- Archive passwords are configurable via `--archive-password` flag or `config.toml` — no hardcoded defaults
- The `items_mut()` method on `ConversionQueue` is private; use `add_item()` or `add_item_direct()` from outside the module
