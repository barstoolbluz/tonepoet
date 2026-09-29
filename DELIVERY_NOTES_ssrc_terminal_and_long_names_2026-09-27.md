# Delivery notes: certified SSRC true-peak terminal and long publish names (#48/#47/#51)

Date: 2026-09-27 (America/New_York)
Base archive: `tonepoet_ssrc_terminal_and_long_names_bundle_2026-09-27.tar.gz`
Base archive SHA-256: `fb4820c6b78e78fdb9e6c73a3c2eb4839f393338bec4738cb8bbf8a971f14b16`
Brief SHA-256: `5314b168d8f0d49a901d4722108a75457123b20dc3b5b4a3dbc3b882c06c5168`
Declared base: `main @ da688cd` / v0.5.3. The supplied archive contains no `.git` directory.

## Outcome

This delivery implements both requested outcomes without broadening the ordinary SSRC or non-SSRC audio lowering paths.

### #48 / #47 — SSRC owns the certified true-peak terminal

When PCM true-peak gain is active and the selected resampler is an execution-qualified SSRC cell, the final sample realization is now one SSRC terminal execution:

1. Tonepoet materializes the protected source-rate Float64 RIFF ingress once.
2. The already-established Binary64 SSRC binding resamples that exact ingress to the final-rate Float64 observation carrier used by true-peak measurement.
3. The exact protected ingress is retained and content-bound by SHA-256.
4. After the hard-ceiling solver chooses the directed-lower gain, the same attested SSRC executable replays the exact ingress with:
   - the same commissioned rate/profile/phase/base-attenuation scope;
   - a diagonal `--mixChannels` matrix containing Tonepoet's exact directed-lower binary64 gain scalar;
   - the user's resolved SSRC dither/noise-shaper ID and PDF;
   - fixed seed `1`, part of the versioned terminal contract;
   - final integer depth and Wave64 output.
5. Tonepoet validates the realized Wave64 geometry and checks every realized integer sample against `measured_final_rate_f64 * directed_lower_gain`. If any sample exceeds the execution-qualified stored-sample error bound, the conversion fails before the terminal output is committed.
6. The downstream codec/container stage receives an already-realized integer PCM carrier with resampling, gain and dither disabled. It can package/encode but may not become the quantizer. Direct W64 byte passthrough is permitted only for an actual W64 target.

This avoids a second source decode and keeps measurement and final replay anchored to identical ingress bytes. The scalar is not forced through SSRC's coarse attenuation option. `--mixChannels` is part of the exact execution-qualified physical terminal cell, and production universally revalidates the resulting samples before commit.

The conversion transcript now records the SSRC terminal command with the description:

`SSRC certified true-peak terminal: resample + bound gain + dither/noise shaping + quantization`

and the conversion-log summary names SSRC as dither owner with the selected dither/PDF rather than claiming SoX TPDF.

### Fail-closed commissioning

No SSRC true-peak terminal record was fabricated in this delivery. `tonepoet-pipeline/src/ssrc_true_peak_terminal_commissioned.rs` is intentionally empty.

Runtime admission requires an exact registry cell binding all of:

- terminal and gain-model contract versions;
- architecture;
- profile, rate pair, channel count and integer target depth;
- dither ID/PDF, phase and base attenuation;
- exact SSRC executable SHA-256, pinned source revision and build identity;
- qualification report SHA-256;
- inclusive characterized gain range;
- conservative stored-sample error bound.

The qualification harness produces execution evidence only. A separate promoter rejects failed, stale, identity-mismatched or hand-edited evidence and deterministically generates the Rust registry. The current source therefore fails closed on this route until the operator performs that execution qualification and promotion on the intended machine.

Ordinary/direct SSRC behavior retains its existing Int32-native-dither restriction. The certified resolver may admit an Int32+dither cell only when that exact physical terminal cell has been execution-qualified and promoted.

### Operator SSRC terminal qualification

Run inside the repository's flake-owned environment on the intended runtime machine:

```sh
nix develop --extra-experimental-features 'nix-command flakes'

cargo build --release -p tonepoet-pipeline --bin ssrc_true_peak_gain_qualification
SSRC_BIN="$(command -v ssrc)"

test -n "$SSRC_BIN"
sha256sum "$SSRC_BIN"

python3 tonepoet-pipeline/qualification/ssrc_true_peak_terminal/qualify_ssrc_true_peak_terminal.py \
  --ssrc "$SSRC_BIN" \
  --gain-helper target/release/ssrc_true_peak_gain_qualification \
  --tonepoet-root "$PWD" \
  --tonepoet-source-identity 'da688cd+#48-#51-delivery' \
  --production-grid \
  --channels 1,2 \
  --jobs "$(nproc)" \
  --output tonepoet-pipeline/qualification/ssrc_true_peak_terminal/outcome_operator.json

python3 tonepoet-pipeline/qualification/ssrc_true_peak_terminal/promote_ssrc_true_peak_terminal.py \
  --report tonepoet-pipeline/qualification/ssrc_true_peak_terminal/outcome_operator.json \
  --tonepoet-root "$PWD" \
  --output tonepoet-pipeline/src/ssrc_true_peak_terminal_commissioned.rs
```

`--channels 1,2` deliberately commissions only mono/stereo. Add every channel count the installation intends to support (for example `--channels 1,2,6,8`) rather than silently treating an unexecuted matrix geometry as qualified. The runtime fails closed for a channel count absent from the generated registry.

The default gain corpus is -24, -12, -6, -1, 0, +1, +6, +12 and +24 dB. If the installation must support gains outside that interval, rerun qualification with repeated `--gain-db` points spanning the intended interval. Runtime rejects gains outside the promoted range.

