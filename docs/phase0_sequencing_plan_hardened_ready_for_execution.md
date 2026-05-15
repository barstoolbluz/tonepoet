# Conversion pipeline rebuild - hardened implementation sequence v2

This version is the implementation source of truth for the local agent. It keeps the chosen architecture and PR order, but closes the remaining contract holes: stage policy, event reporting, durable logging, publish mapping, secret redaction, command diagnostics, blocked-outcome stage records, and Rust-compilable PR-1 stubs.

## Review verdict

This version satisfies the two review questions:

1. PR 1 defines the public contracts PRs 2-10 implement: request data, source identity, output planning, artifact identity, publish mapping, stage policy, tool execution, command diagnostics, terminal event reporting, durable logging, queue status mapping, and every public stage function.
2. Each PR has an exit condition that tests the scope of that PR at its boundary. No PR relies on a later PR to make its own contract meaningful.

Later PRs may add private helpers and public implementation structs such as `SevenZipMaterializer` or `RealToolRunner`. Later PRs must not add or alter public contract types, public stage signatures, terminal statuses, core errors, or source/artifact identity.

## Source constraints

Use the repository at:

```text
https://github.com/barstoolbluz/tonepoet.git
commit: 644ac50
```

Inspect only:

- `src/convert/processor.rs::process_item`
- `src/convert/processor.rs::extract_and_convert_7z`
- `src/tui/cue_parser.rs::extract_single_image_tracks`
- Direct dependencies: `ConversionItem`, `ConversionStatus`, `ConversionOptions`, `AudioFormat`, `ProcessorConfig`, queue orchestration, and `tonepoet-backend`

Do not re-audit unrelated code.

## Runtime order

The canonical runtime order is fixed:

```text
materialize
  -> plan-outputs
  -> convert
  -> merge?
  -> metadata
  -> replaygain
  -> features
  -> publish
  -> durable-log
  -> terminal-event
```

Rules:

- `materialize` parses or unpacks the source into a manifest. It may extract already-discrete archive members. It does not cut, decode, transcode, encode, tag, run ReplayGain, generate feature files, or write final outputs.
- `plan-outputs` assigns final paths before conversion starts.
- `convert` realizes each `TrackSourceRef` into decodable audio, then invokes the backend encoder.
- `merge?` is optional and off by default.
- `metadata`, `replaygain`, and `features` mutate or create staged artifacts only.
- `publish` is the only step that touches final artifact paths.
- `durable-log` writes the durable per-album run record to the configured log sink.
- `terminal-event` is the only point where the queue observes `Completed`, `Partial`, or `Failed`.

A terminal event fires only after all required work for that terminal state has finished. Track failure under the default policy blocks the album. Partial output requires explicit opt-in and maps to `Partial`, never to `Completed`.

## Crash-resume model

A `PreparedSource` is re-derivable, not job state. The queue persists a `PipelineRequest`. On restart, the pipeline deletes orphaned staging dirs and re-runs from `materialize`. The pipeline never trusts a half-finished staging tree as authoritative. `PreparedSource` can be serialized for logs, diagnostics, and tests, but the queue does not reload it as resumable state.

## PR 1 - Contracts

PR 1 defines every public contract PRs 2-10 implement. It does not spawn processes, convert audio, tag files, run ReplayGain, publish artifacts, or route user conversions through the new pipeline.

Important Rust constraint: PR 1 must ship compiling stub bodies for all public free functions listed below. Rust modules cannot contain body-less free-function declarations. PR 1 stubs must not panic. They return `Unsupported`, `Skipped`, or a blocked `PipelineReport` as appropriate. PRs 2-10 replace those bodies without changing signatures.

### Request contract

```rust
pub struct PipelineRequest {
    pub job_id: String,
    pub item_id: String,
    pub container: PathBuf,
    pub source: SourceOptions,
    pub target_format: AudioFormat,
    pub encode: EncodeOptions,
    pub merge: bool,
    pub output_root: PathBuf,
    pub naming: NamingPolicy,
    pub publish: PublishPolicy,
    pub log: LogPolicy,
    pub stages: StagePolicy,
    pub failure_policy: FailurePolicy,
}
```

`PipelineRequest` is the resumable input. `ConversionItem` gets an optional `pipeline_request: Option<PipelineRequest>` during migration. Legacy fields remain until PR 10 finishes the CLI/TUI surface.

```rust
pub struct EncodeOptions {
    pub backend: EncodeBackend,
    pub bitrate: Option<u32>,
    pub compression_level: Option<u8>,
    pub dither: DitherPolicy,
}

pub enum EncodeBackend {
    Auto,
    Ffmpeg,
    Sox,
    BackendCrate,
}

pub enum DitherPolicy {
    Auto,
    Off,
    On,
}
```

Secret handling is a contract, not an implementation detail:

```rust
pub struct SecretString(String);
```

Rules for `SecretString`:

