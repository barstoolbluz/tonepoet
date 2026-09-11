# DSD album NormalizePeak: finite true-peak ceiling authority

## Scope

This note documents album-scoped native DSD `NormalizePeak`. It does not alter the separately certified DSD Reference contract, `headroom64x_authority()`, or Reporting4x.

A persisted target of exactly `0.000000000` remains valid. The persisted target is the user's requested ceiling; Tonepoet derives a separate fixed runtime gain and never rewrites the setting to an internal reserve.

## The waveform governed by the ceiling

The signal-domain input is the retained headerless little-endian Float64 PCM carrier produced by the ordinary DSD reconstruction path at the governed terminal PCM sample rate. For lossless output this is the requested final PCM rate. For lossy hard-ceiling output it must also be a sample rate accepted directly by Tonepoet's configured FFmpeg encoder; unsupported rate/encoder pairs are rejected before the retained carrier is constructed. Channels are independent; the ceiling is the maximum absolute reconstructed amplitude over all channels.

Tonepoet scans that carrier with the fixed `tonepoet-true-peak` dependency's public `CertifiedPeakMeter`, `EdgePolicy::RepeatEndpoints`, and the selected `PeakTier`. All three public tiers (`Reference`, `Standard`, and `Fast`) certify the same HQ1024V1 reconstruction. The tier changes search work and interval tightness; it does not select a different ceiling reconstruction or a different safety allowance.

The ceiling signal term is `PeakCertificate::finite_interval.upper_linear` (exposed by `PeakCertificate::upper_level()`), not the reported point estimate and not a per-tier under-read constant. The certificate contract guarantees that this upper does not under-read the declared true peak. The reported point estimate remains available separately for logs/UI.

The complete HQ1024V1 reconstruction has public conservative induced L-infinity gain `HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER = 4.68`. The standalone true-peak crate owns and independently qualifies that reconstruction and bound. The pipeline consumes the public constant only when deterministic stored-sample terminal error must be propagated into the same reconstructed-waveform domain; it does not duplicate or reach into the crate's private coefficient construction.

## Measurement and proof are separate

Album scanning therefore obtains two deliberately distinct values from one `PeakCertificate`:

- `reported_point_estimate.overall`, retained for reporting;
- `upper_level()`, retained as the signal authority for hard-ceiling arithmetic.

No `<= 0.495 * Fs` property is invented. The production DSD caller does not invoke the retired Headroom64 authority or consume its former per-tier under-read constants. The older band-qualified authority is not used to manufacture spectral support for this path.

## Linear hard-ceiling arithmetic

For each participating track/output pair Tonepoet evaluates

`G * (P_signal + E_pre) + E_post <= C`

where:

- `C` is the exact requested `DbNano` ceiling converted to a conservative linear lower bound;
- `P_signal` is that carrier's certified true-peak upper bound;
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

For the final `RepeatEndpoints` reconstructed waveform, the production bound multiplies the worst deterministic stored-sample terminal error by the fixed dependency's public HQ1024V1 L-infinity upper `4.68`. This edge-safe operator bound deliberately remains separate from the SoX stored-sample proof.

When a dithered integer lossless output can be written by either FFmpeg or SoX, hard-ceiling processing is routed to SoX so the implementation that realizes the samples matches the proved dither recurrence. FFmpeg album gain is explicitly requested with `precision=double`.

## Lossy outputs

For MP3/AAC/Opus and other lossy targets, NormalizePeak governs the PCM presented to the encoder. The retained carrier rate, fixed-gain domain, and encoder-input PCM rate are required to be identical. Tonepoet admits only rates accepted directly by the configured FFmpeg encoder, pins that same rate explicitly on the final lossy command even when gain was realized in a preceding PCM step, and fails closed if the rate pin is missing, mismatched, or unsupported. This prevents FFmpeg from inserting a sample-rate conversion after the proved gain. In particular, `libfdk_aac` hard-ceiling output at 192 kHz is rejected; 96 kHz is admitted and pinned.

This contract does **not** promise that decoded codec output remains below the same true-peak ceiling; a lossy codec can create new overshoots. Tonepoet does not expand this work into decoded-codec normalization.

## Production topology

Each selected DSD track is reconstructed once into its retained Float64 carrier. Independent track preparation is already fanned out. External DSD reconstruction is bounded by the repository's existing per-tool semaphores; after each retained carrier is produced, that carrier is scanned exactly once, sequentially and with bounded memory, in a Tokio blocking worker. The pre-existing preparation futures can overlap those scans, but this work adds no second analysis pool or new unbounded fan-out. There is no second DSD reconstruction.

The submitted-batch barrier aggregates the completed per-track measurements, derives one fixed gain, binds it to every participating DSD output, and preserves that already-resolved gain through scratch retry/rerun. DSD-free companion handling remains unchanged.

## Qualification and regression protection

Ordinary Rust regressions cover the certified scan/certificate integration, silence, edges, chunk boundaries, multichannel maxima, above-full-scale samples, zero-ceiling arithmetic, directional `DbNano` conversion, terminal domains, dither rate selection, and the mutation in which hard-ceiling arithmetic is replaced by naive `target - point` subtraction.

`crates/tonepoet-true-peak/qualification/verify_certified_search.py` independently reconstructs and audits HQ1024V1 from its published design, checks the frozen coefficient construction, and measures the reconstruction operator norm against the crate's conservative public bound. That proof belongs to the fixed true-peak dependency.

`tonepoet-pipeline/qualification/verify_album_ceiling_terminal_bounds.py` independently derives the pinned SoX-ng FIR/IIR stored-sample support constants and rate-selection behavior, then checks that the fixed dependency still exposes the expected public `HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER = 4.68` integration boundary. It intentionally does not duplicate private HQ1024 coefficients or the crate's reconstruction proof.

Neither script is invoked by Cargo, `build.rs`, `flake.nix`, or runtime code. There is no commissioning stamp, executable/profile gate, source fingerprint, runtime warning/error, real-DSD commissioning corpus, or restored R7/R8/R9 authority machinery.
