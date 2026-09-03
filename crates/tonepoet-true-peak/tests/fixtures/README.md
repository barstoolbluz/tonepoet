# Real-program true-peak reference fixture

`real_reference_48k_stereo.f64le` is a one-second excerpt of a genuine saxophone recording. It is checked in as already-decoded interleaved little-endian Float64 PCM so the ordinary Rust tests need no decoder, network access, Python, FFmpeg, SoX, libebur128, or libsoxr at test time.

## Provenance and redistribution

The source is `gradio/media_assets/audio/sax.wav` from the Gradio project (Gradio 6.5.1 as present in the qualification environment):

- upstream project: `https://github.com/gradio-app/gradio`
- upstream asset path: `gradio/media_assets/audio/sax.wav`
- upstream `sax.wav` SHA-256: `12ee32c66257e1c98ed0f2f7b708a1eab638ec09f4c69dda3ec1d78047a7be4d`
- upstream format: 48 kHz, mono, 16-bit PCM WAV, 16.000 seconds
- project license: Apache License 2.0

The Apache-2.0 text distributed with Gradio is preserved next to the fixture as `LICENSE.gradio-apache-2.0.txt`. This fixture is a modified/derived excerpt: it is trimmed and its mono samples are duplicated into two channels, but the audio samples are otherwise unchanged.

## Exact fixture derivation

The fixture contains source frames 384000 through 431999 (8.000 through 9.000 seconds). In the environment that froze the references, FFmpeg 7.1.5 was run as:

```sh
ffmpeg -v error -y \
  -i sax.wav \
  -ss 8 -t 1 -map 0:a:0 \
  -af 'pan=stereo|c0=c0|c1=c0' \
  -c:a pcm_f64le -f f64le \
  real_reference_48k_stereo.f64le
```

Because the source is already 48 kHz, there is no sample-rate conversion in this derivation. The resulting fixture has exactly 48,000 stereo frames / 96,000 `f64` samples / 768,000 bytes. Its SHA-256 is:

`b6ba8b041ebd87543f04f92267487937128acc9905fc743323567682ef77fd20`

A direct comparison in the qualification environment established that each output channel equals the corresponding source `i16` sample divided by 32768.0, with no sample changes beyond mono-to-stereo duplication.

## Frozen independent true-peak references

The reference values below were measured from the exact checked-in `.f64le` bytes before check-in. They are data for the Rust regression test; none of the reference programs or libraries runs during `cargo test`.

### Reporting4x

libebur128 1.2.6 was fed the interleaved Float64 frames via `ebur128_add_frames_double`, then queried with `ebur128_true_peak` for each channel. The frozen overall result is:

`-0.10816109978105748 dBTP`

The Rust regression tolerance is `0.01 dB`, matching the crate's existing independent Reporting4x compatibility threshold for this real-material check.

### Headroom64x independent observation

FFmpeg 7.1.5 with libsoxr 0.1.3 resampled the exact fixture to 256x (12,288,000 Hz) using `resampler=soxr:precision=33:cheby=1:cutoff=1.0` and Float64 output. The maximum absolute Float64 sample was measured outside the meter implementation:

- linear peak: `0.9871581392761376`
- frozen reference: `-0.11226538604726247 dBTP`

The Rust regression uses the historical `0.10 dB` independent-reference observation tolerance. This real-material comparison is an independent anomaly/regression check, not mathematical proof of the published `HEADROOM64X_MAX_UNDERREAD_DB = 0.030` authority reserve. The analytical in-domain tests remain the stronger evidence for that bound.
