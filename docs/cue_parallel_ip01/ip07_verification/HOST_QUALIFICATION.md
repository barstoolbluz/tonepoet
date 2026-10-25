# IP-07 mandatory host qualification and corrective decision

**Input:** Verify the externally published complete output archive SHA-256 and extraction. Preserve the SHA-verified host-accepted IP-05 archive `af66b5c2a40904d4db6fec3fd4185914b4aadd32c74cf6fd6ff15568bf2cbd20` as rollback. Do not infer a Git commit from this Git-less tarball.

## Mandatory toolchain and Rust gates

From the extracted source root (Rust >=1.93 with project platform dependencies installed), save stdout/stderr and exact test counts for each command, even on failure:

```bash
cargo fmt --all -- --check
cargo test --locked cue_ip07 -- --nocapture
cargo test --locked cue_ip02 -- --nocapture
cargo test --locked cue_ip03 -- --nocapture
cargo test --locked cue_ip04 -- --nocapture
cargo test --locked cue_ip05 -- --nocapture
cargo test --locked cue_stream -- --nocapture
cargo test --locked replaygain -- --nocapture
cargo test --locked --workspace
# If, and ONLY if, the known unrelated Reference gate requires it:
cargo test --locked --workspace --exclude tonepoet-true-peak
cargo test --locked -p tonepoet-true-peak
```

A zero-test filter is **NOT PASS**. Host must establish actual execution of the named tests, not merely a successful `cargo test` command with zero matches. Resolve any formatting/compile failures before native qualification; issue a narrowly corrective IP-07 archive (with new SHA) and rerun these gates, not IP-06.

## Native signal and metadata parity

Use the exact same physical source/CUE and same qualified tool binary and settings on each route: S1 (original source pass), original S3 (source meter disabled/unavailable, native post-output), and IP-07 S3 (with opt-in and source-range observation). Prove by gated Stage-A traces that `s3_source_meter_album_settled.source_pass_observed=true` before interpreting the third route. Run distinct `Off`, `Track`, `Album`, `Both` configurations and exclude route, **not ReplayGain mode**, from route-comparison hashes.

Include 16/24-bit and 44.1/48/96-kHz, mono/stereo and 1/3/12/30 authored tracks where actual IP-05 S3 admission permits (IP-05 requires >=2 tracks; one-track cells are rejection tests). Test 588-frame boundaries, nonsecond/FLAC-frame positions, INDEX 00/HTOA, final partial EOF, full original album, selection refusal, inherited trust/recompute, `SkipIfComplete`, clipping-prevention on/off, mathematical unavailability in short/silent PCM, and source-generation drift. Reject gaps/duplicates, nonexistent extents and unavailable rates/depths as dictated by the preflight; never silently broaden admission.

Compare native per-track loudness/peak and album sufficient-statistics reduction, exact serialized gain/peak tags in each mode, sample counts and decoded output PCM hashes, CUE/artwork metadata and output manifests, provenance, logs, stage results and publication (including absence of unexpected sidecars). A tag value alone is not proof of source-observer selection or album identity.

## Failure/cancellation/resource cases

Inject: FFmpeg permit contention with concurrent albums; 1 and 2 S3 worker slots; cancellation before permit and after spawn; reader early EOF, trailing PCM, broken connection, blocked pipe, corrupt CRC, nonzero process after full correct bytes; source replacement and in-place alteration; observer failure/panic; conversion/encoder failure; partial output under each FailurePolicy; failure after valid scan and before tag writing; metadata writer failure; disk-full/staging/publish refusal. Require **bounded elapsed failure and child-reap**, exactly-once permit release, original causality and no new visible publication artefacts. Successful conversions after optional meter failure must not restart; they must use ordinary native post-output ReplayGain. Writer failure after retained scan is fatal, never rescanned.

Capture real native child counts, FD/RSS/CPU high water, queue wait, active meter wall, concurrent workers, actual selected route, fallback/recovery reasons, decoder binary/source SHA, tool versions, output and staging FS types and exact final destination file set. Keep S1 and original S3 as host-tested rollback paths.

## Paired performance (IP-07 observation; eventual IP-06 promotion)

For each representative album/FLAC profile, collect >=5 counterbalanced matched pairs, with a clear admission-to-**visible-publication** stopwatch (no double-counted overlapping intervals). Compare `Off` and `Both` independently on S1, prior S3 post-output and IP-07 S3 source. Report meter queue/active/overlap plus any fallback duration and actual total wall; retain configuration, source, sample, tool and file-system identities and cache state. IP-07 is correct-even-if-slower when all correctness gates pass; leave S3 default-OFF and run IP-06 only after sign-off. IP-06 alone owns the representative >=1.25x threshold and promotion. Apply the attached `IP-06_SEQUENCING_AMENDMENT.md`, especially the separate H-01 mode-hash evaluator fix.
