# tonepoet

A standalone CLI + TUI audio conversion and metadata management toolkit for music collectors who're fastidious as fuck about every. bloody. detail. of their libraries.

But tonepoet is absolutely usable by normies, too: it exposes an intuitive, mouse- and keyboard-driven UX for working with audio file metadata, as well as sensible, opinionated defaults for converting audio files. Extract and demux audio from SACD, DVD-Audio, DVD-Video, and Blu-ray discs. Convert from DSD to PCM, PCM to PCM, PCM to DSD, etc., inheriting sensible, high-performance defaults ... or specifying your own.

> **Status:** tonepoet is unfinished and under active development, with multiple commits landing daily. Features described below are at varying stages of completeness. Expect a few rough edges, occasional breaking changes, and documentation / help gaps.

## What it does

tonepoet is a music library workstation in your terminal: browse and manage your collection, verify disc rips, analyze audio quality, tag from MusicBrainz, and convert between any format — all through a keyboard-and-mouse-driven TUI or batch CLI. It handles single files, multi-track archives, CUE+image decomposition, SACD ISO extraction, DVD-Audio ISO extraction, DVD-Video ISO audio extraction, and Blu-ray audio extraction.

### Browsing and file management

- **File browser** — audio-only filtering, type-ahead and recursive search, visual/range selection, column sorting, right-click context menus, info pane with metadata and analysis
- **Archive browsing** — browse inside 7z/zip/rar/tar/tar.gz/tar.bz2/tar.xz/tar.zst archives transparently. Rename files, edit metadata tags, and delete entries — all changes staged locally and repackaged atomically on navigate-away. Deferred save with SQLite crash recovery. Password keychain for encrypted archives. Info pane shows full metadata and format details for files inside archives. Progress overlay during repackage operations.
- **Template-based naming** — folder and filename templates with tag variables (%ARTIST%, %ALBUM%, %TITLE%, etc.), interactive template builder with saved presets
- **Bulk rename** — tag-based batch renaming with preview
- **Bookmarks** — saved directory shortcuts
- **Recent files** — quick access to recently opened paths

### Metadata and tagging

- **MusicBrainz** — disc-TOC-based release lookup (CD, SACD, DVD-Audio, DVD-Video, Blu-ray via synthetic TOC), interactive release picker, per-track title/artist/ISRC population
- **GNUDB** — freedb/gnudb disc ID lookup with multi-disc support
- **Per-track metadata editor** — four-tab editor (Metadata, Details, ReplayGain, Artwork) with inline tag editing, MusicBrainz integration, CUE preview, revert/restore, dropdown presentation selector for disc sources. Details tab shows technical info with HDCD and pre-emphasis detection status. ReplayGain tab displays per-track gain/peak values with in-editor scanning via loudgain. Artwork tab shows embedded picture inventory with add/replace/remove via built-in file picker
- **Disc metadata sidecars** — persistent metadata sidecars for SACD ISOs (XML), DVD-Audio (foo_input_dvda-compatible XML), DVD-Video (TOML with multi-presentation support), and Blu-ray (TOML with multi-presentation support)
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

**Input (decode-only):** All output formats plus ISO (SACD, DVD-Audio, DVD-Video, Blu-ray), Blu-ray BDMV directories, CUE+image, 7z/zip/rar/tar/tar.gz/tar.bz2/tar.xz/tar.zst archives, SHN, APE, DTS, AC3

**CLI depth policy:** `--bit-depth` accepts `16`, `24`, `32`, `32f`, `64f`, or `source`. With no flag, the CLI retains `source`. For DSD and lossy inputs, where a PCM source width is undefined, `source` resolves to the target format's conservative PCM default (24-bit for FLAC, ALAC, WavPack, WAV, and AIFF) and the conversion log identifies that value as a plan default. A PCM source whose original width cannot be measured still fails closed and asks for an explicit depth. Explicit numeric requests never carry the default-policy label. Twenty-bit DVD-Audio PCM is carried in a 24-bit output container, with the widening identified in the conversion log rather than misreported as a 24-bit source measurement.

**Resamplers:**

- **Sox** (sox_ng) — rate effect with undocumented `-u` ultra mode (701 taps, 210 dB rejection), sinc FIR pre-filter with full parameter control (taps, attenuation, passband, transition band, Kaiser beta, phase)
- **Soxr** — via ffmpeg's aresample filter, up to 33-bit precision, Chebyshev filter option
- **SSRC** — brick-wall sinc interpolation with 7 quality profiles (lightning through insane), ATH psychoacoustic noise shaping, min-phase filters, rate-dependent dither validation
- Automatic dither suppression when target bit depth >= source (no pointless noise addition)

