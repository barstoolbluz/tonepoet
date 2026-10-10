# R25 persistent execution checkpoint

**Checkpoint state: source modifications and archive complete; Rust tests and real-tool acceptance pending.** This file is intentionally inside the packaged repository so a new session can resume from the actual source rather than repeating speculative instructions.

## Read order

1. `BRIEF_LOGGING_R25_provenance_field_and_reconvert_removal_2026-10-10.md` (user authority)
2. `R25_DELIVERY_NOTES_2026-10-10.md` (implementation details, changed-test list, required gate)
3. `R25_CODE_CHANGES.patch` (exact reviewable delta against supplied original)
4. `src/convert/pipeline/materializer_sacd.rs` and `src/convert/pipeline/stages.rs` (actual implementation, sole modified Rust files)
5. `R25_CHANGED_SHA256SUMS.txt` and `R25_STATIC_QA.txt` (fixed delivery checks)

## Why the retry gate has three checks

A merely ACTIVE workspace is **not sufficient**: every new conversion creates one before publishing, and that would silently restore the withdrawn feature. A legitimate after-crash retry needs same batch ID, unfinished marker status plus proven ownership of a previous attempt. ACTIVE with a provably dead local owner can be taken over; FAILED represents a handled previous attempt. After takeover, `ownership_generation>1` and `previous_owner_claim_id` preserve that proof. In both audio paths staged bytes and/or coordination fragments must still match. A completed batch cannot be retried as if it were unfinished.

The rollback journal recovery path is unchanged. Never implement the broad request by deleting `files_have_same_contents`, `staged_file_matches_existing_final`, or `publish_coordination_fragment_entry` wholesale: they are used in true publish recovery.

## Next session

Run the exact commands in `R25_DELIVERY_NOTES_2026-10-10.md` on the configured Rust/Nix host. If an R25 test exposes a narrow defect, correct only that defect, add/adjust the smallest regression, rerun gates, and regenerate this checkpoint/patch/checksums. Requalify Reference **once after final source changes**, not between tentative patches. Perform the inherited #43 TUI ISO/preset acceptance separately. Do not claim runtime or Reference acceptance until both are evidenced.

The uploaded original source was a tarball with no `.git`; the original claimed Git commit could not be verified from archive metadata. All changes herein are source-level and locally auditable. No background task or deferred unrecorded work exists.
