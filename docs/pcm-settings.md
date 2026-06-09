# PCM Settings Documentation

## Preset Basics

| Field | Type | Default | Description |
|---

# Custom Effects Chain

## Custom Effects Override

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| **Command** | Text field | `None` | ⚠️ Custom effects chain overrides ALL default settings |

**Warning**: When a custom command is entered, all GUI settings are bypassed. The custom command takes complete control of the processing pipeline.

---

# Output Configuration

## Output Path & Naming

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| **Destination** | Path + Browse button | Last used directory | Browse button for folder selection |
| **Pattern** | Text field | See default pattern | User-configurable with validation |

### Default Naming Pattern

```
%album artist% - %album% (%date%) [%codec%]/%tracknumber2% - %title%
```

### Available Placeholders

| Placeholder | Description | Example Output | Type |
|------------|-------------|----------------|------|
| `%album artist%` | Album artist name | `The Beatles` | string |
| `%album%` | Album title | `Abbey Road` | string |
| `%date%` | Release date | `1969` | string/int |
| `%codec%` | Audio codec | `FLAC`, `MP3` | string |
| `%tracknumber1%` | Track number (unpadded) | `1`, `12` | int |
| `%tracknumber2%` | Track number (2-digit) | `01`, `12` | int (zero-padded) |
| `%title%` | Track title | `Come Together` | string |
| `%User-Defined%` | Custom string | Any user text | string |

### Path Validation & Sanitization

**Invalid characters in folder/file names are automatically replaced:**

| Invalid Character | Replacement | Context |
|------------------|-------------|---------|
| `/` (in filename) | ` ` (space) | Prevents path traversal |
| `\` (in filename) | ` ` (space) | Windows path separator |
| `:` | `-` | Drive letter confusion |
| `*`, `?`, `"`, `<`, `>`, `\|` | `_` | Filesystem restrictions |

**Example sanitization:**
```
Input:  %album% {CBS/Sony Japan} / %title%
Output: %album% {CBS Sony Japan} / %title%
        (forward slash in {} replaced with space)
```

### Naming Pattern Examples

| Pattern | Result |
|---------|--------|
| `%album artist% - %album% (%date%) [%codec%]/%tracknumber2% - %title%` | `The Beatles - Abbey Road (1969) [FLAC]/01 - Come Together.flac` |
| `%album artist%/%album%/%tracknumber2%. %title%` | `The Beatles/Abbey Road/01. Come Together.flac` |
| `%date% - %album% - %tracknumber1% - %title%` | `1969 - Abbey Road - 1 - Come Together.flac` |

### Parser Implementation Rules

1. **Token format**: Enclosed in `%...%`
2. **Literal text**: Non-token text inserted verbatim
3. **Missing metadata**: Replace with empty string
4. **Escaping**: Use `%%` for literal `%`
5. **Path separators**: `/` creates subdirectories
6. **File extension**: Added automatically based on codec

## File Handling Options

| Field | Options | Default | Notes |
|-------|---------|---------|-------|
| **If file exists** | `Ask`, `Overwrite`, `Create copy` | `Ask` | Collision handling |
| **Transfer tags** | Checkbox | `true` | Copy metadata to output |
| **Merge into single file** | Checkbox | `false` | Concatenate sources† |
| **Create multi-track file** | Checkbox | `false` | Single file with cue points‡ |

†Concatenates all source files into one output file  
‡Creates a single file with internal track markers (e.g., for CD images)

---

# Post-Processing

## After Converting

| Field | Options | Default | Notes |
|-------|---------|---------|-------|
| **ReplayGain scan** | `Off`, `Album`, `Track`, `Both` | `Off` | Analyze and tag levels |

### ReplayGain Scanning Options

**Visibility:** `replaygain_scan != 'Off'`

| Mode | Description | Tags Added |
|------|-------------|------------|
| **Album** | Analyze as album unit | `REPLAYGAIN_ALBUM_GAIN`, `REPLAYGAIN_ALBUM_PEAK` |
| **Track** | Individual track analysis | `REPLAYGAIN_TRACK_GAIN`, `REPLAYGAIN_TRACK_PEAK` |
| **Both** | Album + track analysis | All four tags |

## Additional File Operations

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| **Copy folders** | Checkbox + Text | `false` | Copy specified folders from source§ |
| **Folder patterns** | Text field | `*, artwork, art, covers` | Folder names to copy |
| **Copy files** | Checkbox + Text | `false` | Copy matching files to destination¶ |
| **File patterns** | Text field | See below | File extensions to copy |

§Copies entire folders matching the pattern from `%source_folder%` to destination  
¶Copies individual files matching the pattern

### Default File Copy Patterns

```
*.CUE;*.JPG;*.PNG;*.JPEG;*.PDF;*.M3U;*.M3U8;*.LOG;*.TXT
```
(Case-insensitive matching for all extensions)

## Script Execution

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| **Run scripts** | Checkbox + Text | `false` | Execute after conversion** |
| **Script command** | Text field | - | Command line to execute |

**Script receives environment variables:
- `$SOURCE_FILE`: Original input file path
- `$OUTPUT_FILE`: Converted output file path
- `$CODEC`: Output codec used
- `$SAMPLE_RATE`: Output sample rate
- `$BIT_DEPTH`: Output bit depth (if applicable)

### Post-Processing Examples

```bash
# ReplayGain scanning (sox)
sox --replay-gain album *.flac

# ReplayGain scanning (ffmpeg/loudgain)
loudgain -a -k -s e *.flac

# Copy artwork and logs
cp "$SOURCE_DIR"/*.jpg "$OUTPUT_DIR"/
cp "$SOURCE_DIR"/*.log "$OUTPUT_DIR"/

# Run custom script
./post_process.sh "$OUTPUT_FILE"
```

### Processing Order

1. File conversion completes
2. Tags transferred (if enabled)
3. ReplayGain scanning (if enabled)
4. Additional files/folders copied
5. Scripts executed

-------|------|---------|-------------|
| **Name** | Text input | *placeholder: "Enter preset name"* | User-defined name for environment† |
| **Description** | Text input | *placeholder: "Enter description"* | User-defined description† |
| **View Mode** | Toggle | `Standard` | `Standard` / `Expert` - toggles between basic and advanced settings |

†Text field behavior: Select field + Enter to type → Enter again to commit

### Backend Selection

| Setting | Options | Default | Notes |
|---------|---------|---------|-------|
| **Backend** | `Auto`, `sox_ng`, `ffmpeg` | `Auto` (→ ffmpeg) | Determines preference when backends offer equivalent functions‡ |

‡Backend selection notes:
- **Auto**: Defaults to ffmpeg with automatic sox_ng integration for superior dithering
- **Multi-backend pipelines**: Backend selection doesn't prevent using other backends for specific tasks (e.g., DSD→float64 in sox_ng → resample with ffmpeg/soxr → dither with sox_ng)
- **SSRC availability**: SSRC resampler works with any backend via transparent piping: backend decode → SSRC resample → backend output
- **Future**: `rox` backend planned but not yet available

