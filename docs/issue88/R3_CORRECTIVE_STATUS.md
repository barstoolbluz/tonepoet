# Issue #88 R3: Four residual corrections

**Source state:** The four R3 changes are implemented in the source tree. Build-host and authentic-disc acceptance remain unverified.

**Base:** R2 HEAD `9d21de030c1cf66b756e4322f4d5fb5cd86e8e0b`.

| Finding | Minimal correction | Focused regression / remaining qualification |
| --- | --- | --- |
| R3-01 SACD tag authority | `DiscBrowserState::native_job` resolves track fields against album fields **within each** authored or sidecar authority before applying the selected authority priority. The existing tag writer is unchanged. | Added regression for authored 1998 versus sidecar track DATE=2024, genre and title conflict, both priorities, missing fields and Tags off. Run Rust tests and inspect persisted DSF tags from an actual fixture. |
| R3-02 SACD continuous capacity | `required_native_volume_bytes` now budgets retained extracted SACD tracks + streaming joined SACD output + per-item header allowance. The PCM continuous path retains its larger spool/carrier reserve. | Added 10 GiB / 25 GiB admit and 19 GiB reject checks; existing PCM twenty-chapter test retained. Qualify actual peak storage and DST/DSF continuous joins on build host. |
| R3-03 Space capacity | The results line now uses the destination's available-space measurement for both destination and working-storage figures; the native worker always stages on that volume. The blocker already measures the destination volume. | Added renderer regression with no configured scratch, same-volume scratch and different-volume scratch. Validate with the real mounted-volume UI. |
| R3-04 SACD small header | SACD diagnostic count and `d view` are retained in the two-line 64x20 header, regardless of authored details. Nonzero diagnostics use the warning accent; SACD authored details stay in medium/wide. | Added synthetic 64-column and 96-column renderer tests with/without details and diagnostics, including color. Verify popup navigation in live TUI. |

## Boundaries respected

No edits to Convert, SACD joining, raw PCM/DST realization, extraction worker staging, job queue, cancellation, publication, source protection, or tag persistence. The established metadata editor still saves a source sidecar on an explicit Save as the original #88 specification permits.

## Verification

- `git diff --check`, Git fsck and replay checks are required for packaged source.
- The independent `verification/check_header_contract.py` test only checks synthetic RIFF/RF64/Wave64 references with FFprobe. It is **not** evidence that the Rust implementation compiles or executes.
- Run `bash verification/verify_on_build_host.sh` after extraction on a Rust-capable host (Rust >= 1.93, Cargo, rustfmt, project system dependencies). It now selects the new R3 renderer tests along with the prior suites.
- Test actual SACD tag writes, native DSF/DFF output, true disk-space peaks and 64x20/96x30 TUI with representative fixtures before production integration.
- No Rust compiler or Cargo is available in the current environment, and the Rust distribution host cannot be resolved; compilation and runtime checks cannot be certified here.

**Disposition:** Focused source corrective checkpoint pending build-host and real-media validation. Not a production release.
