# Chunk 2.1.2: Transactional Publish + Manifest-Based Reruns

## For: Reasoning model (GPT Pro)
## Project: tonepoet — CLI + TUI audio conversion toolkit
## Language: Rust (edition 2021, async via Tokio)
## Quality bar: Rigor, correctness, robustness, idempotency, performance (in that order).
## Prerequisites: Chunk 1 (pipeline crate), Chunk 2 (orchestrator), and Chunk 2.1.1 (settings sentinel + fingerprint) are integrated and compiling.

---

## 1. What this chunk does

Make idempotency concrete. Today, reruns either fail (`FailIfExists`) or blindly overwrite (`ReplaceWithBackup`) with no awareness of whether the previous output matches the current settings. This chunk adds:

1. A **conversion manifest** that records what was converted, with what settings, producing what output.
2. A **rerun decision table** that uses the manifest + on-disk state to choose: skip, verify, redo, or fail.
3. **Transactional output states** (.partial → validated → final) so no corrupt output ever appears at the final path.

---

## 2. What exists today

### 2.1 Atomic album publish (already good)

`publish_album_output()` in stages.rs (lines 3096-3228) already does album-level atomic publishing:
1. Creates a temp directory (`.{album}.tmp-{nanos}-{attempt}`)
2. Copies/renames files from staging into temp
3. If overwriting: writes recovery marker, backs up existing album
4. Atomic rename: `temp_dir → album_dir`
5. Removes recovery marker
6. fsync parent directory

Cross-filesystem is handled: tries rename first, falls back to copy+fsync if cross-device.

Recovery from interrupted publish exists: a recovery marker + backup directory enables rollback on next attempt.

### 2.2 What's missing

- **No manifest.** After publish, there's no record of what settings produced this output. The durable JSON log has the settings but is not structured for rerun decisions.
- **No fingerprint in logs.** The settings fingerprint (from Chunk 2.1.1) exists but isn't recorded anywhere in the pipeline output.
- **No content hashes.** Output files have no recorded hash for verification.
- **No skip/verify mode.** `OverwritePolicy` has only `FailIfExists` and `ReplaceWithBackup`. There's no "skip if identical settings" or "verify existing output integrity."
- **No per-track state tracking.** The system operates at album granularity. A partially completed album (3 of 10 tracks published before crash) has no manifest to resume from.
- **No `.partial` or `.validated` file states.** Track-level work files exist only in the staging directory, which is cleaned up on drop.

### 2.3 What the pipeline crate already provides

- `ConversionPlan.cleanup_paths()` — deterministic list of intermediate files to remove
- `Finalization::AtomicRename { from, to }` — per-track atomic rename semantics
- `SettingsFingerprint` — SHA-256 of all conversion-affecting settings (47 fields)
- `settings_fingerprint()` — pure function, deterministic

---

## 3. Deliverables

### 3.1 Conversion manifest schema

Define a per-album manifest that records conversion identity and output facts. Written atomically alongside the published output.

Suggested fields per track entry:

```
source_path: PathBuf
source_size: u64
source_mtime_secs: i64          // seconds since epoch, for cheap staleness check
source_audio_md5: Option<String> // if available from probe/FLAC streaminfo
track_identity: TrackIdentity    // source ordinal, track number, disc number
settings_fingerprint: SettingsFingerprint  // SHA-256 from Chunk 2.1.1
planner_version: String         // tonepoet-pipeline crate version
planned_command_hash: String    // hash of the planned command sequence
output_path: PathBuf
output_size: u64
output_hash: Option<String>     // SHA-256 of output file content (optional, expensive)
validation_status: ValidationStatus  // Passed, Skipped, Failed
publish_timestamp: DateTime<Utc>
```

Album-level fields:

```
manifest_version: u32           // schema version for forward compatibility
album_dir: PathBuf
total_tracks: usize
settings: PipelineSettings      // full settings (not just fingerprint) for human inspection
```

