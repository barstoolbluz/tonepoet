# tonepoet

A standalone CLI + TUI audio conversion and metadata management toolkit for music collectors who're fastidious as fuck about every. bloody. detail. of their libraries.

But tonepoet is absolutely usable by normies, too: it exposes an intuitive, mouse- and keyboard-driven UX for working with audio file metadata, as well as sensible, opinionated defaults for converting audio files. Convert from DSD to PCM, PCM to PCM, PCM to DSD, etc., inheriting sensible, high-performance defaults ... or specifying your own.

> **Status:** tonepoet is unfinished and under active development, with multiple commits landing daily. Features described below are at varying stages of completeness. Expect a few rough edges, occasional breaking changes, and documentation / help gaps.

## What it does

tonepoet is a music library workstation in your terminal: browse and manage your collection, verify disc rips, analyze audio quality, tag from MusicBrainz, and convert between any format — all through a keyboard-and-mouse-driven TUI or batch CLI. It handles single files, multi-track archives, CUE+image decomposition, and SACD ISO extraction.

### Browsing and file management

- **File browser** — audio-only filtering, type-ahead and recursive search, visual/range selection, column sorting, right-click context menus, info pane with metadata and analysis
- **Archive browsing** — preview 7z/zip/rar contents before extraction, password keychain for encrypted archives
- **Template-based naming** — folder and filename templates with tag variables (%ARTIST%, %ALBUM%, %TITLE%, etc.), interactive template builder with saved presets
- **Bulk rename** — tag-based batch renaming with preview
- **Bookmarks** — saved directory shortcuts
- **Recent files** — quick access to recently opened paths

### Metadata and tagging

- **MusicBrainz** — disc-TOC-based release lookup, interactive release picker, per-track title/artist/ISRC population
- **GNUDB** — freedb/gnudb disc ID lookup with multi-disc support
- **Per-track metadata editor** — inline tag editing with MusicBrainz integration, CUE preview, revert/restore
- **SACD sidecar XML** — persistent metadata sidecars for SACD ISOs (MusicBrainz tagging workflow)
- **CUE sheet parsing** — legacy encoding support (CP932/Shift-JIS, EUC-JP, GBK, Big5, Windows-1252), embedded CUESHEET preferred over sidecar
- **CUESHEET embedding** — regenerate and embed CUESHEET tags on metadata save
- **Typed metadata effects** — pipeline tracks source-tag transfer, artwork preservation, and authoritative metadata application independently to prevent silent metadata loss

### Disc verification

- **AccurateRip** — verify CD rips against the AccurateRip database with common offset detection and full offset scanning
- **CUETools DB** — CRC32 verification against the CUETools database
- **Reed-Solomon repair** — built-in parity-based error correction for inaccurate rips using CUETools DB recovery data
- **Drive offset correction** — re-encode with corrected read offset when AccurateRip identifies a mismatch

### Audio analysis

- **Dynamic range (DR)** measurement and reporting
- **Peak/RMS level** analysis per track and album
- **Clipping detection** — identify intersample and sample-level clipping
- **Pre-emphasis detection** — spectral analysis + metadata/catalog heuristics to identify pre-emphasized discs
- **Bit depth analysis** — actual vs container bit depth

### Conversion

**Output:** FLAC, Opus, AAC (libfdk_aac), MP3, ALAC, WAV, WavPack, DSF, DFF, W64, RF64, AIFF, LPCM, WebM, MKV

**Input (decode-only):** All output formats plus ISO (SACD), CUE+image, 7z/zip/rar archives, SHN, APE, DTS, AC3

**Resamplers:**

- **Sox** (sox_ng) — rate effect with undocumented `-u` ultra mode (701 taps, 210 dB rejection), sinc FIR pre-filter with full parameter control (taps, attenuation, passband, transition band, Kaiser beta, phase)
- **Soxr** — via ffmpeg's aresample filter, up to 33-bit precision, Chebyshev filter option
- **SSRC** — brick-wall sinc interpolation with 7 quality profiles (lightning through insane), ATH psychoacoustic noise shaping, min-phase filters, rate-dependent dither validation
- Automatic dither suppression when target bit depth >= source (no pointless noise addition)

### SACD support

