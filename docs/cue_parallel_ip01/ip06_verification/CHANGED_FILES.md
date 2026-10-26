# IP-06 owner-accepted default-on change — 2026-10-26

**Baseline:** `TonePoet_CUE_IP06_FULL_SOURCE_3d561894f1a4.tar.gz`, SHA-256 `9722a3e9551fa2f2e257a3d690ef9c6e19945072c61375d01fb95875d736d921` (the prior independently reviewed default-OFF source).

- **Production:** `src/convert/pipeline/stages.rs` only. Defaults to an S3 request unless `TONEPOET_CUE_S3_PILOT` is exactly trimmed `0`. Existing preflight, workers, ReplayGain and publisher remain unmodified. Restores the former original-candidate selector and unit assertions, with revised honest comments.
- **Current cumulative status headers:** `ARCHITECTURE_DECISIONS.md`, `CONFORMANCE_MATRIX.md`, `VERIFICATION_RESULTS.md`, `IMPLEMENTATION_STATUS.md`, `NEXT_SESSION_HANDOFF.md` and `CHANGED_FILE_SUMMARY.md`, preserving prior status blocks as historical.
- **Decision/receipt:** Current `ip06_verification/DECISION_RECORD.md`, `CHECKPOINT.md`, `README.md` now reflect the owner-selected default ON; previous review decision/checkpoint are copied verbatim to dated historical paths. New pair of patch files and a post-review source audit are additive.
- **Package metadata:** Root `SHA256SUMS` and `ip06_verification/SOURCE_MANIFEST.json` regenerate on packaging; the deterministic packager now records the default-ON decision, previous archive input hash, current epoch and artifact naming.
- **Unchanged:** Other Rust code, historical planning authority, IP-07 status/evidence and the IP-06 H-01 evaluator/tests and reported host timing data. Nothing in the previous evidence has been represented as newly run native tests.

No Git history exists in the supplied archives. File comparisons are based on SHA-256 of source bytes and the independent tar archive baseline; see `DEFAULT_ON_SCOPE_AUDIT.json` and external delivery receipt.
