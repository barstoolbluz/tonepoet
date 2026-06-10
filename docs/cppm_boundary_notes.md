# CPPM boundary notes

The supplied `foo_input_dvda` LPCM reference archive contains LPCM/audio-stream code and does not contain a CPPM decryptor, MKB parser, key-management, or drive/media authentication implementation.

The demux guidance in the Phase 3 brief shows the `foo_input_dvda` decryption seam: after a 2048-byte AOB sector is located and read, `dvdcpxm->decrypt(..., DVDCPXM_PRESERVE_CCI)` runs before MPEG-PS demux. That is enough to identify the correct architecture for a future authorized provider, but not enough to implement CPPM decryption in this bundle.

v20 therefore records a deliberate product boundary:

- Detect CPPM evidence (`DVDAUDIO.MKB` / parser CPPM flag).
- Materialize enough source structure for a blocked-source report.
- Mark tracks blocked before realization.
- Report the policy as `DetectExplainSkip`.
- Record `decryption_supported = false`.
- Tell the user why the track was skipped.

The code does not include, derive, or invoke CPPM decryption.
