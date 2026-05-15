# Conversion pipeline rebuild — implementation sequencing

(Successor to the Phase 0 audit. "Phase 0" was the audit step; this is
the implementation sequence that follows from it. This revision closes
the six PR-1 contract gaps raised in the first invariants review.)

## What we need from you

Review of the implementation sequence below. **Not for debate** — for
invariants. Answer two questions:

1. **Does this sequence create the right contracts before
   implementation starts?** Is every type, trait, and error defined
   in PR 1, so PRs 2+ are pure implementations of pre-existing
   contracts?
2. **Does every PR have a testable exit condition** that, if it
   passed, would prove the PR achieved its stated scope?

If yes to both, say so in a paragraph. If no, name the PR, name the
defect, propose the fix. Don't relitigate the audit's verdicts or the
chosen architecture.

## Context (inlined — do not chase external files)

Fetch `https://github.com/barstoolbluz/tonepoet.git` at commit
`644ac50` **only to inspect the three audited functions and their
dependencies** (`process_item`, `extract_and_convert_7z`,
`extract_single_image_tracks`, `ConversionItem`, `ConversionStatus`,
`ConversionOptions`, `AudioFormat`, `ProcessorConfig`, the queue
layer, the `tonepoet-backend` crate). The audit is summarized below;
its full text is not in that commit.

**Audit verdicts:** `process_item` → refactor; `extract_and_convert_7z`
→ rebuild; `extract_single_image_tracks` → refactor.

**Audit's key findings:** `extract_and_convert_7z` (~1,900 lines)
sends `Completed` before album ReplayGain / features / custom-output
move; partial track failure can still mark an album successful;
per-track failures log `error_message: None`; tools run via ad hoc
command construction with no timeout/cancellation/stderr capture; the
`.extract_*` dir doubles as workdir and final output; `process_item`
has two failure channels.

**Adopted architecture — canonical pipeline stage order:**
```
materialize → plan-outputs → convert → merge? → metadata
  → replaygain → features → publish → complete
```
- `materialize` — parse/unpack a container into a `PreparedSource`
  manifest. **Rule (settles the materialize-vs-convert boundary):** a
  materializer *describes* tracks as `TrackSourceRef`s. It may
  extract container members that already exist as discrete files
  (archive members). It **never** cuts, decodes, or transcodes audio
  — cutting a CUE image into tracks and DST-decoding a SACD track are
  *creation*, and belong to `convert`, not `materialize`.
- `plan-outputs` — assign each track its final output path from the
  request's naming policy + output root.
- `convert` — for each track: `realize` the `TrackSourceRef` into a
  decodable file (no-op for `StagedFile`; ffmpeg segment-cut for
  `ImageSegment`; sacd-rs decode for `SacdTrack`), then run the
  backend encode.
- `merge?` — optional; concatenate converted tracks into one album
  artifact. Off by default.
- `metadata` — apply tags to the post-merge artifact set (N track
  files, or 1 merged file).
- `replaygain` — merged: single-file gain; not merged: album-mode
  scan across the N tracks, applied uniformly.
- `features` — conversion-log + CUE-sheet generation.
- `publish` — atomic move staging → final, rollback on failure.
- `complete` — completion event + durable log; fires last.

**Core invariant:** a source item becomes a manifest of expected
tracks before any conversion starts. The completion event fires only
after every required stage has succeeded for the whole album and the
final artifact(s) are published. A track or required-stage failure
under the default policy blocks the album; partial completion
requires explicit opt-in and is marked **partial**, never successful.

**Crash-resume model:** a `PreparedSource` is *re-derivable*, not
durable state. `materialize` is deterministic given the request, so
the manifest is reproducible. Staging dirs are ephemeral. On restart
the pipeline discards orphaned staging dirs and re-runs the item from
`materialize`. The queue persists the `ConversionItem` (and its
`PipelineRequest`); it does **not** persist a `PreparedSource` as
resumable state. `PreparedSource` is `Serialize`-able for logs /
diagnostics / tests only.

## Decisions taken by the user

- Manifest + request contract first; the tool runner serves them.
- Stub tool runner in PR 1; real runner in PR 2.
- Rebuild `extract_and_convert_7z` through the new pipeline **before**
  SACD/CUE; do not patch it in place. 7z is the first proving client.
- Then CUE, then SACD.
- Archival default = fail-album-on-any-track-failure. Partial output
  requires explicit opt-in and is marked partial, not successful.

