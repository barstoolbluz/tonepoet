# IP-02 persisted execution checkpoint

- Input received: `TonePoet_SNAPSHOT_847fc5e_2026-10-24.tar.gz`; SHA-256 independently matched `880c5e2693ee09ef56723c0ac7b72cbd6080340c08671f0f36e9288b9a994112`.
- Input declared Git archive at `847fc5e`; there is **no `.git`** to prove branch/clean state or make a commit. Compare each file against the original archive rather than relying on a Git diff.
- Read and applied `planning_authority/packets/IP-02_qualify_flac_ranges.md`; kept IP-01 and the source-pass ReplayGain FIFO readiness change.
- Production delta restricted to `mod.rs` (one inert module declaration), `materializer_cue.rs` (reuse exact FLAC source facts), `cue_flac_range.rs` (new standalone adapter + tests).
- Native seek algorithm: input `-ss` at whole source-rate seconds + integer-sample `atrim`; `-err_detect crccheck+explode`; s32le PCM; independent exact byte-count contract.
- Native trap discovered and fixed: plain `-err_detect explode` does **not** check FLAC frame CRC and can return success with corrupted PCM. Never regress to it.
- Native cancellation: parent must always reap the child; when producer blocks on an unread stdout pipe, SIGTERM may require bounded SIGKILL escalation. Future ToolRunner integration must keep existing lifecycle supervisor, not invoke the binary by an unmanaged `spawn`.
- `cargo`/`rustc` unavailable in this environment. No Rust build/tests/fmt claims are permitted. State remains `implemented-unqualified` until build-host evidence.
- Seek output is **not used** by any ordinary TonePoet route. S1 rollback is the unchanged existing path.
- On resumption: verify output archive and manifests; run the host commands documented in `README.md`; record exact source/version SHA and evidence in ledgers, and **only then** accept IP-02 and proceed to IP-03.