Review the report, identity sidecar and generated registry diff before accepting the promotion. Do not hand-author registry records.

### #51 — long names and Publish

User-visible output path components are now preflighted against the target filesystem's component limit before conversion work:

- on Unix, Tonepoet queries `_PC_NAME_MAX` via `pathconf` on the nearest existing output ancestor;
- if the platform cannot provide a trustworthy limit, it uses a conservative 255-byte fallback;
- overlong UTF-8 components are shortened deterministically on code-point boundaries with a 96-bit SHA-256-derived suffix;
- a final file extension is preserved when possible;
- shortening is surfaced before conversion in status and is written to `conversion.log`.

Legal near-limit names are not shortened merely to accommodate Tonepoet's internal files. Instead, publish/action coordination artifacts use fixed-size hash-derived names such as `.tonepoet-publish-<token>.lock`, `.tonepoet-tmp-<token>-...` and `.tonepoet-backup-<token>-...`. This lets the observed 244-byte album directory remain byte-for-byte user-visible while keeping every internal sibling well below a 255-byte component limit.

The output planner also reapplies the component clamp after collision-suffix insertion, so collision recovery cannot recreate an overlong leaf.

For SACD, output planning occurs before audio extraction: materialization at that point establishes descriptive track/TOC metadata; actual SACD audio realization occurs later in Convert. Thus the reported long-name failure is detected/rewritten before its expensive audio work begins.

## Scope discipline

- The DSD Reference decimation/quantization route remains SoX-ng/FFmpeg only; SSRC is not made selectable there.
- The existing direct SSRC PDF values are unchanged; the string mapping was factored into a helper so certified replay can reuse the exact same values.
- Non-SSRC audio realization is not redesigned by #48. #51 necessarily changes internal coordination filenames and only rewrites user-visible names that exceed the target component limit.
- The pre-existing SSRC Binary64 evidence is consumed, not rewritten.
- Generated Python bytecode/cache directories are omitted from the final source delivery.

## Regression coverage added

The Rust source adds focused regressions for, among other cases:

- deterministic UTF-8 shortening and hash discrimination;
- target-extension preservation;
- pre-conversion status/log disclosure;
- a legal 244-byte album directory publishing with compact internal siblings;
- ordinary SSRC Int32 dither remaining unavailable while the separately qualified terminal resolver can select the exact Int32 cell;
- certified W64 terminal carrier lowering to package-only behavior;
- WAV/RF64 targets not byte-passthroughing a W64 carrier;
- SSRC ownership in conversion-log dither reporting;
- replay/binding/gain-range/geometry/identity invariants.

The Python qualification suite covers evidence identity, source-authority binding, deterministic repeated realization, bound derivation/promotion and execution-qualified Int32+dither promotion.

## Validation performed in this runner

This environment does not contain the repository's Rust/Cargo/Nix/SSRC toolchain, and network attempts to install one were unavailable. Therefore this delivery does **not** claim a Rust compile, rustfmt, clippy, workspace-test pass, SSRC physical-cell execution qualification, performance measurement or Reference requalification here.

Validation actually performed:

- new SSRC-terminal qualification unittest: **8/8 PASS**;
- existing SSRC Binary64 qualification unittest: **7/7 PASS**;
- all **21** Python files below `tonepoet-pipeline/qualification` parsed with Python `ast`;
- all **12** TOML files parsed with Python `tomllib`;
- all **7** shell scripts passed `bash -n`;
- no unresolved merge-conflict markers;
- no `target/` or `__pycache__/` directory in the delivery tree;
- full baseline-relative `git diff --no-index --check`: clean (no whitespace errors);
- focused adversarial review of direct SSRC mapping, Int32 policy separation, W64 package-only lowering, terminal ingress identity, per-sample conformance, long-name collision handling and bounded coordination names.

The supplied snapshot records a pre-change operator baseline of **7143 passed / 0 failed / 16 ignored, zero warnings**. Do not substitute older historical counts from `CLAUDE.md` for that snapshot baseline.

## Required operator gate before handoff acceptance

Inside the pinned Nix development shell:

```sh
nix develop --extra-experimental-features 'nix-command flakes'

cargo fmt --check
cargo check --workspace
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
cargo test -p tonepoet-true-peak --no-fail-fast
```

Then:

1. run the SSRC true-peak terminal execution qualification and promoter above;
2. rerun `cargo fmt --check`, `cargo check --workspace` and the workspace tests against the generated registry;
3. because this delivery intentionally changes files covered by `reference_source_lock.rs`, rerun the repository's Reference qualification on the operator machine:

```sh
TONEPOET_REQUIRE_TOOLS=1 cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

and regenerate/reinstall the corresponding Reference evidence as required by the repository's established process;
4. rerun the reported Journey 176.4 kHz / 32-bit FLAC -> 88.2 kHz / 24-bit FLAC case with SSRC + PCM true-peak gain and verify the log names the selected SSRC dither/PDF and no SoX terminal quantizer;
5. rerun the reported Bach 22-track SACD conversion with `%ARTIST% - %ALBUM% (%YEAR%) [%FORMAT%] {%TITLE_EXTRA%}` unchanged and verify the legal near-limit album directory publishes successfully.

Those operator-side execution gates are the only remaining acceptance work. The source delivery intentionally refuses to claim commissioned SSRC terminal cells without them.

## Delivery integrity

`DELIVERY_CHANGED_SHA256SUMS_ssrc_terminal_and_long_names_2026-09-27.txt` pins the final bytes of every changed/added delivery file except the manifest itself. The external archive checksum pins the complete packaged tree.
