# IP-05 verification — 2026-10-25

**D-01/D-02 corrective:** The selected-track admission now checks the full
authored CUE count, and the cumulative source excludes two inherited Python
bytecode files. New named regression:
`cue_ip05_pilot_incomplete_materialized_album_falls_back_before_spawn`
models a twelve-track CUE with selected tracks 1–2 already filtered in both
source and scheduler arrays; it must decline before any child. A complete
materialized selection reaches the existing typed FLAC proof. **Rust execution
NOT RUN (no compiler)**. See `CORRECTIVE_D01_D02_2026-10-25.md` for the
corrected source-integrity results and the new external receipt for the exact
archive SHA. The native fixture results below predate this admission-only
change and are retained unchanged; they are not Rust S3 tests.

**State: implemented-unqualified / pilot OFF by default.** No Rust compiler
or Rust tests executed. The input source was extracted from the SHA-verified
accepted IP-04 git archive. No `.git` metadata in the input.

| Gate | Command | Result |
| --- | --- | --- |
| Input integrity | `sha256sum /mnt/data/TonePoet_SNAPSHOT_9c06f0f_2026-10-25.tar.gz` | PASS, `de88377467699875b265231893623b49ee1cfd6a68c20b284a6bf361444defa0` |
| IP-05 packet read | `cat docs/cue_parallel_ip01/planning_authority/packets/IP-05_integrate_pilot.md` | PASS |
| Input handoffs read | `cat NEXT_SESSION_HANDOFF.md ip04_verification/README.md` | PASS |
| Rust availability | `command -v cargo rustc rustfmt` | NOT INSTALLED |
| Destination checker | `python3 -B .../verify_destination_files.py --self-test` | PASS |
| Native exact FLAC seek with gaps | `python3 -B ip04_verification/native_gap_reference_probe.py` | PASS: 3/3 exact ranges, 40,013 samples at 16 bit |
| Native FLAC S3 quick | `python3 -B ip04_verification/native_flac_pairs.py --quick --output ip05_verification/native_quick_results.json` | PASS: 16-bit and 24-bit, 3/3 each |
| Native FLAC S3 full | `python3 -B ip04_verification/native_flac_pairs.py --output ip05_verification/native_full_results.json` | PASS: 16-bit 12/12, 24-bit 30/30 |
| Complete source manifest/static gate | `python3 -B ip05_verification/verify_static_ip05.py` | See `STATIC_GATE_OUTPUT.txt` |
| `cargo fmt`, Rust filtered tests, full workspace | Commands in `HOST_GATE.md` | **NOT RUN — no Rust toolchain** |
| Real TonePoet S3 actual UI/API album | `TONEPOET_CUE_S3_PILOT=1` real conversion | **NOT RUN** |
| Source-level metadata/RG/publish invariants | Rust source review, old pipeline left unchanged | Static only; host unqualified |
| Destination actual output-set and cross-FS | `verify_destination_files.py --destination ...` | **NOT RUN**: no installed TonePoet binary |

Native simulated fixture identities (full source/track hash lists recorded in
`native_quick_results.json`, `native_full_results.json` and
`native_gap_reference_results.json`):

- Quick 16-bit source FLAC SHA-256:
  `8c309f8d25f26edca0fbad917bc444f98cb513d05881bfb16c163f2b21e11fb3`.
- Quick 24-bit source FLAC SHA-256:
  `b88d095f41e58b250a784970b966b0158cd4e1702a0153985cf2cbbbc2de275a`.
- Full 16-bit source FLAC SHA-256:
  `3cea198ccbde5b895ea0af26b55a3e8075c16d8074a649fa0ad29860db089c23`.
- Full 24-bit source FLAC SHA-256:
  `b88d095f41e58b250a784970b966b0158cd4e1702a0153985cf2cbbbc2de275a`.
- Gap 16-bit source FLAC SHA-256:
  `44f3759e7ee4f7a890aa2b6fb01779dd76ef66ef20dd472cbd446e5d2fb2a5ae`.

These source files were in temporary native fixtures and are not packaged;
the JSON evidence is included. No actual TonePoet S3 source/audio fixture
checksum can be claimed until the host executes the new Rust path.
