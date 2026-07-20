# DSD Reference Policy v6 Qualification Report

**Policy:** `sox_ng_14_8_0_1_v6`  
**Report schema:** `tonepoet-dsd-reference-policy-qualification-report/v1`  
**Policy evidence state:** QUALIFICATION CANDIDATE  
**Implementation certification:** not granted by this source-controlled report. Runtime activation remains fail-closed until the mandatory build, lint, workspace, pinned-tool qualification, live-smoke, and release-certification gates pass unchanged and bind this exact candidate.

## Corrective authority

Policy v6 is an append-only correction to the analyzer carrier binding used by policy v5. Policy v5 remains immutable and decode/history-only. No v5 route, manifest, evidence file, or serialized identity was rewritten.

The v5 terminal-safety contract is inherited without change:

- public post-final ceiling: `-1.000000000 dBTP`;
- analyzer reporting reserve: `0.010000000 dB`;
- Int24 TPDF safe pre-terminal ceiling: `-1.010002327 dBTP`;
- Float32 safe pre-terminal ceiling: `-1.010001164 dBTP`;
- Float64 safe pre-terminal ceiling: `-1.010000001 dBTP`.

The changed behavior is the post-terminal Float32 analyzer decoder route, its exact argv, parser identity, runtime binding checks, semantic plan hash, and qualification evidence. Those changes require and receive the new immutable ID `sox_ng_14_8_0_1_v6`.

## F1 diagnosis: crossed decoder binding

The admission finding inferred that the pre- and post-final measurements had observed different fixture files. An isolated same-file reproduction distinguishes the actual failure:

```text
one SoX-written Float32 W64 fixture, analytic peak -20 dBFS

FFmpeg reads W64 directly                         input_tp -20.00  correct
SoX decodes/re-containers W64 as f64 WAV stream  input_tp  -0.00  broken
```

The earlier empirical record stating `sox f32 -> W64 -> ffmpeg loudnorm: -20.00` is therefore correct. It describes a SoX-written Float32 W64 file read **directly by FFmpeg**. It does not establish that SoX can decode and re-container that Float32 W64 correctly.

Policy v5 applied the f64-W64 workaround unconditionally to both measurement stages. That was the crossed binding: a decoder route qualified for the f64 R64 carrier was also bound to the Float32 QPCM carrier. The pre-final measurement remained correct because R64 is Float64; the post-final Float32 measurement was driven near full scale by SoX's Float32-W64 readback defect.

Policy v6 freezes carrier-sensitive routing:

```text
R64 pre-final measurement, always Float64 W64:
  sox_ng -S -D R64.w64 -t wav -e floating-point -b 64 -
  | ffmpeg -f wav -i pipe:0 ... loudnorm ...

QPCM post-final measurement, Float32 W64:
  ffmpeg -i QPCM.w64 ... loudnorm ...

QPCM post-final measurement, Int24 or Float64 W64:
  sox_ng -S -D QPCM.w64 -t wav -e floating-point -b 64 -
  | ffmpeg -f wav -i pipe:0 ... loudnorm ...
```

The f64 route remains necessary because FFmpeg 7.1 scales SoX-ng 64-bit IEEE-float W64 by exactly `2^31`; the streamed SoX re-container route restores the correct level. The Float32 route is direct because FFmpeg reads that carrier correctly while SoX-ng mis-scales it on readback.

The parser identity is `ffmpeg_loudnorm_input_tp_v3`. The exact route, producer presence, carrier path, measurement ID, purpose, scope, environment, complete argv, and parser are bound by the semantic plan and checked again by the executor against `DsdReferencePlanSummary`. A structurally valid command that points at the wrong stage or uses the wrong decoder route is rejected before execution.

## No RIFF intermediate and no inherited 4 GiB ceiling

`QPCM` remains policy-owned W64 for every enabled terminal depth. Policy v6 does not create an on-disk RIFF/WAV analyzer or packaging intermediate.

