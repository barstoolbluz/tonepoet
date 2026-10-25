# PX-A — independent range adapters for additional lossless CUE inputs (PROVISIONAL)

**Do not implement on the original source based on this outline.** Requires accepted IP-05 and evidence that initial FLAC pilot actually helps total wall time.

1. **WAV/RF64:** use verified canonical source parsing and byte-frame offsets, including RF64 ds64, WaveFormatExtensible valid bits and metadata. The current `src/convert/pipeline/materializer_cue.rs:107-281` preflight is **not** a complete, qualified range decoder. First prove sample-boundary/metadata behavior against existing S1 for 16/24-bit, >4-GiB simulated and real files, then add a one-format adapter under existing admission; no hand-rolled general WAV codec.
2. **Wave64/AIFF/AIFC:** separate typed endian/chunk/float and sample-count qualification; do not transfer RF64 proof. Exact frame authority and speaker order must be verified.
3. **WavPack:** source deliberately excluded from descriptive stream path (`materializer_cue.rs:604-690`); qualify maintained `wvunpack` range and `.wvc` / hybrid / float sample classes, and compare with whole-image fallback; do not remove fallback first.
4. **ALAC, APE, TTA, Shorten:** identify actual CUE candidate acceptance, codecs and demuxed sample timebase. Require independent pre-roll/codec priming and exact PCM identity. Reject unsupported/unknown source variants.

**Promotion:** one packet per adequately scoped subtype/tool API after real fixtures and sample-exactness tests; update `04_FORMAT_CAPABILITY_MATRIX.md` and `CONFORMANCE_MATRIX.md`. No unsupported 'works in FFmpeg' inference.
