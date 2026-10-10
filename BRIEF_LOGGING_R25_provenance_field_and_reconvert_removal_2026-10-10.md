# BRIEF — a provenance field mismatch, and the reconvert-and-compare behaviour comes out

R24 is applied and committed as `cac32f0`. Gate: **7395 passed / 2 failed** —
one expected freshness failure, and one of R24's own regressions, below.
Eighteen of its nineteen new tests pass.

A note on how to read this. Quoted output, file citations and figures taken from
logs are measured. Everything else is our reading of the code and may be wrong;
where we say what we believe, treat it as a starting point rather than a
finding.

## The gain correction was right and our stated belief was wrong

The R24 brief said it believed the fixed term in the logged gain was 18.0206 dB
and flagged that it was uncertain. The audit's 12 dB finding is correct, and we
verified it independently rather than accepting it:
`tonepoet-pipeline/src/dsd_reference.rs:3682` passes `"gain",
"-12.000000000"` during qualified reconstruction, `:3894` passes the selected
scalar at the terminal, and `DSD_COMPENSATION` is reachable only through
`resolve_dsd_general_export_gain`, whose callers are all on the General DSD
export path. Recording this so the correction is not relitigated.

R24 also did not compile. Five E0425 errors at `stages.rs:76523`–`:76538`: a new
test in `chunk_2_1_3_postprocessing_gate_and_phase_tests` calls
`album_scope_test_fragment` and `successful_log_summary`, which are private to
the sibling module `conversion_log_tests`. Fixed locally by widening those two
helpers to `pub(super)` and qualifying the four call sites. This is the third
delivery in seven rounds whose only compile failure is a test naming something
by a path that does not resolve. In the first two the correct path was already in
use elsewhere in the same file; this one needed a visibility change, since the
helpers were private to another module.

---

## 1. The provenance presence test reads a different field from the one that holds the value

R24's own regression fails:

```
r24_sidecar_field_origins_follow_selected_values_without_exporting_log_markers
  materializer_sacd.rs:901
  assertion `left == right` failed
    left: None
   right: Some("SACD disc TOC")
```

With no sidecar, the test sets a TOC catalog number and expects the recorded
origin to read `SACD disc TOC`. Nothing is recorded.

An origin is written only when a presence test passes
(`src/convert/pipeline/materializer_sacd.rs:302`–`:306`), and for this field the
test is:

```rust
("catalog_number", if sidecar_catalog_selected { "SACD sidecar XML" }
    else { "SACD disc TOC" },
    sidecar_catalog_selected || extra.contains_key("sacd_disc_catalog_number")),
```

There are two TOC catalog fields, populated separately (`:224`–`:233`):

```rust
insert_nonempty(&mut extra, "sacd_album_catalog_number", metadata.master_toc.album_catalog_number…)
insert_nonempty(&mut extra, "sacd_disc_catalog_number",  metadata.master_toc.disc_catalog_number…)
```

The presence test asks for the **disc** key, while the test's value is set on
the **album** field. Our reading is that a disc carrying an album catalog number
but no disc catalog number reports the value and silently drops its provenance.
A real Allman Brothers conversion does print `Catalog number: UDSACD 2143`, and
that string appears twice in the disc's master TOC, so this disc may populate
both fields and mask the problem; we did not establish which field the printed
value came from.

**What we want.** A field whose value is reported has its origin reported. Where
a value can come from more than one place, the provenance follows the place the
value actually came from.

---

## 2. The reconvert-and-compare behaviour comes out, and #72 keeps its original scope

The audit recommends narrowing #72 because early refusal "would reject
successful idempotent reconversions". That behaviour is withdrawn, so the
premise no longer holds.

The decision, in the user's words: *"i don't want the idempotent reconversion
feature. it (a) has too much baggage associated with it and (b) is probably not
something that most people want / need. it is the product of the model
misapprehending what i meant by 'idempotent' in my instructions to it months
ago."*

**What comes out.** A conversion does not inspect, compare against, or reason
about output left by previous conversions. There is no notion of a
reconversion — a conversion whose destination happens to be occupied is simply a
conversion that cannot publish there. Today a fresh conversion reads its own
staged output, reads the incumbent, and reports success without writing anything
when the bytes match; that is the behaviour being removed, and it is the reason
the refusal arrives only after the audio has been produced.

**What stays.** Resuming an interrupted publish. That path also compares bytes,
to establish how far a previous attempt got before it was interrupted, and it is
what R23's rollback journal was built around. `stages.rs:64885` describes it:
*"retry treats byte-identical existing roots as published and repairs missing
batch finalization."* Removing it would take out crash recovery. The two
mechanisms look alike and differ in purpose: one lets a new conversion do
nothing and call it success, the other lets an interrupted one finish.

**What #72 then becomes.** With no byte comparison to defer to, an occupied
destination under `--if-exists fail` is refusable from the destination path and
the naming policy alone, before extraction begins. The original wording stands;
the narrowed acceptance claim is declined. `overwrite` and `keep-both` keep
their current behaviour.

Expect existing tests to change with this, since some pin the behaviour being
withdrawn — `matching_prior_manifest_does_not_suppress_incremental_overwrite`
among them. That is correct rather than collateral.

---

## Still owed, and not an R24 defect

The live Browse → Convert → preset gesture against a real SACD ISO, which #43
names as an acceptance prerequisite. It needs the TUI and cannot be run from
here.

## Reference qualification

`stages.rs` has moved again, so the installed qualification does not bind this
tree. Requalification has not been run for R24 because the gate is not yet
clean. The failing test is not in the runner's focused list, so the runner would
reach its `cargo test --workspace` step and stop there, before either black-box
smoke.
Both items here land in `stages.rs` or `materializer_sacd.rs`; the former is in
`REFERENCE_COMMON_SOURCE_PATHS`, so a single requalification after both is the
economical order.