## PR 1 — Contracts (every type, every error, every trait, every
function signature)

This PR defines every contract PRs 2–10 implement. Nothing here
spawns a process, converts audio, or writes to a final output dir.
After PR 1 no later PR introduces a new core type, error, or
signature — only implementations.

### Request contract (defect 1)

```rust
/// Everything a pipeline run needs, fixed before materialize starts.
/// Persisted with the queue's ConversionItem; the resumable input.
pub struct PipelineRequest {
    pub container: PathBuf,
    pub source: SourceOptions,
    pub target_format: AudioFormat,         // existing enum, reused
    pub encode_options: EncodeOptions,      // see note below
    pub merge: bool,
    pub output_root: PathBuf,
    pub naming: NamingPolicy,
    pub failure_policy: FailurePolicy,
}
```

> **`EncodeOptions` vs the legacy `ConversionOptions`:** the existing
> `ConversionOptions` mixes encode knobs (bitrate, compression level,
> backend, dither) with concerns the new pipeline owns elsewhere
> (output format, merge, ReplayGain mode, output paths). PR 1 defines
> `EncodeOptions` as the *encode-only* subset; `target_format`,
> `merge`, ReplayGain mode, and output paths are authoritative on
> `PipelineRequest` / `NamingPolicy` and are never read from the
> legacy struct. PR 10's CLI builds a `PipelineRequest`; it does not
> pass a raw `ConversionOptions` through.

```rust
/// Encode-only knobs (bitrate, compression level, backend choice,
/// dither). The non-encode fields of the legacy ConversionOptions
/// are NOT mirrored here.
pub struct EncodeOptions { /* bitrate, compression_level, backend, dither */ }

/// Source-specific knobs the materializer needs.
pub struct SourceOptions {
    pub archive_password: Option<String>,   // secret; redacted in logs
    pub sacd_area: Option<SacdArea>,        // None = auto-pick
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
}

pub enum CueSidecarPolicy { PreferSidecar, SidecarOnly, EmbeddedOnly }

pub enum TrackSelection {
    All,
    Range { start: u32, end: u32 },         // inclusive, 1-based
    Set(Vec<u32>),
}

/// Output naming. Wraps the existing rename-template machinery so
/// PR 10 adds no new naming contract.
pub struct NamingPolicy {
    pub template: String,                   // e.g. "%NN% - %TITLE%"
    pub per_album_subdir: bool,
}
```

`Materializer` and the planning/convert functions take
`&PipelineRequest`; each reads only the fields it needs.

### Manifest types (defects 2, 8)

```rust
pub enum TrackSourceRef {
    /// A discrete file already on disk (extracted archive member).
    StagedFile(PathBuf),
    /// A sample-range of a single-image audio file — NOT yet cut.
    ImageSegment { image: PathBuf, start_sample: u64, samples: u64 },
    /// A track within a SACD ISO — NOT yet decoded.
    SacdTrack { iso: PathBuf, track_index: u32, area: SacdArea },
}

pub enum SacdArea { Stereo, MultiChannel }
pub enum SourceKind { SevenZip, CueImage, SacdIso }

pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub performer: Option<String>,
    pub genre: Option<String>,
    pub date: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub isrc: Option<String>,
    pub publisher: Option<String>,
    pub copyright: Option<String>,
    pub comment: Option<String>,
    pub pre_emphasis: bool,                  // e.g. CUE `FLAGS PRE`
    pub extra: BTreeMap<String, String>,     // escape hatch
}

pub struct AlbumMetadata {
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub date: Option<String>,
    pub total_tracks: u32,
    pub total_discs: Option<u32>,
    pub disc_number: Option<u32>,
    pub extra: BTreeMap<String, String>,
}

pub struct ExtractionProvenance {
    pub source_kind: SourceKind,
    pub source_sha256: Option<String>,
    pub tool_versions: BTreeMap<String, String>,
    pub extracted_at: DateTime<Utc>,
}

/// One logical track. Carries identity only — NO output path.
/// Output planning is a separate contract (`plan_outputs`).
pub struct PreparedTrack {
    pub track_number: u32,
    pub total_tracks: u32,
    pub source_ref: TrackSourceRef,
    pub metadata: TrackMetadata,
    pub expected_samples: Option<u64>,
    pub sample_rate: u32,
}

pub struct PreparedSource {
    pub container: PathBuf,
    pub kind: SourceKind,
    pub tracks: Vec<PreparedTrack>,
    pub album_metadata: AlbumMetadata,
    pub provenance: ExtractionProvenance,
}

/// Output planning result — `(track_number, output_path)` pairs,
/// keyed by track number (not positional, so a filtered or
/// reordered manifest can't silently misalign paths to tracks).
pub struct AlbumPlan { pub entries: Vec<(u32, PathBuf)> }
```