### Format & Container Selection

| Field | Options | Default | Context |
|-------|---------|---------|---------|
| **Format** | See format list | `FLAC` | Determines codec/encoding |
| **Container** | Format-dependent | `Auto` | Auto selects default container for format |

#### Supported Formats

**Lossless PCM**: FLAC, WAV, LPCM, RF64, W64, AIFF, ALAC, WavPack, APE  
**Lossy**: MP3, AAC, Opus, Ogg Vorbis, DTS, AC3  
**DSD**: DSD64, DSD128, DSD256, DSD512, DSD1024

#### Format → Container Mappings

| Format | Available Containers | Default | Notes |
|--------|---------------------|---------|-------|
| **FLAC** | .flac, .ogg, .mka, .mkv | .flac | Native or Matroska |
| **WAV** | .wav, .rf64, .w64, .mka, .mkv | .wav | Various PCM containers |
| **PCM** | .pcm, .aiff, .wav, .mka, .mkv | .pcm | Raw PCM stream |
| **RF64** | .rf64, .mka, .mkv | .rf64 | >4GB WAV extension |
| **W64** | .w64, .mka, .mkv | .w64 | Sony Wave64 |
| **AIFF** | .aiff, .mka, .mkv | .aiff | Apple format |
| **ALAC** | .m4a, .mp4 | .m4a | Apple Lossless |
| **WavPack** | .wv, .mka, .mkv | .wv | Hybrid codec |
| **APE** | .ape, .mka, .mkv | .ape | Monkey's Audio |
| **MP3** | .mp3, .m4a, .mka, .mkv | .mp3 | MPEG Layer 3 |
| **AAC** | .aac, .m4a, .mp4, .m4b, .mka, .mkv | .m4a | Raw AAC or MP4 |
| **Opus** | .opus, .webm, .weba, .mka, .mkv | .opus | Modern codec |
| **Ogg Vorbis** | .ogg, .webm, .weba, .mka, .mkv | .ogg | Xiph codec |
| **DTS** | .dts, .mka, .mkv, .mp4 | .dts | Raw DTS core |
| **AC3** | .ac3, .mka, .mkv, .mp4 | .ac3 | Dolby Digital |

#### DSD Container Support

| DSD Rate | Native Containers | DoP Containers | Notes |
|----------|------------------|----------------|-------|
| **DSD64** | .dsf, .dff, .wv | .flac | WavPack stores DSD natively |
| **DSD128-1024** | .dsf, .dff, .wv | .flac, .wav | FLAC/WAV via sox_ng DoP |

**Container notes**:
- All output filenames use lowercase extensions
- WavPack (.wv) can store DSD streams natively without conversion
- FLAC/WAV can store DSD via DoP (DSD-over-PCM) using sox_ng
- Matroska (.mka/.mkv) is universal container for most formats

## Global Settings (Always Visible)

The Backend, View Mode, Format, and Container settings are configured in the Preset Basics section above.

## Core PCM Settings

| Field | UI Pattern | Options | Default | Mode | Backend | Context | Backend Commands |
|-------|------------|---------|---------|------|---------|---------|------------------|
| **Sample Rate** | Toggle → Menu | `Auto` → `[Filtered by format]`† | `Auto` | All | All | Always | sox: `rate 44100` / ffmpeg: `-ar 44100` |

†Sample rates include standard (8 kHz - 192 kHz), high-res (352.8 kHz - 1.536 MHz), and experimental ultra-high rates (2.8224 MHz - 6.144 MHz)
| **Bit Depth** | Toggle → Menu | `Auto` → `[8-bit \| 16-bit \| 24-bit \| 32-bit \| float32 \| float64]` | `Auto` | All | All | `format in lossless_formats && format != 'DSD'` | sox: `-b 16` / ffmpeg: `-sample_fmt s16` |

## Format-Specific Bit Depth Constraints

| Format | Supported Bit Depths | Notes |
|--------|---------------------|-------|
| **FLAC** | 8-bit to 32-bit signed integer | No floating point |
| **WAV** | 8-bit to float64 | Full range support |
| **RF64** | 8-bit to float64 | Extended WAV for >4GB |
| **W64** | 8-bit to float64 | Sony Wave64 |
| **AIFF** | 8-bit to float64 | Apple format |
| **ALAC** | 8-bit to 32-bit signed integer | Apple Lossless |
| **WavPack** | 8-bit to 32-bit + float32 | Hybrid codec |
| **APE** | 8-bit to 24-bit signed integer | Monkey's Audio |
| **PCM** | 8-bit to float64 | Raw PCM (.pcm) |
| **DSD** | 1-bit only | No bit depth field shown |

**Note**: Bit Depth field is completely hidden for lossy formats: MP3, AAC, Opus, Ogg Vorbis, AC3, DTS

## Bitrate Control (Lossy Formats Only)

**Section Visibility:** `format in ['MP3', 'AAC', 'Opus', 'Ogg Vorbis', 'AC3', 'DTS']`

| Field | Options | Default | Format | Backend Commands |
|-------|---------|---------|--------|------------------|
| **Bitrate Mode** | `CBR`, `VBR (Quality)`, `Custom` | `VBR` | MP3 | See detailed tables |
| **Bitrate Mode** | `CBR`, `VBR (Quality)`, `Custom` | `VBR` | AAC-LC | See detailed tables |
| **Bitrate Mode** | `CBR`, `Custom` | `CBR` | AAC-HE-v1/v2 | See detailed tables |
| **Bitrate Mode** | `VBR`, `CBR`, `Custom` | `VBR` | Opus | Always VBR internally |
| **Bitrate Mode** | `VBR (Quality)`, `CBR` | `VBR` | Ogg Vorbis | See detailed tables |
| **Bitrate Mode** | `CBR` | `CBR` | AC3, DTS | Fixed bitrates only |

### Format-Specific Bitrate Settings

#### MP3 (LAME)
| Mode | Options | Default | FFmpeg Command |
|------|---------|---------|----------------|
| **CBR Presets** | 96, 128, 160, 192, 224, 256, 320 kbps | 128 | `-c:a libmp3lame -b:a {kbps}k` |
| **VBR Quality** | V0 (best) to V9 (smallest) | V2 | `-c:a libmp3lame -q:a {0-9}` |
| **Custom** | 8-320 kbps | 128 | `-c:a libmp3lame -b:a {kbps}k` |

#### AAC Profiles
| Profile | CBR Presets (kbps) | VBR Quality | FFmpeg Command |
|---------|-------------------|-------------|----------------|
| **AAC-LC** | 96, 128, 192, 256, 320 | 1-5 (default: 3) | `-c:a aac -b:a {kbps}k` or `-q:a {q}` |
| **AAC-HE-v1** | 48, 56, 64, 80, 96 | Not supported | `-c:a aac -profile:a aac_he -b:a {kbps}k` |
| **AAC-HE-v2** | 16, 24, 32, 40, 48 | Not supported | `-c:a aac -profile:a aac_he_v2 -b:a {kbps}k` |