- `Debug` and `Display` always print a redaction marker.
- Tool transcripts, command logs, progress messages, durable logs, and user-facing logs never print the inner value.
- Queue persistence is the only permitted unredacted serialization path. If the implementation chooses not to persist secrets, encrypted archive jobs must fail resume with a structured request-validation error that asks for the password again.
- Durable logs serialize `RedactedPipelineRequest`, not raw `PipelineRequest`.

```rust
pub struct RedactedPipelineRequest {
    pub job_id: String,
    pub item_id: String,
    pub container: PathBuf,
    pub source: RedactedSourceOptions,
    pub target_format: AudioFormat,
    pub encode: EncodeOptions,
    pub merge: bool,
    pub output_root: PathBuf,
    pub naming: NamingPolicy,
    pub publish: PublishPolicy,
    pub log: LogPolicy,
    pub stages: StagePolicy,
    pub failure_policy: FailurePolicy,
}

pub struct RedactedSourceOptions {
    pub archive_password: Option<String>,   // always "<redacted>" when present
    pub sacd_area: Option<SacdArea>,
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
}
```

```rust
pub struct SourceOptions {
    pub archive_password: Option<SecretString>,
    pub sacd_area: Option<SacdArea>,
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
}

pub enum CueSidecarPolicy {
    PreferSidecar,
    SidecarOnly,
    EmbeddedOnly,
    IgnoreCue,
}

pub enum TrackSelection {
    All,
    Range { start: u32, end: u32 },
    Set(BTreeSet<u32>),
}

pub struct NamingPolicy {
    pub template: String,
    pub per_album_subdir: bool,
    pub collision_policy: NamingCollisionPolicy,
}

pub enum NamingCollisionPolicy {
    Fail,
    AppendStableSuffix,
}

pub struct PublishPolicy {
    pub overwrite: OverwritePolicy,
    pub same_filesystem_required: bool,
}

pub enum OverwritePolicy {
    FailIfExists,
    ReplaceWithBackup,
}

pub struct LogPolicy {
    pub root: PathBuf,
    pub write_for_blocked: bool,
}

pub struct StagePolicy {
    pub metadata: StageRequirement,
    pub replaygain: StageRequirement,
    pub features: StageRequirement,
}

pub enum StageRequirement {
    Required,
    Optional,
    Disabled,
}
```

Interpretation:

- `Disabled` creates a `StageRecord { outcome: Skipped }` and cannot block.
- `Optional` records failure but cannot block.
- `Required` blocks on failure.
- `durable-log` is always required for `Complete` and `Partial`. For `Blocked`, it follows `LogPolicy.write_for_blocked`.

### Source and track identity

Track number alone is not a stable key. Multi-disc albums can reuse track numbers, filtered selections can skip numbers, and set selections must not reorder work by accident. PR 1 defines source identity explicitly.

```rust
pub struct TrackId {
    pub source_ordinal: u32,       // 1-based order in the original source
    pub disc_number: Option<u32>,
    pub track_number: u32,         // tag-visible track number
}

pub enum TrackSourceRef {
    StagedFile(PathBuf),
    ImageSegment {
        image: PathBuf,
        start_sample: u64,
        samples: u64,
    },
    SacdTrack {
        iso: PathBuf,
        track_index: u32,
        area: SacdArea,
    },
}

pub enum SacdArea {
    Stereo,
    MultiChannel,
}

pub enum SourceKind {
    SevenZip,
    CueImage,
    SacdIso,
}

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
    pub pre_emphasis: bool,
    pub extra: BTreeMap<String, String>,
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

pub struct PreparedTrack {
    pub id: TrackId,
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
```

### Output planning and artifacts

The manifest never carries final output paths. `plan_outputs` creates a separate plan.

```rust
pub struct AlbumPlan {
    pub album_dir: PathBuf,
    pub entries: Vec<PlannedTrackOutput>,
}

pub struct PlannedTrackOutput {
    pub track_id: TrackId,
    pub final_path: PathBuf,
}
```

Stage outputs use staged artifact paths until publish succeeds.

```rust
pub struct TrackArtifact {
    pub track_id: TrackId,
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub samples: Option<u64>,
}

pub struct MergedArtifact {
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub total_samples: u64,
    pub source_tracks: Vec<TrackId>,
}

pub enum AudioArtifacts {
    Tracks(Vec<TrackArtifact>),
    Merged(MergedArtifact),
}

pub enum SidecarKind {
    ConversionLog,
    CueSheet,
    Other(String),
}

pub struct SidecarArtifact {
    pub kind: SidecarKind,
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
}

pub struct ArtifactSet {
    pub audio: AudioArtifacts,
    pub sidecars: Vec<SidecarArtifact>,
}
```

`ArtifactSet` contains user-facing artifacts only: audio files and sidecars published with the album. It does not contain the durable log. The durable log writes to `LogPolicy.root` after publish.

### Publish contract

Publishing needs a staged-to-final mapping. Moving a directory alone is not enough for selected tracks, merge output, sidecar files, or cross-device fallback.