(`plan_outputs` — the function that builds an `AlbumPlan` — is
declared in the pipeline-function-signatures block below.)

### Outcome / record / stage types (defects 3, 4, 5)

```rust
pub enum FailurePolicy {
    FailAlbumOnAnyTrackFailure,   // default
    AllowPartialAlbum,            // explicit opt-in
}

pub enum TrackOutcome { Ok, Err(String) }   // never empty error text

/// Per-track record. Carries the logical source ref AND the realized
/// input path (the decodable file `realize` produced, if any).
pub struct TrackRecord {
    pub track_number: u32,
    pub outcome: TrackOutcome,
    pub source_ref: TrackSourceRef,
    pub realized_input: Option<PathBuf>,
    pub output_file: Option<PathBuf>,
    pub command_summary: Option<String>,
    pub bytes_in: Option<u64>,
    pub bytes_out: Option<u64>,
    pub duration: Option<Duration>,
}

pub enum PipelineStage {
    Materialize, PlanOutputs, Convert, Merge,
    Metadata, ReplayGain, Features, Publish,
}

pub enum StageOutcome { Ok, Skipped, Failed(String) }

pub struct StageRecord { pub stage: PipelineStage, pub outcome: StageOutcome }

/// Whole-album outcome. Carries both per-track and per-stage records,
/// so a required-stage failure has a real home (defect 4).
pub enum AlbumOutcome {
    Complete { tracks: Vec<TrackRecord>, stages: Vec<StageRecord> },
    Partial  { successful: Vec<TrackRecord>, failed: Vec<TrackRecord>,
               stages: Vec<StageRecord> },
    Blocked  { successful: Vec<TrackRecord>, failed: Vec<TrackRecord>,
               stage_errors: Vec<StageRecord> },
}

pub struct MergedArtifact {
    pub path: PathBuf,
    pub total_samples: u64,
    pub source_tracks: Vec<u32>,
}
```

### Queue / reporting contract (defect 3)

PR 1 extends the existing `ConversionStatus` enum (in `queue.rs`)
with a terminal **partial** status. `AlbumOutcome` maps to queue
status as: `Complete → Completed`, `Partial → Partial { .. }`,
`Blocked → Failed { .. }`.

```rust
// added variant on the existing ConversionStatus enum:
Partial {
    successful: u32,
    failed: u32,
    log_path: PathBuf,        // the durable per-album log
}
```

### Staging + tool contract (defect 6)

```rust
/// Job-scoped staging directory. Drop-cleans its tree. `publish`
/// consumes it by value and `mem::forget`s the guard on a successful
/// final move, so a published album is not deleted.
pub struct StagingDir { /* root path + RAII guard */ }

/// Closed set of every external tool the pipeline invokes. SoX
/// included (tonepoet-backend exposes a SoX path). No later PR adds
/// a variant.
pub enum ToolBinary {
    SevenZip, Ffmpeg, Ffprobe, Sox, Loudgain, Metaflac,
    Opustags, Wvunpack, Wvtag, AtomicParsley,
}

pub struct EnvVar { pub key: String, pub value: String, pub secret: bool }

pub struct ToolCommand {
    pub binary: ToolBinary,
    pub args: Vec<String>,
    pub secret_args: Vec<usize>,       // arg indices to redact in logs
    pub cwd: Option<PathBuf>,
    pub env: Vec<EnvVar>,              // `secret` values redacted in logs
    pub timeout: Duration,
}

/// Process termination — handles signal death / no exit code.
pub enum ProcessExit {
    Code(i32),
    Signal(i32),
    Unknown,
}

pub struct ToolOutput {
    pub exit: ProcessExit,
    pub stdout_tail: String,           // bounded, 64 KiB
    pub stderr_tail: String,           // bounded, 64 KiB
    pub elapsed: Duration,
}
```

### Traits + every pipeline function signature

Every signature the rest of the sequence depends on is fixed here.
PRs 2–10 implement these; they declare no new public signature. The
`-> ...;` forms below are signature declarations — bodies land in
the PR named in each comment.

