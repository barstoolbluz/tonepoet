# TonePoet R35 implementation and resume checkpoint — 2026-10-12

**Delivery status: source correction packaged, static integrity verified, Rust/Reference acceptance open.**

## Task

Correct the lone reproducible assertion in `r33c_album_view_discnumber_follows_selected_carrier_authority` reported by `BRIEF_LOGGING_R35_album_view_fixture_2026-10-12.md`. The input fixture has one-track-per-file FLACs with per-file sidecar CUEs in its second half. Their selected native authority is `File tags`; manufacturing a synthetic image `Album view` would violate the existing structural boundary.

## Engineering decision

- **Production logic stays unchanged**. Treat the problem as a test assumption error, not a reason to change folder grouping, metadata authority, Save routing, or performance paths.
- Keep the original two-track-per-image sidecar-authority half intact.
- For the native one-track-per-file half: assert `File tags` as the initial authority, check both FLACs are in the active presentation and both CUE alternatives remain available; add an initially absent `DISCNUMBER` field through the actual Add Key handler; use explicit positional 1/2 values, not an implied Album-view partition; call the production Save path; prove native files changed, both CUEs did not, and the values reopen as file-scoped, clean metadata.
- Existing regression test name is preserved for script compatibility.

## Files

- **Only existing source file changed:** `src/tui/keybindings.rs`, in one test. No DSP or production Rust file was changed.
- `R35_FROM_INPUT.patch`: exact source delta against the attached R35 input. Apply to an **unmodified** R35 input tree using `patch -p1 --fuzz=0 < R35_FROM_INPUT.patch`; never reapply to the already-corrected full bundle.
- `R35_ACCEPTANCE.sh`: preflight, complete compile target check, focused tests, chained R34D-to-Reference qualification, and a full workspace test gate.
- `R35_STATIC_AUDIT_2026-10-12.md`: verification details, limitations, source identities.
- `R35_SOURCE_SHA256SUMS.txt`: **authoritative checksum inventory of all regular files in this delivered tree**, excluding itself. The directory symlink is preserved and checked separately.

## Resume/qualification instructions

1. Unpack the R35 full-source tarball and enter `tonepoet/`.
2. Run `sha256sum -c R35_SOURCE_SHA256SUMS.txt` (the earlier R34 inventory is historical and stale by one file already in the supplied input).
3. On a host with Rust 1.93+, Cargo, audio fixtures, and **a real SACD ISO**, run `bash ./R35_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso`. Its Reference gate must not be bypassed by environment override.
4. If R35's corrected test fails, inspect the **specific failing assertion** and actual Save result; do not broaden production authority or grouping semantics without an independently demonstrated defect.
5. If Reference qualification fails due the pre-existing `stages.rs` source lock, refresh/requalify using the existing script as required and rerun the full acceptance. A scanner contention flake is distinct from R35.
6. If any source change is necessary, regenerate `R35_FROM_INPUT.patch` and the R35 inventory, verify patch application/reversal, and rebuild the full archive. Do not reuse hashes from this delivery after edits.

No asynchronous job is running. This checkpoint, patch, full tree, and brief are persisted **in the delivery archive**, allowing a subsequent session to resume with no lost implementation decisions.
