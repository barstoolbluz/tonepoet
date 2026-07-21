# DSD Reference policy v9 qualification report

Status: source-controlled qualification candidate; not promoted.

Policy v9 is an append-only admission correction to v8. It inherits v8's complete reconstruction, terminal-bound, packaging, and decoded-sample identity contracts. The only policy change is the disposition of metadata mutation for W64 delivery.

## F5 resolution

The pinned FFmpeg 7.1 W64 muxer is not an admissible metadata writer. For a mono Int24 W64 payload containing 8,820 samples (26,460 data bytes, not divisible by W64's eight-byte chunk alignment), an FFmpeg `-c:a copy -f w64` rewrite includes alignment padding in the declared data extent. The rewritten file decodes to 8,821 samples: the original 8,820-sample prefix plus one zero-valued phantom sample.

Policy v9 therefore:

- retains W64 as a qualified audio delivery target;
- rejects Reference W64 requests when the metadata stage is enabled, before conversion begins;
- returns stable error `DSD-REF-P0-024` with an actionable message;
- independently rejects direct W64 mutation in the metadata writer, preventing alternate call paths from selecting the unsafe muxer;
- permanently exercises the non-eight-aligned W64 defect in the commissioned real-tool gate;
- requires post-metadata decoded-sample identity for all 420 qualified non-W64 package cells; and
- separately exercises an odd-byte mono Int24 RIFF/WAV payload and requires exact sample identity after the FFmpeg metadata rewrite.

## Admission counts

The lossless package matrix remains 480 delivery cases and 60 terminal-bound cases. Post-metadata identity is required for 420 non-W64 cases. The 60 W64 metadata-mutation cases are unavailable by construction and must report `DSD-REF-P0-024`; they are not silently skipped and are not counted as successful mutations.

## Promotion

This candidate may be promoted only after the mandatory pinned real-tool gate emits a schema-version-9 machine report that proves:

1. all 480 delivery cases are sample-exact;
2. all 420 qualified post-metadata cases are sample-exact;
3. all 60 W64 metadata-mutation cases resolve to `DSD-REF-P0-024`;
4. the W64 non-eight-aligned probe reproduces the one-sample muxer defect exactly;
5. the RIFF odd-byte probe remains sample-exact; and
6. all inherited v8 terminal and effects-boundary evidence remains within the compiled bounds.
