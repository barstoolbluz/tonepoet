# tonepoet

A batch audio conversion toolkit with an interactive TUI wizard. Handles archive extraction, format conversion (via ffmpeg/sox), ReplayGain, metadata preservation, file renaming, and CUE sheet generation.

## Status

**Pre-alpha — extracting core from [hexload-tui](https://github.com/barstoolbluz/hexload-tui).**

The conversion engine, command builders, queue system, and wizard UI already exist and work inside hexload-tui. This project extracts them into a standalone tool that works with any audio files, not just ones from a specific archive site.

## Origin

hexload-tui is a TUI app for browsing/downloading/converting audio from a WordPress-hosted archive. Its conversion subsystem is powerful enough to stand on its own:

- Batch queue with concurrency control (semaphore-based, configurable workers)
- ffmpeg and sox backends with automatic fallback
- Archive extraction (7z, with plans for zip/rar/tar)
- Format detection (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus)
- ReplayGain calculation (album and track modes)
- Metadata preservation and transfer across formats
- File/folder renaming based on audio tags
- CUE sheet generation for multi-track albums
- Conversion logging
- Interactive TUI wizard for configuring conversion options
- Preset system for common conversion profiles

## Source Components (in hexload-tui)

These are the pieces being extracted. Listed with their hexload-tui paths and dependency status:

### Fully Independent (no hexload deps)

| Component | Path in hexload-tui | LOC | What it does |
|-----------|-------------------|-----|-------------|
| conversion-backend | `hexloader-tui-conversion-backend-handover/` | ~7,000 | ffmpeg/sox command builders, pipeline execution, tool availability checks |
| conversion-features | `conversion-features/` | ~1,600 | Conversion log writer, CUE sheet generator |
| tui-wizard-core | `tui-wizard-core/` | ~7,500 | ratatui-based audio format/quality wizard UI |
| tui-options-wizard | `integration-package/tui-wizard-core/` | ~9,000 | Enhanced wizard with backend, priority, overwrite options |
| queue.rs | `src/convert/queue.rs` | ~450 | ConversionQueue, ConversionItem, ConversionStatus, persistence |
| formats.rs | `src/convert/formats.rs` | ~360 | AudioFormat, FileFormat, FormatDetector, ConversionOptions |
| labels.rs | `src/convert/labels.rs` | ~610 | Label/pressing info detection from filenames |
| metadata.rs | `src/convert/metadata.rs` | ~150 | FLAC metadata extraction via metaflac |
| renaming.rs | `src/convert/renaming.rs` | ~880 | File/album tagging, folder structure from audio tags |
| simple_wizard.rs | `src/convert/simple_wizard.rs` | ~180 | SimpleWizard state machine (non-UI) |

### Needs Minor Adaptation

| Component | Path in hexload-tui | LOC | What changes |
|-----------|-------------------|-----|-------------|
| processor.rs | `src/convert/processor.rs` | ~4,000 | Replace hexload config/message types with standalone equivalents |
| wizard_integration.rs | `src/convert/wizard_integration.rs` | ~550 | Mapping between wizard UI state and ConversionOptions — extract as-is |
| mod.rs | `src/convert/mod.rs` | ~530 | ConversionManager lifecycle — replace AppState refs with standalone config |

### Hexload-Specific (drop or generalize)

| Feature | Notes |
|---------|-------|
| Download+Convert workflow | Drop for v1. Users provide files directly. |
| Default 7z password | Currently a fallback default in processor.rs; make fully configurable via CLI flag or config file |
| Lineage.txt generation | Generalize as "source metadata annotation" |

## External Tool Dependencies

| Tool | Purpose | Required? |
|------|---------|-----------|
| ffmpeg | Primary conversion backend | Yes (or sox) |
| sox | Secondary conversion backend, resampling/dithering | Yes (or ffmpeg) |
| ffprobe | Audio format/bitdepth detection | Recommended |
| 7z | Archive extraction | For archive input only |
| metaflac | FLAC metadata read/write | For FLAC operations |
| loudgain | ReplayGain calculation | For ReplayGain feature |
| opustags | Opus metadata writing | For Opus output |
| wvtag | WavPack metadata writing | For WavPack output |
| AtomicParsley | M4A/AAC metadata writing | For M4A/AAC output |

## Planned Interface

### CLI
```bash
tonepoet convert ./album.7z --format flac --quality high --replaygain album
tonepoet convert ./tracks/ --format opus --bitrate 128 --workers 4
tonepoet convert . --preset "FLAC re-encode"
tonepoet wizard                    # interactive TUI mode
tonepoet check-tools               # verify external tool availability
tonepoet config --set backend=sox  # persistent configuration
```

### As a Library
```rust
use tonepoet::{ConversionManager, ConversionOptions, AudioFormat};

let options = ConversionOptions {
    output_format: AudioFormat::Flac,
    // ...
};
let mut manager = ConversionManager::new(config);
manager.add_files(&paths, options)?;
manager.process_queue().await?;
```

## Architecture (target)

```
tonepoet/
  src/
    main.rs              -- CLI entry point (clap)
    config.rs            -- TOML config loading/saving
    message.rs           -- ConversionMessage enum for progress reporting
    convert/
      mod.rs             -- ConversionManager
      queue.rs           -- Queue, items, status, persistence
      formats.rs         -- Format detection, AudioFormat, ConversionOptions
      processor.rs       -- Orchestration: extract, convert, replaygain, rename
      metadata.rs        -- Audio metadata extraction
      labels.rs          -- Label/pressing info detection
      renaming.rs        -- Tag-based file/folder renaming
      simple_wizard.rs   -- Wizard state machine
      wizard_integration.rs -- Wizard state -> ConversionOptions mapping
  tui/
    wizard.rs            -- Interactive TUI wizard (from tui-wizard-core)
    progress.rs          -- TUI progress display for batch operations
  backend/
    mod.rs               -- ConversionBackend trait, convert_with_backend()
    ffmpeg.rs            -- FFmpegBuilder
    sox.rs               -- SoxBuilder
    pipeline.rs          -- ConversionPipeline, metadata preservation
    tools.rs             -- Tool availability checking
  features/
    log_writer.rs        -- Conversion result logging
    cue.rs               -- CUE sheet generation
```

## Rust Dependencies (expected)

```toml
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
ratatui = "0.26"
crossterm = "0.27"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
chrono = "0.4"
uuid = { version = "1", features = ["v4"] }
lofty = "0.21"           # audio metadata
log = "0.4"
env_logger = "0.10"
anyhow = "1"
thiserror = "1"
dirs = "5"
regex = "1"
lazy_static = "1"
walkdir = "2"             # recursive directory traversal
tempfile = "3"            # temp dirs for archive extraction
num_cpus = "1"            # default worker count detection
indicatif = "0.17"        # CLI progress bars (new, not from hexload)
```

## Key Design Decisions

1. **CLI-first, wizard second.** Get the conversion pipeline working as a CLI tool before adding the interactive TUI.
2. **No download integration in v1.** Users provide local files or archives. Download can be a plugin later.
3. **Configurable everything.** Archive passwords, output paths, worker counts, backends — all via config file or CLI flags. No hardcoded values.
4. **Preserve the batch architecture.** The semaphore-based concurrent processing with queue persistence is valuable. Keep it.
5. **Library + binary.** Structure as a library crate with a binary entry point so others can embed it.