```rust
#[async_trait]
pub trait Materializer: Send + Sync {
    /// Parse/unpack a container into a PreparedSource. Describes
    /// tracks; never cuts/decodes/transcodes audio.
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError>;
}

#[async_trait]
pub trait ToolRunner: Send + Sync {
    async fn run(
        &self,
        cmd: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError>;
}

/// The set of artifacts a stage operates on: N per-track files
/// (merge off) or one merged file (merge on).
pub enum ArtifactSet {
    Tracks(Vec<PathBuf>),
    Merged(MergedArtifact),
}

// --- source-kind detection + materializer selection ------------
// Owns the "which Materializer" decision. Detection table defined
// in PR 1; each format's detection rule + materializer registration
// lands with that format's materializer (PR 3 / 8 / 9).

/// Classify a request's container. PR 1 ships the dispatch shell +
/// the 7z rule; PR 8/9 add the CUE-pair and SACD-ISO rules.
pub fn detect_source_kind(req: &PipelineRequest)
    -> Result<SourceKind, MaterializeError>;

/// Return the Materializer impl for a kind. PR 1 ships the match
/// shell; arms are wired as PR 3/8/9 land each impl.
pub fn materializer_for(kind: SourceKind) -> Box<dyn Materializer>;

// --- per-track realize ------------------------------------------
/// Realize one TrackSourceRef into a decodable file in staging.
/// One function; its match arms are completed across PRs:
/// `StagedFile` (PR 4, identity), `ImageSegment` (PR 8, ffmpeg cut),
/// `SacdTrack` (PR 9, sacd-rs decode).
pub async fn realize_track(
    src: &TrackSourceRef,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<PathBuf, ConvertError>;

// --- output planning (PR 4 body) --------------------------------
pub fn plan_outputs(source: &PreparedSource, req: &PipelineRequest)
    -> Result<AlbumPlan, PlanError>;

// --- pipeline stages (PR 4 = convert/publish; PR 5 = merge;
//     PR 6 = metadata/replaygain/features) ---------------------
pub async fn convert_tracks(
    source: &PreparedSource,
    plan: &AlbumPlan,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> AlbumOutcome;

pub async fn merge_tracks(
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<MergedArtifact, MergeError>;

pub async fn apply_metadata(
    artifacts: &ArtifactSet,
    source: &PreparedSource,
    cancel: &CancellationToken,
) -> StageRecord;

pub async fn apply_replaygain(
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> StageRecord;

pub async fn run_features(
    outcome: &AlbumOutcome,
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
) -> StageRecord;

pub fn publish_album_output(staging: StagingDir, final_dir: &Path)
    -> Result<(), PublishError>;

// --- orchestrator (PR 4 body, final shape) ----------------------
pub async fn run_pipeline_item(
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> AlbumOutcome;
```

### Every error type (all `thiserror`-derived)

- `MaterializeError` — `Extraction`, `Parse`, `Encrypted`,
  `Cancelled`, `Tool(ToolRunnerError)`
- `PlanError` — `NamingCollision`, `InvalidTemplate`, `EmptyManifest`
- `ToolRunnerError` — `Spawn`, `Timeout { elapsed }`, `Cancelled`,
  `NonZeroExit { exit: ProcessExit, stderr_tail: String }`
- `ConvertError` — `Realize(String)`, `TrackValidation(String)`,
  `Backend(String)`, `Tool(ToolRunnerError)` (covers the whole
  per-track path: realize + encode + post-encode validation)
- `MergeError` — `DurationMismatch`, `Io`, `Tool(ToolRunnerError)`
- `PublishError` — `StagingMissing`, `DestinationExists`,
  `CrossDeviceMove`, `RollbackFailed`, `Io`

All new code under `#![forbid(unsafe_code)]`.

### Stub `ToolRunner`

Records each `ToolCommand` into an inspectable transcript (with
redaction applied to `secret_args` + secret env); never spawns a
process; returns a test-configured `ToolOutput`.

### Does not ship

Any `Materializer` impl, real process spawning, conversion,
`realize` bodies, merge, publish, ReplayGain, metadata application.

### Testable exit condition

- Workspace compiles with the new `convert::pipeline` module; every
  type/trait/error/fn signature above is defined and `pub`.
