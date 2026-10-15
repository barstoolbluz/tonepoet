# Resume checkpoint — TonePoet R19 HDCD R4 corrective

Canonical full source and supporting evidence are in the root of this extracted archive. Begin with `DELIVERY_REPORT_R19_HDCD_R4_POLICY_CORRECTIVE_2026-10-14.md` and `BRIEF_R19_HDCD_R4_defaults_depth_dither_lossy_2026-10-14.md`, then verify `R19_HDCD_R4_PACKAGE_SHA256SUMS.txt` before further edits.

**Stable state at handoff:** six original Rust source files corrected (database v28, UI defaults/preset provenance, Int16 refusal and typed terminal dither, lossy encoder format ownership), two independent Python/FFmpeg probes added, original upstream project retained. Reference qualification JSON intentionally unchanged. The attached source delta is `R19_HDCD_R4_SOURCE_DELTA.patch`. Base SHA-256 is recorded in the delivery report.

**Actual gates passed:** original HDCD source-first smoke, original analysis-cache SQL probe, modeled v28 SQLite migration replay test (14 cases), new FFmpeg precision probe for Opus/MP3/TPDF, Python syntax checks. **Not run:** all Rust compilation/tests, pinned libfdk AAC, actual installed DB upgrade, full Reference fixture requalification, performance testing.

**Next iteration priority:** configure Rust 1.93, dependencies, pinned FFmpeg/SoXR/libfdk and Reference fixture; first `cargo check --workspace --all-targets` and the focused tests, repair only reproducible failures, run the entire configured gate, then execute `scripts/requalify_reference_r20.sh` with its real fixture and inspect all four generated sidecars. If the source changes after requalification, rerun that gate. Preserve explicit manual user choices and the certified true-peak rejection boundaries; do not introduce a separate source scanner, effect scheduler or terminal quantizer. Record each actual command/result, not an inferred status.

**Idempotent continuation:** work from this full archive, not a patch against an unknown revision. Revalidate hashes before changes and regenerate only package inventory and handoff metadata after the new Rust/qualification gate. Historical checksum/qualification files from earlier rounds are historical evidence, not current gate passes.