```rust
pub struct PublishPlan {
    pub album_dir: PathBuf,
    pub entries: Vec<PublishEntry>,
}

pub struct PublishEntry {
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub role: PublishRole,
}

pub enum PublishRole {
    Audio,
    Sidecar(SidecarKind),
}

pub struct PublishedAlbum {
    pub album_dir: PathBuf,
    pub entries: Vec<PublishedEntry>,
}

pub struct PublishedEntry {
    pub final_path: PathBuf,
    pub role: PublishRole,
    pub bytes: u64,
}
```

Publish semantics:

- Same filesystem: stage into a temp dir under the final parent, then atomically rename into place.
- Cross filesystem: copy into a temp dir under the final parent, fsync when supported, then atomically rename into place.
- Existing destination with `ReplaceWithBackup`: move the existing destination to a backup path before final rename; restore the backup if the new publish fails.
- No consumer should observe an incomplete final album dir.
- If publish fails, the terminal status is `Failed`; durable log still writes to `LogPolicy.root` if `write_for_blocked` is true.

### Outcome, stage, and queue status contracts

```rust
pub enum FailurePolicy {
    FailAlbumOnAnyTrackFailure,
    AllowPartialAlbum,
}

pub enum TrackOutcome {
    Ok,
    Err(String),     // non-empty
}

pub struct CommandRecord {
    pub binary: ToolBinary,
    pub sanitized_args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env_keys: Vec<String>,
    pub exit: Option<ProcessExit>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub elapsed: Duration,
}

pub struct TrackRecord {
    pub track_id: TrackId,
    pub outcome: TrackOutcome,
    pub source_ref: TrackSourceRef,
    pub realized_input: Option<PathBuf>,
    pub output_file: Option<PathBuf>,
    pub commands: Vec<CommandRecord>,
    pub bytes_in: Option<u64>,
    pub bytes_out: Option<u64>,
    pub duration: Option<Duration>,
}

pub enum PipelineStage {
    Materialize,
    PlanOutputs,
    Convert,
    Merge,
    Metadata,
    ReplayGain,
    Features,
    Publish,
    DurableLog,
}

pub enum StageOutcome {
    Ok,
    Skipped,
    Failed(String),  // non-empty
}

pub struct StageRecord {
    pub stage: PipelineStage,
    pub outcome: StageOutcome,
}

pub enum BlockReason {
    TrackFailures,
    RequiredStageFailure(PipelineStage),
    MaterializeFailed,
    PlanFailed,
    PublishFailed,
    DurableLogFailed,
    Cancelled,
}

pub enum AlbumOutcome {
    Complete {
        tracks: Vec<TrackRecord>,
        stages: Vec<StageRecord>,
    },
    Partial {
        successful: Vec<TrackRecord>,
        failed: Vec<TrackRecord>,
        stages: Vec<StageRecord>,
    },
    Blocked {
        successful: Vec<TrackRecord>,
        failed: Vec<TrackRecord>,
        stages: Vec<StageRecord>,
        reason: BlockReason,
    },
}

pub struct PipelineReport {
    pub request: RedactedPipelineRequest,
    pub source: Option<PreparedSource>,
    pub plan: Option<AlbumPlan>,
    pub artifacts: Option<ArtifactSet>,
    pub published: Option<PublishedAlbum>,
    pub outcome: AlbumOutcome,
    pub durable_log: Option<PathBuf>,
}
```

`AlbumOutcome::Blocked` carries all stage records, not only failed records. The durable log must be able to show which stages succeeded, skipped, or failed before the block.

PR 1 extends the existing queue terminal states:

```rust
pub enum ConversionStatus {
    // existing non-terminal variants remain

    Completed {
        output_path: PathBuf,
        log_path: Option<PathBuf>,
    },

    Partial {
        output_path: PathBuf,
        successful: u32,
        failed: u32,
        log_path: PathBuf,
    },

    Failed {
        error: String,
        log_path: Option<PathBuf>,
    },
}
```

Status mapping:

- `AlbumOutcome::Complete` plus successful publish plus durable log -> `Completed`
- `AlbumOutcome::Partial` plus successful publish plus durable log -> `Partial`
- `AlbumOutcome::Blocked` -> `Failed`
- Any required-stage error after publish -> `Failed` with `log_path` when available; the report must state that final artifacts may already exist
- If durable-log write fails after publish, emit `Failed { error: "durable log write failed: ..." }` and include `published` in the report

Queue semantics updated in PR 1:

- `Partial` is terminal for `is_finished`.
- `Partial` is counted separately from completed and failed items.
- Retry behavior for `Partial` is explicit: either `can_retry_partial` or `can_retry` includes it by design. The test must lock the chosen behavior.

### Event reporting contract

PR 1 defines the event channel. Tests can subscribe to this reporter and prove terminal ordering directly.

```rust
pub enum PipelineEvent {
    StageStarted {
        item_id: String,
        stage: PipelineStage,
    },
    StageFinished {
        item_id: String,
        record: StageRecord,
    },
    Progress {
        item_id: String,
        stage: PipelineStage,
        phase_progress: f32,
        message: Option<String>,
    },
    Terminal {
        item_id: String,
        status: ConversionStatus,
    },
}

#[async_trait]
pub trait PipelineReporter: Send + Sync {
    async fn emit(&self, event: PipelineEvent);
}
```