- `PipelineRequest` / `PreparedSource` / `AlbumPlan` / `AlbumOutcome`
  / `TrackRecord` are `Serialize`/`Deserialize` and roundtrip through
  JSON (`PipelineRequest` for queue persistence; the rest for logs /
  diagnostics / tests — not as resumable job state).
- Unit test: `AlbumOutcome` aggregation — N `TrackRecord`s with one
  track failure → `FailAlbumOnAnyTrackFailure` yields `Blocked`,
  `AllowPartialAlbum` yields `Partial`, all-success yields
  `Complete`; a required-stage `StageOutcome::Failed` yields
  `Blocked` with a populated `stage_errors`.
- Unit test: `ConversionStatus` maps from each `AlbumOutcome` variant
  per the table above; `Partial` roundtrips through queue JSON.
- Unit test: stub `ToolRunner` transcript records a known command
  sequence; `secret_args` and secret env values are redacted.
- `cargo test -p tonepoet` green; no `unsafe`.

## PR 2 — Real `ToolRunner`

**Ships:** real `ToolRunner` impl — async child-process spawn via
tokio, per-command timeout, `CancellationToken` abort with child
reaping, bounded 64 KiB stdout/stderr capture, `cwd` + `env`
application, command logging with redaction of `secret_args` and
secret env, `ToolBinary` → path resolution via `ProcessorConfig`,
`ProcessExit` derived from real wait-status (including signal death).

**How stub and real divide testing (no false interchangeability):**
materializer / orchestrator PRs are generic over `&dyn ToolRunner`
and test against the stub (fast, deterministic, no spawning). PR 2's
real runner gets its own behavior tests for what the stub
structurally cannot exercise. "Interchangeability" is a compile-time
property, demonstrated in PR 7 when the PR-3 materializer runs
against the real runner in the parity sweep.

**Testable exit condition:**
- Timeout test: a command over its timeout → `ToolRunnerError::
  Timeout { elapsed }`; child reaped.
- Cancellation test: token cancelled mid-run → `Cancelled`; no
  orphaned child after a bounded grace window.
- Signal test: a child killed by signal → `ProcessExit::Signal(n)`,
  not `Code`.
- Redaction test: a 7z invocation with an archive password, and a
  secret env var, log no secret literal.
- Non-zero-exit test: failing command → `NonZeroExit` with non-empty
  `stderr_tail`.
- Bounded-capture test: a command emitting >64 KiB → exactly a
  64 KiB tail.

## PR 3 — 7z materializer

**Ships:** `SevenZipMaterializer: Materializer`. Migrates the
keep-worthy extraction behavior from `extract_and_convert_7z` —
multithreaded 7z, scratch-directory support, archive password (via
`secret_args`-redacted `ToolCommand`), source-metadata + sample-rate
probe (ffprobe through the runner). Extracts archive members (a
discrete-file unpack, allowed by the materialize rule) and returns a
`PreparedSource` whose tracks carry `TrackSourceRef::StagedFile`. No
conversion, no realize, no publish.

**Testable exit condition:**
- Corpus ≥5 archives: plain 7z, password-protected, malformed,
  empty, mixed audio+non-audio. Corpus built as a PR-3 deliverable;
  archive fixtures committed with a `.gitattributes binary` entry.
- Each archive → expected `PreparedSource` (track count, ordered
  `StagedFile` paths, detected sample rate, captured metadata).
- `TrackSelection` honored: `Range`/`Set` yield only the selected
  tracks in the manifest.
- Cancellation mid-extract → scratch dir removed, `MaterializeError::
  Cancelled`.
- Malformed / permission-denied → structured `MaterializeError`, no
  panic.

## PR 4 — Orchestrator + output planning + per-track convert +
atomic publish

This PR ships `run_pipeline_item()` — the **new orchestrator, in its
final shape, test-only**. `process_item` is **not** modified here;
it keeps routing every format to its legacy path (defect 7). PR 7 is
where `process_item` first dispatches into `run_pipeline_item()`.

All function signatures below are the PR-1 contracts; this PR
implements the bodies — it declares no new signature.

**Ships:**
- `detect_source_kind` dispatch shell + the 7z detection rule;
  `materializer_for` match shell wired for `SourceKind::SevenZip`.
- `plan_outputs` body.
- `realize_track` body — dispatch + the `StagedFile` arm (identity).
  The `ImageSegment` and `SacdTrack` arms land in PRs 8 / 9.
