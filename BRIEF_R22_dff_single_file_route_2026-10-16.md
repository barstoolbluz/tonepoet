# BRIEF — R21C accepted; one conversion route regressed

R21/R21C applied, compiled as received, 1030 of 1030 checksums. No
source-locked file moved. Gate: **7518 passed / 1 failed**.

The metadata authority work is right. With only `sidecar-cue` admitted, native
FLAC tags are no longer writable merely because they are taggable, and a
sidecar-only save rejects a residual native mutation before dispatch. Omitted
targets are now forbidden rather than appended back, so a configuration can
finally express "do not write this carrier". The de-emphasis dither test from
the last brief passes; `hdcd` is 42 of 42, `rf64_` 23 of 23, `r21_` 4 of 4,
`r21c_` 1 of 1.

## The failure

```
convert::pipeline::stages::cue_real_output_matrix_tests::nine_dff_metadata_sidecar_album_drives_real_conversion_naming_and_flac_tags
  DFF track 1 should remain on the SingleFile route
  left  Some(EmbeddedOnly)
  right Some(IgnoreCue)
```

Pre-existing, 2 of 2 alone. A DFF track that should bypass CUE handling now
resolves to the embedded-only policy, so a real nine-DFF sidecar album takes a
different conversion route than before.

The test sets no target priority, so it runs on the defaults, and we could not
establish which of this round's changes moves it. `CueSidecarPolicy` is
referenced across twenty-two files, including `keybindings.rs` and `app.rs`,
both of which this round changed.

**What we want:** a DFF with a metadata sidecar keeps the SingleFile route and
the CUE stays metadata, as the test says — or, if the new authority rules
legitimately change that routing, the test says what the route is now and why.

## Still needing the user

The five R5 UI changes have never been looked at. #75's SACD tag-transfer round
trip has never run against a real disc. #62's real album — the ten-track one
with the 2.19 GB RF64 file — has not been opened since the read path landed.

Issue #73, markdown in the popup body, remains deliberately deferred.
