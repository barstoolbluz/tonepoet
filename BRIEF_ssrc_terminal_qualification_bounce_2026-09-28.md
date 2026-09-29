# Bounce: SSRC certified-terminal qualification fails on this machine (#48)

2026-09-28. Base: branch `apply/ssrc-terminal-long-names-2026-09-28` @ f21790e, which is
main da688cd plus your R2 delivery plus four operator fixes confined to delivered tests
(listed in that commit). Bundle: `tonepoet_ssrc_terminal_qualification_bounce_2026-09-28.tar.gz`.

I am fallible. The measurements below were taken once, on one machine; my reading of their
causes may be wrong. The raw report is in the bundle so you can check.

## What happened

The delivery compiles with zero warnings and its tests pass. The operator-machine execution
qualification you specified was run exactly as documented: production grid, channels 1,2,
16 jobs, SSRC 2.4.2 at the flake's store path (sha256 502af766…). It ran 4,536 cells in
5 h 28 min and reported `passed: false`, 2,154 cells failed. The registry therefore stays
empty and the certified SSRC route fails closed, so nothing changed for the user.

Files in the bundle: `outcome_operator_2026-09-28.json.gz` (full report, 83 MB inflated),
`outcome_operator_2026-09-28.summary.json` (one line per cell: key, failure class, observed
peak at 0 dB), and `ssrc_repro/` (the hand reproductions below).

## Failure classes

| class | cells | what I found |
|---|---|---|
| saturation premise | 1,194 | all upsampling pairs; not one upsampling cell passed |
| Wave64 alignment padding | 447 | Int8, Int16 mono, Int24 mono and stereo |
| Wave64 truncated chunk header | 345 | same family, other geometries |
| SSRC exit 255, usage text | 168 | every cell with target 176,400 Hz and `--dither 99 --pdf 1` |
| passed | 2,382 | all downsampling |

## 1. The fixture is a Nyquist tone, so every upsampling cell overshoots

`fixture_frames` applies `scale = -scale if (channel + i) & 1`, which negates every other
sample of channel 0. Each DC plateau therefore becomes a full-amplitude square wave at the
source Nyquist frequency. Resampling that upward puts the signal on the anti-alias filter's
edge, and the ringing is large:

| resampler, fixture 176.4 kHz to 192 kHz | output peak (input peak -4.5 dBFS) |
|---|---|
| SSRC 2.4.2 `--profile high --bits -64` | +8.25 dBFS (2.586 linear) |
| FFmpeg soxr precision 33 | +1.31 dBFS |
| sox_ng `rate -v` | clipped at 0 dBFS |

So the harness's own `no_saturation_premise` fails at 0 dB and above on every upsampling
pair. SSRC is not at fault: a -24 dBFS sine resampled by SSRC across every up and down pair,
all three profiles, comes back within 0.1 dB, and at the same rate it is bit-exact in level.
Downsampling cells "pass" only because the filter removes the Nyquist tone; their observed
peaks are 0.13 to 0.97 of the input, so the error bounds they derive come from a signal that
is mostly gone. I do not think those 2,382 passes are evidence of anything.

## 2. SSRC pads Wave64 data chunks to 2 bytes, not 8

A 16-bit mono output with 17,833 frames is written as data chunk size 35,690 (24 + 35,666)
with 2 trailing bytes; the file is 35,772 bytes, not a multiple of 8. The Wave64 spec pads
chunks to 8. `parse_w64` in the harness refuses this ("invalid Wave64 alignment padding" or
"truncated Wave64 chunk header"). The runtime's exact Wave64 validation in `stages.rs` uses
the same rule, so even a promoted cell would be refused at conversion time for every geometry
whose payload is not a multiple of 8 bytes. `w64.rs` already tolerates the equivalent
FFmpeg pad for the Reference Int32 terminal; this reader does not.

## 3. SSRC has no dither table at 176.4, 352.8 or 384 kHz

`ssrc --dither help` lists tables for 44.1, 48, 88.2, 96, 192 kHz and the three low rates
only. Any `--dither` id, 99 included, at a 176.4 kHz target makes SSRC print usage and exit
255. The harness enumerates `(99, triangular)` for every target rate. Production's direct
route maps TPDF and None to id 99 for every rate (`mapping.rs`, `ssrc_dither_selection`);
whether its `SsrcDitherAvailability` resolver refuses 176.4 kHz cleanly before building the
command, I did not verify.

## Outcome required

- A qualification fixture that is what its comment says: ordinary audio amplitude, no
  content at or above the source Nyquist frequency by construction, cross-channel
  discrimination done per channel, not per sample. Its saturation premise must hold on every
  cell of the production grid at every gain in the corpus, up and down.
- Wave64 outputs padded to 2 bytes are accepted by the harness and by the runtime validator,
  exactly as the existing FFmpeg pad tolerance is, without weakening the payload-length check.
- Cells that SSRC cannot execute are never enumerated; the grid derives its dither cells from
  what SSRC exposes for each target rate, and the direct route refuses such a selection with a
  clear message rather than an invalid command.
- The full production grid passes on this machine, so the registry can be promoted. The run
  is long; anything that reduces the 5.5 h without reducing evidence is welcome but not
  required.
- Everything in the original brief still stands, including the standing rule that no hidden
  files or markers are left in any output folder.
