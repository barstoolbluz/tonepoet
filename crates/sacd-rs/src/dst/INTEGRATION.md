# PR 2 — DST Decoder Integration Brief

This document is the contract between the DST decoder port (PR 2) and
the rest of `sacd-rs`. The reasoning model porting the decoder from
[Sound-Linux-More/sacd-extract][upstream] should treat this file as
the integration spec: signature, error model, fixtures, and what the
rest of the crate expects.

[upstream]: https://github.com/Sound-Linux-More/sacd-extract

## Upstream source

Port from `libdstdec/` in the upstream C repo (fetch directly from
GitHub; we deliberately do not vendor a snapshot). The decoder is GPL
v2.0; this crate is `GPL-2.0-or-later` to match.

## Public surface

The primary entry point the rest of the crate calls is:

```rust
pub fn decode_frame(input: &[u8], channel_count: u8) -> Result<Vec<u8>, DstError>;
```

(If the port turns out to need persistent state, `DstDecoder` from the
"State" section below becomes an additional public entry point.
`decode_frame` then becomes a thin wrapper.)

Semantics:

- **input**: raw DST-encoded payload of exactly one SACD frame. This
  is the bytes the frame reader hands us via `Frame::data` when
  `Frame::dst_encoded == true`. Length is per-frame variable (3411,
  3011, 2986 bytes observed for the three staged stereo fixtures —
  roughly **3× compression** vs the 9408-byte uncompressed frame).
- **channel_count**: 2 (stereo) or 6 (multi-channel). Must be
  validated by the implementation; any other value is
  `MalformedFrame("invalid channel_count")`. The decoder derives
  everything else (frame rate is implicitly DSD64; frame duration is
  1/75 s; per-channel byte length = 4704 = 64 × 44100 / 75 / 8).
- **return**: decoded clustered-frame DSD, byte-interleaved across
  channels, with each byte MSB-first in time order (oldest sample in
  the high bit) — i.e. **exactly the byte layout `Frame::data` has
  when `dst_encoded == false`**, as produced by the existing
  uncompressed path in `frame.rs`. Length must be exactly
  `channel_count * 4704` bytes (`Vec::len()`, not capacity). This is
  the critical invariant: downstream code (DSF demux, DFF passthrough)
  cannot tell decoded frames apart from native uncompressed frames.

Two cases for the free function:

- **Stateless decoder**: each call is independent. `decode_frame` is
  the only entry point needed.
- **Stateful decoder**: each call allocates fresh state internally
  and discards it on return. Per-call allocation cost is the
  trade-off; if it's prohibitive, expose `DstDecoder` (next section)
  and use that from the orchestrator hot loop.

Either way, the *free function* must not leak state across calls.

## State

If the upstream decoder requires persistent state across frames in a
single track (probability tables, filter coefficients, etc.), expose
it as:

```rust
pub struct DstDecoder { /* … */ }

impl DstDecoder {
    pub fn new(channel_count: u8) -> Self;
    pub fn decode_frame(&mut self, input: &[u8]) -> Result<Vec<u8>, DstError>;
}
```

…and keep `decode_frame(...)` as a thin convenience wrapper that
constructs a fresh `DstDecoder` per call. The orchestrator
(`extract.rs`) will be updated to thread a `DstDecoder` through the
extraction loop if state is required. If frames are independent
(no cross-frame state — TBD by the port author), the free function
is sufficient.

**Decide based on the upstream source, not speculation.** If the C
implementation has a `dst_decoder_t` struct that lives across
`dst_decoder_decode()` calls, mirror it. If it's stateless, expose
only the free function.

## Error model

The staged enum lives in `src/dst/mod.rs` (hand-rolled `Display` /
`Error` impls, no external deps). Variants:

- `UnexpectedEof { consumed: usize }` — bit/byte reader ran past the
  end of the input frame. `consumed` is in **bytes** (whole bytes
  consumed from `input` before exhaustion).
