# IP-05 — integrated S3 pilot, deliberately OFF by default

**Corrective D-01/D-02 (2026-10-25):** The authored CUE track count now
participates in the prelaunch selection gate, so a materialized selection of
two tracks out of twelve cannot enter S3. A named regression covers the real
two-filtered-arrays shape. Both inherited `__pycache__/*.pyc` files have been
removed from this cumulative delivery. Read `CORRECTIVE_D01_D02_2026-10-25.md`
for scope and verification. Rust tests and actual publication remain host-gated.
The authoritative corrected archive SHA is in its **new external receipt**;
the former `4467bd785bbb` archive is superseded, not accepted.

**Packet:** `planning_authority/packets/IP-05_integrate_pilot.md`

**Input:** `TonePoet_SNAPSHOT_9c06f0f_2026-10-25.tar.gz`, SHA-256
`de88377467699875b265231893623b49ee1cfd6a68c20b284a6bf361444defa0`.
The owner's input identity note reports clean `main` commit `9c06f0f` and host-accepted IP-04, including IP-04's host load qualification. The exact input has no `.git`; these Git/host assertions cannot be independently checked from the tar. Earlier in-tree IP-04 ledgers saying "not accepted" are historical, **superseded by the supplied current identity note**, not grounds for treating earlier implementations as unaccepted here.

**State:** `implemented-unqualified` — Rust compiler and host-level end-to-end
qualification not available in this authoring environment. S3 remains **default
OFF**. Do not claim IP-05 acceptance, run IP-06, or enable default S3 until the
host runs and signs off the gates in `HOST_GATE.md`.

## Integration scope

- Two Rust production modules only: `src/convert/pipeline/stages.rs` (single
  album-level gate and downstream connection, five named regression tests / helpers)
  and `src/convert/pipeline/track_executor.rs` (shared-permit wait adapter and
  native contention/cancel regression).
- Hidden opt-in `TONEPOET_CUE_S3_PILOT=1`, **exact value only**. Unset or any
  other value uses the intact S1/staged carrier route. Not a user preference,
  not persisted to destination. No automatic selection or UI change.
- One physical exact FLAC+CUE image, whole selected album, two or more tracks,
  source-rate and source-depth FLAC, no registered effects or active PCM gain,
  only supported Unix two-process runner, IP-02 exact source facts, verified
  planner actions, unique staging paths, and weighted native child budget.
  Any prelaunch failure selects the original album route. No partial S3 group.
- IP-04 independently verified ranged worker runs at most two concurrently,
  owns both supervised child records, sample extent, post-encode validation and
  staged artifact. Shared IP-03 permits held across native children. Once
  selected, **temporary shared permit contention queues** (cancellable)
  instead of becoming an inexplicable failed track. No fallback after spawn.
- `ScheduledCueStreamBatch.source_replaygain=None` deliberately invokes the
  existing, native post-output ReplayGain stage for `Track`, `Album`, `Both`,
  and applicable `SkipIfComplete` semantics. The source-pass FIFO observer is
  not capable of analyzing independent concurrent readers; it is not used by
  this pilot. No RG preference is changed.
- No changes to metadata/artwork priority, verification, failure policy,
  album publisher, conversion log, cross-device publication or output naming.
  Real visible destination must contain **only requested audio and enabled
  log/CUE/artwork**. Never leave manifests, dotfiles, scratch or control state
  in the output destination. The Python exact-set gate is available below.

## Evidence and commands

`native_quick_results.json`, `native_full_results.json`, and
`native_gap_reference_results.json` are independently executed **native
FFmpeg simulations**, including source FLAC digests and decoded source/track
PCM hashes. They do not compile or call TonePoet's new Rust branch.

`verify_destination_files.py --self-test` is an executed Python test. To
validate **real** output after a host conversion, create a JSON array with
*exactly* the user-requested relative paths (audio, plus enabled optional
sidecars), then run:

```sh
python3 -B docs/cue_parallel_ip01/ip05_verification/verify_destination_files.py \
  --destination /actual/output/directory --expected-json /outside/output/expected.json
```

Place expected lists and verification reports **outside** the destination.
`verify_static_ip05.py` verifies complete source identity and wiring using
`SOURCE_MANIFEST.json`, `SHA256SUMS` and source inventory; it does **not**
qualify runtime semantics. `SOURCE_DIFF.json` is the full tree-level delta
against the exact accepted IP-04 archive, excluding cyclical self-hashes.
`PRODUCTION_DIFF.patch` is the complete Rust diff.

## Exact next packet

Use the **external SHA-bound delivery receipt** accompanying the complete
IP-05 tarball, not this in-archive handoff, for the exact generated archive
filename and digest. A tarball cannot contain its own SHA-256. IP-06 consumes
**only that exact tarball after independent host acceptance**, and must
preserve default-OFF behavior unless IP-06's own qualification succeeds.
