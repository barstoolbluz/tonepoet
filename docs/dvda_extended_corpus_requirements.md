# DVD-Audio expanded corpus requirements

The Phase 3 MLP corpus proves ATS-relative, unencrypted, 192 kHz / 24-bit / stereo MLP extraction. It does not prove every DVD-Audio topology.

Two paths need real-disc coverage before they should be described as complete market support:

1. **Multi-format ATS active-format resolution**
   - Required evidence: at least one ISO where an ATS has more than one present audio-format table entry and at least one materialized track records `dvda_audio_format_resolution=track_type_audio_format_index` or `dvda_audio_format_resolution=multiple_present_formats_unknown_until_aob_demux`.
   - The test must realize the matching track to WAV, probe it, and verify a nonzero decoded rate/channel/sample count.
   - Strict gate: `TONEPOET_DVDA_PHASE3_REQUIRE_MULTIFORMAT_ATS_CORPUS=1`.

2. **SAMG absolute-sector realization**
   - Required evidence: at least one ISO-backed materialized track using `DvdaSectorAddressSpace::SamgAbsolute`.
   - Directory-copy inputs do not count because they do not preserve original disc LBAs.
   - The test must realize the matching track to WAV, probe it, and verify a nonzero decoded rate/channel/sample count.
   - Strict gate: `TONEPOET_DVDA_PHASE3_REQUIRE_SAMG_ABSOLUTE_CORPUS=1`.

`TONEPOET_DVDA_PHASE3_EXTENDED_CORPUS_STRICT=1` enables both strict gates. These tests intentionally skip when the corpus lacks the required media unless a strict gate is set.
