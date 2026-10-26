# Durable review checkpoint — IP-06 S3 promotion

**Date:** 2026-10-25 (America/New_York).

**Input:** `TonePoet_SNAPSHOT_4d6b04c_s3promote_2026-10-25.tar.gz`, SHA-256 `1f1fcf261ac35aa585580489a64989541a71916af58f88bc592f0a91aa196d2e`. A user-supplied `S3_PROMOTION.patch` describes the selector change. Snapshot has no Git metadata; base commits cannot be verified.

**Reviewed outcome:** global default-ON promotion REVERTED. The old `TONEPOET_CUE_S3_PILOT=1` requirement is restored; S3 and IP-07 source ReplayGain code remain. Do not re-enable ordinary S3 selection without the missing hardware/configuration/publication evidence and a narrow policy.

**Source work:** `git apply --reverse S3_PROMOTION.patch` applied cleanly; `src/convert/pipeline/stages.rs` is the only production file modified relative to uploaded snapshot. Two inherited CPython bytecode cache files were excluded from the source release; their hashes and paths are documented separately. `docs/cue_parallel_ip01/ip06_verification/evaluate_trials.py` supersedes historical H-01 evaluator without editing the planning bundle. The cumulative ledger top sections describe the IP-06 rejection/rollback.

**Test scope:** 11 new Python H-01 cases passed, 15 existing trace-summary tests passed, destination verifier self-test passed, 25 independent FFmpeg range PCM slices passed under FFmpeg 7.1.5, and 140 supplied timing rows were internally validated and recalculated. These are not Rust build tests, source-bound commercial-album comparisons, or release-wide execution proof. Cargo/rustc/rustfmt unavailable. See `VERIFICATION_RESULTS.md` for exact command/status, plus standalone logs in this directory.

**Resumption rule:** first verify the *new* full-source artifact by SHA-256 from the external delivery receipt and `sha256sum -c SHA256SUMS`; confirm default-off source expression. Do not work from only the original `S3_PROMOTION.patch`, which represents the rejected default-on candidate. IP-06 remains blocked until native conformance and representativeness gates pass. Keep the original input tarball for historical comparison.

**Next step:** owner may accept this limited rollback and merge it; separate requalification with matched canonical-config/source audio hashes and a hardware-bounded auto-selection policy is a new implementation step. No unseen native qualifications are implied.