#### Opus
| Mode | Range | Default | FFmpeg Command |
|------|-------|---------|----------------|
| **VBR** | 6-510 kbps | 160 | `-c:a libopus -b:a {kbps}k` |
| **CBR** | 6-510 kbps | 160 | `-c:a libopus -b:a {kbps}k -vbr off` |

#### Ogg Vorbis
| Mode | Options | Default | FFmpeg Command |
|------|---------|---------|----------------|
| **VBR Quality** | -1 to 10 | 4 | `-c:a libvorbis -q:a {q}` |
| **CBR Presets** | 96, 128, 160, 192, 256, 320 kbps | 128 | `-c:a libvorbis -b:a {kbps}k` |

#### AC3 & DTS
| Format | CBR Options (kbps) | FFmpeg Command |
|--------|-------------------|----------------|
| **AC3** | 192, 384, 448, 640 | `-c:a ac3 -b:a {kbps}k` |
| **DTS** | 768, 1509 | `-c:a dca -b:a {kbps}k` |

#### Lossless Compression
| Format | Setting | Range | Default | FFmpeg Command |
|--------|---------|-------|---------|----------------|
| **FLAC** | Compression Level | 0-12 | 5 | `-c:a flac -compression_level {level}` |

## Sample Rate Constraints by Format

```toml
[sample_rates]
# Master list - all supported rates
all_rates = [8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000, 705600, 768000, 1411200, 1536000, 2822400, 3072000, 5644800, 6144000]

[sample_rate_constraints]
"WAV" = "all_rates"
"AIFF" = "all_rates"
"RF64" = "all_rates"
"W64" = "all_rates"
"PCM" = "all_rates"
"WavPack" = "all_rates"
"APE" = "all_rates"
"FLAC" = "filter(all_rates, rate <= 384000)"
"ALAC" = "filter(all_rates, rate <= 384000)"
"MP3" = "filter(all_rates, rate <= 48000)"
"AAC" = "filter(all_rates, rate <= 192000)"  # Unofficial support up to 192kHz
"Opus" = "[48000]"  # Forced 48kHz
"WebM" = "[48000]"  # Forced 48kHz
"WebA" = "[48000]"  # Forced 48kHz
"Ogg Vorbis" = "filter(all_rates, rate <= 192000)"
"AC3" = "[48000]"
"DTS" = "[48000, 96000]"
"DSD" = "[2822400, 5644800, 11289600, 22579200]"  # DSD64/128/256/512
```

**Dynamic Format Hiding**: When a sample rate is selected that exceeds a format's maximum, that format is hidden from the output format menu. If switching to a format with a lower maximum rate, the sample rate automatically adjusts to that format's highest supported rate.

## Resampling Settings (Conditional Section)

**Section Visibility:** `(sample_rate == 'Custom' && target_rate != source_rate) || (format in ['Opus', 'WebM', 'WebA'] && source_rate != 48000)`

| Field | Options | Default | Mode | Backend | Context | Backend Commands |
|-------|---------|---------|------|---------|---------|------------------|
| **Forced Resampling Notice** | "Format requires 48 kHz" | - | All | All | `format in ['Opus', 'WebM', 'WebA', 'AC3'] && source_rate != 48000` | Info text only |
| **Resampler** | `sox_ng`, `soxr`, `swr`, `SSRC` | `soxr` | All | All* | Always† | See detailed sections |
| **Resample Quality** | `Fast`, `Normal`, `High`, `Very High`, `Ultra`, `Sinc`, `FIR` | `High`‡ | All | sox_ng | `resampler == 'sox_ng'` | See sox section |
| **Resample Quality** | `Normal`, `High`, `Very High`, `Ultra` | `High`‡ | All | ffmpeg | `resampler == 'soxr'` | Built into soxr |
| **Filter Profile** | `Short`, `Normal`, `Long` | `Normal` | All | SSRC | `resampler == 'SSRC'` | `ssrc --profile {profile}` |

*sox_ng specific to sox_ng backend; soxr/swr specific to ffmpeg backend; SSRC available with any backend via transparent piping  
†Within the resampling section  
‡Default is `Very High` for Opus/WebM/WebA, `High` for others

### Sox_ng Resampling Options

| Field | Options | Default | Mode | Context | Backend Commands |
|-------|---------|---------|------|---------|------------------|
| **Sinc taps** | `16K (2^14)`, `64K (2^16)`, `256K (2^18)`, `1M (2^20)`, `4M (2^22)`, `16M (2^24)`, `32M (2^25)`, `64M (2^26)` | `64K (2^16)` | Expert | `resample_quality == 'Sinc'` | `sinc -n {taps}` |
| **Sinc attenuation** | 80-200 dB | 120 | Expert | `resample_quality == 'Sinc'` | `sinc -a {att}` |
| **Sinc phase** | `Linear`, `Minimum`, `Intermediate` | `Linear` | Expert | `resample_quality == 'Sinc'` | `-L`, `-M`, `-I` |
| **Upsample factor** | `2x`, `4x`, `8x`, `16x`, `32x` | `2x` | Expert | `resample_quality == 'Sinc' && source_rate < 352800` | Applied before filter§ |
| **Kaiser β** | `Auto` or user value | `Auto` | Expert | `resample_quality == 'Sinc'` | `sinc -b {beta}` |
| **FIR taps** | `256 (2^8)` through `16M (2^24)` | `16K (2^14)` | Expert | `resample_quality == 'FIR'` | Determines coefficient file |
| **FIR window** | `Auto`, `Kaiser`, `Remez` | `Kaiser` | Expert | `resample_quality == 'FIR'` | Selects window type |
| **Transition** | Hz value | 99% of Nyquist | Expert | `resample_quality in ['FIR', 'Sinc']` | Transition bandwidth |
| **Stopband** | `Auto`, `126`, `150`, `186`, `210`, `252`¶, `300`¶ dB | `Auto`** | Expert | `resample_quality == 'FIR'` | Stopband attenuation |
| **Chebyshev** | Checkbox | `false` | Expert | Always† | `rate -s` |
| **Bandwidth** | 74-99.7% | 95% | Expert | `!chebyshev` | `rate -b {bw}` |
| **Phase** | 0-100 | 50 | Expert | `!chebyshev` | `rate -p {phase}` |
| **Allow Aliasing** | `Yes`, `No` | `No` | Expert | Always† | `rate -a` (when Yes) |

§Warning displayed for 16x/32x: "Extreme upsampling factor - may require significant processing time"  
  Warning for rates ≥2.8 MHz: "Experimental sample rate - no playback hardware exists"  
  Upsampling disabled for rates ≥6.144 MHz  
¶252/300 dB requires rox backend with 80-bit precision (not yet available)  
**Auto = 126 dB for 16-bit, else 6 dB above target bit depth noise floor

