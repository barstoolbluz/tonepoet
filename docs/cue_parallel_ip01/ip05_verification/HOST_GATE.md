# IP-05 host qualification — required before acceptance

## Inputs and controls

Extract the **exact** supplied IP-05 full-source archive whose SHA-256 is
bound in the external delivery receipt. Keep the accepted IP-04 source archive
(`de883774...defa0`) available as the S1 control. Use the same FFmpeg/SoX,
source files, target settings and publication filesystem. The currently
reported build host has FFmpeg 7.1.3, 32 CPU cores and NVMe; those values
are **owner-reported**, not independently verified here. Capture actual tool
versions and binary/source hashes. The user-facing destination set is exactly
requested audio plus enabled log, CUE and artwork, **no** internal marker.

## Required command gate (save stdout/stderr and exact exit status)

```sh
sha256sum TonePoet_CUE_IP05_FULL_SOURCE_<ACTUAL_HASH12>.tar.gz
python3 -B docs/cue_parallel_ip01/ip05_verification/verify_static_ip05.py
python3 -B docs/cue_parallel_ip01/ip05_verification/verify_destination_files.py --self-test
cargo fmt --all -- --check
cargo test --workspace --exclude tonepoet-true-peak cue_stream -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue -- --nocapture
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
cargo test --workspace --exclude tonepoet-true-peak cue_ip05_pilot_opt_out_and_nonidentity_rejections_are_whole_album -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_ip05_pilot_incomplete_materialized_album_falls_back_before_spawn -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_ip05_pilot_waits_for_shared_permit_and_reaps_on_cancel -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_ip05_pilot_selected_real_three_tracks_16_24_off_and_both -- --nocapture
cargo test --workspace --exclude tonepoet-true-peak cue_ip05_pilot_native_twelve_track_rg_mode_matrix -- --ignored --nocapture
```

**Note:** The named filter must discover and run at least one test. A zero-test
filter is NOT PASS. No `cargo` commands were run in the implementation
container (there is no Rust toolchain); the host must run the commands and
record durations/exits. Verify known input suite context: latest host reports
7618 passed and two non-IP-05 failures (Reference lock-source drift and a
concurrency-contention flake). Do not misattribute these to IP-05; rerun in
isolation and document.

## Real Convert UI / API end-to-end acceptance

1. With `TONEPOET_CUE_S3_PILOT` **unset**, convert each reference single-FLAC
   CUE to source-rate/depth FLAC with ReplayGain Both (actual UI default) and
   Off. Record S1 route, complete destination tree, source/output PCM SHA-256,
   tags and artwork, samples/depth, native ReplayGain evidence, command records
   and visible publication time.
2. Set `TONEPOET_CUE_S3_PILOT=1` in the actual conversion process. Repeat
   both modes for 12-track 16-bit and 24-bit albums, preserving every other
   setting. Observe explicit `s3_pilot_decision.selected=true`; compare **each
   decoded PCM sample/channel**, frame count/depth, all tags including repeated
   fields/artwork and exact ReplayGain pairs to the S1 references. Include
   Track, Album, Both, Off, and SkipIfComplete with both complete and missing
   inherited tags. Compare the full album, not just early tracks.
3. Test opt-in rejections for incomplete selection (12 authored tracks, 1–2 selected,
   and a fully selected album expressed as Range/Set), multi-physical-FILE,
   non-FLAC, metadata-only CUE, indexed gaps/HTOA, HDCD, de-emphasis,
   true-peak/fixed gain, dither, rate/depth change, resample, registered
   effects, unqualified probe, insufficient two-FFmpeg budget and non-Unix.
   Show **no S3 child launched** for each rejected whole album, while the
   original fallback continues correctly. For gaps/HTOA eligible cases,
   compare authored sample starts and source ordinals; do not change semantics.
4. Inject decoder/encoder spawn, short/surplus bytes, slow consumer,
   mid-encode error, native RG writer/meter error, metadata/validation
   failure, cancellation during permit wait/encode/RG and cross-device
   publisher rename/copy. Check S1-equivalent FailAlbum and AllowPartial
   behavior, owned child termination, private scratch cleanup, bounded live
   children/FDs, retry and exactly-once album publication. A real S3 child
   failure must never trigger a second S1 conversion.
5. Inspect the **visible destination** for every route (including failure,
   cancellation and cross-device). Write outside it a JSON list containing
   *exactly* the audio, enabled conversion.log, CUE sheet and artwork actually
   requested, then execute:

   ```sh
   python3 -B docs/cue_parallel_ip01/ip05_verification/verify_destination_files.py \
     --destination /real/visible/output --expected-json /other/place/expected.json
   ```

   The host must reject **any** dotfile, lock, marker, manifest, unrequested
   log/evidence or staging directory anywhere in the destination. No control
   file is permitted there even transiently; inspect during conversion too.
6. Run parallel sessions/partial failures, evaluate worker scheduling and
   global semaphore coexistence. Compare host actual 14.9 s (RG Off) and
   18.4 s (Both) **only as owner-reported baselines**; IP-05 itself does not
   establish S3 whole-run speedup. Do not make admission policy changes in
   this packet: IP-06 determines performance and possible promotion.

Report full source, binary and fixture digests, tool versions, exact
output/metadata hashes, command invocations, test results and failing cells.
Only after *all* required conformance gates pass may the owner mark IP-05
**accepted**, then supply the exact SHA-bound IP-05 archive to IP-06.