- `MalformedFrame(&'static str)` — frame header or bitstream syntax
  violated the DST spec. Also used for invalid `channel_count` and
  for the decoder producing fewer output bytes than expected.
- `OutputOverflow { limit: usize }` — decoder tried to emit more
  output than `channel_count * 4704`. `limit` is the budget.
- `InternalDecodeError(&'static str)` — catch-all for upstream
  `return -1` paths that don't fit a typed variant. Use sparingly.

If the porter wants `thiserror`-derived `Display`, add it to
`Cargo.toml` and replace the hand-rolled impls. No dependency is
required to ship the port.

Rules:

- **Never panic** on malformed input. Every C `assert()` /
  `return -1` path becomes a `DstError` variant.
- **No `unsafe`** unless absolutely required for performance and
  justified inline with a `SAFETY:` comment. The sacd-rs crate does
  not currently set `#![forbid(unsafe_code)]`; the porter MAY add it
  on a per-module basis (`#![forbid(unsafe_code)]` at the top of
  `dst/mod.rs`) to enforce this rule locally.
- Output length is fixed: `channel_count * 4704`. If the decoder
  finishes with fewer bytes, return `MalformedFrame("short output")`;
  if it tries to write more, return `OutputOverflow { limit }`.

## Fixtures

Six fixtures are staged at
`crates/sacd-rs/src/dst/fixtures/`:

| Frame | Input (DST) | Output (DSD) | Input SHA-256 (head) | Output SHA-256 (head) |
|---|---|---|---|---|
| 1 | 3411 B | 9408 B | `a788eb38…` | `4ba63697…` |
| 2 | 3011 B | 9408 B | `fd77fa6f…` | `d138a1d8…` |
| 3 | 2986 B | 9408 B | `9a788271…` | `506f08c2…` |

Verify integrity with `sha256sum -c SHA256SUMS` (in the fixtures
directory).

Provenance:

