# Upstream Issue Drafts — toolchain defects found by the DSD Reference qualification gate

Status: DRAFTS, not yet filed. Five real toolchain defects were isolated
with minimal reproductions during the DSD Reference P0 qualification
rounds (2026-07-19..21). File when convenient; none block tonepoet (all
are routed around or rejected fail-closed by policy), but upstream fixes
would simplify future policy re-attestations.

---

## 1. sox_ng (file at codeberg.org/sox_ng/sox_ng; also applies to barstoolbluz/sox_ng fork)

**Title:** WAV writer emits 32-bit-wrapped RIFF/data sizes instead of
streaming sentinels for >4 GiB output to unseekable streams

**Version:** sox_ng 14.8.0.1 (also present in 14.6.1)

**Summary:** When SoX-ng writes WAV to an unseekable output
(stdout/pipe) and the payload exceeds 4 GiB, the initial header's RIFF
and `data` chunk sizes are the true 64-bit sizes truncated modulo 2^32,
rather than the conventional streaming sentinels (`0xFFFFFFFF`) used
when the final size cannot be seek-patched. A downstream reader that
honors the declared sizes truncates the stream — for a payload of
4 GiB + 8 bytes, the header declares `data` = 8 bytes.

**Reproduction (Linux, sparse file — no real 4 GiB of I/O):**

```python
# make_big_w64.py — writes a valid W64, f64 mono 48 kHz, data = 4 GiB + 8 bytes (sparse)
import struct, uuid
def guid(s): return uuid.UUID(s).bytes_le
riff, wave = guid('66666972-912E-11CF-A5D6-28DB04C10000'), guid('65766177-ACF3-11D3-8CD1-00C04F8EDB8A')
fmt, data = guid('20746D66-ACF3-11D3-8CD1-00C04F8EDB8A'), guid('61746164-ACF3-11D3-8CD1-00C04F8EDB8A')
fmt_body = struct.pack('<HHIIHH', 3, 1, 48000, 48000*8, 8, 64)
data_size = 0x1_0000_0008
with open('big.w64', 'wb') as f:
    f.write(riff + struct.pack('<Q', 16+8+16+16+8+len(fmt_body)+16+8+data_size) + wave)
    f.write(fmt + struct.pack('<Q', 24+len(fmt_body)) + fmt_body)
    f.write(data + struct.pack('<Q', 24+data_size))
    f.seek(f.tell() + data_size - 1); f.write(b'\0')
```

```console
$ python3 make_big_w64.py
$ sox --info big.w64          # reader is CORRECT: 536,870,913 samples
$ sox -D big.w64 -t wav - | head -c 8 | od -A d -t x1
0000000 52 49 46 46 3a 00 00 00      # RIFF size = 0x3a — wrapped, not 0xFFFFFFFF
```

The `data` chunk size in the streamed header is likewise
`(4 GiB + 8) mod 2^32 = 8`. Expected behavior for unseekable output
where the size exceeds `u32`: write the streaming sentinels so
consumers read to EOF (matching common practice for piped WAV).

**Impact:** any pipeline of the form `sox big.w64 -t wav - | consumer`
silently truncates for >4 GiB audio if the consumer honors declared
sizes. The W64 *reader* is unaffected (verified above).

**Candidate fix:** one-line-class — when the output is unseekable and
the computed size exceeds `u32::MAX`, emit `0xFFFFFFFF` for both RIFF
and data sizes. After an upstream/fork fix: bump the tonepoet flake pin,
re-attest the toolchain closure, and lift any streamed-carrier capacity
cap as a new append-only policy ID.

---

## 2. sox_ng (file at codeberg.org/sox_ng/sox_ng; also applies to barstoolbluz/sox_ng fork)

**Title:** W64 writer finalizes header-only/empty size fields for
all-zero (digital-silence) content, while the full payload is present on
disk

**Version:** sox_ng 14.8.0.1

**Summary:** When SoX-ng writes an **all-zero** payload to W64, both the
RIFF-GUID size field (finalized to the header length) and the `data`
chunk size field (finalized to empty) declare a header-only/empty file,
even though the complete zero-sample payload IS written to disk. A reader that honors the declared sizes (FFmpeg)
correctly refuses the file; SoX round-trips its own broken output because
its reader reads to EOF and ignores the size fields. This is a sibling of
defect #1 (the streamed-WAV >4 GiB size wrap): SoX-ng's WAV/W64 size
accounting has more than one finalization bug.

**Reproduction (Linux; two mono 88.2 kHz Float64 W64 files, 8,820
frames each, both 70,696 bytes on disk):**