- `convert_tracks` body — per track: `realize_track` then backend
  encode into staging; each track yields a `TrackRecord` carrying
  `source_ref` + `realized_input` (failed records always carry error
  text — `ConvertError` is caught per track and its `Display`
  recorded into `TrackOutcome::Err`).
- `publish_album_output` body — atomic move, rollback on failure,
  cross-device fallback; consumes the `StagingDir` and
  `mem::forget`s its guard on success.
- Startup **stale-staging-dir GC**.
- `FailurePolicy` enforced at `AlbumOutcome` aggregation; mapping to
  `ConversionStatus` (incl. `Partial`).
- `run_pipeline_item()` body: `preflight → materialize → plan_outputs →
  convert → merge? → metadata → replaygain → features → publish →
  complete`, with `merge` / `metadata` / `replaygain` / `features`
  as stage-function calls. In PR 4 those four are no-ops (merge
  gated off; the others pass through, each emitting a
  `StageRecord { outcome: Skipped }`). PRs 5–6 fill the bodies;
  `run_pipeline_item()` itself never changes again.
- Internal failures are all `Err`; the single conversion to a
  terminal `ConversionStatus` happens only at the queue boundary.
- `Completed` / `Partial` emitted only after `publish_album_output`
  returns `Ok`.

**Testable exit condition:**
- 7z archive through `run_pipeline_item()` (merge off) → per-track
  output whose **decoded PCM** is byte-identical to legacy
  `extract_and_convert_7z` output for the same archive + settings.
- `plan_outputs`: a naming template producing two identical paths →
  `PlanError::NamingCollision`.
- One track fails mid-convert: default policy → `AlbumOutcome::
  Blocked`, no publish, final dir untouched; `AllowPartialAlbum` →
  `Partial`, publish proceeds, queue status `Partial`.
- Fault injected between convert and publish → no half-published
  output; the orphaned staging dir is removed by the next run's GC
  (test drives two runs).
- Event-ordering test: a subscriber never observes `Completed` /
  `Partial` before `publish_album_output` returns `Ok`.
- Every failed `TrackRecord` has non-empty error text.

## PR 5 — Album merge stage

**Ships:** the body of the `merge_tracks` stage function (signature
fixed in PR 1). Optional, gated by `PipelineRequest.merge`. Own
validation (merged duration ≈ Σ track durations) and cleanup. Runs
after `convert`, before `metadata`. When merge is on, downstream
stages operate on the single `MergedArtifact`. `run_pipeline_item()`
is **not modified** — only the stage-function body lands; on
success the stage emits a `StageRecord { stage: Merge, outcome: Ok }`.

**Testable exit condition:**
- Multi-track album, `merge` on → single `MergedArtifact`; decoded
  PCM equals the concatenation of per-track decoded PCM.
- Merge duration-validation rejects a deliberately truncated input
  → `MergeError::DurationMismatch`, album `Blocked` with a
  `Merge`/`Failed` `StageRecord`.
- `merge` off → behavior identical to PR 4.
- Legacy `extract_and_convert_7z` merge path vs new merge path:
  decoded PCM byte-identical for the same archive.

## PR 6 — Metadata, ReplayGain, feature stages + durable log

**Ships:** the bodies of the `apply_metadata`, `apply_replaygain`,
and `run_features` stage functions (signatures fixed in PR 1).
`run_pipeline_item()` is **not modified**.
- `apply_metadata` — apply `TrackMetadata` / `AlbumMetadata` to the
  post-merge artifact set (format-correct: ID3 / Vorbis / MP4).
- `apply_replaygain` — merged: single-file gain; not merged:
  album-mode scan across the N tracks, applied uniformly.
- `run_features` — conversion-log + CUE-sheet generation via
  `tonepoet-features`.
- The **durable per-album log** — structured record (every
  `TrackRecord`, every `StageRecord`, the `AlbumOutcome`,
  source/staging/final paths, tool command summaries, timings),
  written as a required finalization step before `complete`. Its
  path is the `log_path` carried in `ConversionStatus::Partial` /
  recorded for all outcomes.
- Each stage carries an explicit required-vs-optional policy. A
  required-stage failure → `AlbumOutcome::Blocked` with the failing
  `StageRecord` in `stage_errors`, no publish. An optional-stage
  failure → album still completes, the `StageRecord` is recorded
  `Failed` but non-blocking.