- Source ISO: Al Jarreau — *All I Got* (2002), DST-encoded SACD.
- Track 1 ("Random Act Of Love"), stereo area, first 3 frames after
  the time filter (tc=150, 151, 152 — i.e. the 2-second / 150-frame
  silence-trim point that matches `sacd_extract`'s default behavior).
- Inputs extracted by `examples/dump_dst_frames.rs` (read-only ISO
  access). Outputs derived from `sacd_extract -2 -p -c -t 1` decoded
  DFF, audio offset 144, sliced into three 9408-byte chunks.
- Library ISO mtime verified unchanged after extraction (sacrosanct
  constraint).

### Fixture caveats

Three concerns the porter should know about up front:

1. **Pairing is unfalsifiable until the decoder ships.** The DST
   inputs were extracted with sacd-rs's frame reader; the DSD outputs
   were extracted by slicing `sacd_extract`'s decoded DFF at offset
   144. The alignment assumes both paths apply the time filter
   identically and that the DFF's first 9408 audio bytes correspond
   to tc=150. If a decode produces correct-looking but non-matching
   bytes, suspect the *fixture pairing* before suspecting the port —
   then re-derive by decoding a known-good uncompressed track via the
   port and comparing.
2. **Stereo only.** The `channel_count` parameter accepts 6, but no
   6ch fixtures are staged yet (the same source ISO has a 6ch DST
   area; fixtures will be added in PR 3e).
3. **Three consecutive frames at a silence-trim boundary.** These do
   not exercise mid-stream conditions, frame-N-of-N end-of-track
   edges, or pause frames. They cover the happy path only.

Test scaffold (the porter writes this; suggested location
`src/dst/mod.rs`):

```rust
#[cfg(test)]
mod fixture_tests {
    use super::decode_frame;

    fn pair(n: u8) -> (&'static [u8], &'static [u8]) {
        match n {
            1 => (
                include_bytes!("fixtures/frame_001.dst.bin"),
                include_bytes!("fixtures/frame_001.dsd.bin"),
            ),
            2 => (
                include_bytes!("fixtures/frame_002.dst.bin"),
                include_bytes!("fixtures/frame_002.dsd.bin"),
            ),
            3 => (
                include_bytes!("fixtures/frame_003.dst.bin"),
                include_bytes!("fixtures/frame_003.dsd.bin"),
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn frame_1_byte_exact() {
        let (inp, expect) = pair(1);
        let got = decode_frame(inp, 2).expect("decode");
        assert_eq!(got, expect);
    }
    // … same for frames 2 and 3 …
}
```

These three byte-exact gates are the **acceptance criterion** for
PR 2 to merge. Multi-channel (6ch) fixtures will be added in a
follow-up using the same source ISO.

## Integration into `extract.rs`

Once `decode_frame` lands, the orchestrator change is small. Today
`drain_frames` is a free function with a closure sink
(`extract.rs:282`):

```rust
fn drain_frames<F>(
    reader: &mut FrameReader<'_>,
    time_filter: Option<TimeFilter>,
    mut write_data: F,
) -> Result<ExtractStats, ExtractError>
where
    F: FnMut(&[u8]) -> Result<(), ExtractError>,
{
    let mut stats = ExtractStats::default();
    while let Some(frame) = reader.next_frame()? {
        if let Some(filter) = time_filter {
            if !filter.includes(frame.timecode.as_frame_count()) {
                continue;
            }
        }
        if frame.dst_encoded {
            return Err(ExtractError::DstFrameUnsupported);
        }
        write_data(&frame.data)?;
        stats.frames_read += 1;
        stats.audio_bytes += frame.data.len() as u64;
    }
    Ok(stats)
}
```

After PR 2, the DST branch decodes instead of bailing:

```rust
        let payload: Cow<'_, [u8]> = if frame.dst_encoded {
            Cow::Owned(crate::dst::decode_frame(&frame.data, frame.channel_count)?)
        } else {
            Cow::Borrowed(&frame.data)
        };
        write_data(payload.as_ref())?;
        stats.frames_read += 1;
        stats.audio_bytes += payload.len() as u64;
```

Two follow-ups the porter owns alongside the decoder change:

1. `ExtractError::DstFrameUnsupported` becomes unreachable; replace
   it with `ExtractError::Dst(#[from] DstError)` and propagate via
   `?`.
2. `stats.audio_bytes` historically counted `frame.data.len()` (the
   bytes the reader handed over). With the `Cow` sketch above it
   instead counts the *written* payload: uncompressed-frame length
   for non-DST frames (unchanged from today), decoded length for
   DST frames. That's the reasonable default since `audio_bytes` is
   meant to mirror what landed in the output file. The `Cow` import
   is `use std::borrow::Cow;` — the sketch omits it for brevity.
   Verify against `sacd_extract`'s user-visible reporting if it
   matters.

## What the port author does NOT need to handle

- Frame reading / LSN math / clustered-frame parsing — done.
- DSF demux / DFF passthrough — done.
- ID3 / DIIN / COMT / MARK metadata — done.
- Time filter / pause trimming — done.
- ISO access — done (read-only `IsoReader`).

(Note: `scripts/verify_all_tracks.py` is currently hardcoded to Solo
Monk and **must** be extended to Al Jarreau as part of PR 2 — see
"What success looks like" below.)

## What success looks like

1. `cargo test -p sacd-rs --release` passes, including the three new
   `frame_N_byte_exact` tests.
2. `cargo build -p sacd-rs --release` clean.
3. End-to-end: extracting Al Jarreau track 1 produces a DFF/DSF that
   is byte-exact against `sacd_extract`'s output. Validation requires
   extending `scripts/verify_all_tracks.py` (currently hardcoded to
   Solo Monk) with a new `TRACKS` table + `ISO` path for Al Jarreau —
   that scaffolding work is part of PR 2, not a separate task.

No "good enough" — byte-exact or it doesn't merge.
