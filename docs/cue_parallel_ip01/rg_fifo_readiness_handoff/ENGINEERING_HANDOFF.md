# TonePoet ReplayGain FIFO readiness correction — engineering handoff

**Date:** 2026-10-24  
**Scope:** CUE S1 source-pass ReplayGain FIFO observer; no other conversion route or native meter changed.  
**Provisional decision:** **READY FOR HOST QUALIFICATION**, *not* an assertion that the build or Billie Holiday performance test passed.

## Source authority

- Source archive: `TonePoet_SNAPSHOT_b1431d0_2026-10-24.tar.gz`.
- Source archive SHA-256: `2193ab1ab32c4794e407a2344c18969efaaec3dd5f5667a26ce7e0f2d4903d49`.
- `b1431d0` is the supplied snapshot label, **not** an independently verified Git commit: the archive contains no `.git` directory. All 1,198 original regular files were compared against the source archive; only the four documented paths differ.
- Diagnostic evidence archive SHA-256: `538b167981f85a4521d8cedc51aabb5a0f8c78f81ba7bfb6653782cfac8362a7`.
- Reconnaissance note SHA-256: `d0a78a8af5b9357b8111c43e29aa18f55e05ca5c98ece70668d9f8c12302ada2`.

## Protocol reconstructed from the fresh snapshot

The CUE planner makes one mode-0600 Unix FIFO per selected track in `.cue-replaygain-stream` and creates a fixed WAV-format prefix and exact PCM byte count. Observer tasks are spawned with `tokio::task::spawn_blocking`; each opens its read descriptor with `O_NONBLOCK` and waits for header, exact PCM length, and EOF. The segmented producer opens each FIFO writer with `O_NONBLOCK`, retains that same descriptor, removes its `O_NONBLOCK` flag for ordinary backpressure, writes the prefix, then copies exact byte ranges using 64-KiB transport chunks. The writer is dropped at the end of the segment. The observer rejects wrong prefix, surplus bytes, truncated payload, and errors; its owned meter statistics are reduced only on successful completion. The planner joins observers and conversion, cancels on first-cause failures, removes the mirror directory, and retains the existing source-scan/output-scan and one-shot recovery policy.

There were three independent 5-ms sleeps in `replaygain.rs`: (1) reader receives EOF before the first producer has attached, (2) `EAGAIN` during exact header/PCM read, and (3) `EAGAIN` while awaiting EOF or surplus bytes. The historical ~14,140 active-transfer `EAGAIN` events times 5 ms account for roughly 70.7 seconds of artificial delay. The native meter's ~10 seconds of arithmetic is not the primary bottleneck.

## Implemented correction

After `EAGAIN` during exact read or EOF verification, the observer uses **`libc::poll(POLLIN | POLLHUP | POLLERR)`** with a 50-ms maximum timeout. This follows the existing in-tree `script_supervisor.rs` approach; it introduces **no dependencies**. Each timeout checks cancellation; `EINTR` retries, with cancellation checked again. `POLLNVAL` produces `EBADF`, and `POLLERR` produces `EIO`. A `POLLHUP` is *not* declared completion: the code retries `read` to drain any buffered PCM, then the existing exact-length and EOF checks decide success or truncation. Readiness is advisory, so any subsequent `EAGAIN` simply waits again. The FIFO stays nonblocking; no buffer sizes, writer behavior, arithmetic, encoding, or publishing policy changed.

The **pre-first-writer `read()==0` path retains its 5-ms cancellable backoff** rather than polling. After a writer closes, a FIFO may repeatedly signal `POLLHUP` even if it has no unread data, so replacing setup waits with unguarded polling would risk a spin. A newly created FIFO with *no writer ever connected* may instead show no `poll` events, as the local Linux kernel smoke test confirmed. Neither behavior invalidates the backoff choice. In addition, an `EAGAIN` proves a writer was connected; if that writer subsequently closes without emitting even the first byte, this is now treated as truncated transport instead of being mistaken for the never-connected state. If a zero-byte producer attaches and disappears entirely between separate reader inspections, that event cannot be distinguished from a never-connected writer solely by `read`/`poll`; the pipeline's existing cancellation protocol remains authoritative.

## Exactly four changed files

1. `src/convert/replaygain.rs` — bounded readiness wait in the two active/EOF `EAGAIN` paths; initial-writer state preserved; `EINTR` and bad-fd handling; five narrowly focused Unix unit tests. No changes to loudness/true-peak mathematics or PCM decoding.
2. `src/convert/pipeline/rg_profile.rs` — additive counters for readiness syscall count, timeout count, and inclusive readiness-wait nanoseconds; a strategy identifier distinguishes historical rows. Existing keys remain compatible.
3. `tooling/rg_diag/analyze.py` — interprets strategy-aware counters and ceases describing corrected observations as 5-ms EAGAIN polling. Validates new counters while accepting the existing historical schema.
4. `tooling/rg_diag/test_analyze.py` — one focused analyzer validation regression.

Source `rg_diag_schema` remains `tonepoet.cue.rgdiag.v1` as a backward-compatible **additive** extension. Historical `would_block_poll_count` is still the count of EAGAIN **read attempts**, not the count of kernel `poll` calls; `poll_sleep_call_ns` now covers **initial no-writer backoff only** on Unix, whereas older recordings included EAGAIN sleep time. Do not compare these fields as if their semantics were identical across revisions. New `readiness_wait_count` is the number of `poll` syscalls (including timeouts and interrupted calls), `readiness_timeout_count` counts timeouts, and `readiness_wait_call_ns` is inclusive wall time in `poll` only. These per-observer totals overlap transport and other observers and **must not be summed into end-to-end album elapsed time**.

## Adversarial review: relevant invariants

- A `poll` hangup is never interpreted as terminal EOF without a `read`—unread bytes retain their original order and bit identity.
- After every partial read, the next `read` is attempted before any blocking wait. No extra sample conversion or framing occurs.
- EOF-after-payload still rejects the first surplus byte and waits for writer closure when the writer remains attached.
- A disconnected FIFO is not polled in the initial no-writer state; the 5-ms backoff bounds setup CPU use. The 50-ms polling timeout bounds cancellation checks during active transfer. Normal cancellation is also checked on each exact read / EOF loop.
- `POLLNVAL`/`POLLERR` are terminal errors; system-call `EINTR` is retried. A post-poll `EAGAIN` re-enters the wait without spinning.
- Reader and writer lifetimes, `rg_cancel`, observer join, failure-cause ordering, one-time observer recovery, output-measured ReplayGain fallback, and cleanup are unmodified.
- The correct default ReplayGain Off path neither invokes this helper nor gains new polling overhead. Non-Unix code continues to compile against its legacy fallback definitions, subject to host build qualification.

## Scope restraint

No new runtime, worker scheduling, FIFO sizing, encoder, meter, source-scan eligibility rule, metadata policy, S3 observer, IP phase, or refactor was introduced. `RG_FIFO_READINESS.patch` is the exact four-file patch. `VERIFICATION_RESULTS.md` records which checks actually ran; `HOST_QUALIFICATION.md` provides the remaining gates. The patch can be independently applied to the original snapshot and compared byte-for-byte with the delivered source.
