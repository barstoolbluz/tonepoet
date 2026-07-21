# DSD Reference policy v15 qualification report

Status: **candidate; pinned real-tool qualification not run**

Policy v15 is an append-only hardening of the v14 measurement architecture. It does not alter the DSD reconstruction, gain, terminal-realization, packaging, or publication chains. It changes three operational authorities: multi-tool permit acquisition, true-peak residual evidence, and analyzer deadlines.

## Multi-tool executor liveness

Every Reference producer/consumer pipeline declares its complete tool-family set before execution. The executor deduplicates those families and acquires them in the frozen global order SoX, FFmpeg, SSRC, independent of data-flow direction. Partial acquisition is owned by one RAII permit-set guard and is dropped automatically on cancellation or later-acquisition failure.

The deterministic regression first reconstructs the historical circular wait with one SoX permit, one FFmpeg permit, and barriers proving that opposite-direction tasks each own one resource before requesting the other. Cancellation is used only to dismantle that demonstrated legacy cycle. The same two route declarations are then submitted through composite acquisition; a barrier starts both together, explicit release channels serialize whichever complete acquisition wins, and both finish without sleep-based scheduling assumptions.
A companion deterministic regression verifies that repeated binaries collapse to one permit per tool family and that cancelling while a composite request holds SoX and waits for FFmpeg releases the partial set before returning the cancellation error.

## True-peak residual authority

The unchanged conservative analyzer residual is 0.100000000 dB. Policy v15 no longer presents the ideal sample-grid result as complete authority. It decomposes the bound as follows:

- ideal 16x grid component: 0.041925957 dB, analytically derived as `-20 log10(cos(pi / 32))` and rounded upward;
- pinned SoX-ng resampler component: at most 0.058074043 dB, admitted only by the pinned real-tool qualification matrix;
- reporting quantization: 0.010000000 dB, added outside the analyzer residual;
- conservative one-sided total: 0.110000000 dB.

The resampler component is therefore empirical authority for the exact pinned SoX-ng revision, not a source-level theorem about every possible resampler. Runtime activation remains fail-closed until the pinned matrix proves the component limit and binds the resulting report.

The intended matrix retains all 1,968 v14 cases and adds 200 adversarial cases: impulse, near-band-edge burst, alternating-sign, deterministic broadband, and boundary-transient fixtures across ten target rates, mono/stereo, and early/late placement. Each adversarial production result is compared with a 64x pinned-tool oracle, and the report must expose the maximum observed total under-read and the corresponding resampler-component residual.

## Workload-derived analyzer deadline

Neither the direct SoX route nor the Float32 FFmpeg-to-SoX route may inherit the generic one-hour timeout. The planner computes one deadline from the admitted programme workload and binds that identical duration to every command in the measurement:

`guarded_frames = ceil(duration_ns * sample_rate_hz / 1e9) + 1`

`workload = guarded_frames * channels * 16`

`deadline_seconds = 120 + ceil(workload / 1,000,000)`

The streamed-WAV capacity gate limits the maximum admitted analyzer workload to 8,589,934,480 oversampled sample values, which yields a maximum derived deadline of 8,710 seconds. The exact deadline is stored in the plan summary, serialized into the v15 semantic-plan identity, and required to match every analyzer command at runtime. Qualification must demonstrate at least 1,000,000 oversampled sample values per second for the pinned toolchain and include a maximum-admission arithmetic check. This deadline is a conservative failure bound, not an expected completion time.

## Required promotion gates

Promotion requires byte-identical current and candidate v15 manifests, the v15 deterministic derivation check, formatting, compilation, the full workspace suite, Clippy with warnings denied, zero cold-build warnings, the complete pinned-tool qualification matrix, the liveness regression, the workload-throughput gate, live smoke coverage, and atomic binding of the passing certification report and candidate manifest hashes. Until then, v15 remains a source-controlled candidate and Reference activation fails closed.
