# Issue #88 R4: Two localized source corrections

**Base:** R3 HEAD `2f88777da97e3ab8fab54b069642df89be5228c3`.

**Disposition:** Both reported defects are real and have narrowly scoped source corrections. Rust build and real-media qualification are still outstanding.

## R4-01: Separate-track SACD disk-space preflight

`DiscBrowserState::required_native_volume_bytes()` used the PCM `total + 3 * largest + allowance` estimate for separate-track SACD. The SACD materializer prepares source references and the worker extracts and publishes one track at a time, so those three PCM carrier copies do not exist.

The existing calculation now distinguishes three cases:

- **Separate-track SACD:** selected output bytes plus 128 KiB per selected item for container and metadata allowance.
- **Continuous SACD:** twice the selected bytes plus the same allowance; the joined file coexists with the retained track files.
- **PCM:** unchanged formula with the existing decoded-carrier and, for continuous output, raw-spool reserve.

Focused Rust test `sacd_separate_tracks_preflight_uses_selected_payload_without_pcm_reserve` constructs four 15-minute stereo DSD64 tracks. The selected payload is 2,540,160,000 bytes. The corrected estimate admits 3 GiB and rejects 2 GiB. The pre-existing 10 GiB continuous SACD and 20-chapter continuous PCM regressions remain in place.

## R4-02: SACD filename metadata source priority

`native_extract::name_value()` previously used the navigation title for `{title}` and the program year for `{year}` and `{date}`, even when the selected SACD metadata authority resolved different track values.

The resolver now prefers `track.tags.title` over `track.name`, and `track.tags.year` over `program.tags.year`. All existing sanitization, token syntax, and destination publication logic remain unchanged. The non-SACD job builders already set these fields consistently or omit them, so their navigation-title and program-year fallback remains intact.

Focused Rust test `extracted_sacd_filename_uses_the_resolved_tag_authority` builds a `NativeExtractionJob` through `DiscBrowserState::native_job()` with conflicting disc/sidecar track titles and dates. It checks both authority priorities against the rendered DSF destination names, requested tag values, track-to-album year fallback, missing-title navigation fallback, and non-SACD fallback. It does not require a disc fixture to calculate the proposed path; actual embedded tags still need a native media test.

## Engineering boundary

Only `src/tui/disc_browser.rs`, `src/convert/pipeline/native_extract.rs`, and this status file changed relative to R3. The R3 SACD tag-resolution work, native DSF/DFF joining, Convert, job lifecycle, private staging, cancellation, and atomic publication are untouched.

## Verification and remaining qualification

- `git diff --check`, manifest SHA-256 checks, Git fsck and patch replay are performed by the archive verifier.
- `verification/check_header_contract.py` exercises synthetic WAV reference cases only; it is not a Rust test.
- `bash verification/verify_on_build_host.sh` was attempted and stopped at `missing rustc (requires Rust >= 1.93)`; Rust/Cargo are not installed and the distribution host was not resolvable. The new Rust tests were **not executed** in this environment.
- On a Rust >= 1.93 build host, execute `bash verification/verify_on_build_host.sh`. Measure actual peak destination-volume usage for separate-track and continuous SACD, then verify authored/sidecar filename and DSF tag agreement on representative fixtures. Repeat the existing PCM continuous test.

No further general hardening is requested in this corrective.
