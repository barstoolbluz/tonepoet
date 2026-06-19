# Phase 1 Exclusions

The following are deliberately out of scope for this bundle:

* materializer code,
* TrackSourceRef variants,
* PreparedSource / PreparedTrack creation,
* MPEG-PS packet parsing,
* LPCM unpacking,
* MLP decode command planning,
* FFmpeg invocation,
* CPPM decryption,
* AUDIO_SV.IFO parsing,
* user-facing selection UI.

This module may be used by those later phases, but it should not import them.