### SACD support

Native SACD ISO extraction via the built-in sacd-rs crate (byte-exact against sacd_extract). DSD-to-PCM conversion through sox with auto-gain peak normalization (`norm` effect), configurable safety margin, and rate-dependent lowpass filtering. DST frame decoding for compressed SACD layers.

### DVD-Audio support

Native DVD-Audio ISO and directory extraction via the built-in dvda-demuxer crate. IFO/AOB parsing, MLP and LPCM demuxing with framed MLP access-unit reassembly and strict/tolerant fallback. foo_input_dvda-compatible stereo extraction: cross-ATS presentations use the backing multichannel group's chapter boundaries, native MLP substream 0 extraction for authored stereo via in-process libavcodec, coefficient pan-filter fallback for single-substream MLP and PCM. IFO-authored downmix matrix support. Disc browser with stream picker, multi-group metadata editor with MusicBrainz integration via synthetic CD TOC lookup.

### DVD-Video support

DVD-Video ISO and directory audio extraction with per-chapter track splitting. LPCM extraction via in-process demuxer with IFO audio attribute override from packet sub-headers (corrects unreliable IFO sample rate and bit depth). Disc browser with stream picker showing all VTS/title/audio-stream combinations. Multi-presentation TOML metadata sidecars with MusicBrainz integration via synthetic CD TOC lookup and text search fallback. Metadata editor with dropdown presentation selector for discs with many programs, smart default selection preferring LPCM with existing sidecar metadata.

### Blu-ray support

Blu-ray ISO and BDMV directory audio extraction with per-chapter track splitting. LPCM streams are extracted in-process via libbluray with M2TS 192-byte packet demuxing, PES reassembly, big-endian to little-endian byte swap, and multi-clip PTS continuity mapping. Compressed codecs (TrueHD, DTS-HD MA, DTS-HD HR, DTS, AC-3, E-AC-3) are decoded via ffmpeg's `bluray://` protocol. Bit depth is probed from the actual stream via ffprobe at browse time, not hardcoded. Disc browser with stream picker showing all playlists, audio streams, and angles. Multi-presentation TOML metadata sidecars with MusicBrainz integration via synthetic CD TOC from chapter durations. AACS decryption via libaacs when KEYDB.cfg is available at `~/.config/aacs/KEYDB.cfg`.

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
cargo run --release -- convert ./album.iso --format flac --bit-depth 24
cargo run --release -- check-tools   # verify external tools
cargo run --release -- config --show

# Test
cargo test --workspace
```

## TUI

The TUI is the primary interface. Five screens: Browse, Library, Convert, Queue, Config. Browse is the home screen — it's where you manage your collection, verify rips, analyze audio, tag from MusicBrainz, and stage files for conversion.

- **Browse** — full-featured file browser with audio-only filtering, type-ahead and recursive search, visual/range selection, column sorting, right-click context menus with disc verification, audio analysis, metadata editing, and conversion actions. Info pane shows metadata, analysis results, and artwork.
- **Convert** — four-pane staging screen (source, metadata, format, output options) with pill-based controls, per-codec settings overlays (FLAC, AAC, Opus, MP3, WavPack), per-resampler settings overlays (SSRC, Sox, Soxr), preset system, DSD-aware gain controls
- **Queue** — batch conversion monitor with per-track progress, expandable sub-lines, pause/resume, retry failed, clear completed
- **Config** — settings editor with archive password keychain, 24 built-in themes (dark + light) with theme builder, performance tuning for archive browsing

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
    ├── dvda-demuxer/       # DVD-Audio IFO/AOB parser + LPCM/MLP demuxer
    ├── tonepoet-backend/   # FFmpeg/Sox command builders
    ├── tonepoet-features/  # Log writer, CUE sheet generator
    └── tui-file-picker/    # Standalone reusable TUI file browser/picker
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
| AtomicParsley | Required for authoritative non-native/custom tags in M4A/MP4 outputs; conversions fail closed rather than silently dropping those keys |
| libbluray | Blu-ray disc reading + AACS integration |
| libaacs | Blu-ray AACS decryption (requires user-provided KEYDB.cfg) |
| 7z (p7zip) | Archive extraction + repackaging |

## License

GPL-3.0-or-later
