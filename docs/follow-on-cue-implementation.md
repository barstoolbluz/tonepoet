We need a follow-on implementation and audit for multi-format CUE image decomposition.

Current state:

* The CUE materializer can discover image files with extensions including `flac`, `wav`, `wave`, `aiff`, `aif`, `aifc`, `wv`, `mp3`, `m4a`, `mp4`, `aac`, `opus`, `ogg`, `ape`, `w64`, and `rf64`.
* The planner already models output formats including FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus, and ALAC.
* The metadata writer already has format-specific paths for:

  * FLAC via `metaflac`
  * Opus/Ogg via `opustags`
  * WavPack via `wvtag`
  * MP3/M4A/AAC/WAV/AIFF via ffmpeg metadata rewrite
* However, CUE `ImageSegment` realization currently hardcodes segment output audio to FLAC with ffmpeg `-c:a flac`.

Goal:
Add or validate support for CUE image decomposition where the realized track segments and final metadata path work correctly for:

* WAV
* WavPack
* Opus
* AAC
* MP3
* ALAC

First, determine the current intended architecture:

1. Does `ImageSegment` realization always create a temporary/intermediate FLAC, with a later planner stage converting to the requested final output format?
2. Or should `ImageSegment` realization directly emit the requested final target format?
3. Identify the format value available at the point where `cut_segment_ffmpeg_args()` is constructed. If the target output format is not available there, propose the smallest API change needed to pass it in without breaking existing callers.

Implementation requirements:

1. Do not regress the current CUE→FLAC path.
2. Preserve the current typed metadata effects model:

   * planner-level source tag/artwork transfer must not satisfy authoritative materializer metadata
   * materializer CUE metadata must still be written by the metadata stage
   * repeated runs must converge rather than duplicate managed tags
3. Make format handling explicit:

   * FLAC: current behavior can remain as the default/lossless baseline.
   * WAV: emit PCM WAV only if WAV is the requested final target; document and test metadata/artwork limitations.
   * WavPack: use an ffmpeg/WavPack path that produces `.wv`, then verify `wvtag` can write materializer tags idempotently.
   * Opus: use an Ogg Opus output path compatible with `opustags`; verify Vorbis comment names and artwork behavior.
   * AAC: distinguish raw `.aac` from `.m4a`/MP4 container behavior. Prefer M4A for metadata/artwork if that is how the rest of the pipeline represents AAC outputs.
   * MP3: produce MP3 only when lossy final output is requested; verify ffmpeg metadata rewrite produces stable ID3 tags.
   * ALAC: produce ALAC in an M4A/MP4 container, not raw AAC; verify ffmpeg can write metadata and preserve artwork as intended.
4. Avoid assuming `-map_metadata 0` and `-map 0:v? -c:v copy` work identically across all target containers. Validate per format.
5. Preserve or deliberately reapply:

   * album title
   * album artist
   * track title
   * track artist
   * performer
   * date/year
   * genre
   * track number
   * total tracks
   * disc number / total discs where available
   * ISRC
   * catalog
   * embedded artwork, where the target format/container supports it

Required tests:

1. Unit tests for ffmpeg argument generation for each target format.
2. Planner/bridge tests showing CUE `ImageSegment` tracks keep the correct metadata obligations.
3. Metadata writer idempotency tests for each supported tag writer path.
4. Fixture-level integration tests, or the closest available harness, for:

   * bare WAV + CUE → FLAC
   * bare WAV + CUE → WAV
   * tagged FLAC + CUE → WavPack
   * tagged FLAC + CUE → Opus
   * tagged FLAC + CUE → AAC/M4A
   * tagged FLAC + CUE → MP3
   * tagged FLAC + CUE → ALAC/M4A
   * conflicting image-level ARTIST vs CUE PERFORMER
   * CUE with INDEX 00 pregap
   * selected track ranges
5. Inspect final outputs with appropriate tools:

   * `metaflac` or `ffprobe` for FLAC
   * `wvtag` or `ffprobe` for WavPack
   * `opustags` or `ffprobe` for Opus
   * `ffprobe`/ID3-aware tooling for MP3
   * `ffprobe`/MP4-aware tooling for AAC and ALAC
   * `ffprobe` for WAV, with clear documentation of tag/artwork limitations

Deliverables:

1. Code changes.
2. Tests.
3. A short architecture note explaining whether CUE segments are intermediate FLACs or target-format outputs.
4. A compatibility matrix listing, for each format, audio codec, container, metadata writer, artwork support, and known limitations.
5. A final audit statement saying exactly which formats are fully supported, partially supported, or intentionally unsupported.
