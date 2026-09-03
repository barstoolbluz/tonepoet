# DSD album NormalizePeak: finite true-peak ceiling authority

## Scope

This note documents album-scoped native DSD `NormalizePeak`. It does not alter the separately certified DSD Reference contract, `headroom64x_authority()`, or Reporting4x.

A persisted target of exactly `0.000000000` remains valid. The persisted target is the user's requested ceiling; Tonepoet derives a separate fixed runtime gain and never rewrites the setting to an internal reserve.

## The waveform governed by the ceiling

The signal-domain input is the retained headerless little-endian Float64 PCM carrier produced by the ordinary DSD reconstruction path at the governed terminal PCM sample rate. For lossless output this is the requested final PCM rate. For lossy hard-ceiling output it must also be a sample rate accepted directly by Tonepoet's configured FFmpeg encoder; unsupported rate/encoder pairs are rejected before the retained carrier is constructed. Channels are independent; the ceiling is the maximum absolute reconstructed amplitude over all channels.

For each channel Tonepoet:

1. applies the existing Headroom64x finite-stream `RepeatEndpoints` edge convention;
2. evaluates the same six-stage 2x interpolation cascade used by Headroom64x, using the checked-in 384-tap first-stage coefficient set and the existing later Blackman stages;
3. does **not** apply the Headroom64x point estimator's `0.9995395890030878` calibration to the ceiling reconstruction;
4. over the nominal interval from the first stored frame through the last, defines the continuous waveform by straight-line interpolation between adjacent 64x reconstruction knots.

A linear segment's absolute maximum is attained at an endpoint. Therefore the real-valued peak of this declared finite waveform is the maximum 64x knot magnitude, plus the explicit binary64 evaluation enclosure. This is a deliberately finite and auditable reconstruction convention. It is not a claim about an unspecified DAC, ideal infinite sinc reconstruction, or decoded output from a lossy codec.

The maximum absolute polyphase coefficient sum of the complete uncalibrated reconstruction is independently derived as `4.089899431660599`; production uses the deliberately widened upper `4.09`, leaving `0.000100568339401` absolute (`~2.46e-5` relative) margin above the independently derived value so last-bit platform variation in the later Blackman-stage `sin`/`cos` construction cannot rest the bound on a one-ULP accident. This operator norm converts deterministic sample-domain terminal error into a reconstructed-waveform bound, including `RepeatEndpoints` edges.

## Measurement and proof are separate

`Headroom64x` remains the public high-accuracy point estimator and keeps its existing calibration and published accuracy contract. Album scanning now obtains two deliberately distinct values in one pass:

- the unchanged calibrated Headroom64x point estimate, retained for reporting;
- an uncalibrated finite-reconstruction upper value used only for hard-ceiling arithmetic.

The latter receives a `1e-11 * decoded_sample_peak` binary64 evaluation allowance. An independent qualification implementation derives a pessimistic floating-error budget below `3.4e-12 * decoded_sample_peak`, so the frozen allowance remains conservative.

No `<= 0.495 * Fs` property is invented. The production DSD caller does not call `headroom64x_authority()` and does not consume `HEADROOM64X_MAX_UNDERREAD_DB`. The standalone band-qualified API remains available for callers that actually know their spectral support.

## Linear hard-ceiling arithmetic

For each participating track/output pair Tonepoet evaluates

`G * (P_signal + E_pre) + E_post <= C`

where:

- `C` is the exact requested `DbNano` ceiling converted to a conservative linear lower bound;
- `P_signal` is that carrier's finite reconstruction upper bound;
- `E_pre` is any reconstructed realization error introduced before the fixed gain;
- `E_post` is the deterministic reconstructed error introduced after the fixed gain;
- `G` is the permitted linear gain.

Thus

`G_max = (C - E_post) / (P_signal + E_pre)`

with every safety-direction arithmetic step rounded outward. Each track is paired with the terminal bound of the output that will actually consume it. The one shared album gain is the minimum permitted `G_max` over participants, which is both deterministic and tighter than combining the loudest signal from one track with the worst terminal error from an unrelated output.

Conversion from the permitted linear gain to `DbNano` is directional. `log10()` supplies only an initial integer-nanodecibel seed; a directed interval implementation of `10^(dB/20)` proves the final candidate does not exceed `G_max`, walking downward if necessary. A 16-nanodecibel realization guard sits below the mathematical boundary before that proof check. At unity this is about `1.842e-9` in linear amplitude, over eight million binary64 epsilons; it is reserved specifically for the pinned SoX/FFmpeg decimal-parse and gain-realization layer rather than being folded into the signal estimate.

All-silent submitted scope still receives exactly `0 dB` gain. Positive gain remains possible for quiet material, attenuation remains possible for hot material, above-full-scale Float64 input is not clamped, and the loudest participating track still controls the shared result.