`run_pipeline_item` receives a reporter. A terminal event may appear only after `StageFinished(DurableLog, Ok)` for `Complete` and `Partial`, or after `StageFinished(DurableLog, Ok|Skipped|Failed)` for `Blocked` according to `LogPolicy.write_for_blocked` and the log-write result.

### Tool and staging contracts

```rust
pub struct StagingDir {
    pub root: PathBuf,
    pub job_id: String,
}
```

`StagingDir` is a job-scoped RAII cleanup guard. Drop deletes its tree unless publish consumes it and marks it published. Startup GC deletes stale job-scoped staging dirs from prior crashes.

```rust
pub enum ToolBinary {
    SevenZip,
    Ffmpeg,
    Ffprobe,
    Sox,
    Loudgain,
    Metaflac,
    Opustags,
    Wvunpack,
    Wvtag,
    AtomicParsley,
}

pub struct EnvVar {
    pub key: String,
    pub value: SecretString,
    pub secret: bool,
}

pub struct ToolCommand {
    pub binary: ToolBinary,
    pub args: Vec<String>,
    pub secret_args: Vec<usize>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<EnvVar>,
    pub timeout: Duration,
}

pub enum ProcessExit {
    Code(i32),
    Signal(i32),
    Unknown,
}

pub struct ToolOutput {
    pub exit: ProcessExit,
    pub stdout_tail: String,   // bounded, 64 KiB
    pub stderr_tail: String,   // bounded, 64 KiB
    pub elapsed: Duration,
    pub command: CommandRecord,
}
```

Failed tool calls must still carry sanitized command diagnostics. The error type below carries `CommandRecord` where a command existed.

### Traits and public stage signatures

```rust
#[async_trait]
pub trait ToolRunner: Send + Sync {
    async fn run(
        &self,
        cmd: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError>;
}

#[async_trait]
pub trait Materializer: Send + Sync {
    async fn materialize(
        &self,
        req: &PipelineRequest,
        staging: &StagingDir,
        runner: &dyn ToolRunner,
        cancel: &CancellationToken,
    ) -> Result<PreparedSource, MaterializeError>;
}

pub fn validate_request(req: &PipelineRequest)
    -> Result<(), RequestValidationError>;

pub fn detect_source_kind(req: &PipelineRequest)
    -> Result<SourceKind, SourceDetectError>;

pub fn materializer_for(kind: SourceKind)
    -> Result<Box<dyn Materializer>, SourceDispatchError>;

pub async fn realize_track(
    src: &TrackSourceRef,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<PathBuf, ConvertError>;

pub fn plan_outputs(
    source: &PreparedSource,
    req: &PipelineRequest,
) -> Result<AlbumPlan, PlanError>;

pub struct ConvertStageResult {
    pub tracks: Vec<TrackRecord>,
    pub artifacts: ArtifactSet,
    pub record: StageRecord,
}

pub async fn convert_tracks(
    source: &PreparedSource,
    plan: &AlbumPlan,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> ConvertStageResult;

pub fn aggregate_album_outcome(
    tracks: Vec<TrackRecord>,
    stages: Vec<StageRecord>,
    policy: FailurePolicy,
) -> AlbumOutcome;

pub async fn merge_tracks(
    artifacts: ArtifactSet,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(ArtifactSet, StageRecord), MergeError>;

pub async fn apply_metadata(
    artifacts: &ArtifactSet,
    source: &PreparedSource,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<StageRecord, MetadataError>;

pub async fn apply_replaygain(
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<StageRecord, ReplayGainError>;

pub async fn run_features(
    artifacts: ArtifactSet,
    outcome: &AlbumOutcome,
    source: &PreparedSource,
    req: &PipelineRequest,
    staging: &StagingDir,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(ArtifactSet, StageRecord), FeatureError>;

pub fn build_publish_plan(
    artifacts: &ArtifactSet,
    req: &PipelineRequest,
) -> Result<PublishPlan, PublishError>;

pub fn publish_album_output(
    staging: StagingDir,
    plan: &PublishPlan,
    policy: PublishPolicy,
) -> Result<PublishedAlbum, PublishError>;

pub fn write_durable_log(
    report: &PipelineReport,
    log: &LogPolicy,
) -> Result<PathBuf, LogError>;

pub async fn run_pipeline_item(
    req: PipelineRequest,
    runner: &dyn ToolRunner,
    reporter: &dyn PipelineReporter,
    cancel: &CancellationToken,
) -> PipelineReport;
```

Transitional behavior:

- `materializer_for` may return `Unsupported(SourceKind)` until that materializer PR lands.
- `realize_track` may return `UnsupportedTrackSource` for `ImageSegment` and `SacdTrack` until PRs 8 and 9 implement those arms.
- PR 1 stubs for stage functions return `Skipped` or blocked reports; they do not panic.

### Error types

All new errors derive `thiserror::Error`.