**Testable exit condition:**
- Converted outputs carry expected tags, verified per output format.
- Album ReplayGain, merge off: all N outputs carry identical
  album-gain/peak tags; track-gain differs per track. Merge on: the
  merged file carries one coherent gain/peak.
- Durable log written for every album (complete / partial /
  blocked); failed tracks' error text and failed stages' error text
  both present.
- Required-stage failure → `Blocked` + populated `stage_errors`, no
  publish. Optional-stage failure → album completes, failure logged.
- 7z album through PR 3→4→5→6: full output (audio + tags + RG + log)
  matches legacy `extract_and_convert_7z` for the same input.

## PR 7 — 7z parity gate + retire `extract_and_convert_7z`

**Ships, in order within the PR:**
1. Record a legacy baseline: run legacy `extract_and_convert_7z`
   over the regression corpus; capture per-archive outputs + tags +
   RG + conversion-log content as fixtures.
2. Run the new pipeline (`run_pipeline_item()`) over the same
   corpus; assert parity.
3. Modify `process_item` to dispatch 7z into `run_pipeline_item()`
   (the first user-facing format on the new pipeline). CUE/SACD
   stay on legacy.
4. Delete `extract_and_convert_7z`.

**Testable exit condition:**
- Regression corpus (PR 3 archives + a wider real-archive set): for
  every archive, new-pipeline output vs the recorded legacy baseline
  — decoded PCM byte-identical, tags equal, RG values equal,
  conversion-log content equivalent.
- `grep -rn 'extract_and_convert_7z' src/` returns zero hits.
- `cargo test -p tonepoet` green.

## PR 8 — CUE materializer

**Ships:** `CueImageMaterializer: Materializer` — parses the CUE
sheet (honoring `CueSidecarPolicy`), probes the image's sample
count/rate, and returns a `PreparedSource` whose tracks carry
`TrackSourceRef::ImageSegment` and full CUE metadata (titles,
performers, ISRC, `FLAGS PRE` → `pre_emphasis`, `REM` date/genre).
It does **not** run ffmpeg/wvunpack — per the materialize rule, no
cutting. The `ImageSegment` arm of `realize_track` lands in this PR:
ffmpeg `-ss`/`-t` segment cut (wvunpack fallback for WavPack v4),
post-cut validation (file exists, nonzero size, sample count within
tolerance of `expected_samples`), cleanup guard on partial failure.
Adds the CUE-pair detection rule to `detect_source_kind` and wires
`SourceKind::CueImage` in `materializer_for`. `process_item`
modified to dispatch CUE single-image pairs into
`run_pipeline_item()` — net-new capability (today the conversion
path ignores the CUE), not a regression flip.

**Testable exit condition:**
- Corpus ≥5: standard FLAC+CUE, WavPack-v4+CUE, APE+CUE, CUE with
  `FLAGS PRE`, CUE with embedded `CUESHEET` conflicting with a
  sidecar (`CueSidecarPolicy::PreferSidecar` → sidecar wins).
- Each → expected manifest (`ImageSegment` refs + metadata).
- `realize_track` on an `ImageSegment` with forced `-ss` drift →
  sample-count validation fails → `ConvertError::TrackValidation`,
  track recorded failed.
- Malformed CUE → `MaterializeError::Parse`; no temp residue.
- CUE album end-to-end through `run_pipeline_item()` → expected
  outputs.

## PR 9 — SACD materializer

**Ships:** `SacdIsoMaterializer: Materializer` — parses the SACD TOC,
maps TOC text → `TrackMetadata` / `AlbumMetadata`, applies
`SacdArea` selection, and returns a `PreparedSource` whose tracks
carry `TrackSourceRef::SacdTrack`. It does **not** decode audio. The
`SacdTrack` arm of `realize_track` lands in this PR: in-process
`sacd-rs` extraction of one track to a staged DSF/DFF. Encrypted ISO
→ `MaterializeError::Encrypted` (no panic). Adds the SACD-ISO
detection rule to `detect_source_kind` and wires
`SourceKind::SacdIso` in `materializer_for`. `process_item` modified
to dispatch SACD ISOs into `run_pipeline_item()` — net-new
capability.

**Testable exit condition:**
- Solo Monk ISO → 13-track manifest (titles, ISRCs, durations,
  sample rate correct), `SacdTrack` refs.
- Al Jarreau ISO → 11-track manifests for both `SacdArea::Stereo`
  and `SacdArea::MultiChannel`.