## Terminal realization

The retained SoX Float64 carrier is an exact power-of-two representation of SoX's signed Q1.31 sample state and reads back into that state exactly, so the production carrier contributes no pre-gain conversion term.

For a SoX terminal gain, the deterministic gain-realization term is `2^-32 + 2^-51` full scale: one Q1.31 nearest rounding plus the frozen binary64 coefficient/arithmetic allowance already source-audited for the pinned SoX-ng implementation.

Stored-sample and reconstructed-waveform bounds are separate:

- floating output charges only the floating realization that actually occurs;
- undithered integer output charges the relevant nearest-rounding half-LSB (with Int32 avoiding a second quantizer on the SoX route);
- TPDF/sloped-TPDF uses a `1.5`-target-LSB deterministic stored-sample support bound (one LSB of bounded triangular dither plus one half-LSB nearest rounding);
- classic FIR shapers use upward integer ceilings of `1.5 * (1 + sum(abs(c)))` target LSBs from the pinned SoX-ng recurrence;
- Gesemann uses a separately bounded stable four-state IIR recurrence and a 22-LSB stored-sample ceiling;
- SoX's first-within-5%-of-design-rate selection is mirrored exactly; when no named filter matches, the bound falls back to the `1.5`-LSB TPDF behavior rather than charging an inapplicable 44.1/48-kHz shaper.

For the final `RepeatEndpoints` reconstructed waveform, the production bound multiplies the worst deterministic stored-sample terminal error by the complete reconstruction L-infinity upper `4.09`. A tighter interior LTI shaper/reconstruction convolution was investigated, but it is intentionally **not** used as authority because repeating the finite endpoint error outside the stream changes the boundary operator. An edge-aware proof would be required before that tightening could be promoted.

When a dithered integer lossless output can be written by either FFmpeg or SoX, hard-ceiling processing is routed to SoX so the implementation that realizes the samples matches the proved dither recurrence. FFmpeg album gain is explicitly requested with `precision=double`.

## Lossy outputs

For MP3/AAC/Opus and other lossy targets, NormalizePeak governs the PCM presented to the encoder. The retained carrier rate, fixed-gain domain, and encoder-input PCM rate are required to be identical. Tonepoet admits only rates accepted directly by the configured FFmpeg encoder, pins that same rate explicitly on the final lossy command even when gain was realized in a preceding PCM step, and fails closed if the rate pin is missing, mismatched, or unsupported. This prevents FFmpeg from inserting a sample-rate conversion after the proved gain. In particular, `libfdk_aac` hard-ceiling output at 192 kHz is rejected; 96 kHz is admitted and pinned.

This contract does **not** promise that decoded codec output remains below the same true-peak ceiling; a lossy codec can create new overshoots. Tonepoet does not expand this work into decoded-codec normalization.

## Production topology

Each selected DSD track is reconstructed once into its retained Float64 carrier. Independent track preparation is already fanned out. External DSD reconstruction is bounded by the repository's existing per-tool semaphores; after each retained carrier is produced, that carrier is scanned exactly once, sequentially and with bounded memory, in a Tokio blocking worker. The pre-existing preparation futures can overlap those scans, but this work adds no second analysis pool or new unbounded fan-out. There is no second DSD reconstruction.

The submitted-batch barrier aggregates the completed per-track measurements, derives one fixed gain, binds it to every participating DSD output, and preserves that already-resolved gain through scratch retry/rerun. DSD-free companion handling remains unchanged.

## Qualification and regression protection

Ordinary Rust regressions cover the finite reconstruction values, analytical and real-material cross-checks, silence, edges, chunk boundaries, multichannel maxima, above-full-scale samples, zero-ceiling arithmetic, directional `DbNano` conversion, terminal domains, dither rate selection, and the mutation in which hard-ceiling arithmetic is replaced by naïve `target - point` subtraction.

`crates/tonepoet-true-peak/qualification/verify_ceiling_contract.py` independently reconstructs the complete 64x filter from the checked-in coefficient values and freezes the reconstruction/operator references used by Rust tests.

`tonepoet-pipeline/qualification/verify_album_ceiling_terminal_bounds.py` independently derives the pinned SoX-ng FIR/IIR stored-sample support constants and rate-selection behavior. Its additional interior-LTI reconstruction numbers are diagnostic only; production deliberately keeps the `RepeatEndpoints` edge-safe norm bound described above.

Neither script is invoked by Cargo, `build.rs`, `flake.nix`, or runtime code. There is no commissioning stamp, executable/profile gate, source fingerprint, runtime warning/error, real-DSD commissioning corpus, or restored R7/R8/R9 authority machinery.