- `RequestValidationError` - `MissingContainer`, `InvalidOutputRoot`, `InvalidTemplate`, `InvalidSecretState`, `InvalidStagePolicy`
- `SourceDetectError` - `UnknownSource`, `AmbiguousCue`, `Io`
- `SourceDispatchError` - `Unsupported(SourceKind)`
- `MaterializeError` - `Extraction`, `Parse`, `Encrypted`, `InvalidTrackSelection`, `Cancelled`, `Tool(ToolRunnerError)`, `Io`
- `PlanError` - `NamingCollision`, `InvalidTemplate`, `EmptyManifest`, `InvalidTrackSelection`, `PathOutsideOutputRoot`
- `ToolRunnerError` - `Spawn { command: CommandRecord }`, `Timeout { elapsed: Duration, command: CommandRecord }`, `Cancelled { command: CommandRecord }`, `NonZeroExit { exit: ProcessExit, stderr_tail: String, command: CommandRecord }`, `Io`
- `ConvertError` - `UnsupportedTrackSource`, `Realize(String)`, `TrackValidation(String)`, `Backend(String)`, `Tool(ToolRunnerError)`, `Io`
- `MergeError` - `DurationMismatch`, `UnsupportedFormat`, `Io`, `Tool(ToolRunnerError)`
- `MetadataError` - `UnsupportedTagFormat`, `Io`, `Tool(ToolRunnerError)`
- `ReplayGainError` - `UnsupportedFormat`, `Io`, `Tool(ToolRunnerError)`
- `FeatureError` - `CueGeneration`, `ConversionLogGeneration`, `Io`
- `PublishError` - `StagingMissing`, `DestinationExists`, `PathOutsideOutputRoot`, `CrossDeviceCopy`, `AtomicRename`, `BackupFailed`, `RollbackFailed`, `Io`
- `LogError` - `Io`, `Serialization`
- `PipelineError` - wrapper for orchestration callers that need a single error type

All new code lives under `#![forbid(unsafe_code)]`.

### Stub implementations

PR 1 ships:

- `StubToolRunner`: transcript-backed, no process spawn, configurable outputs/errors, redacts `SecretString`, `secret_args`, and secret env values.
- `RecordingReporter`: stores emitted `PipelineEvent`s for ordering tests.
- Compiling no-panic stubs for the public free functions listed above.
- No materializer implementation, no real process runner, no conversion body, no publish body.

### PR 1 exit condition

- Workspace compiles with `convert::pipeline`.
- Every public type, trait, function signature, and error listed above exists and is `pub` where needed.
- Public free functions have compiling no-panic stubs.
- `PipelineRequest`, `RedactedPipelineRequest`, `PreparedSource`, `AlbumPlan`, `ArtifactSet`, `AlbumOutcome`, `PipelineReport`, and `TrackRecord` roundtrip through JSON where intended.
- Queue persistence behavior for `SecretString` is tested; durable-log/report serialization is redacted.
- `SecretString` redacts in `Debug`, `Display`, stub transcripts, command records, reporter messages, and durable logs.
- Outcome aggregation tests:
  - all tracks success -> `Complete`
  - one track failure + default policy -> `Blocked`
  - one track failure + partial policy -> `Partial`
  - required-stage failure -> `Blocked`
  - optional-stage failure -> non-blocking failed `StageRecord`
  - disabled stage -> `Skipped`
- Queue status mapping tests:
  - `Complete` -> `Completed`
  - `Partial` -> `Partial`
  - `Blocked` -> `Failed`
  - `Partial` serializes/deserializes through queue JSON
  - `Partial` is terminal for queue accounting
- Reporter ordering unit test: no `Terminal` event can appear before the durable-log stage has finished according to the rules above.
- Stub runner transcript test: command args and secret env values redact.
- `cargo test -p tonepoet` passes.
- A repository search shows no `unsafe` in new pipeline modules.

## PR 2 - Real `ToolRunner`

### Ships

- Async child process runner using tokio.
- Per-command timeout.
- Cancellation token aborts the child and reaps it.
- Bounded stdout/stderr tail capture at 64 KiB.
- `cwd` and env application.
- Redacted command logging.
- `ToolBinary` path resolution from `ProcessorConfig`.
- `ProcessExit` mapping for normal exit, signal death, and unknown termination.
- Sanitized `CommandRecord` carried on both success and tool errors.

### Exit condition

- Timeout test: command exceeding timeout returns `ToolRunnerError::Timeout` with command record and leaves no child.
- Cancellation test: mid-run cancellation returns `Cancelled` with command record and leaves no child.
- Signal test: killed child maps to `ProcessExit::Signal(n)`.
- Redaction test: archive password and secret env value never appear in logs, transcripts, command records, tool errors, or durable reports.
- Non-zero-exit test: failing command returns `NonZeroExit` with non-empty stderr tail and command record.
- Bounded-capture test: output over 64 KiB stores exactly the 64 KiB tail.
- Path-resolution test: every `ToolBinary` maps through `ProcessorConfig`, including SoX.

## PR 3 - 7z materializer

### Ships

`SevenZipMaterializer: Materializer`.

It ports only the archive materialization behavior:

- multithreaded 7z extraction
- scratch-directory support
- archive password via `SecretString`
- ffprobe sample-rate/sample-count probing through `ToolRunner`
- source metadata capture
- `TrackSelection` filtering

It returns `PreparedSource` with `TrackSourceRef::StagedFile` entries. It does not convert, tag, merge, run ReplayGain, generate feature files, publish, write durable logs, or emit terminal events.

### Exit condition

- Corpus includes at least: plain 7z, password-protected 7z, malformed archive, empty archive, mixed audio/non-audio archive, multi-disc archive.
- Each valid archive yields expected `PreparedSource`: `TrackId`, track order, `StagedFile` paths, sample rates, expected samples when available, metadata, and provenance.
- `TrackSelection::Range` and `TrackSelection::Set` produce deterministic sorted output and reject invalid selections with `MaterializeError::InvalidTrackSelection`.
- Password test proves command records redact the password in both success and failure cases.
- Cancellation mid-extract returns `MaterializeError::Cancelled` and deletes the staging subtree for that job.
- Malformed and permission-denied cases return structured errors and do not panic.

## PR 4 - Orchestrator, output planning, per-track conversion, publish

### Ships

`run_pipeline_item()` in final shape, still test-only. `process_item` does not route real user conversions into the new pipeline yet.

PR 4 implements:

- `validate_request` body for the new request fields.
- 7z source detection rule.
- `materializer_for(SourceKind::SevenZip)`.
- `plan_outputs`.
- `realize_track` dispatch with `StagedFile` identity arm; other arms return `UnsupportedTrackSource`.
- `convert_tracks`.
- `aggregate_album_outcome`.
- `build_publish_plan`.
- `publish_album_output`.
- startup stale-staging-dir deletion.
- event reporting through `PipelineReporter`.
- terminal queue-status mapping, including `Partial`.

`merge_tracks`, `apply_metadata`, `apply_replaygain`, `run_features`, and `write_durable_log` use test-only skipped/minimal bodies until PRs 5 and 6 fill them. `run_pipeline_item()` still calls them in the final order. PRs 5 and 6 change those function bodies, not the orchestrator signature or call order.

PR 4 durable-log stub rule: the temporary `write_durable_log` body must still produce a successful minimal durable log for `Complete` and `Partial` outcomes and emit `StageRecord { stage: DurableLog, outcome: Ok }`. It must not treat durable logging as skipped for successful terminal states. For `Blocked`, it follows `LogPolicy.write_for_blocked` exactly.

`convert_tracks` contract:

- It creates `TrackRecord`s for every manifest track.
- `ArtifactSet` contains only artifacts for successful tracks.
- Every failed `TrackRecord` has non-empty error text.
- Track IDs in records and artifacts must align with `AlbumPlan`.

### Exit condition

- 7z archive through `run_pipeline_item()` with merge off produces decoded PCM byte-identical to legacy `extract_and_convert_7z` for the same input and settings.
- `plan_outputs` rejects:
  - duplicate final paths
  - paths outside `output_root`
  - invalid template expansion
  - case-fold collisions on case-insensitive filesystems when testable
- One track fails mid-convert:
  - default policy -> `Blocked`, no publish, final dir untouched
  - partial policy -> `Partial`, publish proceeds for successful tracks, queue status `Partial`
- Fault injected before publish leaves no final output.
- Fault injected during publish leaves no incomplete final album dir and restores any backup.
- Next run deletes the prior orphaned staging dir.
- Event test proves no `Terminal` event fires before publish and the durable-log stage completion required by this PR's stub behavior.
- Complete and Partial PR-4 runs write a minimal durable log and emit `StageFinished(DurableLog, Ok)` before the terminal event; Blocked runs follow `LogPolicy.write_for_blocked`.
- Every failed `TrackRecord` has non-empty error text.
- Every tool failure attached to a track carries a sanitized `CommandRecord`.
- `process_item` still uses legacy routing for all real user conversions.

## PR 5 - Album merge stage

### Ships

`merge_tracks` body.

Rules:

- Optional; controlled by `PipelineRequest.merge`.
- Input: `ArtifactSet` with per-track audio artifacts.
- Output: `ArtifactSet` with one `MergedArtifact`, plus existing sidecars if any.
- Runs after `convert`, before `metadata`.
- Validates merged duration/sample count against the sum of source tracks within a documented tolerance.
- Deletes failed merge scratch files from staging.

`run_pipeline_item()` does not change.

### Exit condition

- Multi-track album with merge on yields one merged artifact.
- Decoded PCM equals concatenated decoded PCM of the track artifacts.
- Deliberately truncated merge returns `MergeError::DurationMismatch`.
- Merge failure maps to `Blocked` with all stage records present and `BlockReason::RequiredStageFailure(PipelineStage::Merge)` when merge is required by request semantics.
- Merge off matches PR 4 behavior.
- Legacy 7z merge output and new merge output are decoded PCM byte-identical for the same archive and settings.

## PR 6 - Metadata, ReplayGain, features, durable log

### Ships

Bodies for:

- `apply_metadata`
- `apply_replaygain`
- `run_features`
- `write_durable_log`