```console
$ sox -D -r 88200 -n -e floating-point -b 64 -c 1 tone.w64  synth 0.1 sine 1000 gain -6
$ sox -D -r 88200 -n -e floating-point -b 64 -c 1 zeros.w64 synth 0.1 sine 1000 vol 0

$ sox --info tone.w64 && sox --info zeros.w64   # SoX reads BOTH: 8,820 samples

# RIFF-GUID size field (bytes 16..24, little-endian u64):
#   tone.w64  -> 0x00011428 = 70,696   (correct: whole file)
#   zeros.w64 -> 0x00000088 = 136       (HEADER-ONLY — bogus)
# data-chunk size field:
#   zeros.w64 -> 0x18 = 24              (declares EMPTY payload; correct
#                value = 0x113b8 = 70,584, i.e. 70,560 payload bytes +
#                24-byte W64 chunk header — the payload IS on disk)

$ ffprobe tone.w64    # opens fine
$ ffprobe zeros.w64   # "Invalid data" — correctly honoring the bogus size
$ ffprobe -f w64 zeros.w64   # forcing the demuxer does NOT bypass
```

**Impact:** any all-zero (or effects-quantized-to-zero) W64 SoX-ng
writes is unreadable by size-honoring consumers (FFmpeg, and any strict
W64 parser), while SoX itself masks the corruption by ignoring its own
size fields. Digital-silence carriers and silent lead-in/-out fixtures
are the natural triggers.

**Candidate fix:** one-line-class — in the W64 size finalization path,
compute the final RIFF and `data` sizes from the actual bytes written,
not from a running counter/flag that stays at its initial (empty) state
for all-zero content. Verify the byte-count update fires on all-zero
blocks identically to nonzero ones. Suggested to characterize the exact
trigger (all-zero-whole-file vs first-block-silence vs a threshold) with
leading-/trailing-silence controls while in the code. After the fix:
bump the tonepoet flake pin, re-attest, and lift the all-zero W64
refusal accommodation as a new append-only policy ID.

---

## 3. ffmpeg (trac.ffmpeg.org) — W64 demuxer mis-scales plain-IEEE_FLOAT f64 by 2^31

**Version:** ffmpeg 7.1

**Summary:** Decoding a W64 file whose fmt chunk uses the plain
`WAVE_FORMAT_IEEE_FLOAT (0x0003)` tag with 64-bit samples (as written
by SoX) yields samples scaled by exactly 2^31 (+186.64 dB). ffprobe
identifies the stream correctly as `pcm_f64le`; the corruption is in
decode scaling. ffmpeg's own W64 muxer writes `WAVE_FORMAT_EXTENSIBLE`
and reads its own files correctly. Repro: `sox -r 88200 -n -e
floating-point -b 64 t.w64 synth 1 sine 1000 gain -20` then
`ffmpeg -i t.w64 -af astats -f null -` → peak +166.64 dB (expect −20).
f32-in-W64 with the same plain tag decodes correctly; f64 WAV (RIFF)
decodes correctly.

## 4. ffmpeg (trac.ffmpeg.org) — W64 muxer folds alignment padding into the data chunk

**Version:** ffmpeg 7.1

**Summary:** `ffmpeg -i in.w64 -c:a copy -f w64 out.w64` on a stream
whose data byte length is not a multiple of W64's 8-byte alignment
(e.g. mono 24-bit, 8,820 samples = 26,460 bytes) produces a file that
decodes to one extra phantom sample (8,821): the muxer includes the
alignment padding in the declared data extent. Identical prefix,
zero-valued trailing sample. Aligned sizes round-trip cleanly.

## 5. ffmpeg (trac.ffmpeg.org) — f32 W64 mis-measured via streamed f64 WAV re-container

**Version:** ffmpeg 7.1 (interaction with SoX-ng 14.8.0.1 producer)

**Summary:** Streaming a SoX-written Float32 W64 through
`sox in.w64 -t wav -e floating-point -b 64 - | ffmpeg -f wav -i pipe:0`
measures near full scale (`input_tp ≈ -0.00` for a −20 dBFS fixture),
while direct ffmpeg decode of the same f32 W64 file is correct — the
inverse of defect #2 (f64: direct broken, streamed correct). Needs
joint isolation to attribute producer vs consumer before filing; the
empirical route matrix is recorded in tonepoet's
`docs/handoff_dsd_reference_p0_current.md` (v6 analyzer correction).

---

Provenance: all five were surfaced by tonepoet's DSD Reference
qualification gate (policy lineage `sox_ng_14_8_0_1_v4..v16`) and
isolated with the minimal fixtures described in
`docs/findings_dsd_reference_p0_admission_round.md` (F1/F5/F6 §373,
F10 §812) and the policy handoff docs. The two sox_ng writer defects
(#1, #2) are the fork-fix track; see `docs/handoff_sox_ng_fork_fixes.md`.