### FFmpeg Resampling Options

| Field | Options | Default | Mode | Backend/Resampler | Context | Backend Commands |
|-------|---------|---------|------|-------------------|---------|------------------|
| **Filter Size** | 16, 24, 32, 48, 64 | 32 | Expert | ffmpeg+swr | Always† | `-af aresample=resampler=swr:filter_size={size}` |
| **Precision** | 16-33 | 28 | Expert | ffmpeg+soxr | Always† | `-af aresample=resampler=soxr:precision={prec}` |
| **Chebyshev** | Checkbox | `false` | Expert | ffmpeg+soxr | Always† | `-af aresample=resampler=soxr:cheby=1` |
| **Bandwidth** | 74-99.7% | 95% | Expert | ffmpeg+soxr/swr | `!chebyshev` | `cutoff=0.{bw}` |
| **Phase** | 0-100 | 50 | Expert | ffmpeg+soxr | `!chebyshev` | `phase_shift={phase}` |
| **Phase Accuracy** | 1-30 | 10 | Expert | ffmpeg+swr | Always† | `phase_shift={acc}` |

### SSRC Resampling Options

**Visibility:** `resampler == 'SSRC'`

| Field | Options | Default | Mode | Backend Commands |
|-------|---------|---------|------|------------------|
| **Filter Profile** | `Short`, `Normal`, `Long` | `Normal` | All | `--profile {profile}` |
| **Two-pass** | Checkbox | `false` | Expert | `--twopass` |
| **Normalize** | Checkbox | `false` | Expert | `--normalize` |
| **Prevent Clipping** | `Off`, `Auto`, `Manual` | `Off` | Expert | See below |
| **Attenuation** | 0 to -99.9 dB | -3 | Expert* | `--att {value}` |

*Visible only when Prevent Clipping = Manual

### SSRC Command Examples

```bash
# Basic resampling with normal profile
ssrc --rate 96000 --profile normal input.wav output.wav

# Two-pass with clipping prevention
ssrc --rate 48000 --twopass --att -3 input.wav output.wav

# With dither and noise shaping (see Dither section)
ssrc --rate 44100 --bits 16 --dither 2 input.wav output.wav
```

## Dither & Noise Shaping (Conditional Section)

**Section Visibility:** `bit_depth == 'Custom' && target_bit_depth < source_bit_depth`

### Default Dither/Noise Shaping Strategy

| Target Bit Depth | Backend | Default Dither | Default Noise Shaping | Implementation |
|-----------------|---------|----------------|---------------------|----------------|
| ≤16-bit | sox_ng | Shibata | (built-in) | Direct sox_ng |
| ≤16-bit | ffmpeg | Shibata* | (built-in)* | Auto hybrid: ffmpeg → sox_ng |
| ≤16-bit | SSRC | TPDF | ATH-based (ID 2) | Direct SSRC |
| >16-bit | sox_ng | TPDF | None | Direct sox_ng |
| >16-bit | ffmpeg | Triangular | None | Direct ffmpeg |
| >16-bit | SSRC | None | None | Pass-through |

*Transparently handled by sox_ng in the pipeline

### Backend-Specific Dither Options

#### Sox_ng Dither
| Field | Options | Default | Mode | Context | Backend Commands |
|-------|---------|---------|------|---------|------------------|
| **Dither Method** | `None`, `TPDF`, `Sloped TPDF`, `Triangular`, `Shaped` | See defaults | All | Always* | `dither -f {method}` |
| **Dither Method** | `Shibata`, `Low-Shibata`, `High-Shibata` | `Shibata`† | All | `target ≤ 16-bit` | `dither -f {method}` |
| **Noise Shaping** | `None`, `F-weighted` | `None` | All | `dither_method == 'TPDF'` | `dither -s` |
| **Noise Shaping** | Various filters‡ | - | Expert | `dither_method == 'TPDF' && target ≤ 16-bit` | `dither -s -t {filter}` |

†For ≤16-bit targets  
‡Shibata, Low-Shibata, High-Shibata, E-weighted, Modified E-weighted, Improved E-weighted, Gesemann, Lipshitz

#### FFmpeg Dither
| Field | Options | Default | Mode | Context | Backend Commands |
|-------|---------|---------|------|---------|------------------|
| **Dither Method** | `None`, `Rectangular`, `Triangular`, `Triangular HP` | `Triangular` | All | `target > 16-bit`§ | `-af adither=dither_method={method}` |
| **Noise Shaping** | `Lipshitz`, `Shibata`, `F-weighted`, `Highpass` | `None` | Expert | `target == 16-bit`§ | `-af adither=noise_shaping={shape}` |
| **Decode HDCD** | Checkbox | `false` | Expert | Always | `-af hdcd` |

§FFmpeg dither only for >16-bit; ≤16-bit uses automatic sox_ng pipeline

#### SSRC Dither & Noise Shaping
| Field | Options | Default | Mode | Backend Commands |
|-------|---------|---------|------|------------------|
| **Dither Method** | `None`, `TPDF` | `TPDF` | All | `--dither 99` (for TPDF) |
| **Noise Shaping** | `None`, `Low-Shibata`, `Normal`, `High-Shibata`, `Extreme/Saturated` | `Normal` | All | See matrix below |
| **PDF Type** | `Rectangular`, `Triangular`, `Two-level` | `Triangular` | Expert | `--pdf {0\|1\|3}` |

### SSRC Noise Shaping Matrix

The available noise shaping options depend on the target sample rate:

| Sample Rate | Available Curves | Recommended Mappings | SSRC Command |
|------------|------------------|---------------------|--------------|
| **44.1 kHz** | A: 0-6, B: 0-6, Legacy: Low/Mid/High | Low→1, Normal→3, High→6 | `--dither {id}` |
| **48 kHz** | A: 0-6, B: 0-6, Legacy: Low/Mid | Low→1, Normal→3, High→6 | `--dither {id}` |
| **88.2/96 kHz** | A: 0-2 | Low→1, Normal→1, High→2 | `--dither {id}` |
| **192 kHz** | A: 0-2 | Low→1, Normal→1, High→2 | `--dither {id}` |
| **≤22.05 kHz** | A: 0-1, Saturated: 9 | Low→1, Normal→1, High/Extreme→9 | `--dither {id}` |

**Note**: Run `ssrc --dither help` to see all available noise shapers for the current configuration.

### Noise Shaping Filter Sample Rate Restrictions (Sox)

| Filter | Supported Sample Rates | Notes |
|--------|----------------------|-------|
| **Lipshitz** | 44.1 kHz only | Classic noise shaping |
| **F-weighted** | 46 kHz | Perceptually weighted |
| **E-weighted** | 46 kHz | Base E-weighted filter |
| **Modified E-weighted** | 46 kHz | Modified variant |
| **Improved E-weighted** | 46 kHz | Improved variant |
| **Gesemann** | 44.1, 48 kHz | Limited to common rates |
| **Shibata** | 8, 11.025, 16, 22.05, 32, 37.8, 44.1, 48 kHz | Wide range support |
| **Low-Shibata** | 44.1, 48 kHz | Gentler shaping |
| **High-Shibata** | 44.1 kHz only | Aggressive shaping |

