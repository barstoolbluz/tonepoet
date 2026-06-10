# CPPM boundary notes

The supplied `foo_input_dvda` LPCM reference archive contains LPCM/audio-stream code and does not contain a CPPM decryptor, MKB parser, key-management, or drive/media authentication implementation.

The demux guidance in the Phase 3 brief shows the `foo_input_dvda` decryption seam: after a 2048-byte AOB sector is located and read, `dvdcpxm->decrypt(..., DVDCPXM_PRESERVE_CCI)` runs before MPEG-PS demux. That is enough to identify the correct architecture for a future authorized provider, but not enough to implement CPPM decryption in this bundle.

v39 changes the detection policy so `DVDAUDIO.MKB` is treated as metadata/evidence, not proof that the AOB payload is unreadable.

## Detection policy

- Record `mkb_present` from `DVDAUDIO.MKB` presence.
- When MKB is present, probe the first backed AOB file in title-set/part order.
- Read sector 0 from that AOB and classify it as readable when it exposes a valid MPEG-PS pack header plus parseable packet headers, preferably a DVD-Audio Private Stream 1 substream (`0xA0` LPCM or `0xA1` MLP).
- Set `cppm_detected = false` when the first backed AOB sector looks readable, even if MKB metadata remains in `AUDIO_TS`.
- Set `cppm_detected = true` only when MKB is present and the first backed AOB sector does not parse as readable MPEG-PS audio data.
- If no backed AOB can be probed, record a warning and do not block solely because MKB exists.
- Support an explicit caller override, `SourceOptions::dvda_assume_decrypted`, for sources known to contain already-readable AOB sectors when probing cannot classify the payload.

This policy handles decrypted ISO images that retain `DVDAUDIO.MKB`, while still blocking sources whose AOB sectors appear encrypted or garbled.

The code still does not include, derive, or invoke CPPM decryption.
