# TonePoet R29 — implementation handoff (2026-10-11)

**State: source implementation complete; static inspection passed; compiled/build-host acceptance and Reference requalification NOT RUN in this environment.** This is an implementer handoff, **not** a qualified Reference release. The four installed v18 qualification sidecars in this source tree are retained unchanged and **do not certify the modified source**.

## Source

- `tonepoet/` is the complete project source tree, based on the supplied R29/R28 input archive, with a single coherent R29 change set.
- `R29_CHANGES.patch` applies with `git apply` at the repository root of the unchanged supplied base. The full tree is authoritative; the patch is also included for narrow integration/review.
- `R29_RESUME_CHECKPOINT.md` records the provenance, coverage, changed files, gates and unfinished build-host verification.
- `R29_ACCEPTANCE.sh` runs targeted tests followed by the repository's already-established **transactional R20 requalification runner**; optionally pass a real SACD ISO to exercise a positive reference smoke. Do not run requalification against incomplete/unformatted code.

## Changes, limited to the brief

1. **Ceiling provenance / gain**: typed `TruePeakTargetOrigin` preserves `default`, `explicit`, `preset`, and `unknown` distinct from automatic music-level gain. Old requests/fragments without the new field remain backward-readable and never claim explicit/default provenance. Reference album log uses separate ceiling/gain statements, and near-silence does not masquerade as a measured peak. Exact stored gain math/evidence is not changed.
2. **Ceiling controls**: `SampleGainPolicy::TARGET_SELECT_STEP` is common to PCM/DSD TUI selectors at 0.25 dB; both clamp to inclusive -12..0 dBTP; custom entry via `:set pcm-true-peak-target VALUE` or `:set dsd-true-peak-target VALUE` accepts up to 9 decimal places via existing `DbNano`. Display/log formats retain significant digits without forced zero padding. The analyzer's 0.01 dB reserve is *not* treated as a 0.01 dB measurement resolution (the analyzer uses full-resolution floating input and then represents dB in DbNano).
3. **PCM CLI**: `--pcm-gain` selects off/true-peak-guard/true-peak-normalize/fixed-gain. `--pcm-gain-db` is valid with fixed-gain. The requested `--pcm-true-peak-target-dbtp`, `--pcm-true-peak-scope`, and `--pcm-true-peak-scan` options configure an active PCM true-peak mode. They may refine a preset's active policy without reselecting one. Scope/scan/target alone are refused if gain is Off or Fixed: a numeric ceiling is not silently converted into a change to audio processing.
4. **Stored lossless checksum**: FLAC STREAMINFO MD5 and optional native WavPack MD5/ALT_MD5 are read directly from bounded file metadata for successful staged native output only, before publish. No decoding or newly calculated checksum; no empty line if unavailable, invalid, hybrid, or unsupported. No claim of decoder/sample verification from the stored value. Encoder's checksum must actually have been embedded in the output. Existing post-encode validation line remains distinct.
5. **One output statement**: `Written to: <final path> — <format if recorded>; encoded by <tool if unambiguously linked>`; the `Output target` duplicate and nested `Output file: / Write output file` were removed from the human view only. A failed/blocked track says `Intended output (not delivered)` instead. Other artifact work remains visible; the forensic typed operations are unchanged. Do not attribute PCM terminal work to the subsequent FLAC writer without a typed link.

## Operational examples

```bash
# PCM source with an explicitly specified ceiling, album scope and reference scan
./target/release/tonepoet convert SOURCE.wav --format flac \
  --pcm-gain true-peak-normalize \
  --pcm-true-peak-target-dbtp=-0.125 \
  --pcm-true-peak-scope album \
  --pcm-true-peak-scan reference

# Existing DSD Reference CLI controls remain available, including explicit 0.00 dBTP
./target/release/tonepoet convert SOURCE.iso --format flac \
  --dsd-path reference --dsd-gain auto \
  --dsd-true-peak-target-dbtp=0
```

## Baseline and acceptance limitations

The supplied brief says R28 was committed at `a65d7fd`, with 7412/1 and then successful Reference requalification followed by a separately observed lease-ownership flake in the workspace suite. That is **user-provided build-host history, not independently verified here**. This offline container has no `cargo`, `rustc`, `rustfmt`, or `nix`; therefore there are **zero compiled Rust test passes asserted by this handoff**. `git diff --check`, delimiter scanning with Python/Pygments, and checks of key integration paths were performed. No qualification sidecars were edited, no bypasses set, and no tests disabled. See checkpoint and acceptance script for the remaining mandatory gates.

**Release decision: HOLD** until the acceptance runner, source-lock freshness, positive and negative Reference smokes, and any required code formatting are verified on the qualified build host.
