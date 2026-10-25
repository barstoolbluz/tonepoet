# IP-07 — concurrent source-range native ReplayGain (implementation candidate)

**2026-10-25 | Status: IMPLEMENTED-UNQUALIFIED / NOT HOST ACCEPTED.**

Source-of-truth input: the accepted IP-05 snapshot `TonePoet_SNAPSHOT_46696d1_2026-10-25.tar.gz`, externally checked SHA-256 `af66b5c2a40904d4db6fec3fd4185914b4aadd32c74cf6fd6ff15568bf2cbd20`. The original input is embedded unchanged in the user-supplied IP-07 packet ZIP; it is the rollback. This implementation has no `.git`, so a Git commit, cleanliness or merge cannot be independently asserted. The independently computed IP-07 **output archive** SHA is in its separately delivered receipt, never inside the archive itself.

## Implementation in brief

- Existing IP-05 `cue_s3_pilot_preflight` and exact `TONEPOET_CUE_S3_PILOT=1` gate remain unchanged. Before each S3 album, an **independent optional** source-meter plan checks the current `cue_source_replaygain_eligible` policy, admitted identity-PCM typed FLAC and strict contiguous CUE sample extents, the qualified IP-02 source decoder, complete manifest and immutable file generation (canonical path, dev/inode, byte size, mtime/ctime). ReplayGain Off starts no meter decoder.
- Run the existing IP-02 CRC-checking FFmpeg per-range command through the existing `RealToolRunner` supervised command execution, with its `stdout` redirected to a private CLOEXEC anonymous pipe. A private `ToolRunner` method explicitly supports the handoff and refuses by default; no stdout diagnostic tail, WAV fake header, FIFO, PCM file, user-visible setting or child scheduler is introduced.
- The native `NativeReplayGainObserver`, existing s32le signed32 normalization and reporting-peak meter are fed frame-aligned chunks of at most 16,384 frames. The reader demands exact expected bytes, EOF and frame count; the supervisor separately demands a clean decoder process exit. Either fails the optional scan, never converts operational faults into mathematical unavailability.
- At most two album-local source meters run concurrently, additionally bounded by FFmpeg family capacity minus an FFmpeg/FFmpeg S3 worker's two permits. Each meter waits only on its **one** global FFmpeg permit; it holds that permit until reader and decoder have both been supervised and settled. It does not consume an S3 worker slot. Native statistics storage is divided across all authored tracks using the existing 256-MiB aggregate budget. The independent album reducer retains its existing separate 256-MiB allowance.
- Each owned native sufficient-statistics observation is placed at its admission-time authored index, then the unchanged `reduce_cue_observations` runs **once** in authored order. Only a complete, valid staged cohort with an unchanged source generation can set `ScheduledCueStreamBatch.source_replaygain` to `Some`. Otherwise it remains `None` and the unmodified native post-output measurement remains responsible. The existing album stage owns tag application/fatal metadata errors and publication.
- Conversion failure cancels and reaps related meter work; source-meter failure alone never retries a successful conversion. Existing S1 and S3 post-output paths are retained. Output/publication paths and naming remain owned by the existing code.

## Read order

1. `VERIFICATION_RESULTS.md` for exact pass/NOT RUN boundaries and risk list.
2. `HOST_QUALIFICATION.md` for mandatory cargo, native, failure, publication and paired-performance commands.
3. `CONFORMANCE_MATRIX.md`, `CHANGED_FILES.txt`, `PRODUCTION_DIFF.patch`.
4. `SOURCE_INVENTORY.json` and `verify_ip07_static.py` for original-vs-candidate scope verification; `NATIVE_FFMPEG_RANGE_RESULTS.json` for **standalone FFmpeg only** exact sample evidence.
5. Current `../NEXT_SESSION_HANDOFF.md` and `../planning_authority/packets/IP-06_qualify_and_promote.md` with the authoritative `IP-06_SEQUENCING_AMENDMENT.md` copied from the attached packet.

## Critical distinction

This archive contains an implementation and targeted Rust regression tests, **not verified host execution**. This authoring environment has no Cargo/rustc/rustfmt and cannot fetch a Rust toolchain; no Rust compilation, native app-level observation, actual destination publication, cancellation/failure injection or IP-06 performance qualification is claimed. Keep `TONEPOET_CUE_S3_PILOT` **unset** until IP-07 has been compiled, checked, numerically qualified and host-accepted. Only then execute IP-06; never auto-promote from this candidate.