Rules:

- Metadata follows `PipelineRequest.stages.metadata`.
- ReplayGain follows `PipelineRequest.stages.replaygain`.
- Features follow `PipelineRequest.stages.features`.
- Metadata applies to staged artifacts after merge decision:
  - per-track tags for N track files
  - album-level tags for a merged file
- ReplayGain applies after metadata:
  - merged: single-file gain
  - unmerged: album-mode scan across all track files, applied uniformly
- Features generate staged sidecars:
  - user-facing conversion log sidecar
  - CUE sheet sidecar where applicable
- `run_features` returns an updated `ArtifactSet` so publish includes sidecars.
- Durable log is not the user-facing conversion-log sidecar. Durable log writes to `LogPolicy.root` for all `Complete` and `Partial` outcomes, and for `Blocked` when `write_for_blocked` is true.
- A required-stage failure blocks. An optional-stage failure records a failed `StageRecord` but does not block. A disabled stage records `Skipped` and does not block.

`run_pipeline_item()` does not change.

### Exit condition

- Tags match expected values per output format.
- Metadata failure with `StageRequirement::Required` yields `Blocked`, no publish.
- Metadata failure with `StageRequirement::Optional` logs a failed stage and continues.
- Metadata with `StageRequirement::Disabled` yields `Skipped`.
- Album ReplayGain, merge off: all tracks carry identical album-gain/peak tags; per-track gain can differ.
- ReplayGain, merge on: merged file carries one coherent gain/peak.
- Feature generation returns sidecar artifacts; publish includes them.
- Durable log writes for `Complete`, `Partial`, and configured `Blocked` outcomes.
- Durable log includes:
  - redacted request
  - source manifest when materialization succeeded
  - plan when planning succeeded
  - track records
  - all stage records
  - command records, including failed tool calls
  - artifact paths
  - published entries when publish succeeded
  - non-empty error text for failed tracks and failed stages
- Durable log never contains archive passwords or secret env values.
- Durable log failure blocks terminal success and emits `Failed`.
- 7z through PRs 3-6 matches legacy output for the same input: decoded PCM byte-identical, tags equal, ReplayGain values equal within the legacy tolerance, feature sidecars equivalent.

## PR 7 - 7z parity gate and retire `extract_and_convert_7z`

### Ships

In this order:

1. Record legacy baseline by running `extract_and_convert_7z` over the regression corpus.
2. Run `run_pipeline_item()` over the same corpus.
3. Assert parity.
4. Modify `process_item` so 7z dispatches into `run_pipeline_item()`.
5. Delete `extract_and_convert_7z`.

CUE and SACD still do not route through the new path until PRs 8 and 9.

### Exit condition

- Regression corpus includes PR 3 archives plus a wider real-archive set.
- For every archive, new output vs legacy baseline:
  - decoded PCM byte-identical
  - tags equal
  - ReplayGain equal within existing tolerance
  - user-facing conversion-log sidecar equivalent
  - durable log contains at least the legacy diagnostics plus the new structured fields
  - durable log contains no secrets
- `grep -rn 'extract_and_convert_7z' src/` returns zero hits.
- 7z user-facing conversions now route through `run_pipeline_item()`.
- `cargo test -p tonepoet` passes.

## PR 8 - CUE materializer and `ImageSegment` realization

### Ships

`CueImageMaterializer: Materializer`.

Materializer behavior:

- Honors `CueSidecarPolicy`.
- Parses CUE sheet.
- Detects single-image CUE layouts.
- Probes image sample rate and sample count.
- Returns `PreparedSource` with `TrackSourceRef::ImageSegment`.
- Captures CUE metadata: title, performer, ISRC, `FLAGS PRE`, `REM DATE`, `REM GENRE`, and unmapped fields in `extra`.
- Does not cut or decode audio.

Realization behavior:

- Implements the `ImageSegment` arm of `realize_track`.
- Uses ffmpeg segment cutting through `ToolRunner`.
- Uses wvunpack fallback for WavPack v4 when needed.
- Validates output file exists, has nonzero size, and matches `expected_samples` within tolerance.
- Deletes failed realization temp files.

Routing:

- Adds CUE-pair detection to `detect_source_kind`.
- Wires `SourceKind::CueImage` in `materializer_for`.
- Modifies `process_item` to route single-image CUE pairs into `run_pipeline_item()`.
- `CueSidecarPolicy::IgnoreCue` leaves the item on the single-file legacy path until PR 10 maps final CLI/TUI semantics.
- This is net-new behavior: legacy conversion ignored the CUE and emitted one blob.

### Exit condition

- Corpus includes at least:
  - standard FLAC+CUE
  - WavPack-v4+CUE
  - APE+CUE
  - CUE with `FLAGS PRE`
  - embedded CUESHEET plus sidecar conflict
  - malformed CUE
  - Unicode path/name case
- Each valid case yields expected manifest: `TrackId`, `ImageSegment`, metadata, sample rate, expected samples.
- Sidecar precedence tests:
  - `PreferSidecar` picks sidecar
  - `SidecarOnly` fails without sidecar
  - `EmbeddedOnly` ignores sidecar
  - `IgnoreCue` stays on single-file legacy path