**File location:** `{album_dir}/.tonepoet-manifest.json`

**Write strategy:** Atomic write (write to `.tonepoet-manifest.json.tmp`, fsync, rename). The manifest is the last file written after all tracks are published and validated.

### 3.2 OverwritePolicy expansion

Add new variants to `OverwritePolicy`:

```rust
pub enum OverwritePolicy {
    FailIfExists,         // Current: error if output exists
    ReplaceWithBackup,    // Current: backup and replace
    SkipIfManifestMatch,  // NEW: skip if manifest fingerprint matches current settings
    VerifyIfManifestMatch,// NEW: verify output integrity if fingerprint matches, redo if mismatch
    AlwaysRedo,           // NEW: always reconvert, no manifest check (for testing/debugging)
}
```

### 3.3 Rerun decision table

When the orchestrator encounters an existing album directory at publish time, it consults the manifest:

| On-disk state | Manifest state | Settings match? | Action |
|--------------|----------------|-----------------|--------|
| Album dir exists | Manifest present, valid | Fingerprint matches | **Skip** (SkipIfManifestMatch) or **Verify** (VerifyIfManifestMatch) |
| Album dir exists | Manifest present, valid | Fingerprint differs | **Redo** (backup old, reconvert) |
| Album dir exists | Manifest present, corrupt/unreadable | — | **Redo** with warning |
| Album dir exists | No manifest | — | Apply OverwritePolicy (FailIfExists or ReplaceWithBackup) |
| Album dir missing | Manifest present (orphan) | — | Ignore manifest, proceed normally |
| Album dir missing | No manifest | — | Proceed normally |
| `.partial` temp dir exists | — | — | Delete and proceed (interrupted previous attempt) |
| Backup dir exists | Recovery marker present | — | Repair (existing recovery path) |

### 3.4 Per-track validation

After encoding and before publishing, optionally verify each track:

- **FLAC:** `flac -t -s` (native decode test) — already planned by tonepoet-pipeline's FlacPlugin
- **Other formats:** ffmpeg decode-to-null (`ffmpeg -i output -f null -`) — already planned by FfmpegPlugin
- **Hash recording:** If validation passes, record output file hash in manifest entry

Validation is controlled by `VerificationSettings.verify_after_encode` (already in PipelineSettings).

### 3.5 Manifest-based skip logic

When `OverwritePolicy::SkipIfManifestMatch`:

1. Read `{album_dir}/.tonepoet-manifest.json`
2. Compare `settings_fingerprint` in manifest vs current `settings_fingerprint()`
3. If match: check all output files exist and sizes match manifest
4. If all good: skip conversion, report as `AlbumOutcome::Skipped`
5. If any file missing or size mismatch: redo

When `OverwritePolicy::VerifyIfManifestMatch`:

Same as skip, but after confirming manifest match, also verify each output file's integrity (decode test or hash check if `output_hash` is present in manifest).

### 3.6 Manifest in the durable log

Embed the settings fingerprint in the existing durable JSON log (`PipelineReport`). Add:

```rust
pub struct PipelineReport {
    // ... existing fields ...
    pub settings_fingerprint: Option<SettingsFingerprint>,
    pub manifest_path: Option<PathBuf>,
}
```

---

## 4. Design constraints

1. **The tonepoet-pipeline crate is not modified.** The manifest, rerun logic, and OverwritePolicy expansion live in the main crate.
2. **Atomic writes everywhere.** Manifests, recovery markers, and output files use write-to-temp + rename. No partial writes at final paths.
3. **Manifest is the source of truth for reruns.** File existence alone is not sufficient — the manifest proves settings identity.
4. **Manifest failure is not fatal.** If the manifest can't be written (permissions, disk full), the conversion still succeeds — the output is published, a warning is logged, and the next rerun treats the album as "no manifest" (apply OverwritePolicy).
5. **Content hashing is optional and expensive.** `output_hash` is populated only when `VerificationSettings.verify_after_encode` is true. Skip decisions can use `output_size` as a cheap proxy.
6. **Backward compatible.** Albums converted before manifests were added have no manifest. They're treated as "no manifest" and the existing OverwritePolicy applies.
7. **Manifest schema is versioned.** `manifest_version: u32` enables forward-compatible changes.

