# DVD-Audio AOB sector fixture validation

v34 adds the uploaded real-sector samples as demuxer fixtures. Static validation in this bundle found the following facts:

| Fixture | Sectors | PS1 packets | MLP packets | MLP payload bytes | Cyclic range | PES stream IDs | MLP extra_header_length | Status |
|---|---:|---:|---:|---:|---|---|---|---|
| `ap_eye_in_the_sky_first_16_sectors.bin` | 16 | 16 | 16 | 32,059 | 0..15 | 0xBB,0xBD | 6 | ok |
| `ap_friendly_card_first_16_sectors.bin` | 16 | 16 | 16 | 32,059 | 0..15 | 0xBB,0xBD | 6 | ok |
| `ap_i_robot_first_16_sectors.bin` | 16 | 16 | 16 | 32,059 | 0..15 | 0xBB,0xBD | 6 | ok |
| `hawks_and_doves_first_16_sectors.bin` | 16 | 16 | 16 | 32,059 | 32..47 | 0xBB,0xBD | 6 | ok |
| `hdad2009_first_16_sectors.bin` | 16 | 16 | 16 | 32,059 | 0..15 | 0xBB,0xBD | 6 | ok |
| `mgletsgetiton_first_16_sectors.bin` | 16 | 16 | 16 | 32,059 | 32..47 | 0xBB,0xBD | 6 | ok |
| `talking_heads_77_first_16_sectors.bin` | 16 | 16 | 16 | 32,059 | 32..47 | 0xBB,0xBD | 6 | ok |

Key implementation result: the real MLP fixtures consistently declare `extra_header_length = 6`. v34 therefore treats six bytes as the canonical real-disc MLP extra header while continuing to parse payload start from each packet's declared length for compatibility with odd discs.

## ffmpeg decode smoke validation

After demuxing each 16-sector snippet to raw MLP, ffmpeg decoded every payload snippet to `pcm_s32le` WAV. ffprobe observed:

| Fixture | Sample rate | Channels | Decoded samples in snippet |
|---|---:|---:|---:|
| `ap_eye_in_the_sky_first_16_sectors.bin` | 192000 | 2 | 9920 |
| `ap_friendly_card_first_16_sectors.bin` | 192000 | 2 | 9760 |
| `ap_i_robot_first_16_sectors.bin` | 192000 | 2 | 9600 |
| `hawks_and_doves_first_16_sectors.bin` | 176400 | 2 | 16000 |
| `hdad2009_first_16_sectors.bin` | 192000 | 2 | 9280 |
| `mgletsgetiton_first_16_sectors.bin` | 96000 | 5 | 3920 |
| `talking_heads_77_first_16_sectors.bin` | 96000 | 6 | 34160 |

These snippets are intentionally partial windows, not whole-track elementary streams, so the full-file MLP inspector should not require EOF-aligned access units for this fixture class. The unit coverage parses the first complete major-sync frame from each fixture instead.

## v36 MLP access-unit parser coverage

v36 validates the MLP inspector against the same authored-sector fixtures by walking all complete access units in each demuxed payload. The final access unit is expected to be partial because these samples are fixed 16-sector windows, not whole-track elementary streams. Full-track inspection still rejects a truncated final frame.

| Fixture | Complete MLP frames | Major-sync frames | Trailing partial bytes |
|---|---:|---:|---:|
| `ap_eye_in_the_sky_first_16_sectors.bin` | 62 | 8 | 179 |
| `ap_friendly_card_first_16_sectors.bin` | 61 | 8 | 481 |
| `ap_i_robot_first_16_sectors.bin` | 60 | 8 | 19 |
| `hdad2009_first_16_sectors.bin` | 58 | 8 | 473 |
| `hawks_and_doves_first_16_sectors.bin` | 100 | 13 | 245 |
| `mgletsgetiton_first_16_sectors.bin` | 49 | 2 | 549 |
| `talking_heads_77_first_16_sectors.bin` | 427 | 54 | 215 |
