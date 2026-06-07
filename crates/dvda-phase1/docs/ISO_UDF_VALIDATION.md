# ISO/UDF backend validation

The `IsoDvdaVolume` implementation is feature-gated behind `iso-isomage` and uses the public `isomage` APIs that parse ISO 9660/UDF into a `TreeNode` and stream file bytes with `cat_node`.

This adapter is not considered production-validated merely because it compiles. It must be run against the seven Phase 0 ISO images, because the corpus requirement is specifically UDF 1.02 DVD-Audio images.

Expected local validation command from the Tonepoet workspace after the fixture ISOs are available:

```sh
DVDA_PHASE1_ISO_ROOT=/path/to/dvda-isos \
  cargo test -p tonepoet --features iso-isomage --test dvda_phase1_iso_validation
```

The test expects one ISO per fixture, named:

```text
hdad2009.iso
ap_i_robot.iso
ap_friendly_card.iso
ap_eye_in_the_sky.iso
mgletsgetiton.iso
hawks_and_doves.iso
talking_heads_77.iso
```

Each ISO test asserts the same structural conditions as the extracted-directory fixture tests: AMG/AOTT resolution, ATSI title and track hierarchy, CPPM marker detection by `DVDAUDIO.MKB`, and active audio-format index exposure. MGLETSGETITON additionally asserts that ATS 01 exposes both format 0 and format 2.

Until that command has passed on the real ISO corpus, the directory backend should be treated as the validated Phase 1 backend and the ISO backend as a feature-gated adapter under validation.