- W64 delivery uses `QPCM` directly as the staged final audio object.
- RIFF, RF64, FLAC, AIFF, WavPack, and ALAC package sample-preservingly from W64 `QPCM`.
- RIFF capacity admission applies only when the requested final target is ordinary RIFF.
- Float32 W64 and RF64 plans retain W64 `QPCM`, including high-rate programmes that exceed the ordinary RIFF capacity.
- The streamed f64 analyzer path uses a shell-free stdout-to-stdin stream and reads to EOF; it does not materialize a size-limited RIFF file.

Qualification must cover a 15-minute, 768-kHz, stereo Float32 W64 and RF64 topology, exact package argv, no disk-backed RIFF intermediate, and the existing greater-than-4-GiB streamed analyzer fixture.

## F2: exact pre-promotion legacy gain behavior

Before Reference promotion, DSD-to-PCM exposes and executes the frozen legacy gain family rather than native Reference controls:

```text
disabled -> legacy Disabled wire -> no gain effect
auto     -> legacy Auto wire     -> SoX `norm -<margin>`
manual   -> legacy Manual wire   -> SoX `gain <signed dB>`
```

The gain row and its manual-value/auto-margin rows remain visible for a DSD source targeting PCM. Reference path, Reference profile, Reference gain, NativeLevel, and native NormalizePeak remain hidden and disabled until promotion. Generic resampler and dither controls remain available on the legacy route.

TUI state construction writes the exact `LegacyFlatV1` settings wire and canonicalizes non-authoritative gain fields. It never stages a native/legacy hybrid and never silently discards an accepted legacy selection. Integration regressions build a real legacy plan from each TUI mode and pin the emitted SoX argv.

### v4 preset adjudication

Pre-promotion v4 preset application uses this explicit policy:

- default `dsd_path=reference` and `dsd_profile=reference` are accepted as behaviorless no-ops;
- `dsd_gain=reference` maps to exact legacy Disabled because early v4 presets captured that UI default before Reference promotion;
- `disabled` maps to legacy Disabled;
- `auto` maps to legacy Auto;
- historical `normalize` maps to legacy Auto using the magnitude of its target as the legacy safety margin, matching the existing legacy-to-native mirror (`Auto` <-> `NormalizePeak(-margin)`);
- `fixed` or `manual` maps to legacy Manual;
- native-only `native`, manual DSD pathway, and `wideband` remain explicit refusals before promotion;
- DSD-source fields are ignored, not refused, when the preset is applied to a conversion that is not DSD-to-PCM;
- an incompatible `output_target` remains a refusal.

Tests pin complete and refused-field reports so this behavior cannot drift silently.

## Unchanged policy surface

All v5 source admission, profile selection, terminal bounds, package/decode-back rules, supported cells, rejection precedence, public ceiling, and deferred surfaces remain unchanged. Enabled source cells remain native DSF and DSDIFF/DSD at DSD64/128/256 mono/stereo, plus DSDIFF/DST DSD64 stereo. SACD, Int16, Manual Reference workflows, and lossy Reference delivery remain unavailable.

The expanded supported-cell count remains `13,248`; the canonical cell-contract digest remains `8655f32296e3ac0012357c321cae026eb0effbcb3e128d5a1fad673fe12927a3`.

All Reference external commands use `ClearAndSet` with only `LC_ALL=C`.

## Mandatory gates

```text
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v5_terminal_bounds.py --check
python3 tonepoet-pipeline/qualification/derive_dsd_reference_v6_terminal_bounds.py --check
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
TONEPOET_DSD_REFERENCE_REPORT_PATH="$PWD/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v6_certification.json" \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

The tool-gated report must be schema version 6 and prove both opposite decoder defects, exact planner/executor route binding, the isolated F1 Reference-gain regression, high-rate Float32 W64/RF64 topology, all package/sample-hash cases, the default-settings legacy DSD64 DSF-to-FLAC smoke, the complete analyzer matrix, terminal-chain acceptance, environment isolation, bounded supervision, and every pre-existing required qualification surface.
