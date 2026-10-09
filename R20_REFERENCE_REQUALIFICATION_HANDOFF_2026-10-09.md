# R20 Reference qualification: required build-host checks

**Status: UNQUALIFIED MODIFIED SOURCE. NOT FOR REFERENCE RELEASE.** R20 alters at least `src/convert/pipeline/stages.rs` and `src/fs_limits.rs`, both listed by the installed `tonepoet-pipeline/qualification/dsd_reference_common_v18_source_lock.json`; the installed four certification sidecars are deliberately unchanged and stale against this tree.

Use the configured Reference build host with genuine qualified tools. The existing R18-derived script does not permit `TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE=1` and does not accept inherited reports as evidence:

```bash
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

The script runs the actual `complete_p0_reference_qualification_report` in release profile with real-tool gate, redirects all *four* outputs into a new empty directory, verifies each output, report/certification/evidence coherence and source-lock SHA-256 against final checkout, then installs sidecars as one rollback-protected set, requiring the canonical freshness gate. It also runs R20/R19 regression gates and a real SACD FLAC-without-manifest positive smoke. Running it without the ISO path does **not** complete release acceptance.

## Wrong-source refusal smoke (requires a separate scratch checkout)

Do not mutate the requalified release checkout. Copy the source after requalification into a new scratch path (exclude `target`, optionally copy configured Cargo caches externally); retain the freshly qualified sidecars in that copy. Append a harmless comment to one locked file *only in the scratch copy*, then run:

```bash
cargo test --test reference_qualification_freshness
```

**Expected outcome: nonzero.** It must detect the mismatch. Then, using a known-good real SACD ISO, attempt the qualified Reference route in the scratch copy:

```bash
cargo run --release -- convert /absolute/path/to/real-SACD.iso \
    --track 1 --area stereo --dsd-path reference --format flac \
    --output /absolute/path/to/empty-scratch-output
```

**Expected outcome: nonzero with Reference authority/refusal diagnostic, and NO output audio or publication artifacts.** Check the refusal is due to the source lock rather than missing tools or an invalid ISO. Never use the unqualified override to make this pass. Discard the scratch checkout afterwards. Keep the positive and negative smoke logs alongside qualification evidence. If any R20 source-locked file changes after requalification, the four sidecars must be regenerated and both smokes repeated.
