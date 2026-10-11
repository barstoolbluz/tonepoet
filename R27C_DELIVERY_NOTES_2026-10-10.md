# TonePoet R27C — Reference human-log eligibility corrective

## Authority, baseline, and scope

- Authority: the user's independent R27 audit finding concerning qualified Reference `--dsd-gain off` and Track-scope auto normalization, read alongside `R27_AUTHORITY_BRIEF_2026-10-10.md`.
- Input baseline: the uploaded `TonePoet_LOGGING_R27_album_processing_corrective_2026-10-10.tar.gz`, SHA-256 `14a0497b652014c3cc7cd1ab5c593ecdb0e56657fcf8d5dcc2209a7419aebe4a`.
- Exactly one **preexisting** source file changed: `src/convert/pipeline/stages.rs`; 2 focused `r27c_` unit regressions were added in that file.
- No changes to signal processing, Reference planner/gain authority, the portable execution schema, encoding or packaging, defaults, CLI/TUI/preset interpretation, dependency graph, or the audio execution hot path.
- `R27C_CODE_CHANGES_FROM_R27.patch` is a minimal source-only delta that applies to the *R27* baseline above, not to R26. Historical R27/R26 patch and checksum files remain in the complete tree for traceability; verify current source with `R27C_CHANGED_SHA256SUMS.txt` instead.

## Independent root-cause confirmation

R27 only called `humanize_reference_processing` when the conversion-log fragment carried `reference_album_gain`, a projection emitted for the album-auto Reference carrier alone. Gain Off and Track auto use the same protected Float64 Reference reconstruction and completed typed terminal, but neither has that album-auto carrier. On these paths the human log could promote or render the raw `Applied terminal scalar` and `Fixed Reference protection restored` values and the protected `Target precision: 64-bit floating-point` intermediary.

## Corrective behavior

1. **Humanization eligibility is independent of album-auto gain:** the new `completed_reference_processing` predicate recognizes *completed delivered* `reference_terminal_realization` and the associated canonical 64-bit R64 `dsd_to_pcm` operation. It does not infer processing from the album-auto carrier, selected settings, or command-line strings. The original `album_uniform_reference_gain` corroboration remains unchanged.
2. **Gain Off:** only the recognized fixed +12 dB restoration is removed from the transient human view when the measured music-level scalar is zero. The generic `Resolved qualified Reference gain policy` decision label is hidden for Off because it is not a requested gain step. Unexpected nonzero music-level gains remain visible rather than being silently falsified. A pre-R27C serialized fragment without a gain-mode hint conservatively omits a fixed-only 0 dB gain claim.
3. **Track auto:** the temporary human operation shows the actually completed scalar *minus* the exact 12 dB Reference protection/restoration term. A genuine automatic zero music gain remains visible, unlike explicit Off. Track auto never promotes this gain as an album-wide decision merely because gains agree; completed values remain per track.
4. **PCM output:** the existing typed completed terminal-contract compaction no longer depends on album gain promotion. Uniform complete Reference albums with gain Off or Track auto show delivered rate, depth, channel count and dither in `Converted to PCM`. The intermediate 64-bit carrier is not labeled as the delivered output. Ordinary non-Reference album fallbacks are preserved.
5. **Forensic preservation:** every change to `TrackExecutionEvidence` takes place in a cloned projection. The persisted completed `apply_gain` with the original terminal scalar, 12 dB restoration parameter, R64 format, and invocation linkage remains unchanged. The optional `reference_gain_is_off` coordination-fragment display hint is derived from the typed Reference plan; it changes no portable forensic schema.

## Added tests, not executed in this environment

- `r27c_reference_gain_off_hides_private_restore_but_keeps_delivered_pcm`: two successful tracks; no gain/restoration/intermediate precision in album or per-track human text, final PCM and dither present, legacy fragment without mode hint not misleading, original forensic evidence (including invocation indices) unchanged.
- `r27c_reference_track_auto_is_not_promoted_as_album_gain`: 2 independent automatic gains including a legitimate zero plus the singleton case; no false album gain, correct per-track music gain, PCM output compacted, private restoration not displayed.
- Existing `r27_` album-auto assertions are left unchanged in substance; legacy non-Reference gain-promotion logic remains in place.

## Verification and acceptance status

Static verification: source-only diff, `git diff --check`, patch application against the extracted R27 archive, byte-for-byte result comparison, file-scope comparison and Rust lexical/delimiter scan. Details and hashes: `R27C_STATIC_QA.txt` and `R27C_CHANGED_SHA256SUMS.txt`.

**Not yet build-host qualified:** this environment has no `cargo`, `rustc`, `rustfmt`, or `nix`; no compiled Rust tests or production SACD conversions were run. Reference source freshness is invalidated by modifying `stages.rs`, regardless of unchanged DSP. An acceptance host must run:

```bash
cargo fmt --all -- --check
cargo test --lib r27c_
cargo test --lib r27_
cargo test --lib album_scope_
cargo test --lib r24_applied_gain_appears_once_when_uniform_and_on_each_track_when_divergent
cargo test --workspace
cargo build --workspace
cargo build --release
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
```

If the tests and real-tool prerequisites pass, run **one final** requalification on the final accepted tree (without unqualified override):

```bash
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

Then rerun the repository's expected Reference freshness/full regression gate, and perform the separately outstanding real-SACD Browse → Convert → preset gesture (#43). Do not claim either one passed until it has actually run.

## Explicit non-goals

No new recovery/coordination layer, no changes to sample precision or terminal arithmetic, no new provenance schema, no reduction of album-auto corroboration requirements, no parser clamp to two-decimal peak ceilings, and no broad rewrite of other DSD routes. Any further hardening must be justified by an observed, reproducible defect.