- `realize_track` on a `SacdTrack` → staged DSF/DFF byte-identical
  to the prior `sacd-rs` gauntlet output — the 70/70 baseline.
- SACD album end-to-end through `run_pipeline_item()` → expected
  converted outputs.
- Encrypted ISO → `MaterializeError::Encrypted`, no panic.

## PR 10 — CLI + TUI multi-track surface + docs

**Ships:** CLI flags mapping to `PipelineRequest` / `SourceOptions`
(`--track`/`--track-range` → `TrackSelection`, `--area` → `SacdArea`,
`--no-cue` → `CueSidecarPolicy`, partial-output opt-in →
`FailurePolicy`); TUI source-pane per-track listing, area pill,
track-selection, queue per-track expansion, `Partial` status
rendering; README + CLAUDE.md updated; memory artifact capturing the
migration. `determine_output_path` stays for the single-file fast
path — `plan_outputs` owns multi-track output planning.

**Testable exit condition:**
- CLI: `tonepoet convert disc.iso --format opus -o out/` produces N
  correctly-named, correctly-tagged files; `--track` /
  `--track-range` / `--area` / `--no-cue` / partial-opt-in each
  covered by a CLI test that asserts the resulting `PipelineRequest`.
- TUI: dropping a SACD ISO and a CUE+FLAC pair each shows the
  per-track listing and queues N expanded items; a `Partial` album
  renders as partial, not completed (driven test, or documented
  manual check if the TUI can't be automated).
- `cargo test` green workspace-wide.

## Invariants the sequence guarantees

1. **Contracts before behavior** — PR 1 ships every type, trait,
   error, and function signature. Types/errors: `PipelineRequest`,
   `SourceOptions`, `EncodeOptions`, `NamingPolicy`, `AlbumPlan`,
   `ArtifactSet`, `StageRecord`/`StageOutcome`, `AlbumOutcome` with
   stage records, `ConversionStatus::Partial`, `MergedArtifact`,
   `MergeError`, `PlanError`, `StagingDir`, `ToolBinary` incl. SoX,
   `ProcessExit`. Function signatures: `detect_source_kind`,
   `materializer_for`, `realize_track`, `plan_outputs`,
   `convert_tracks`, `merge_tracks`, `apply_metadata`,
   `apply_replaygain`, `run_features`, `publish_album_output`,
   `run_pipeline_item`. PRs 2–10 implement these bodies and declare
   no new public type, error, or signature.
2. **No retroactive contract growth** — `TrackSourceRef` and
   `ToolBinary` are closed enums fully defined in PR 1;
   `TrackMetadata`/`AlbumMetadata` carry `extra` maps; output paths
   are computed by `plan_outputs`, not back-filled into the manifest.
3. **Materialize describes, convert realizes** — a materializer
   produces `TrackSourceRef`s and may unpack discrete archive
   members, but never cuts/decodes/transcodes. `realize_track`
   (called inside `convert`) does all per-track realization. One
   rule; PR 8/9 conform.
4. **Stable orchestrator** — `run_pipeline_item()` is written once,
   in PR 4, in final shape. PRs 5–6 fill stage-function bodies;
   PRs 8–9 fill `realize_track` arms; none touch
   `run_pipeline_item()`. `process_item` changes only to flip
   dispatch (PR 7 for 7z, PR 8 for CUE, PR 9 for SACD).
5. **Re-derivable manifest** — the queue persists `PipelineRequest`,
   never a `PreparedSource`; crash-resume re-runs from `materialize`.
6. **Fixed stage order** — materialize → plan-outputs → convert →
   merge? → metadata → replaygain → features → publish → complete;
   no PR reorders it; no PR emits `Completed`/`Partial` before
   publish.
7. **Failure policy is data** — materializers are policy-agnostic;
   `AlbumOutcome` aggregation enforces policy; both track failures
   and required-stage failures yield `Blocked`; partial is never
   "successful."
8. **No regression window** — through PRs 4–6 every format stays
   user-facing on its legacy path; the new pipeline is test-only.
   PR 7 is the one destructive step (deletes legacy 7z) and the
   first user-facing flip; PRs 8–9 flip CUE/SACD, both net-new.

## Not in scope for this review

- Whether the audit's verdicts were right (decided).
- Whether the architecture is correct (decided).
- Whether the user's sequencing preferences are correct (decided).
- Time estimates.
- Speculative additions.

Markdown output, as terse as the review allows. If the sequence is
sound on both questions, say so in one paragraph.
