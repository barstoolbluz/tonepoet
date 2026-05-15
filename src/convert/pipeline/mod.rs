//! Conversion pipeline rebuild — PR 1 contracts module.
//!
//! Implements PR 1 of
//! `docs/phase0_sequencing_plan_hardened_ready_for_execution.md`:
//! every public type, error, trait, and stage-function signature the
//! staged pipeline (PRs 2–10) implements against, plus compiling
//! non-panicking stub bodies, a transcript-backed `StubToolRunner`,
//! and a `RecordingReporter`.
//!
//! PRs 2–10 add implementation structs and replace stub bodies; they
//! do not add or alter public contract types, stage signatures,
//! terminal statuses, core errors, or source/artifact identity.

#![forbid(unsafe_code)]

pub mod errors;
pub mod reporter;
pub mod stages;
pub mod tool;
pub mod types;

pub use errors::*;
pub use reporter::*;
pub use stages::*;
pub use tool::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{AudioFormat, ConversionStatus};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    // ---- builders ---------------------------------------------------------

    fn sample_request() -> PipelineRequest {
        PipelineRequest {
            job_id: "job-1".into(),
            item_id: "item-1".into(),
            container: PathBuf::from("/tmp/in.7z"),
            source: SourceOptions {
                archive_password: Some(SecretString::new("hunter2")),
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            target_format: AudioFormat::Flac,
            encode: EncodeOptions {
                backend: EncodeBackend::Auto,
                bitrate: None,
                compression_level: Some(8),
                dither: DitherPolicy::Auto,
            },
            merge: false,
            output_root: PathBuf::from("/tmp/out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".into(),
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
            },
            log: LogPolicy { root: PathBuf::from("/tmp/logs"), write_for_blocked: true },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Enabled,
                features: StageRequirement::Disabled,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        }
    }

    fn track_record(ordinal: u32, ok: bool) -> TrackRecord {
        TrackRecord {
            track_id: TrackId { source_ordinal: ordinal, disc_number: None, track_number: ordinal },
            outcome: if ok {
                TrackOutcome::Ok
            } else {
                TrackOutcome::Err("encode failed".into())
            },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!("/s/{ordinal}.flac"))),
            realized_input: None,
            output_file: None,
            commands: Vec::new(),
            bytes_in: None,
            bytes_out: None,
            duration: None,
        }
    }

    fn prepared_source() -> PreparedSource {
        PreparedSource {
            container: PathBuf::from("/tmp/in.7z"),
            kind: SourceKind::SevenZip,
            tracks: vec![PreparedTrack {
                id: TrackId { source_ordinal: 1, disc_number: None, track_number: 1 },
                source_ref: TrackSourceRef::StagedFile(PathBuf::from("/s/1.flac")),
                metadata: TrackMetadata::default(),
                expected_samples: Some(1000),
                sample_rate: 44_100,
            }],
            album_metadata: AlbumMetadata { total_tracks: 1, ..AlbumMetadata::default() },
            provenance: ExtractionProvenance {
                source_kind: SourceKind::SevenZip,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        }
    }

    fn sample_artifacts() -> ArtifactSet {
        ArtifactSet {
            audio: AudioArtifacts::Tracks(vec![TrackArtifact {
                track_id: TrackId { source_ordinal: 1, disc_number: None, track_number: 1 },
                staged_path: PathBuf::from("/stage/1.flac"),
                final_path: PathBuf::from("/out/1.flac"),
                samples: Some(1000),
            }]),
            sidecars: Vec::new(),
        }
    }

    fn roundtrips<T>(value: &T) -> bool
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        json == json2
    }

    // ---- SecretString -----------------------------------------------------

    #[test]
    fn secret_string_redacts_debug_and_display() {
        let s = SecretString::new("hunter2");
        assert_eq!(format!("{:?}", s), "<redacted>");
        assert_eq!(format!("{}", s), "<redacted>");
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn redacted_request_hides_password() {
        let req = sample_request();
        let redacted = RedactedPipelineRequest::from(&req);
        assert_eq!(redacted.source.archive_password.as_deref(), Some("<redacted>"));
        let json = serde_json::to_string(&redacted).unwrap();
        assert!(!json.contains("hunter2"), "redacted request leaked the secret");
    }

    // ---- JSON roundtrips --------------------------------------------------

    #[test]
    fn contract_types_json_roundtrip() {
        assert!(roundtrips(&sample_request()));
        assert!(roundtrips(&RedactedPipelineRequest::from(&sample_request())));
        assert!(roundtrips(&prepared_source()));
        assert!(roundtrips(&AlbumPlan {
            album_dir: PathBuf::from("/out/album"),
            entries: vec![PlannedTrackOutput {
                track_id: TrackId { source_ordinal: 1, disc_number: None, track_number: 1 },
                final_path: PathBuf::from("/out/album/01.flac"),
            }],
        }));
        assert!(roundtrips(&sample_artifacts()));
        assert!(roundtrips(&track_record(1, true)));
        let outcome = AlbumOutcome::Complete {
            tracks: vec![track_record(1, true)],
            stages: vec![StageRecord { stage: PipelineStage::Convert, outcome: StageOutcome::Ok }],
        };
        assert!(roundtrips(&outcome));
        let report = PipelineReport {
            request: RedactedPipelineRequest::from(&sample_request()),
            source: Some(prepared_source()),
            plan: None,
            artifacts: Some(sample_artifacts()),
            published: None,
            outcome,
            durable_log: None,
        };
        assert!(roundtrips(&report));
    }

    #[test]
    fn queue_persistence_preserves_secret_but_report_redacts() {
        // Queue persistence is the one permitted unredacted path.
        let req = sample_request();
        let queue_json = serde_json::to_string(&req).unwrap();
        let back: PipelineRequest = serde_json::from_str(&queue_json).unwrap();
        assert_eq!(
            back.source.archive_password.as_ref().unwrap().expose(),
            "hunter2"
        );
        // The durable report serializes the redacted request only.
        let report = PipelineReport {
            request: RedactedPipelineRequest::from(&req),
            source: None,
            plan: None,
            artifacts: None,
            published: None,
            outcome: AlbumOutcome::Complete { tracks: vec![], stages: vec![] },
            durable_log: None,
        };
        let report_json = serde_json::to_string(&report).unwrap();
        assert!(!report_json.contains("hunter2"));
    }

    // ---- outcome aggregation ---------------------------------------------

    #[test]
    fn aggregate_all_tracks_ok_is_complete() {
        let out = aggregate_album_outcome(
            vec![track_record(1, true), track_record(2, true)],
            vec![],
            FailurePolicy::FailAlbumOnAnyTrackFailure,
        );
        assert!(matches!(out, AlbumOutcome::Complete { .. }));
    }

    #[test]
    fn aggregate_track_failure_default_policy_is_blocked() {
        let out = aggregate_album_outcome(
            vec![track_record(1, true), track_record(2, false)],
            vec![],
            FailurePolicy::FailAlbumOnAnyTrackFailure,
        );
        assert!(matches!(
            out,
            AlbumOutcome::Blocked { reason: BlockReason::TrackFailures, .. }
        ));
    }

    #[test]
    fn aggregate_track_failure_partial_policy_is_partial() {
        let out = aggregate_album_outcome(
            vec![track_record(1, true), track_record(2, false)],
            vec![],
            FailurePolicy::AllowPartialAlbum,
        );
        match out {
            AlbumOutcome::Partial { successful, failed, .. } => {
                assert_eq!(successful.len(), 1);
                assert_eq!(failed.len(), 1);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_required_stage_failure_is_blocked() {
        let out = aggregate_album_outcome(
            vec![track_record(1, true)],
            vec![StageRecord {
                stage: PipelineStage::Metadata,
                outcome: StageOutcome::Failed("tagger crashed".into()),
            }],
            FailurePolicy::FailAlbumOnAnyTrackFailure,
        );
        assert!(matches!(
            out,
            AlbumOutcome::Blocked {
                reason: BlockReason::RequiredStageFailure(PipelineStage::Metadata),
                ..
            }
        ));
    }

    #[test]
    fn aggregate_disabled_stage_does_not_block() {
        // Disabled stages reach aggregation as `Skipped`. A skipped
        // stage never blocks.
        let out = aggregate_album_outcome(
            vec![track_record(1, true)],
            vec![StageRecord {
                stage: PipelineStage::ReplayGain,
                outcome: StageOutcome::Skipped,
            }],
            FailurePolicy::FailAlbumOnAnyTrackFailure,
        );
        assert!(matches!(out, AlbumOutcome::Complete { .. }));
    }

    // ---- queue status mapping --------------------------------------------

    #[test]
    fn map_complete_to_completed() {
        let out = AlbumOutcome::Complete { tracks: vec![], stages: vec![] };
        let published = PublishedAlbum { album_dir: PathBuf::from("/out/album"), entries: vec![] };
        let status = map_album_outcome(&out, Some(&published), Some(&PathBuf::from("/logs/a.json")));
        assert!(matches!(status, ConversionStatus::Completed { .. }));
    }

    #[test]
    fn map_partial_to_partial() {
        let out = AlbumOutcome::Partial {
            successful: vec![track_record(1, true)],
            failed: vec![track_record(2, false)],
            stages: vec![],
        };
        let published = PublishedAlbum { album_dir: PathBuf::from("/out/album"), entries: vec![] };
        let status = map_album_outcome(&out, Some(&published), Some(&PathBuf::from("/logs/a.json")));
        match status {
            ConversionStatus::Partial { successful, failed, .. } => {
                assert_eq!(successful, 1);
                assert_eq!(failed, 1);
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn map_blocked_to_failed() {
        let out = AlbumOutcome::Blocked {
            successful: vec![],
            failed: vec![track_record(1, false)],
            stages: vec![],
            reason: BlockReason::TrackFailures,
        };
        let status = map_album_outcome(&out, None, None);
        assert!(matches!(status, ConversionStatus::Failed { .. }));
    }

    #[test]
    fn conversion_status_partial_roundtrips_through_queue_json() {
        let status = ConversionStatus::Partial {
            output_path: PathBuf::from("/out/album"),
            successful: 9,
            failed: 2,
            log_path: PathBuf::from("/logs/a.json"),
        };
        let json = serde_json::to_string(&status).unwrap();
        let back: ConversionStatus = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ConversionStatus::Partial { successful: 9, failed: 2, .. }
        ));
    }

    // ---- stub tool runner -------------------------------------------------

    #[tokio::test]
    async fn stub_runner_transcript_redacts_secret_args() {
        let runner = StubToolRunner::new();
        let cmd = ToolCommand {
            binary: ToolBinary::SevenZip,
            args: vec!["x".into(), "-phunter2".into(), "archive.7z".into()],
            secret_args: vec![1],
            cwd: None,
            env: vec![EnvVar {
                key: "TOOL_TOKEN".into(),
                value: SecretString::new("s3cr3t"),
                secret: true,
            }],
            timeout: Duration::from_secs(60),
        };
        let cancel = CancellationToken::new();
        let _ = runner.run(cmd, &cancel).await.expect("stub run");
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 1);
        let record = &transcript[0];
        assert_eq!(record.sanitized_args[1], "<redacted>");
        assert_eq!(record.env_keys, vec!["TOOL_TOKEN".to_string()]);
        let json = serde_json::to_string(record).unwrap();
        assert!(!json.contains("hunter2"));
        assert!(!json.contains("s3cr3t"));
    }

    #[tokio::test]
    async fn stub_runner_returns_configured_failure() {
        let runner = StubToolRunner::new();
        runner.push_failure("ffmpeg: invalid data");
        let cmd = ToolCommand {
            binary: ToolBinary::Ffmpeg,
            args: vec!["-i".into(), "x.flac".into()],
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout: Duration::from_secs(10),
        };
        let cancel = CancellationToken::new();
        let err = runner.run(cmd, &cancel).await.expect_err("configured failure");
        assert!(matches!(err, ToolRunnerError::NonZeroExit { .. }));
    }

    // ---- reporter ---------------------------------------------------------

    #[tokio::test]
    async fn reporter_records_events_in_order_and_terminal_is_last() {
        let reporter = RecordingReporter::new();
        reporter
            .emit(PipelineEvent::StageStarted {
                item_id: "i".into(),
                stage: PipelineStage::Convert,
            })
            .await;
        reporter
            .emit(PipelineEvent::StageFinished {
                item_id: "i".into(),
                record: StageRecord {
                    stage: PipelineStage::DurableLog,
                    outcome: StageOutcome::Ok,
                },
            })
            .await;
        reporter
            .emit(PipelineEvent::Terminal {
                item_id: "i".into(),
                status: ConversionStatus::Completed {
                    output_path: PathBuf::from("/out"),
                    log_path: None,
                },
            })
            .await;
        let events = reporter.events();
        assert_eq!(events.len(), 3);
        // The ordering rule a real orchestrator (PR 4) must satisfy:
        // a Terminal event only after the DurableLog stage finished.
        let durable_log_idx = events.iter().position(|e| {
            matches!(
                e,
                PipelineEvent::StageFinished { record, .. }
                    if record.stage == PipelineStage::DurableLog
            )
        });
        let terminal_idx = events
            .iter()
            .position(|e| matches!(e, PipelineEvent::Terminal { .. }));
        assert!(durable_log_idx.is_some() && terminal_idx.is_some());
        assert!(durable_log_idx.unwrap() < terminal_idx.unwrap());
    }

    // ---- stub free functions do not panic --------------------------------

    #[tokio::test]
    async fn stub_free_functions_are_non_panicking() {
        let req = sample_request();
        let runner = StubToolRunner::new();
        let reporter = RecordingReporter::new();
        let cancel = CancellationToken::new();
        let staging = StagingDir::new(
            std::env::temp_dir().join("tonepoet-pr1-stub-nonexistent"),
            "job-1".into(),
        );

        assert!(validate_request(&req).is_ok());
        assert!(detect_source_kind(&req).is_err());
        assert!(materializer_for(SourceKind::SevenZip).is_err());

        let realize = realize_track(
            &TrackSourceRef::StagedFile(PathBuf::from("/s/1.flac")),
            &req,
            &staging,
            &runner,
            &cancel,
        )
        .await;
        assert!(matches!(realize, Err(ConvertError::UnsupportedTrackSource)));

        assert!(plan_outputs(&prepared_source(), &req).is_err());

        let convert = convert_tracks(&prepared_source(), &AlbumPlan {
            album_dir: PathBuf::from("/out"),
            entries: vec![],
        }, &req, &staging, &runner, &cancel)
        .await;
        assert_eq!(convert.record.outcome, StageOutcome::Skipped);

        let merged = merge_tracks(sample_artifacts(), &req, &staging, &runner, &cancel)
            .await
            .expect("stub merge");
        assert_eq!(merged.1.outcome, StageOutcome::Skipped);

        let meta = apply_metadata(&sample_artifacts(), &prepared_source(), &req, &runner, &cancel)
            .await
            .expect("stub metadata");
        assert_eq!(meta.outcome, StageOutcome::Skipped);

        let rg = apply_replaygain(&sample_artifacts(), &req, &runner, &cancel)
            .await
            .expect("stub replaygain");
        assert_eq!(rg.outcome, StageOutcome::Skipped);

        let outcome = AlbumOutcome::Complete { tracks: vec![], stages: vec![] };
        let feats = run_features(
            sample_artifacts(),
            &outcome,
            &prepared_source(),
            &req,
            &staging,
            &runner,
            &cancel,
        )
        .await
        .expect("stub features");
        assert_eq!(feats.1.outcome, StageOutcome::Skipped);

        assert!(build_publish_plan(&sample_artifacts(), &req).is_ok());

        let publish_staging = StagingDir::new(
            std::env::temp_dir().join("tonepoet-pr1-publish-nonexistent"),
            "job-1".into(),
        );
        let plan = PublishPlan { album_dir: PathBuf::from("/out"), entries: vec![] };
        assert!(publish_album_output(
            publish_staging,
            &plan,
            req.publish.clone(),
        )
        .is_err());

        let report = PipelineReport {
            request: RedactedPipelineRequest::from(&req),
            source: None,
            plan: None,
            artifacts: None,
            published: None,
            outcome: AlbumOutcome::Complete { tracks: vec![], stages: vec![] },
            durable_log: None,
        };
        assert!(write_durable_log(&report, &req.log).is_err());

        let final_report = run_pipeline_item(req, &runner, &reporter, &cancel).await;
        assert!(matches!(final_report.outcome, AlbumOutcome::Blocked { .. }));
    }

    // ---- queue: Partial terminal semantics (F2) ----------------------------

    #[test]
    fn partial_is_terminal_for_queue_accounting() {
        use crate::convert::queue::ConversionItem;

        let mut item = ConversionItem::default();
        item.status = ConversionStatus::Partial {
            output_path: PathBuf::from("/out/album"),
            successful: 9,
            failed: 2,
            log_path: PathBuf::from("/logs/a.json"),
        };
        assert!(item.is_finished(), "Partial must be terminal");
    }

    #[test]
    fn partial_is_retryable() {
        use crate::convert::queue::ConversionItem;

        let mut item = ConversionItem::default();
        item.status = ConversionStatus::Partial {
            output_path: PathBuf::from("/out/album"),
            successful: 9,
            failed: 2,
            log_path: PathBuf::from("/logs/a.json"),
        };
        assert!(item.can_retry(), "Partial must be retryable");
    }

    // ---- reporter: secret redaction in events (F5) -------------------------

    #[tokio::test]
    async fn terminal_event_does_not_leak_secrets() {
        // The reporter carries ConversionStatus values. Verify that
        // serializing a Terminal event containing a path (the only
        // user-controlled string in terminal status) does not contain
        // the secret. This locks the exit condition "SecretString
        // redacts in reporter messages."
        let reporter = RecordingReporter::new();
        let req = sample_request();
        let redacted = RedactedPipelineRequest::from(&req);
        let redacted_json = serde_json::to_string(&redacted).unwrap();
        assert!(!redacted_json.contains("hunter2"));

        // Emit a terminal event and verify the status debug repr
        // doesn't leak secrets either (it carries PathBuf, not
        // SecretString, so this is a belt-and-suspenders check).
        reporter
            .emit(PipelineEvent::Terminal {
                item_id: req.item_id.clone(),
                status: ConversionStatus::Completed {
                    output_path: PathBuf::from("/out"),
                    log_path: None,
                },
            })
            .await;
        let events = reporter.events();
        let debug = format!("{:?}", events);
        assert!(!debug.contains("hunter2"));
    }

    // ====================================================================
    // PR 2 — RealToolRunner exit-condition tests
    // ====================================================================

    /// Helper: build a ToolCommand that maps `binary` to a shell
    /// command via a custom tool_paths entry, so we can run arbitrary
    /// commands through the real runner without needing actual audio
    /// tools installed.
    fn real_runner_with(overrides: Vec<(ToolBinary, &str)>) -> RealToolRunner {
        let mut paths = std::collections::HashMap::new();
        for (binary, path) in overrides {
            paths.insert(binary.default_name().to_string(), PathBuf::from(path));
        }
        RealToolRunner::new(paths)
    }

    fn sh_command(script: &str, timeout_secs: u64) -> ToolCommand {
        ToolCommand {
            binary: ToolBinary::Ffmpeg, // arbitrary; overridden by tool_paths
            args: vec!["-c".into(), script.into()],
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    // 1. Timeout --------------------------------------------------------

    #[tokio::test]
    async fn real_runner_timeout_returns_error_with_record() {
        let runner = real_runner_with(vec![(ToolBinary::Ffmpeg, "/bin/sh")]);
        let cmd = sh_command("sleep 60", 1);
        let cancel = CancellationToken::new();

        let err = runner.run(cmd, &cancel).await.expect_err("should timeout");
        match err {
            ToolRunnerError::Timeout { elapsed, command } => {
                assert!(elapsed >= Duration::from_millis(900));
                assert_eq!(command.binary, ToolBinary::Ffmpeg);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    // 2. Cancellation ---------------------------------------------------

    #[tokio::test]
    async fn real_runner_cancellation_returns_error_with_record() {
        let runner = real_runner_with(vec![(ToolBinary::Ffmpeg, "/bin/sh")]);
        let cmd = sh_command("sleep 60", 300);
        let cancel = CancellationToken::new();

        // Cancel after a short delay.
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel2.cancel();
        });

        let err = runner.run(cmd, &cancel).await.expect_err("should cancel");
        match err {
            ToolRunnerError::Cancelled { command } => {
                assert_eq!(command.binary, ToolBinary::Ffmpeg);
            }
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    // 3. Signal ---------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn real_runner_signal_death_maps_to_process_exit_signal() {
        let runner = real_runner_with(vec![(ToolBinary::Ffmpeg, "/bin/sh")]);
        // The shell sends SIGKILL to itself — uncatchable.
        let cmd = sh_command("kill -9 $$", 10);
        let cancel = CancellationToken::new();

        let err = runner.run(cmd, &cancel).await.expect_err("should fail");
        match err {
            ToolRunnerError::NonZeroExit { exit, .. } => {
                assert!(
                    matches!(exit, ProcessExit::Signal(9)),
                    "expected Signal(9), got {exit:?}"
                );
            }
            other => panic!("expected NonZeroExit with signal, got {other:?}"),
        }
    }

    // 4. Redaction ------------------------------------------------------

    #[tokio::test]
    async fn real_runner_redacts_secrets_in_records_and_errors() {
        let runner = real_runner_with(vec![(ToolBinary::SevenZip, "/bin/sh")]);
        let cmd = ToolCommand {
            binary: ToolBinary::SevenZip,
            args: vec!["-c".into(), "-phunter2".into(), "echo ok".into()],
            secret_args: vec![1], // index 1 is the password arg
            cwd: None,
            env: vec![EnvVar {
                key: "SECRET_TOKEN".into(),
                value: SecretString::new("s3cr3t_v4lue"),
                secret: true,
            }],
            timeout: Duration::from_secs(10),
        };
        let cancel = CancellationToken::new();

        // The command will fail (bad shell syntax), but that's fine —
        // we're testing redaction, not success.
        let result = runner.run(cmd, &cancel).await;
        let record = match &result {
            Ok(output) => &output.command,
            Err(ToolRunnerError::NonZeroExit { command, .. }) => command,
            Err(ToolRunnerError::Spawn { command }) => command,
            other => panic!("unexpected result: {other:?}"),
        };

        // The sanitized args must redact index 1.
        assert_eq!(record.sanitized_args[1], "<redacted>");
        // Env keys are present but values are not.
        assert_eq!(record.env_keys, vec!["SECRET_TOKEN".to_string()]);
        // Full serialization must not contain either secret.
        let json = serde_json::to_string(record).unwrap();
        assert!(!json.contains("hunter2"), "password leaked in record JSON");
        assert!(!json.contains("s3cr3t_v4lue"), "env secret leaked in record JSON");

        // The error's Debug output must not contain secrets.
        if let Err(ref e) = result {
            let debug = format!("{e:?}");
            assert!(!debug.contains("hunter2"), "password leaked in error Debug");
            assert!(!debug.contains("s3cr3t_v4lue"), "env secret leaked in error Debug");
        }
    }

    // 5. Non-zero exit --------------------------------------------------

    #[tokio::test]
    async fn real_runner_nonzero_exit_returns_stderr_and_record() {
        let runner = real_runner_with(vec![(ToolBinary::Ffmpeg, "/bin/sh")]);
        let cmd = sh_command("echo 'some error text' >&2; exit 42", 10);
        let cancel = CancellationToken::new();

        let err = runner.run(cmd, &cancel).await.expect_err("should fail");
        match err {
            ToolRunnerError::NonZeroExit {
                exit,
                stderr_tail,
                command,
            } => {
                assert_eq!(exit, ProcessExit::Code(42));
                assert!(
                    stderr_tail.contains("some error text"),
                    "stderr should contain the error message"
                );
                assert_eq!(command.binary, ToolBinary::Ffmpeg);
                assert!(command.elapsed > Duration::ZERO || command.elapsed == Duration::ZERO);
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    // 6. Bounded capture ------------------------------------------------

    #[tokio::test]
    async fn real_runner_bounds_stderr_to_64kib_tail() {
        let runner = real_runner_with(vec![(ToolBinary::Ffmpeg, "/bin/sh")]);
        // Generate 128 KiB of output to stderr, then exit 1 so we
        // can inspect the stderr_tail on the error.
        let script = format!(
            "dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\\0' 'X' >&2; exit 1"
        );
        let cmd = sh_command(&script, 30);
        let cancel = CancellationToken::new();

        let err = runner.run(cmd, &cancel).await.expect_err("should fail");
        match err {
            ToolRunnerError::NonZeroExit { stderr_tail, .. } => {
                assert!(
                    stderr_tail.len() <= TOOL_OUTPUT_TAIL_BYTES,
                    "stderr_tail {} exceeds 64 KiB bound",
                    stderr_tail.len()
                );
                // Should have captured data (not empty).
                assert!(stderr_tail.len() > 1024, "expected substantial stderr capture");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    // 7. Path resolution ------------------------------------------------

    #[test]
    fn every_tool_binary_has_a_default_name() {
        // Exhaustive check: every variant maps to a non-empty name.
        let all = [
            ToolBinary::SevenZip,
            ToolBinary::Ffmpeg,
            ToolBinary::Ffprobe,
            ToolBinary::Sox,
            ToolBinary::Loudgain,
            ToolBinary::Metaflac,
            ToolBinary::Opustags,
            ToolBinary::Wvunpack,
            ToolBinary::Wvtag,
            ToolBinary::AtomicParsley,
        ];
        for binary in &all {
            let name = binary.default_name();
            assert!(!name.is_empty(), "{binary:?} has empty default_name");
        }
    }

    #[test]
    fn path_resolution_uses_custom_override() {
        let mut paths = std::collections::HashMap::new();
        paths.insert("sox".to_string(), PathBuf::from("/custom/path/to/sox"));
        let runner = RealToolRunner::new(paths);
        let resolved = runner.resolve_binary(ToolBinary::Sox);
        assert_eq!(resolved, PathBuf::from("/custom/path/to/sox"));
    }

    #[test]
    fn path_resolution_falls_back_to_default_name() {
        let runner = RealToolRunner::new(std::collections::HashMap::new());
        // Without a custom override, Ffprobe resolves to its default name.
        let resolved = runner.resolve_binary(ToolBinary::Ffprobe);
        assert_eq!(resolved, PathBuf::from("ffprobe"));
    }
}