## Combined Command Examples

```bash
# Sox: High-quality resampling with Shibata dither
sox input.wav -b 16 output.wav rate -v 96000 dither -f Shibata

# Sox: Extreme quality Sinc with 32x upsampling
sox input.wav output.wav sinc -n 67108864 -a 140 rate 96000

# FFmpeg: Resample with soxr, automatic sox_ng dither for 16-bit
ffmpeg -i input.wav -af "aresample=resampler=soxr:precision=33:cutoff=0.95:out_sample_fmt=dbl" -sample_fmt dbl -f wav - | sox - -b 16 output.wav dither -f Shibata

# SSRC: Two-pass with ATH-based noise shaping
ssrc --rate 44100 --bits 16 --twopass --dither 2 --profile long input.wav output.wav

# Opus encoding with forced 48kHz and optimal dithering
ffmpeg -i input.wav -af "aresample=resampler=soxr:cheby=1:out_sample_fmt=dbl" -sample_fmt dbl -ar 48000 -f wav - | sox - -b 16 - dither -f Shibata | ffmpeg -i - -c:a libopus -b:a 160k output.opus

# MP3 VBR encoding with LAME V2
ffmpeg -i input.wav -c:a libmp3lame -q:a 2 output.mp3

# AAC-HE-v2 for low bitrate
ffmpeg -i input.wav -c:a aac -profile:a aac_he_v2 -b:a 32k output.aac

# FLAC with maximum compression
ffmpeg -i input.wav -c:a flac -compression_level 12 output.flac
```

## Automatic Hybrid Processing Pipeline

**This happens automatically when:**
- Backend = ffmpeg
- Target bit depth ≤ 16-bit
- No user configuration required

The system seamlessly executes:
1. **FFmpeg processes in float64**: `-af "aresample=resampler=soxr:out_sample_fmt=dbl"`
2. **Output as float64**: `-sample_fmt dbl -f wav -`
3. **Sox reads float64 and applies dither**: `| sox - -b 16 output.wav dither -f Shibata`

**Result**: Users get sox_ng's superior Shibata dither/noise shaping even when using ffmpeg backend, completely transparently.

## Visibility Matrix

```toml
[visibility_rules]
# Format-based visibility
bit_depth_field = "format in lossless_formats && format != 'DSD'"
bitrate_control = "format in lossy_formats"
compression_level = "format == 'FLAC'"

# Lossless formats
lossless_formats = ["FLAC", "WAV", "RF64", "W64", "AIFF", "ALAC", "WavPack", "APE", "PCM"]
lossy_formats = ["MP3", "AAC", "Opus", "Ogg Vorbis", "WebM", "WebA", "AC3", "DTS"]

# Resampling visibility
resampling_section = "(sample_rate == 'Custom' && target_rate != source_rate) || (format in ['Opus', 'WebM', 'WebA', 'AC3'] && source_rate != required_rate)"
ssrc_options = "resampler == 'SSRC'"
sox_quality_options = "resampler == 'sox_ng' && backend == 'sox_ng'"
soxr_options = "resampler == 'soxr' && backend == 'ffmpeg'"
swr_options = "resampler == 'swr' && backend == 'ffmpeg'"

# Dither visibility
dither_section = "bit_depth == 'Custom' && target_bit_depth < source_bit_depth"
ssrc_noise_shaping = "resampler == 'SSRC' && dither_section"
sox_dither_options = "backend == 'sox_ng' && dither_section"
ffmpeg_dither_direct = "backend == 'ffmpeg' && target_bit_depth > 16 && dither_section"
ffmpeg_auto_sox_dither = "backend == 'ffmpeg' && target_bit_depth <= 16 && dither_section"

# Advanced options (Expert view mode)
sinc_options = "view_mode == 'Expert' && backend == 'sox_ng' && resample_quality == 'Sinc'"
fir_options = "view_mode == 'Expert' && backend == 'sox_ng' && resample_quality == 'FIR'"
ssrc_advanced = "view_mode == 'Expert' && resampler == 'SSRC'"
prevent_clipping_manual = "view_mode == 'Expert' && resampler == 'SSRC' && prevent_clipping == 'Manual'"

# Expert-only sections
gain_normalization_section = "view_mode == 'Expert'"
fade_silence_section = "view_mode == 'Expert'"
channel_operations_section = "view_mode == 'Expert'"

# Sample rate dynamic hiding
hide_format_if_rate_incompatible = "selected_rate > format_max_rate"
auto_adjust_rate_on_format_change = "new_format_max_rate < current_rate"
```

## Future Backend Support (rox - Not Yet Available)

When the rox backend becomes available, it will add:
- **Precision**: `f32`, `f64`, `f80` options
- **Stopband**: Extended to 252/300 dB with 80-bit precision
- Additional resampling algorithms and optimizations

---

# Signal Correction & EQ

## Signal Correction & EQ Options

**Section Visibility:** `view_mode == 'Expert'` (entire section is Expert mode only)

| Field | Type | Options | Default | Backend | Context | Backend Commands |
|-------|------|---------|---------|---------|---------|------------------|
| **Remove DC offset** | Checkbox | - | `false` | All | Always* | sox: `dcshift 0`<br>ffmpeg: See methods below |
| **Invert phase** | Dropdown | `Off`, `Stereo`, `Left`, `Right` | `Off` | All | Always* | See phase inversion |
| **Deemphasis** | Dropdown | Backend-dependent† | - | All | Always* | See deemphasis section |
| **Phono EQ** | Dropdown | Backend-dependent‡ | - | All | Always* | See phono EQ section |
| **Dolby NR** | Checkbox | - | `false` | sox_ng | `backend == 'sox_ng'` | `dolbyb` decoder |

*Within the Expert-only Signal Correction & EQ section  
†CD only for sox_ng; CD/FM(US)/FM(EU) for ffmpeg  
‡RIAA only for sox_ng; RIAA/EMI/Columbia/BSI for ffmpeg

### DC Offset Removal

**Important**: Apply DC offset removal at the start of your processing chain, before gain or dynamics changes.

| Backend | Method | Command |
|---------|--------|---------|
| **sox_ng** | Auto-center | `sox in.wav out.wav dcshift 0` |
| **ffmpeg** | Fixed shift | `ffmpeg -i in.wav -af "dcshift=-0.05" out.wav` |
| **ffmpeg** | Highpass (removes DC + subsonic) | `ffmpeg -i in.wav -af "highpass=f=20" out.wav` |

**FFmpeg notes**: No auto-center flag; must measure first with `astats` or use highpass filter at ~20Hz to remove DC and infrasonic content simultaneously (common in mastering).

### Phase Inversion

