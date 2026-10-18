# Resumption checkpoint — DVD-Audio assignment 18

- Current source is the complete previous DVD-A stereo-fold corrected repository plus the narrow 2026-10-18 assignment-18 fix.
- Original baseline archive SHA-256: `203589445803fbde82b7b0c1d1c8d74ea3a9b3f72f317abf53520b03b6f3d1c1`.
- Changed implementation files: `src/convert/pipeline/dvda_channel_layout.rs`, `src/convert/pipeline/dvda_realize.rs`, `src/convert/pipeline/stages.rs`.
- Changed focused physical test: `validation/dvda_stereo_fold_corrective.py`.
- New reports: `DVDA_ASSIGNMENT18_DELIVERY_2026-10-18.md`, this checkpoint.
- Alias for code 18 now `0x3b` (not `4.1`) in wave-ordered mode. FFprobe describes the mask as `5 channels (FL+FR+LFE+BL+BR)`; the existing WAV validator normalizes it. Old cached `4.1` layouts are invalidated via preexisting `dvda_wav_is_ready()` quarantine/regenerate behavior. Source-order default unchanged.
- Completed: 6 DVD-Audio physical decoded-audio checks, 14 speaker-layout checks, 13 channel/DC checks, 18 independent code-18 LPCM reference vectors; all PASS. Reproducer covers three code18 isolated speakers, wrong versus corrected output, old versus corrected WAV mask, SoX Float64 intermediate, repeated cached carrier consumption, channel-preserving original order.
- Outstanding: Rust toolchain unavailable in the execution environment; Rust compile, workspace gate, integrated DVD-Audio real-disc conversion and release qualification unrun. Next engineer: run commands and media acceptance tests in the delivery report; correct only a demonstrated failure. Avoid planner/DSP rewrites.
- To replay offline checks: `python3 validation/dvda_stereo_fold_corrective.py && python3 validation/channel_layout_binding_corrective.py && python3 validation/channel_dc_smoke.py` from repo root. Requires `ffmpeg`, `ffprobe`, `sox`, Python 3.
- Complete full-source archive and code patch are separately delivered with SHA-256 manifest. The patch applies to precisely the baseline archive identified above.