- Forced seek/sample drift returns `ConvertError::TrackValidation`; the track record fails with non-empty error text.
- Malformed CUE returns `MaterializeError::Parse` and leaves no temp output.
- End-to-end CUE album through the full pipeline yields expected split, encoded, tagged, ReplayGain-processed, published outputs plus logs.

## PR 9 - SACD materializer and `SacdTrack` realization

### Ships

`SacdIsoMaterializer: Materializer`.

Materializer behavior:

- Parses SACD TOC using `sacd-rs`.
- Applies `SacdArea` selection.
- Maps TOC text into `TrackMetadata` and `AlbumMetadata`.
- Returns `PreparedSource` with `TrackSourceRef::SacdTrack`.
- Does not decode audio.

Realization behavior:

- Implements the `SacdTrack` arm of `realize_track`.
- Uses in-process `sacd-rs` extraction to a staged DSF/DFF.
- Validates staged output.
- Maps encrypted ISO to `MaterializeError::Encrypted` or `ConvertError::Realize`, depending on detection point; never panics.

Routing:

- Adds SACD-ISO detection to `detect_source_kind`.
- Wires `SourceKind::SacdIso` in `materializer_for`.
- Modifies `process_item` to route SACD ISO into `run_pipeline_item()`.
- This is net-new behavior.

### Exit condition

- Solo Monk ISO yields a 13-track manifest with correct titles, ISRCs, durations, sample rate, and `SacdTrack` refs.
- Al Jarreau ISO yields 11-track manifests for both stereo and multichannel areas.
- `realize_track` for SACD produces staged DSF/DFF byte-identical to the prior `sacd-rs` gauntlet baseline.
- SACD end-to-end through the full pipeline yields expected converted outputs.
- Encrypted ISO returns a structured error and leaves no temp output.
- Wrong or unsupported area selection returns structured error.
- `cargo test` passes for the workspace.

## PR 10 - CLI, TUI, docs

### Ships

CLI and TUI now construct `PipelineRequest` directly.

CLI:

- `--track N`
- `--track-range a-b`
- `--area stereo|multichannel`
- `--no-cue`
- partial-output opt-in flag
- output root and naming template flags mapped to `NamingPolicy`
- overwrite/backup flags mapped to `PublishPolicy`
- stage requirement flags mapped to `StagePolicy` where exposed

TUI:

- source-pane per-track listing
- SACD area indicator
- track selection
- queue expansion for multi-track sources
- `Partial` terminal status rendering
- durable log link/path display

Docs:

- README update
- CLAUDE.md update
- migration notes describing legacy vs pipeline routes
- diagnostic-log schema example

`determine_output_path` remains for the single-file fast path. `plan_outputs` owns multi-track output planning.

### Exit condition

- CLI tests assert the resulting `PipelineRequest` for:
  - SACD ISO full album
  - `--track`
  - `--track-range`
  - `--area`
  - `--no-cue`
  - partial opt-in
  - overwrite policy
  - naming template
  - stage policy where exposed
- CLI integration test: `tonepoet convert disc.iso --format opus -o out/` produces N named, tagged files.
- TUI test or documented manual check:
  - SACD ISO shows track list and area
  - CUE+FLAC shows track list
  - selected tracks queue correctly
  - `Partial` renders as partial, not completed
  - durable log path is visible
- Docs describe:
  - stage order
  - failure policy
  - partial semantics
  - crash-resume model
  - log location
  - secret redaction behavior
- `cargo test` passes workspace-wide.

## Global invariants

1. PR 1 ships every public contract. Later PRs add implementation structs and bodies only.
2. PR 1 free-function stubs compile and do not panic; later PRs replace bodies without signature changes.
3. `PipelineRequest` is the persisted job input. `PreparedSource` is re-derivable diagnostic data.
4. Durable logs use `RedactedPipelineRequest`; raw secrets never enter reports or logs.
5. `TrackId` is the stable key. Track number alone never keys output planning.
6. Materializers describe tracks. `realize_track` creates decodable per-track audio.
7. Output planning is separate from materialization.
8. `ArtifactSet` carries user-facing audio and sidecar artifacts. It does not carry the durable log.
9. Publish receives an explicit staged-to-final `PublishPlan`.
10. Durable logs are separate from user-facing conversion-log sidecars.
11. `PipelineReporter` owns progress and terminal events. Terminal events happen last.
12. `Partial` is a terminal queue status, not a successful-completion alias.
13. Track failures and required-stage failures both block by default.
14. Optional stage failures are logged but non-blocking. Disabled stages are skipped.
15. Every tool failure that reached process construction carries a sanitized `CommandRecord`.
16. The new pipeline stays test-only until 7z parity passes in PR 7.
17. PR 7 is the only destructive legacy-code step.
18. CUE and SACD land only after the 7z path proves the pipeline.
19. No PR changes `run_pipeline_item()` after PR 4; later PRs fill stage bodies or source-specific match arms.