| Mode | Sox Command | FFmpeg Command |
|------|-------------|----------------|
| **Stereo** | `sox in.wav out.wav vol -1` | `ffmpeg -i in.wav -af "volume=-1" out.wav` |
| **Left only** | `sox in.wav out.wav remix 1v-1 2` | `ffmpeg -i in.wav -af "pan=stereo\|c0=-1*c0\|c1=c1" out.wav` |
| **Right only** | `sox in.wav out.wav remix 1 2v-1` | `ffmpeg -i in.wav -af "pan=stereo\|c0=c0\|c1=-1*c1" out.wav` |

**Note**: Multiplying amplitude by -1 flips the waveform vertically (180° phase shift).

### Deemphasis

#### Sox_ng (CD only)

| Type | Command | Notes |
|------|---------|-------|
| **CD** | `sox in.wav out.wav deemph` | Red Book CD preemphasis correction |

#### FFmpeg (Multiple standards)

| Type | Command | Standard |
|------|---------|----------|
| **CD** | `ffmpeg -i in.wav -af "aemphasis=mode=reproduction:type=cd" out.wav` | Red Book CD |
| **FM (US)** | `ffmpeg -i in.wav -af "aemphasis=mode=reproduction:type=75fm" out.wav` | 75μs curve |
| **FM (EU)** | `ffmpeg -i in.wav -af "aemphasis=mode=reproduction:type=50fm" out.wav` | 50μs curve |

### Phono EQ

#### Sox_ng (RIAA only)

| Curve | Command | Requirements |
|-------|---------|--------------|
| **RIAA** | `sox in.wav out.wav riaa` | Sample rate: 44.1, 48, 88.2, 96, or 192 kHz |

**Note**: Supports `--plot` global option for visualization.

#### FFmpeg (Multiple curves)

| Curve | Command | Usage |
|-------|---------|-------|
| **RIAA** | `ffmpeg -i in.wav -af "aemphasis=mode=reproduction:type=riaa" out.wav` | Standard LP curve |
| **Columbia** | `ffmpeg -i in.wav -af "aemphasis=mode=reproduction:type=col" out.wav` | Pre-1954 Columbia LPs |
| **EMI** | `ffmpeg -i in.wav -af "aemphasis=mode=production:type=emi" out.wav` | EMI/HMV recordings |
| **BSI** | `ffmpeg -i in.wav -af "aemphasis=mode=reproduction:type=bsi:level_in=2:level_out=0.8" out.wav` | 78 RPM records |

### Dolby NR (Sox_ng Only)

**Visibility:** `backend == 'sox_ng'`

| Field | Options | Default | Command Options |
|-------|---------|---------|-----------------|
| **Dolby B Mode** | `Decode`, `Encode` | `Decode` | `-d` (decode) or `-e` (encode) |
| **Upsample Rate** | `Auto`, `Disabled`, Custom | `Auto` | `-u[rate]` or `-u1` (disable) |
| **Threshold Gain** | 0.5-2.0 | 1.0 | `-t<gain>` |
| **Decode Accuracy** | -10.0 to 0.0 dB | -5.0 | `-a<prec>` |
| **Filter Type** | 1-4 | 4 | `-f{1\|2\|3\|4}` |

**Dolby B command structure:**
```bash
# Basic decode (default)
sox in.wav out.wav dolbyb

# Encode
sox in.wav out.wav dolbyb -e

# Decode with tuned threshold
sox in.wav out.wav dolbyb -d -t1.2

# High-quality decode with filter 4
sox in.wav out.wav dolbyb -d -f4 -a-3.0
```

**Tuning notes:**
- **Threshold (`-t`)**: Tune by ear; too low → dull, too high → bright
- **Filter 4**: Recommended for phase accuracy when recombining signal paths
- **Upsampling**: For best quality, handle externally rather than using `-h`
- **Accuracy**: 0.0 dB = ~1 sample value accuracy (very slow)

### Command Examples

```bash
# Sox: Remove DC offset and apply RIAA curve
sox in.wav out.wav dcshift 0 riaa

# FFmpeg: Complete phono chain with DC removal
ffmpeg -i phono.wav -af "highpass=f=20,aemphasis=mode=reproduction:type=riaa" out.wav

# Sox: Invert left channel phase
sox stereo.wav out.wav remix 1v-1 2

# FFmpeg: CD deemphasis with phase correction
ffmpeg -i cd.wav -af "aemphasis=mode=reproduction:type=cd,volume=-1" out.wav

# Sox: Dolby B decode with custom settings
sox tape.wav decoded.wav dolbyb -d -t1.1 -f4

# FFmpeg: FM broadcast deemphasis (US)
ffmpeg -i fm_recording.wav -af "aemphasis=mode=reproduction:type=75fm" out.wav
```

### Processing Notes

1. **Chain position**: DC offset removal should be first in chain
2. **Deemphasis**: Only apply if source was preemphasized
3. **Phono EQ**: Match curve to record label/era
4. **Dolby NR**: Threshold tuning is critical for proper decoding
5. **Phase inversion**: Useful for correcting polarity issues or creative effects

---

# DSD Settings

## DSD Configuration

**Backend Support:** DSD is only supported with `sox_ng` backend (rox support planned but not yet available)

**Note:** Bit Depth field is hidden for DSD as it's always 1-bit

| Field | Options | Default | Mode | Backend | Context | Backend Commands |
|-------|---------|---------|------|---------|---------|------------------|
| **DSD Rate** | `DSD64`, `DSD128`, `DSD256`, `DSD512`, `DSD1024` | Source rate | All | sox_ng | `format == 'DSD'` | Corresponds to 2.8224/5.6448/11.2896/22.5792/45.1584 MHz |
| **Effective Sample Rate** | Display only | - | All | sox_ng | `format == 'DSD'` | Shows MHz rate dynamically |
| **Modulator** | `Auto`, `SDM`, `CLANS` | `Auto` | Expert | sox_ng | `format == 'DSD'` | SDM or CLANS modulation |
| **Order** | `4th` through `8th` | Rate-dependent* | Expert | sox_ng | `format == 'DSD'` | Modulator order |
| **Trellis Order** | `Auto`, 4-32 | `Auto` | Expert | sox_ng | `modulator == 'CLANS'` | Trellis Viterbi order† |
| **Trellis Paths** | `Auto`, 4-32 | `Auto` | Expert | sox_ng | `modulator == 'CLANS'` | Search paths† |
| **Trellis Latency** | `Auto` or samples | `Auto` | Expert | sox_ng | `modulator == 'CLANS'` | Processing latency |

*Default orders by rate:
- DSD64: 8th order CLANS
- DSD128: 7th order CLANS  
- DSD256: 6th order CLANS
- DSD512: 5th order CLANS
- DSD1024: 4th order SDM

†Warning: Trellis orders >16 can require hours of processing for minutes of audio

## DSD → PCM Conversion

### Lowpass Filtering Method

**Section Visibility:** Converting from DSD to PCM

