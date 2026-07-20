# DSD Reference Policy v3 Qualification Report

**Policy:** `sox_ng_14_8_0_1_v3`
**Report schema:** `tonepoet-dsd-reference-policy-qualification-report/v1`
**Policy evidence state:** QUALIFICATION CANDIDATE
**Implementation certification:** not granted by this source-controlled report. Runtime activation remains disabled until the mandatory build and commissioned real-tool gates pass unchanged and their exact machine report is cryptographically bound into a promoted artifact.

## Corrective authority

Policy v3 is append-only. Historical v1 and v2 artifacts are preserved. The v3 amendment freezes deterministic planner rejection precedence, removes Int16 from the enabled matrix because no conservative Shibata peak bound is available, and limits source-front-end admission to production paths exercised by the release gate.

Enabled source cells are native DSF and DSDIFF/DSD at DSD64/128/256 mono/stereo, plus DSDIFF/DST DSD64 stereo. Predictive DST outside that cell rejects with `DSD-REF-P0-021`; SACD DSD/DST rejects with `DSD-REF-P0-023`.

Enabled terminal cells are Int24 and, for WAV-family targets, Float32/Float64. Int16 rejects with `DSD-REF-P0-022`. The WavPack Int24 `-bits_per_raw_sample 24` command remains frozen under v3.

The expanded supported-cell count is `13,248` and the canonical v3 digest is `8655f32296e3ac0012357c321cae026eb0effbcb3e128d5a1fad673fe12927a3`.

## Analyzer evidence contract

Promotion requires 1,200 planner-command/production-parser cases spanning all ten target rates, mono/stereo, two single-tone frequencies and phases, a phase-aligned four-tone family with fractional-sample peaks, analytic levels −120.003/−12.003/−0.500 dBFS, durations 0.125/0.500 seconds for single tones, and early/late peak placement. Every nonzero near-silence case must remain finite, every fixed-cell level sweep must be monotonic, and the conservative reported upper bound must never fall below analytic truth. The report describes this as systematic empirical qualification of the exact pinned analyzer closure, not as a mathematical error theorem for arbitrary analyzer builds.

## Source-front-end evidence contract

The mandatory test invokes the production private-copy and container-inspection seam. It verifies non-hard-linked DSF and DSDIFF/DSD materialization for all enabled native rate/channel cells, CMPR-based DSDIFF/DST classification, production DST decode, DSTC validation, canonical DFF write/readback, and exact oracle bytes for the enabled predictive cell. Every enabled source/rate/channel cell then executes and probes its exact planner-emitted render command. The executed-evidence v2 digest binds the original source kind, admitted source-content hash, and canonical materialization hash, and a canonical-hash mutation must change that digest. Decoder-primitive fixtures remain separate supporting evidence. SACD is not described as qualified.

## Metadata evidence scope

The package matrix performs a test-only FFmpeg stream-copy metadata rewrite and verifies decoded-sample identity. This is package stream-copy metadata evidence only; it is not a claim that the production metadata/artwork mutator itself was independently qualified. Production retains mandatory post-mutation decode-and-compare before publication.

## Mandatory gates

```text
cargo fmt -p tonepoet -p tonepoet-pipeline -p sacd-rs -- --check
cargo test -p tonepoet-pipeline
cargo test -p sacd-rs
cargo test -p tonepoet --test settings_sentinel
cargo test --workspace
cargo clippy -p tonepoet -p tonepoet-pipeline -p sacd-rs \
  --all-targets --all-features -- -D warnings
TONEPOET_REQUIRE_TOOLS=1 \
  cargo test -p tonepoet --test dsd_reference_qualification -- --nocapture
```

The final gate must emit schema-version-3 machine evidence, including exact tool identities, D1 responses, the 1,200-case analyzer digest, production source-front-end results, gain/terminal-chain results, primitive DST corpus results, all 480 planner-derived package cases, all 60 enabled terminal-bound cells, the v3 expanded-cell count/digest, and a pass outcome.
