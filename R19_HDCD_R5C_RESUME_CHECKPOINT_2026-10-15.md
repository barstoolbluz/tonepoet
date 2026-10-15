# Resume checkpoint — TonePoet R19 HDCD R5C (October 15, 2026)

## Base and authority

`TonePoet_R19_HDCD_R5_EIGHT_FAILURES_UI_CORRECTIVE_2026-10-15.tar.gz`, SHA-256 `ae99786572e2314a3813eb22b91b6e3002cdac7ef6e049bfb8cc760b643ca98e`. All 1,011 R5 manifest hashes independently verified. Authority for this correction: user-provided independent R5 audit describing one named-preset warning-loss defect. **Do not expand scope beyond it without build-host evidence.**

## Implemented state

- `src/tui/app.rs`: preset continuation gains `status_notes: Vec<String>`.
- `src/tui/command.rs`: expansion attaches first ordinary and distinct cumulative-CUE warnings to active preset or post-load continuation. Matching probe completion transfers them into the post-load scan continuation; immediate or refusal status appends them directly; existing final queue completion retains its own status precedence.
- New async named-preset regression: valid WAV and v5 preset, asynchronous `ProbeResult`, HDCD scan completion, final Queued with both warnings once. Its source topology has one valid item, so there is no asynchronous de-emphasis **batch** event; this is an intentional test fidelity detail. Existing batch regressions cover both scan completions.
- Existing R5 no-preset regression untouched; all unrelated R5 source/config/Reference files untouched.

## Verification and gaps

- 2-file diff and reversible patch PASS. Reversal recreates all 1,011 R5 hashes.
- 16 structural contract checks and four compile-free SQL/FFmpeg tests PASS.
- **Not performed:** Rust compilation, new/existing targeted Rust tests, full workspace gate, pinned libfdk AAC, Reference requalification. Toolchain and qualified host unavailable.
- Previous R5 Reference freshness failure remains owed. Do not alter qualification evidence without genuine build-host requalification.

## On configured build host

```bash
cd tonepoet
sha256sum -c R19_HDCD_R5C_PACKAGE_SHA256SUMS.txt
cargo test --workspace --locked async_named_preset_commit_preserves_both_cue_warnings_across_probe_and_scans
cargo test --workspace --locked async_browse_queue_review_commit_preserves_cue_warning_after_final_commit_status
cargo test --workspace --locked context_menu_convert_last_used_audio_directory_commits_after_async_expansion
cargo test --workspace --locked
scripts/requalify_reference_r20.sh
cargo test --workspace --locked
```

If a concrete failure remains, use the minimal focused correction and create a new archive, manifest, and checkpoint. Preserve user-visible warning fidelity, original queue-status precedence, and unchanged audio pipeline/performance.
