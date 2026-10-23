# PX-B — parallel encoding to additional target formats (PROVISIONAL)

**Dependency:** accepted sample-exact FLAC input and integrated album lifecycle IP-05. **Never treat new output encoding as an input seek problem.**

One packet per planner-equivalent output family: (a) uncompressed WAV/RF64/AIFF; (b) WavPack and ALAC (respect their source-depth/floating class rules); (c) MP3/AAC/Vorbis/Opus lossily encoded outputs (encoder padding/delay, native ReplayGain applicability and clipping policy). Inspect current `src/convert/pipeline/track_executor.rs:999-1539`, the typed planner, chosen external command and available `ToolRunner` pipeline. Require correct float/int carrier, resampler/dither application, per-member metadata and artwork, exact destination duration expectation and checksum/reference semantics appropriate to lossy output. Preserve current planned DSP, verify content/metadata and safe publication. Output encoders must be globally process-budgeted even if input seek is FLAC-qualified.

**Promotion:** specific format+profile command shape, actual tool version/platform/fixtures, failure handling and total-wall benchmark; do not promise universal output support.
