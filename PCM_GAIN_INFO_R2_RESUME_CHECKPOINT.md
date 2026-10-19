# PCM Gain Info R2 / DC Shift R2E — Resume checkpoint (2026-10-18)

## Source authority
Fresh extraction of `TonePoet_PCM_Gain_Info_R1_FULL_SOURCE_2026-10-18.tar.gz`.
Retain the working PCM gain information Notice unchanged. Fix the two reported typed terminal-admission defects in `tonepoet-pipeline/src/semantic_plan.rs`.

## Confirmed source findings
- Early multichannel DC raw-layout terminal constraint excludes `request.source.is_dsd()` even for Custom DSD-to-PCM + registered native DC + certified true-peak. Existing DSD certified prepass produces a raw Float64 multichannel carrier with a verified layout mask.
- Early check assumes `PreferredTool::Ssrc` is necessarily the terminal rather than checking the already-selected SSRC resampler, even though candidate selection runs separately for resampling and final encoding.
- Additional prerequisite: protected Binary64 SSRC + post-resample native DC was refused for a distinct reason: the native DC effect's typed transform contract is `plain_transform()` and does not record the native worker's verified Float64, nonclipping behavior. The worker itself performs finite Float64 subtraction without integer narrowing, clipping, or dither and refuses nonfinite output.

## Planned changes
1. Extend early terminal candidate constraint to Custom DSD split true-peak with multichannel DC; preserve Reference restrictions.
2. Admit an explicit SSRC preference at that terminal *only* if the already-selected resampler is SSRC, while requiring FFmpeg terminal capability. Do not permit SoX terminal substitution.
3. Bind the existing native DC effect to its actual Float64/no-clamp output contract, so a commissioned protected SSRC cell with post-resample DC can genuinely satisfy the existing precision/overload obligation; leave all other effects unchanged.
4. Add focused typed-plan tests: DSD Track/Album positive and negative; SSRC commissioned protected positive and uncommissioned/terminal proof negative; untouched stereo, legacy, Reference paths.
5. Execute available source/FFmpeg regressions, archive with manifest and honest Rust toolchain limitations.

## Qualification
No Rust/Cargo/Nix toolchain detected here; Rust checks and TonePoet end-to-end audio qualification must run on build host. Update this checkpoint and engineering handoff with actually executed commands/results and SHA-256.

## Implemented correction and current state

- Production changes are confined to `tonepoet-pipeline/src/semantic_plan.rs`. The DSD multi-channel raw-layout terminal obligation no longer incorrectly excludes Custom DSD; Reference delivery remains excluded.
- An explicit `PreferredTool::Ssrc` is accepted at the terminal boundary only when a prior typed resampler node actually selected SSRC. The FFmpeg layout obligation, terminal proof and unchanged final mask assertion continue to apply.
- A separate existing refusal was found: native Float64 DC had not declared its own overload-preserving transform contract, so protected SSRC -> post-resample DC could not be admitted. Declared actual native finite Float64 arithmetic/no-clamp capability on DC candidates only and their typed output states. Native worker unchanged.
- Tests added: four `tonepoet-pipeline` unit tests covering Custom DSD Track/Album, positive/negative SSRC evidence and terminal admission, and native DC contract; one `tonepoet` bridge test for six-channel DSD raw mask.
- Scope comparison: only two original Rust files modified. PCM gain Notice files unchanged.

## Completed validation

See `PCM_GAIN_INFO_R2_DC_SHIFT_R2E_ENGINEERING_HANDOFF.md` for exact test commands and limitations. New source static audit 16/16, prior static audits 15/15 and 13/13, channel/DC 13, layout 14, DVD-A 6, FFmpeg 3+2+1 passed. `nix`, `cargo`, `rustc` absent. New Rust tests NOT RUN; no executable TonePoet conversion was performed here.

## Resume next

1. Verify new source manifest `sha256sum -c PCM_GAIN_INFO_R2_DC_SHIFT_R2E_SHA256SUMS.txt` at root after archive extraction, then enter pinned `nix develop`.
2. Run Rust format, all-target workspace check, focused typed planner and bridge tests, full gates and release build, as in engineering handoff. Fix **demonstrated** compile/test errors only.
3. Run real supported multichannel Custom DSD/SSRC encoded-output fixtures with verified channel roles, true-peak, shift and honest evidence. If commissioning forbids a cell, refuse with the prior proof/capacity reason; do not forge proof or weaken terminal qualification.
4. Stop once all actual requirements are satisfied. No new scheduler, DSP framework or unnecessary hardening is warranted.

The complete full-source bundle and adjacent archive SHA-256 checksum are the resumption artifact; historical manifests in the tree refer to older source versions.
