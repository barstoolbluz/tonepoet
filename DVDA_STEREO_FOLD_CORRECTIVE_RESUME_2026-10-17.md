# Resume checkpoint — DVD-Audio speaker-order corrective

Authoritative base: `TonePoet_speaker_layout_corrective_FULL_SOURCE_2026-10-17.tar.gz`
(complete previously delivered speaker-binding candidate). Current repository
contains that candidate plus this localized correction.

## Implementation complete in source tree

- `src/convert/pipeline/stages.rs`: selects WFX LPCM source order for DVD-A
  `StereoFold` and binds the FFmpeg pan layout only when DVD-A source assignment
  and the actual WFX carrier mask agree. No change to non-DVD layout binding.
- `src/convert/pipeline/dvda_realize.rs`: policy-specific WAV cache name and
  cache/fresh expectations, `for_stereo_fold` policy selector, regression tests.
- `validation/dvda_stereo_fold_corrective.py`: physical reproductions and
  corrected output, including separate cached WFX carrier readback.
- `DVDA_STEREO_FOLD_CORRECTIVE_DELIVERY_2026-10-17.md`: evidence and build gate.

## Results obtained

- FFmpeg/SoX: 3/3 newly targeted DVD-A synthetic audio cases.
- Prior FFmpeg/SoX suites: 14/14 + 13/13.
- Independently compiled C++ foo_input_dvda reference: 36/36 positional checks
  for assignments 3 and 19.
- `git diff --check` passed.

## Remaining qualification blocker

**Rust toolchain unavailable in this container**, no workspace compile or test
claim. On a build host, run `cargo check --workspace` and
`cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak` first.
Then verify real DVD-A LPCM decoding, true-peak/terminal integration, and cache
reuse per the delivery report. If compilation or a real decoded-output test
fails, implement only a demonstrated correction. Do not broaden this into
channel-layout framework work, MLP redesign, scheduler or DSP changes.