| Field | Options | Default | Mode | Context | Backend Commands |
|-------|---------|---------|------|---------|------------------|
| **DSD Lowpass** | `Auto`, `High`, `Very High`, `Ultra`, `FIR`, `SSRC Brick-wall` | `Auto`‡ | Expert | DSD→PCM | See detailed options |

‡Auto defaults to `Very High` for rates ≤48kHz, `SSRC Brick-wall` for higher rates

### Brick-wall Frequency Limits by DSD Rate

| Source DSD Rate | Safe Brick-wall Frequency | Reason |
|----------------|--------------------------|--------|
| **DSD64** | ≤24 kHz | Heavy quantization noise above |
| **DSD128** | ≤48 kHz | Noise shaping pushes noise higher |
| **DSD256** | ≤96 kHz | More headroom for content |
| **DSD512** | ≤192 kHz | Substantial clean bandwidth |
| **DSD1024** | ≤384 kHz | Maximum usable bandwidth |

### SSRC Brick-wall Options (DSD → PCM)

**Visibility:** `dsd_lowpass == 'SSRC Brick-wall'`

| Field | Options | Default | Backend Commands |
|-------|---------|---------|------------------|
| **SSRC Profile** | `Long` | `Long` | `--profile long` |
| **Two-pass** | Checkbox | `true` | `--twopass` |
| **Pre-attenuation** | -3 to -12 dB | -6 dB | `--att {value}` |
| **Transition Band** | 100-2000 Hz | 500 Hz | Determines output rate§ |

**Key Point**: DSD is 1-bit. Converting to PCM produces 32-bit representation. SSRC filters this PCM to remove DSD ultrasonic noise.

§SSRC command construction for DSD64→44.1kHz:
```bash
# First stage: Convert 1-bit DSD to 32-bit PCM at high rate
sox input.dsf -t wav -b 32 - rate -v 352800 | \
# Second stage: SSRC brick-wall filtering to remove DSD noise
ssrc --rate 44100 --profile long --twopass --att -6 - output.wav
# Output remains 32-bit PCM unless user explicitly requests bit depth reduction
```

**Note**: DSD is always 1-bit. The conversion to PCM creates a 32-bit intermediate. SSRC filters out ultrasonic DSD noise while maintaining 32-bit depth. Dithering only applies if subsequently reducing to 16/24-bit.

### FIR Options (DSD → PCM)

**Visibility:** `dsd_lowpass == 'FIR'`

| Field | Options | Default | Context | Backend Commands |
|-------|---------|---------|---------|------------------|
| **FIR Taps** | `256 (2^8)` through `16M (2^24)` | `1M (2^20)` | Always | Coefficient file selection |
| **FIR Window** | `Kaiser`, `Remez` | `Kaiser` | Always | Window function |
| **Passband Limit** | Rate-dependent¶ | Rate default | Always | Frequency cutoff |
| **Transition** | 50-2000 Hz | 500 Hz | Always | Transition band |
| **Stopband** | 126-300 dB** | 186 dB | Always | Attenuation |

¶Passband options by DSD rate:
- DSD64: 20k, 22k, 24k Hz
- DSD128: 40k, 44k, 48k Hz  
- DSD256: 88k, 96k Hz
- DSD512: 176k, 192k Hz
- DSD1024: 352k, 384k Hz

**252/300 dB requires rox backend (not yet available)

## PCM → DSD Conversion

### PCM Lowpass (Optional)

**Section Visibility:** `mode == 'Expert' && source_format == 'PCM' && target_format == 'DSD'`

| Field | Options | Default | Backend | Context | Backend Commands |
|-------|---------|---------|---------|---------|------------------|
| **Enable PCM Lowpass** | Checkbox | `false` | All | PCM→DSD | Activates filtering |
| **Lowpass Method** | `soxr`, `FIR`, `Sinc`, `SSRC` | `soxr` | All** | `enable_pcm_lowpass` | See sections |

**If backend is ffmpeg, automatic pipeline: ffmpeg (float64) → sox_ng for DSD conversion

### SSRC Options (PCM → DSD)

**Visibility:** `lowpass_method == 'SSRC'`

| Field | Options | Default | Notes |
|-------|---------|---------|-------|
| **Profile** | `Normal`, `Long` | `Long` | Steep filtering preferred |
| **Target Bandwidth** | Based on target DSD | Auto†† | Prevents aliasing |

††Auto bandwidth by target DSD rate:
- →DSD64: 22 kHz
- →DSD128: 44 kHz
- →DSD256: 88 kHz
- →DSD512: 176 kHz
- →DSD1024: 352 kHz

## DoP (DSD-over-PCM) Encoding

| Field | Options | Default | Mode | Context | Backend Commands |
|-------|---------|---------|------|---------|------------------|
| **DoP Mode** | `Off`, `Same as source`, `DSD64` through `DSD1024` | `Off` | All | DSD output | See mapping |

### DoP PCM Rate Mapping

| DSD Rate | PCM Container Rate | Sox Command |
|----------|-------------------|-------------|
| **DSD64** | 176.4 kHz | `sox input.dsf output.wav dop rate 176400` |
| **DSD128** | 352.8 kHz | `sox input.dsf output.wav dop rate 352800` |
| **DSD256** | 705.6 kHz | `sox input.dsf output.wav dop rate 705600` |
| **DSD512** | 1411.2 kHz | `sox input.dsf output.wav dop rate 1411200` |
| **DSD1024** | 2822.4 kHz | `sox input.dsf output.wav dop rate 2822400` |

### DoP Rate Conversion Options

**Visibility:** `dop_mode != 'Off' && dop_mode != 'Same as source'`

| Field | Options | Default | Context | Backend Commands |
|-------|---------|---------|---------|------------------|
| **Resample Quality** | `High`, `Very High`, `Ultra`, `FIR`, `SSRC` | `Ultra` | Rate change | Quality selector |
| **Bandwidth** | 53-100% | 95% | `quality != 'FIR' && quality != 'SSRC'` | `-b {bw}` |
| **Transition** | 95-99.77% of Nyquist | 95% | `quality != 'FIR' && quality != 'SSRC'` | Rolloff |

### DSD Rate Change with SSRC

When using SSRC for DSD rate changes (e.g., DSD256→DSD64):

```bash
# Stage 1: DSD to high-rate PCM
sox input_dsd256.dsf -t wav -b 32 - rate -v 2822400 | \
# Stage 2: SSRC brick-wall (96kHz for DSD256→24kHz for DSD64)
ssrc --rate 352800 --profile long --twopass --att -6 - - | \
# Stage 3: Convert back to DSD64
sox - output_dsd64.dsf
```

## Processing Pipeline Examples

### DSD64 → 16-bit/44.1kHz PCM (Optimal Quality)

