# Implementation report — album-gain carrier corruption on 57d1e0c

Date: 2026-08-28
Target baseline: `57d1e0c38638344966b43b1926bc58bf0ab34404` (v0.4.9)
Status: carrier correction applied to target source state; mandatory Nix gate cannot be executed in this runtime.

## Correction

No carrier redesign was introduced beyond the reviewed fix. Album-scoped DSD
auto-gain now retains post-reconstruction audio as headerless little-endian
Float64 PCM (`.f64le`). SoX writes raw Float64 little-endian samples. Consumers
bind the raw facts explicitly instead of relying on container autodetection:

- FFmpeg: `-f f64le -ar <rate> -ac <channels>`.
- SoX: raw input, floating-point, 64-bit, little-endian, explicit rate/channels.

The carrier remains post-reconstruction PCM, so there is one expensive DSD
reconstruction per track. Submitted-batch album gain is still applied once.
Scratch retry continues to use the retained carrier. Silence verification is
in-place and does not create a second full-size proof carrier.

## Baseline correction

The prior delivery was authored against parent `1c8d87d`. This delivery instead
reconstructs the requested `57d1e0c` source state from that exact parent
snapshot plus the five-file public 57d1e0c delta. Every changed target blob was
verified against the Git object ID published by the 57d1e0c patch:

- implementation report: `ff723413`
- `src/concurrency.rs`: `aa8d7233`
- `src/convert/cap_fs.rs`: `0d826b95`
- `src/convert/pipeline/stages.rs`: `b7ca94bb`
- `src/db.rs`: `c2287a4a`

The existing carrier patch applies to this target state cleanly with
`--whitespace=error-all`; no context resolution was required. Reverse
application reproduces the reconstructed target source state byte-for-byte.

## Verification completed here

SoX 14.4.2 -> explicit FFmpeg 7.1.5 raw-f64le consumption was measured at a
known -6 dBFS level. Stereo returned -6.00 dBFS peak. Six-channel input retained
all six channels and returned -6.00 dBFS peak on every channel. Raw files were
frame-aligned (`size % (8 * channels) == 0`).

The regression test remains present:
`sox_written_album_carrier_survives_production_ffmpeg_consumer_at_known_level`.
The included certification script sets `TONEPOET_REQUIRE_TOOLS=1` for its
focused invocation so absent SoX/FFmpeg is a failure, not a skip.

## Mandatory gate still unavailable in this runtime

This runtime has no `nix`, `cargo`, or `rustc`. Its execution environment also
cannot resolve external DNS, so Nix cannot be bootstrapped here. Therefore I do
not claim the work order's compilation/test certification, warning status, or
handoff-ready verdict.

The remaining required commands are exactly:

```text
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --workspace --no-fail-fast
cargo test --workspace --no-fail-fast
```

Every `test result:` line must report `0 failed`, both times. The focused
cross-tool regression must execute rather than print its skip message. The
included `RUN_REQUIRED_CERTIFICATION.sh` automates those checks without changing
the production implementation.
