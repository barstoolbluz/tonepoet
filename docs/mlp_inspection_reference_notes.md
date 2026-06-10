# MLP inspection reference notes

The uploaded `foo_input_dvda` LPCM reference archive is useful for the shared DVD-Audio stream metadata path, but it is not an independent MLP frame-inspection oracle.

Observed reference coverage:

- `dvda_block.h` defines the DVD-Audio Private Stream 1 sub-header, including `MLP_STREAM_ID = 0xA1` and the named 5-byte MLP extra-header fields ending in CCI; the real AOB fixtures added in v34 declare `extra_header_length = 6`, with CCI still at offset 8 and one trailing reserved/padding byte.
- `audio_stream_info.h` defines `STREAM_TYPE_MLP = 0xbb` and `STREAM_TYPE_TRUEHD = 0xba`.
- `audio_stream_info.cpp` defines the 21-entry MLP/PCM channel-assignment table and derives WAVEFORMATEXTENSIBLE masks from it.
- The provided files contain the LPCM group decoder (`pcm_audio_stream_t`) but do not contain an MLP access-unit parser, MLP parity validator, major-sync parser, or MLP decoder.

Product decision in v30:

- Keep the MLP inspector because it yields useful payload/frame/major-sync audit data and can detect clearly wrong elementary streams.
- Run it in advisory mode by default: inspection failures warn and ffmpeg still gets the extracted MLP payload.
- Enforce inspection only when `TONEPOET_DVDA_PHASE3_STRICT_MLP_INSPECT=1` is set. Corpus strict mode does not promote the independent MLP parser to a hard gate; it tightens duration/sample validation only.
- Keep `TONEPOET_DVDA_PHASE3_SKIP_MLP_INSPECT=1` as a complete bypass for known-deviant discs.

This keeps ffmpeg as the authoritative MLP decoder until the inspector has a larger corpus-derived proof base.

## v31 channel-arrangement validation

The MLP major-sync channel-arrangement decoder now uses the shared DVD-Audio
MLP/PCM assignment table instead of a separate hard-coded channel-count table.
The test suite compares all 21 valid DVD-Audio MLP/PCM assignment codes against
foo_input_dvda's `audio_stream_info_t::mlppcm_table` group-count model and has
explicit rejection coverage for reserved major-sync channel-arrangement values
21 through 31.

The MLP inspector remains advisory for frame-structure findings by default, but
major-sync audio-fact mismatches against IFO/source expectations now fail the
realization by default. `TONEPOET_DVDA_PHASE3_SKIP_MLP_INSPECT=1` remains the
operator override for discs that need to bypass the inspector entirely.

## v36 authored-sector fixture proof

The seven real AOB sector fixtures now exercise the MLP access-unit parser across every complete frame present in each 16-sector window, not just the first major-sync frame. Because the fixtures are fixed-size windows cut from authored discs, each payload ends in a truncated access unit. Fixture validation therefore uses a prefix-inspection mode that accepts a trailing partial frame and records its byte count. Full-track inspection keeps the default EOF rule and rejects the same truncated tail.

Current authored-sector coverage:

| Fixture | Complete frames | Major-sync frames | Trailing partial bytes | Major-sync facts |
|---|---:|---:|---:|---|
| `ap_eye_in_the_sky_first_16_sectors.bin` | 62 | 8 | 179 | 192 kHz, 24-bit, 2 ch, arrangement 1 |
| `ap_friendly_card_first_16_sectors.bin` | 61 | 8 | 481 | 192 kHz, 24-bit, 2 ch, arrangement 1 |
| `ap_i_robot_first_16_sectors.bin` | 60 | 8 | 19 | 192 kHz, 24-bit, 2 ch, arrangement 1 |
| `hdad2009_first_16_sectors.bin` | 58 | 8 | 473 | 192 kHz, 24-bit, 2 ch, arrangement 1 |
| `hawks_and_doves_first_16_sectors.bin` | 100 | 13 | 245 | 176.4 kHz, 24-bit, 2 ch, arrangement 1 |
| `mgletsgetiton_first_16_sectors.bin` | 49 | 2 | 549 | 96 kHz, 24-bit, 5 ch, arrangement 19 |
| `talking_heads_77_first_16_sectors.bin` | 427 | 54 | 215 | 96 kHz, 24-bit, 6 ch, arrangement 20 |

This improves the proof base, but it still does not make strict MLP inspection a universal default. Real full-track MLP corpus coverage should continue to grow before the parser becomes a CI hard gate.