```bash
# DSD is 1-bit; conversion to PCM creates 32-bit intermediate
# Stage 1: Convert 1-bit DSD to 32-bit PCM at high rate
sox input.dsf -t wav -b 32 -r 352800 - | \
# Stage 2: SSRC brick-wall filtering removes DSD ultrasonic noise
ssrc --rate 44100 --profile long --twopass --att -6 - - | \
# Stage 3: Only if user wants 16-bit output, apply dithering
sox - -b 16 output.wav dither -f Shibata
```

### 24-bit/96kHz PCM → DSD128 (With Pre-filtering)

```bash
# Using SSRC for PCM lowpass
ssrc --rate 96000 --profile long --att -3 input.wav - | \
sox - -r 5644800 output.dsf
```

### DSD256 → DSD64 (Rate Conversion)

```bash
# Multi-stage with SSRC brick-wall
sox input_dsd256.dsf -t wav -b 32 - rate -v 2822400 | \
ssrc --rate 352800 --profile long --twopass - - | \
sox - output_dsd64.dsf
```

---

# Gain & Normalization Settings

## Gain & Normalization Options

**Section Visibility:** `view_mode == 'Expert'` (entire section is Expert mode only)

| Field | UI Type | Options | Default | Mode | Backend | Context | Backend Commands | Chain Order |
|-------|---------|---------|---------|------|---------|---------|------------------|-------------|
| **ReplayGain** | Dropdown | `Off`, `Track`, `Album` | `Off` | Expert | ffmpeg | Always* | `-af "volume=replaygain=track"` or `replaygain=album` | First |
| **Processing precision** | Dropdown | `Auto`, `Float`, `Double` | `Auto` | Expert | ffmpeg | Always* | `-af "volume=1:precision=double"` | With gain |
| **Loudness normalization** | Checkbox | - | `false` | Expert | ffmpeg | Always* | `-af "loudnorm=I=-16:TP=-1:LRA=9"` | Last (before output) |
| **Dynamic normalization** | Checkbox | - | `false` | Expert | ffmpeg | Always* | `-af "dynaudnorm=peak=1:maxgain=5"` | Before limiter |
| **Channel balance** | Number (-1 to 1) | - | `0` | Expert | sox_ng | Always* | `gain -b <value>` | Early |
| **Peak normalization** | Number (dB) | - | `Off` | Expert | All | Always* | sox: `gain -n` or `norm -1`<br>ffmpeg: via `volumedetect` + `volume` | Last |
| **Headroom limiting** | Checkbox | - | `false` | Expert | sox_ng | Always* | `gain -h` or `gain -l` | Before normalization |

*Within the Expert-only Gain & Normalization section

### Advanced Normalization Options

**Visibility:** `mode == 'Expert' && (loudness_normalization == true || dynamic_normalization == true)`

| Field | Options | Default | Backend | Context | Backend Commands |
|-------|---------|---------|---------|---------|------------------|
| **Target loudness (LUFS)** | `-30` to `0` | `-16` | ffmpeg | `loudness_normalization` | `I=<value>` |
| **True peak (dB)** | `-9` to `0` | `-1` | ffmpeg | `loudness_normalization` | `TP=<value>` |
| **Loudness range (LU)** | `1` to `50` | `9` | ffmpeg | `loudness_normalization` | `LRA=<value>` |
| **Max gain (dB)** | `0` to `30` | `5` | ffmpeg | `dynamic_normalization` | `maxgain=<value>` |
| **Target peak** | `0` to `1` | `1` | ffmpeg | `dynamic_normalization` | `peak=<value>` |

---

# Fade & Silence Settings

## Fade & Silence Options

**Section Visibility:** `mode == 'Expert'` (entire section is Expert mode only)

| Field | UI Type | Default | Mode | Backend | Context | Backend Commands | Notes |
|-------|---------|---------|------|---------|---------|------------------|-------|
| **Fade in** | Number (seconds) | `0` | Expert | All | Always* | sox: `fade t <fade-in>`<br>ffmpeg: `afade=t=in:ss=0:d=<fade-in>` | Duration of fade-in from silence |
| **Fade out** | Number (seconds) | `0` | Expert | All | Always* | sox: `fade t 0 <duration> <fade-out>`<br>ffmpeg: `afade=t=out:st=<start>:d=<fade-out>` | sox: stop-time=0 for auto |
| **Silence padding start** | Number (seconds) | `0` | Expert | All | Always* | sox: `pad <start> 0`<br>ffmpeg: `adelay=<ms>\|<ms>` | ffmpeg: milliseconds per channel |
| **Silence padding end** | Number (seconds) | `0` | Expert | All | Always* | sox: `pad 0 <end>`<br>ffmpeg: `apad=pad_dur=<duration>` | Adds silence at end |
| **Remove silence** | Checkbox | `false` | Expert | All | Always* | See advanced options | Removes leading/trailing silence |

*Within the Expert-only Fade & Silence section

### Remove Silence Advanced Options

**Visibility:** `mode == 'Expert' && remove_silence == true`

| Field | Options | Default | Backend | Backend Commands |
|-------|---------|---------|---------|------------------|
| **Start threshold** | `-60dB` to `0dB` | `-50dB` | All | sox: `1%` / ffmpeg: `-50dB` |
| **Start duration** | `0.01` to `1.0` sec | `0.1` | All | sox: `0.1` / ffmpeg: `start_duration=0.1` |
| **End threshold** | `-60dB` to `0dB` | `-50dB` | All | sox: `1%` / ffmpeg: `-50dB` |
| **End duration** | `0.01` to `1.0` sec | `0.1` | All | sox: `0.1` / ffmpeg: `stop_duration=0.1` |

---

# Channel Operations

## Channel Operations Options

**Section Visibility:** `mode == 'Expert'` (entire section is Expert mode only)

| Field | UI Type | Options | Default | Mode | Backend | Context | Backend Commands | Notes |
|-------|---------|---------|---------|------|---------|---------|------------------|-------|
| **Channel layout** | Dropdown | `Auto`, `Mono`, `Stereo`, `5.1`, `7.1`, `Custom` | `Auto` | Expert | All | Always* | ffmpeg: `-ac N` or `-channel_layout`<br>sox: `remix ...` | Arbitrary channel mapping |
| **Stereo reverse** | Checkbox | - | `false` | Expert | All | `source_channels == 2`* | ffmpeg: `-af "pan=stereo\|c0=c1\|c1=c0"`<br>sox: `remix 2 1` | Swaps L/R |
| **Stereo fold-down** | Checkbox | - | `false` | Expert | All | `source_channels == 2`* | ffmpeg: `-ac 1` or `-af "pan=mono\|c0=0.5*c0+0.5*c1"`<br>sox: `remix -m` | To mono |

*Within the Expert-only Channel Operations section

### Custom Channel Mapping

**Visibility:** `mode == 'Expert' && channel_layout == 'Custom'`

| Field | UI Type | Default | Backend | Backend Commands | Notes |
|-------|---------|---------|---------|------------------|-------|
| **Channel map** | Text field | - | All | ffmpeg: `pan` filter<br>sox: `remix` spec | Free-form channel routing |