Native SACD ISO extraction via the built-in sacd-rs crate (byte-exact against sacd_extract, validated across 70+ tracks). DSD-to-PCM conversion through sox with auto-gain peak normalization (`norm` effect), configurable safety margin, and rate-dependent lowpass filtering. DST frame decoding for compressed SACD layers.

## Building

**Requires nix.** The project uses a nix flake for the Rust toolchain, all runtime audio tools, and ffmpeg development libraries.

```bash
# Enter dev shell
nix develop --extra-experimental-features 'nix-command flakes'

# Build
cargo build --release

# Run
cargo run --release -- tui           # TUI (main interface)
cargo run --release -- convert ./file.flac --format opus
cargo run --release -- check-tools   # verify external tools
cargo run --release -- config --show

# Test
cargo test --lib --workspace
```

## TUI

The TUI is the primary interface. Five screens: Browse, Library, Convert, Queue, Config. Browse is the home screen — it's where you manage your collection, verify rips, analyze audio, tag from MusicBrainz, and stage files for conversion.

- **Browse** — full-featured file browser with audio-only filtering, type-ahead and recursive search, visual/range selection, column sorting, right-click context menus with disc verification, audio analysis, metadata editing, and conversion actions. Info pane shows metadata, analysis results, and artwork.
- **Convert** — four-pane staging screen (source, metadata, format, output options) with pill-based controls, per-codec settings overlays (FLAC, AAC, Opus, MP3, WavPack), per-resampler settings overlays (SSRC, Sox, Soxr), preset system, DSD-aware gain controls
- **Queue** — batch conversion monitor with per-track progress, expandable sub-lines, pause/resume, retry failed, clear completed
- **Config** — settings editor with archive password keychain

Every action has three input paths: keyboard (vi-style colon commands), mouse clicks, and right-click context menus. Format-specific settings overlays include scrollable context-sensitive help (`?` key).

## Architecture

```
tonepoet/
├── src/
│   ├── main.rs             # CLI: tui, convert, check-tools, config
│   ├── config.rs           # TOML config (~/.config/tonepoet/config.toml)
│   ├── ctdb_rs/            # CUETools DB client + Reed-Solomon decoder
│   ├── convert/            # Conversion engine
│   │   ├── pipeline/       # Staged pipeline: materialize → plan → convert →
│   │   │                   #   merge → metadata → replaygain → publish
│   │   ├── processor.rs    # Orchestration
│   │   └── formats.rs      # Format detection
│   └── tui/                # Terminal interface
│       ├── app.rs          # Central state
│       ├── keybindings.rs  # Key + mouse dispatch
│       ├── probe.rs        # In-process ffmpeg audio probing
│       ├── accuraterip.rs  # AccurateRip verification
│       ├── ctdb.rs         # CUETools DB verification
│       ├── musicbrainz.rs  # MusicBrainz release lookup + tagging
│       ├── gnudb.rs        # GNUDB/freedb disc lookup
│       ├── analyze.rs      # Audio analysis (DR, peaks, clipping)
│       ├── preemphasis/    # Pre-emphasis detection (spectral + heuristic)
│       └── draw_*.rs       # Screen rendering
├── tonepoet-pipeline/      # Conversion planning crate
│   ├── settings.rs         # PipelineSettings (69 fingerprinted fields)
│   ├── plugins.rs          # Tool plugins (ffmpeg, sox, ssrc, loudgain)
│   └── plan.rs             # Conversion planner
└── crates/
    ├── sacd-rs/            # SACD ISO reader + DST decoder
    ├── tonepoet-backend/   # FFmpeg/Sox command builders
    └── tonepoet-features/  # Log writer, CUE sheet generator
```

## External tools

All provided by the nix flake:

| Tool | Purpose |
|------|---------|
| ffmpeg 7.1 (unfree) | Primary backend + in-process probing |
| sox_ng | Resampling, DSD conversion, dithering |
| ssrc 2.4.2 | Brick-wall resampling |
| flac, metaflac | FLAC encode/decode, metadata |
| lame | MP3 encoding |
| opusenc, opustags | Opus encoding, metadata |
| wavpack, wvtag | WavPack encoding, metadata |
| loudgain | ReplayGain analysis |
| AtomicParsley | AAC/M4A metadata |
| 7z (p7zip) | Archive extraction |

## License

GPL-3.0-or-later