---

## 5. Integration points

### 5.1 Where the manifest is written

In `publish_album_output()` (stages.rs), after the atomic rename succeeds and before the recovery marker is deleted:

```
temp_dir created
  → files copied/renamed into temp
  → manifest written to temp/.tonepoet-manifest.json
temp_dir atomically renamed to album_dir
recovery marker deleted
```

The manifest is part of the published album — it moves atomically with the output files.

### 5.2 Where the manifest is read

In the orchestrator, before the convert stage begins. If the output album directory already exists:

1. Try to read `{album_dir}/.tonepoet-manifest.json`
2. Apply the rerun decision table (Section 3.3)
3. If skip: short-circuit the entire pipeline for this album
4. If redo: proceed with conversion (backup handled by existing publish logic)

### 5.3 Where the fingerprint is recorded

- In the manifest (`settings_fingerprint` field)
- In the durable log (`PipelineReport.settings_fingerprint`)
- In the conversion log (tonepoet-features, if we add it there)

---

## 6. Code files the reasoning model needs

1. **stages.rs excerpts** — `publish_album_output()`, recovery marker handling, atomic write utilities
2. **types.rs** — `PublishPolicy`, `OverwritePolicy`, `PipelineReport`, `StagingDir`
3. **tonepoet-pipeline/src/fingerprint.rs** — `SettingsFingerprint`, `settings_fingerprint()`
4. **tonepoet-pipeline/src/plan.rs** — `ConversionPlan`, `Finalization`, `cleanup_paths()`

---

## 7. Deliverables

1. **Manifest schema** — `ConversionManifest` struct with per-track entries and album-level metadata. Serde-serializable to JSON.

2. **Manifest I/O** — `write_manifest()` (atomic) and `read_manifest()` (fallible, returns Option) functions.

3. **OverwritePolicy expansion** — add `SkipIfManifestMatch`, `VerifyIfManifestMatch`, `AlwaysRedo` variants.

4. **Rerun decision function** — given album path + current settings + OverwritePolicy, returns `RerunDecision` (Skip, Verify, Redo, Proceed, Fail).

5. **Integration into publish_album_output()** — manifest written as part of the atomic publish.

6. **Integration into orchestrator** — rerun check before convert stage.

7. **Fingerprint in PipelineReport** — add `settings_fingerprint` field to the durable log.

8. **Tests:**
   - Manifest round-trip (write, read, compare)
   - Rerun decision for every row in the decision table (Section 3.3)
   - Skip behavior: matching manifest → no conversion runs
   - Redo behavior: mismatched fingerprint → full reconversion
   - Corrupt manifest → redo with warning
   - Missing manifest → apply OverwritePolicy
   - Orphan manifest (no album dir) → ignored
   - Stale `.partial` temp dir → deleted on next run
   - Manifest survives atomic publish (written inside temp dir before rename)

9. **Sequenced implementation plan** — which files change, in what order.

For each item: struct definitions, function signatures, data flow. Do not modify the tonepoet-pipeline crate.

---

## 8. Acceptance criteria

- [ ] A `ConversionManifest` struct exists with per-track entries and settings fingerprint
- [ ] Manifest is written atomically inside the published album directory
- [ ] `OverwritePolicy` has skip/verify/redo variants
- [ ] Rerun decision table is implemented and tested for every state combination
- [ ] Matching manifest + matching files → skip (no redundant work)
- [ ] Mismatched fingerprint → full redo
- [ ] Missing or corrupt manifest → falls back to existing OverwritePolicy
- [ ] Settings fingerprint appears in the durable JSON log
- [ ] All tests pass, clippy clean, no new warnings
