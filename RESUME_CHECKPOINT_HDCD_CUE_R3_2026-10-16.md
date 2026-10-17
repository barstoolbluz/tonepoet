# Persistent continuation checkpoint - HDCD / CUE R3

**STATE:** Minimal corrective R3 source patch packaged with complete repository.
The originally failing test's terminal-selection assertion is corrected for
lossless PCM vs lossy MP3; all five new `missing_docs` field warnings have
`///` documentation. The incoming archive already carried the two compile
fixes. **Build-host and pinned-Reference qualification remain OPEN.**

**INPUT:** `TonePoet_LOGGING_HDCD_CUE_R3_2026-10-17.tar.gz` SHA-256
`23dc1f5ae370781e7813ff3de87a8e0607eded22e9d551486be96a573f408e5d`.
**OUTPUT:** `TonePoet_HDCD_CUE_R3_CORRECTIVE_FULL_REPO_2026-10-16.tar.gz`
and its external `.sha256` file. Inside: complete `tonepoet/`,
`SHA256SUMS_HDCD_CUE_R3.txt`, `SOURCE_DELTA_HDCD_CUE_R3.patch`,
`ENGINEERING_HANDOFF_HDCD_CUE_R3_2026-10-16.md`,
`VERIFICATION_HDCD_CUE_R3_2026-10-16.md`,
`R3_HDCD_CUE_SMOKE_LOG.txt` and this checkpoint.

**SOURCE CHANGES FROM INPUT:** one file only:
`tonepoet-pipeline/src/semantic_plan.rs`. All other production files are
identical to the exact incoming archive. The input's
`src/tui/command.rs` and `src/convert/processor.rs` already include their
required compile fixes. Their changes relative to the prior R2 inventory
explain the inherited R2 checksum mismatches; R3's current manifest passes.

**VERIFIED HERE:** source comparison 1049 unchanged/1 changed, original
symlink retained, R3 patch replay byte-exact; six HDCD FFmpeg/SQL scripts
passed with real recognized Peak Extend fixture. Last model evidence:
44,433 matching post-HDCD-cut frames, zero mismatches on correct route,
390 on deliberately wrong cut-first route; ordinary neighboring member S16
samples unchanged.

**NOT VERIFIED:** Rust build and tests, actual TonePoet runtime route and logs,
compiler warning silence, production timing/memory, complete Reference lock
requalification. The host has FFmpeg and Python, but neither a Rust toolchain
nor Nix nor external network. The package is explicitly unqualified.

**RESUME INSTRUCTIONS:**

1. Verify external SHA-256 sidecar and internal `SHA256SUMS_HDCD_CUE_R3.txt`.
   Read R3 handoff and verification ledger before acting on the R2 history.
2. On a proper Rust build host, run `cargo fmt --all -- --check`,
   `cargo check --workspace`, the revised eight-cell HDCD gain test,
   all HDCD/CUE named tests, `cargo test --workspace`, and pinned Reference
   qualification. Fix a genuine failure; do not replace the lossy MP3
   realization with a synthetic PCM terminal just to satisfy a test.
3. Exercise actual TonePoet FLAC/MP3 effectual HDCD CUE playback/conversion,
   certified Track and Album Guard/Normalize, mixed CUE members and failure
   reporting. Establish realistic rescan/decoder cost before any new caching
   or grouping optimization.
4. Once qualified, record exact command outputs, source-lock evidence and
   package checksum. Rebuild the complete tar and manifest if anything changes.

**ENGINEERING BOUNDARY:** Avoid generalized graphs, DSP redesign, altered
qualified certificates, speculative hardening, or unrelated regressions.
Preserve source-first continuous decode then sample-exact cut, per-member
HDCD eligibility, truthful gain/terminal precision, and terminal proof ownership.
