# Brief: SSRC inside the certified true-peak chain (#48) and long album names at Publish (#51)

2026-09-27. Base: main @ da688cd (v0.5.3), bundle
`tonepoet_ssrc_terminal_and_long_names_bundle_2026-09-27.tar.gz`. Full record: `docs/OUTSTANDING_ISSUES.md`
#48 and #51; #47 is the log omission related to #48.

I am fallible. The description below is my reading of the code and one field conversion;
it may be wrong in places. Verify before building on it.

## The requirement

When the user selects SSRC as the resampler, SSRC resamples and performs the dither and noise
shaping of the output. That must hold whether or not the certified true-peak gain stage is in
play. This requirement was stated by the user before #48 was filed; it is not an inference.

## What happens today

- Direct route (no true-peak gain): SSRC resamples and dithers with the selection mapped
  per target rate in `tonepoet-pipeline/src/mapping.rs` (`ssrc_dither_selection_for_rate`).
- True-peak gain on: the chain is measure-then-gain. SSRC is admitted only as a Binary64
  resampler (`tonepoet-pipeline/src/ssrc_binary64.rs`, 42 commissioned rate pairs) that
  produces the Float64 carrier. The terminal quantizer is always the commissioned SoX
  quantizer with TPDF, or the FFmpeg Int32 triangular terminal. The user's SSRC dither and
  shaping selection is silently overridden, and the log says "Dither: yes (TPDF via SoX)".
- Field case: Journey, Greatest Hits, 176.4 kHz / 32-bit FLAC to 88.2 kHz / 24-bit FLAC,
  SSRC selected, PCM true-peak gain on. The pipeline observer shows SSRC ran once and SoX
  twice per track; the conversion log shows only the SoX terminal.

## Outcome required

- With SSRC selected and true-peak gain on, SSRC is the certified terminal: it performs the
  resample, the bound gain, and the user's selected dither and noise shaping.
- The hard-ceiling guarantee is unchanged: the SSRC terminal carries certified error bounds
  of the same kind the SoX and FFmpeg terminals carry, and its qualification evidence is
  execution evidence produced on the operator's machine by the existing harness, not
  hand-authored.
- The user's dither selection is honoured on this route, and the conversion log names the
  tool that dithered and the shaping used (this also closes #47 for this route).
- Conversions that do not select SSRC are unchanged, bit for bit.

## Scope boundary

The DSD Reference path (`tonepoet-pipeline/src/dsd_reference.rs`) decimates and quantizes
with sox_ng and FFmpeg only; SSRC is not selectable there, and this brief does not change it.

## Second item: Publish fails on album names near the filename limit (#51)

Bach, Cantatas Vol. 1, SACD ISO, converted from the TUI with a preset whose folder template
is `%ARTIST% - %ALBUM% (%YEAR%) [%FORMAT%] {%TITLE_EXTRA%}`. The album directory name comes
out at 244 bytes. Every stage succeeds; each track then fails at Publish with
`io error: File name too long (os error 36)`, and that line is all the user sees.

What I believe happens: the 244-byte name fits ext4's 255-byte limit, but Publish derives
sibling names from it, `.<name>.tmp-…`, `.<name>.backup-…`, `.<name>.lock`
(`src/convert/pipeline/stages.rs`: `backup_dir_prefix`, `cleanup_orphan_publish_temps`,
`album_lock_path`), and those overflow. Nothing checks or clamps component length anywhere in
naming or publish, so a template that expands past 255 bytes fails the same way. Multi-artist
classical metadata makes `%ARTIST%` alone exceed 100 bytes routinely. The same conversion with
`--folder-naming "%ALBUM%"` converts 22/22.

### Outcome required

- An album directory or file name the target filesystem cannot hold is detected before any
  audio work starts and shortened deterministically; the shortening is disclosed in the status
  line and the conversion log.
- Publish's derived sibling names never push a legal album name over the limit.
- This disc converts with the preset's template unchanged.

## Constraints

- Files under the Reference source lock are expected to change; the operator will requalify on return.
- No back-compat scope; tonepoet has no users.
- Standing rule, every delivery: the destination receives only the user's artifacts, the audio
  files and, when enabled, the conversion log, CUE sheet and companion artwork. No hidden files,
  manifests, markers or locks may be written into or left in any output folder on any route.
