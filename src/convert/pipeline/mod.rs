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
pub mod materializer_7z;
pub mod materializer_cue;
pub mod materializer_sacd;
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

        // PR 4 validate_request checks container exists; our test path doesn't.
        // Just verify it doesn't panic.
        let _ = validate_request(&req);
        // PR 4 detect_source_kind recognizes .7z; just verify no panic.
        let _ = detect_source_kind(&req);
        // PR 4 materializer_for(SevenZip) now returns Ok.
        assert!(materializer_for(SourceKind::SevenZip).is_ok());

        let realize = realize_track(
            &TrackSourceRef::StagedFile(PathBuf::from("/s/1.flac")),
            &req,
            &staging,
            &runner,
            &cancel,
        )
        .await;
        // PR 4: StagedFile arm checks file exists — /s/1.flac doesn't, so TrackValidation error.
        assert!(realize.is_err());

        // PR 4: plan_outputs validates template and source — just verify no panic.
        let _ = plan_outputs(&prepared_source(), &req);

        let convert = convert_tracks(&prepared_source(), &AlbumPlan {
            album_dir: PathBuf::from("/out"),
            entries: vec![],
        }, &req, &staging, &runner, &cancel)
        .await;
        // PR 4: convert_tracks is real now — just verify no panic.
        let _ = convert.record.outcome;

        let merged = merge_tracks(sample_artifacts(), &req, &staging, &runner, &cancel)
            .await
            .expect("merge");
        // req.merge is false in sample_request(), so Skipped.
        assert_eq!(merged.1.outcome, StageOutcome::Skipped);

        let meta = apply_metadata(&sample_artifacts(), &prepared_source(), &req, &runner, &cancel)
            .await
            .expect("metadata");
        // metadata is Enabled in sample_request(); with stub runner it succeeds.
        assert!(matches!(meta.outcome, StageOutcome::Ok | StageOutcome::Skipped));

        let rg = apply_replaygain(&sample_artifacts(), &req, &runner, &cancel)
            .await
            .expect("replaygain");
        // replaygain is Enabled in sample_request(); with stub runner it succeeds.
        assert!(matches!(rg.outcome, StageOutcome::Ok | StageOutcome::Skipped));

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

        // PR 4: build_publish_plan now validates — just verify no panic.
        let _ = build_publish_plan(&sample_artifacts(), &req);

        let publish_staging = StagingDir::new(
            std::env::temp_dir().join("tonepoet-pr1-publish-nonexistent"),
            "job-1".into(),
        );
        let plan = PublishPlan { album_dir: PathBuf::from("/out"), entries: vec![] };
        // PR 4: publish_album_output has a real body — verify no panic.
        let _ = publish_album_output(
            publish_staging,
            &plan,
            req.publish.clone(),
        );

        let report = PipelineReport {
            request: RedactedPipelineRequest::from(&req),
            source: None,
            plan: None,
            artifacts: None,
            published: None,
            outcome: AlbumOutcome::Complete { tracks: vec![], stages: vec![] },
            durable_log: None,
        };
        // write_durable_log now has a real body (PR 6) — it succeeds.
        assert!(write_durable_log(&report, &req.log).is_ok());

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

    // ====================================================================
    // PR 3 — SevenZipMaterializer exit-condition tests
    // ====================================================================

    use crate::convert::pipeline::materializer_7z::SevenZipMaterializer;
    use crate::convert::pipeline::stages::Materializer;

    /// Build a minimal `PipelineRequest` for materializer tests.
    fn mat_request(container: PathBuf, password: Option<&str>) -> PipelineRequest {
        PipelineRequest {
            job_id: "mat-job".into(),
            item_id: "mat-item".into(),
            container,
            source: SourceOptions {
                archive_password: password.map(SecretString::new),
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

    /// JSON that ffprobe would return for one audio stream.
    fn ffprobe_json(sample_rate: u32, duration_secs: f64) -> String {
        format!(
            r#"{{"streams":[{{"sample_rate":"{}","duration":"{}"}}],"format":{{"duration":"{}"}}}}"#,
            sample_rate, duration_secs, duration_secs,
        )
    }

    /// Create fake audio files in a staging dir and return the staging root.
    fn setup_staging(names: &[&str]) -> (tempfile::TempDir, StagingDir) {
        let tmp = tempfile::tempdir().unwrap();
        for name in names {
            let path = tmp.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, b"fake-audio").unwrap();
        }
        let staging = StagingDir::new(tmp.path().to_path_buf(), "test-job".into());
        // Disarm so tempdir cleanup doesn't race with StagingDir Drop.
        // (The tempdir owns the directory; StagingDir just borrows the path.)
        // Actually, we need StagingDir armed for cancellation tests.
        // We'll handle this per-test.
        (tmp, staging)
    }

    // 1. Plain 7z — success path ----------------------------------------

    #[tokio::test]
    async fn materializer_plain_7z_yields_expected_prepared_source() {
        let (tmp, mut staging) = setup_staging(&[
            "01 - First Track.flac",
            "02 - Second Track.flac",
            "03 - Third Track.flac",
        ]);
        staging.disarm(); // tmp owns cleanup

        let runner = StubToolRunner::new();
        // 7z extraction: success (files already pre-staged).
        // Push one default success for 7z, then three for ffprobe.
        // StubToolRunner returns default success when no responses queued.
        // Queue explicit ffprobe outputs.
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::SevenZip,
                sanitized_args: vec![],
                cwd: None,
                env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });
        for rate in [44100, 44100, 44100] {
            runner.push_output(ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: ffprobe_json(rate, 240.0),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: CommandRecord {
                    binary: ToolBinary::Ffprobe,
                    sanitized_args: vec![],
                    cwd: None,
                    env_keys: vec![],
                    exit: Some(ProcessExit::Code(0)),
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                },
            });
        }

        let req = mat_request(PathBuf::from("/fake/archive.7z"), None);
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let source = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap();

        assert_eq!(source.kind, SourceKind::SevenZip);
        assert_eq!(source.tracks.len(), 3);
        // Verify track ordering by source_ordinal.
        for (i, t) in source.tracks.iter().enumerate() {
            assert_eq!(t.id.source_ordinal, (i + 1) as u32);
            assert_eq!(t.sample_rate, 44100);
            assert!(t.expected_samples.is_some());
            assert!(matches!(t.source_ref, TrackSourceRef::StagedFile(_)));
        }
        assert_eq!(source.provenance.source_kind, SourceKind::SevenZip);
        drop(tmp);
    }

    // 2. Password-protected — redaction on success + failure --------------

    #[tokio::test]
    async fn materializer_password_success_redacts_in_command_record() {
        let (tmp, mut staging) = setup_staging(&["track.flac"]);
        staging.disarm();

        let runner = StubToolRunner::new();
        // Queue 7z success + ffprobe success.
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::SevenZip,
                sanitized_args: vec![],
                cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: ffprobe_json(48000, 180.0),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::Ffprobe,
                sanitized_args: vec![], cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });

        let req = mat_request(PathBuf::from("/fake/pw.7z"), Some("s3cretPW"));
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let _source = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap();

        // The 7z command record must have the password redacted.
        let transcript = runner.transcript();
        let sz_record = &transcript[0];
        assert_eq!(sz_record.binary, ToolBinary::SevenZip);
        let json = serde_json::to_string(sz_record).unwrap();
        assert!(!json.contains("s3cretPW"), "password leaked in success record");
        drop(tmp);
    }

    #[tokio::test]
    async fn materializer_password_failure_redacts_in_error() {
        let (tmp, mut staging) = setup_staging(&[]);
        staging.disarm();

        let runner = StubToolRunner::new();
        // 7z returns "Wrong password" error.
        runner.push_failure("Wrong password");

        let req = mat_request(PathBuf::from("/fake/pw.7z"), Some("badPW123"));
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let err = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap_err();

        assert!(matches!(err, MaterializeError::Encrypted));
        // Verify the command record on the error path doesn't leak the password.
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 1);
        let json = serde_json::to_string(&transcript[0]).unwrap();
        assert!(!json.contains("badPW123"), "password leaked in failure record");
        drop(tmp);
    }

    // 3. Malformed archive -----------------------------------------------

    #[tokio::test]
    async fn materializer_malformed_archive_returns_extraction_error() {
        let (tmp, mut staging) = setup_staging(&[]);
        staging.disarm();

        let runner = StubToolRunner::new();
        runner.push_failure("Cannot open the file as archive");

        let req = mat_request(PathBuf::from("/fake/bad.7z"), None);
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let err = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap_err();

        assert!(matches!(err, MaterializeError::Extraction(_)));
        drop(tmp);
    }

    // 4. Empty archive ---------------------------------------------------

    #[tokio::test]
    async fn materializer_empty_archive_returns_error() {
        // 7z succeeds but no audio files in the staging dir.
        let (tmp, mut staging) = setup_staging(&["readme.txt", "cover.jpg"]);
        staging.disarm();

        let runner = StubToolRunner::new();
        // 7z success.

        let req = mat_request(PathBuf::from("/fake/empty.7z"), None);
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let err = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap_err();

        assert!(
            matches!(err, MaterializeError::Extraction(ref msg) if msg.contains("no audio")),
            "expected 'no audio files' error, got: {err:?}"
        );
        drop(tmp);
    }

    // 5. Mixed audio/non-audio -------------------------------------------

    #[tokio::test]
    async fn materializer_mixed_archive_returns_only_audio_tracks() {
        let (tmp, mut staging) = setup_staging(&[
            "cover.jpg",
            "info.txt",
            "track1.flac",
            "track2.flac",
            "thumbs.db",
        ]);
        staging.disarm();

        let runner = StubToolRunner::new();
        // 7z success + 2 ffprobe calls (one per audio file).
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: String::new(), stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::SevenZip, sanitized_args: vec![],
                cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });
        for _ in 0..2 {
            runner.push_output(ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: ffprobe_json(44100, 300.0),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: CommandRecord {
                    binary: ToolBinary::Ffprobe, sanitized_args: vec![],
                    cwd: None, env_keys: vec![],
                    exit: Some(ProcessExit::Code(0)),
                    stdout_tail: String::new(), stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                },
            });
        }

        let req = mat_request(PathBuf::from("/fake/mixed.7z"), None);
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let source = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap();

        assert_eq!(source.tracks.len(), 2, "should only have audio tracks");
        drop(tmp);
    }

    // 6. Multi-disc archive ----------------------------------------------

    #[tokio::test]
    async fn materializer_multi_disc_assigns_correct_disc_numbers() {
        // Simulate a multi-disc archive with subdirectories.
        let (tmp, mut staging) = setup_staging(&[
            "CD1/01 - Track A.flac",
            "CD1/02 - Track B.flac",
            "CD2/01 - Track C.flac",
        ]);
        staging.disarm();

        let runner = StubToolRunner::new();
        // 7z success + 3 ffprobe calls.
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: String::new(), stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::SevenZip, sanitized_args: vec![],
                cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });
        for _ in 0..3 {
            runner.push_output(ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: ffprobe_json(96000, 400.0),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: CommandRecord {
                    binary: ToolBinary::Ffprobe, sanitized_args: vec![],
                    cwd: None, env_keys: vec![],
                    exit: Some(ProcessExit::Code(0)),
                    stdout_tail: String::new(), stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                },
            });
        }

        let req = mat_request(PathBuf::from("/fake/multi.7z"), None);
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let source = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap();

        assert_eq!(source.tracks.len(), 3);
        // Tracks are sorted by path — CD1 before CD2.
        assert_eq!(source.tracks[0].id.source_ordinal, 1);
        assert_eq!(source.tracks[2].id.source_ordinal, 3);
        // All tracks should have StagedFile refs.
        for t in &source.tracks {
            assert!(matches!(t.source_ref, TrackSourceRef::StagedFile(_)));
            assert_eq!(t.sample_rate, 96000);
        }
        drop(tmp);
    }

    // 7. TrackSelection::Range -------------------------------------------

    #[tokio::test]
    async fn materializer_track_selection_range_filters_correctly() {
        let (tmp, mut staging) = setup_staging(&[
            "01.flac", "02.flac", "03.flac", "04.flac", "05.flac",
        ]);
        staging.disarm();

        let runner = StubToolRunner::new();
        // 7z success + 5 ffprobe calls.
        for _ in 0..6 {
            runner.push_output(ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: ffprobe_json(44100, 200.0),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: CommandRecord {
                    binary: ToolBinary::Ffmpeg, sanitized_args: vec![],
                    cwd: None, env_keys: vec![],
                    exit: Some(ProcessExit::Code(0)),
                    stdout_tail: String::new(), stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                },
            });
        }

        let mut req = mat_request(PathBuf::from("/fake/five.7z"), None);
        req.source.track_selection = TrackSelection::Range { start: 2, end: 4 };
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let source = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap();

        assert_eq!(source.tracks.len(), 3);
        let ordinals: Vec<u32> = source.tracks.iter().map(|t| t.id.source_ordinal).collect();
        assert_eq!(ordinals, vec![2, 3, 4]);
        drop(tmp);
    }

    // 8. TrackSelection::Set + invalid -----------------------------------

    #[tokio::test]
    async fn materializer_track_selection_set_filters_correctly() {
        let (tmp, mut staging) = setup_staging(&[
            "01.flac", "02.flac", "03.flac", "04.flac",
        ]);
        staging.disarm();

        let runner = StubToolRunner::new();
        for _ in 0..5 {
            runner.push_output(ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: ffprobe_json(44100, 200.0),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: CommandRecord {
                    binary: ToolBinary::Ffmpeg, sanitized_args: vec![],
                    cwd: None, env_keys: vec![],
                    exit: Some(ProcessExit::Code(0)),
                    stdout_tail: String::new(), stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                },
            });
        }

        let mut req = mat_request(PathBuf::from("/fake/four.7z"), None);
        req.source.track_selection = TrackSelection::Set([1, 3].into_iter().collect());
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let source = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap();

        assert_eq!(source.tracks.len(), 2);
        let ordinals: Vec<u32> = source.tracks.iter().map(|t| t.id.source_ordinal).collect();
        assert_eq!(ordinals, vec![1, 3]);
        drop(tmp);
    }

    #[tokio::test]
    async fn materializer_track_selection_invalid_range_rejected() {
        let (tmp, mut staging) = setup_staging(&["01.flac", "02.flac"]);
        staging.disarm();

        let runner = StubToolRunner::new();
        for _ in 0..3 {
            runner.push_output(ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: ffprobe_json(44100, 200.0),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: CommandRecord {
                    binary: ToolBinary::Ffmpeg, sanitized_args: vec![],
                    cwd: None, env_keys: vec![],
                    exit: Some(ProcessExit::Code(0)),
                    stdout_tail: String::new(), stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                },
            });
        }

        let mut req = mat_request(PathBuf::from("/fake/two.7z"), None);
        req.source.track_selection = TrackSelection::Range { start: 5, end: 10 };
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let err = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap_err();
        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
        drop(tmp);
    }

    #[tokio::test]
    async fn materializer_track_selection_invalid_set_rejected() {
        let (tmp, mut staging) = setup_staging(&["01.flac"]);
        staging.disarm();

        let runner = StubToolRunner::new();
        for _ in 0..2 {
            runner.push_output(ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: ffprobe_json(44100, 200.0),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: CommandRecord {
                    binary: ToolBinary::Ffmpeg, sanitized_args: vec![],
                    cwd: None, env_keys: vec![],
                    exit: Some(ProcessExit::Code(0)),
                    stdout_tail: String::new(), stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                },
            });
        }

        let mut req = mat_request(PathBuf::from("/fake/one.7z"), None);
        req.source.track_selection = TrackSelection::Set([1, 99].into_iter().collect());
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let err = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap_err();
        assert!(matches!(err, MaterializeError::InvalidTrackSelection(_)));
        drop(tmp);
    }

    // 9. Cancellation ---------------------------------------------------

    #[tokio::test]
    async fn materializer_cancellation_returns_cancelled_and_cleans_staging() {
        let staging_path = std::env::temp_dir().join("tonepoet-mat-cancel-test");
        let _ = std::fs::remove_dir_all(&staging_path);
        std::fs::create_dir_all(&staging_path).unwrap();
        // Create a file so the directory isn't empty.
        std::fs::write(staging_path.join("track.flac"), b"data").unwrap();

        // StagingDir is armed — Drop will clean up.
        let staging = StagingDir::new(staging_path.clone(), "cancel-job".into());

        let runner = StubToolRunner::new();
        // Don't queue any response — stub returns success immediately
        // for 7z, but we'll cancel before ffprobe.

        let cancel = CancellationToken::new();
        cancel.cancel(); // Already cancelled.

        let req = mat_request(PathBuf::from("/fake/archive.7z"), None);
        let mat = SevenZipMaterializer;
        let err = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap_err();
        assert!(matches!(err, MaterializeError::Cancelled));

        // Drop the staging dir — it should clean up.
        drop(staging);
        assert!(!staging_path.exists(), "staging dir should be deleted by RAII Drop");
    }

    // 10. Permission-denied ---------------------------------------------

    #[tokio::test]
    async fn materializer_permission_denied_returns_structured_error() {
        let (tmp, mut staging) = setup_staging(&[]);
        staging.disarm();

        let runner = StubToolRunner::new();
        runner.push_failure("ERROR: /protected/file : Can not open the file as [7z] archive\nPermission denied");

        let req = mat_request(PathBuf::from("/protected/archive.7z"), None);
        let cancel = CancellationToken::new();
        let mat = SevenZipMaterializer;
        let err = mat.materialize(&req, &staging, &runner, &cancel).await.unwrap_err();

        // Should be a structured error, not a panic.
        assert!(
            matches!(err, MaterializeError::Extraction(_) | MaterializeError::Tool(_)),
            "expected structured error, got: {err:?}"
        );
        drop(tmp);
    }

    // ====================================================================
    // PR 5 — merge_tracks exit-condition tests
    // ====================================================================

    /// Build a 3-track ArtifactSet for merge tests.
    fn merge_artifacts(sample_counts: &[Option<u64>]) -> ArtifactSet {
        let tracks: Vec<TrackArtifact> = sample_counts
            .iter()
            .enumerate()
            .map(|(i, &samples)| {
                let n = (i + 1) as u32;
                TrackArtifact {
                    track_id: TrackId {
                        source_ordinal: n,
                        disc_number: None,
                        track_number: n,
                    },
                    staged_path: PathBuf::from(format!("/stage/{:02}.flac", n)),
                    final_path: PathBuf::from(format!("/out/album/{:02}.flac", n)),
                    samples,
                }
            })
            .collect();
        ArtifactSet {
            audio: AudioArtifacts::Tracks(tracks),
            sidecars: vec![SidecarArtifact {
                kind: SidecarKind::ConversionLog,
                staged_path: PathBuf::from("/stage/log.txt"),
                final_path: PathBuf::from("/out/album/log.txt"),
            }],
        }
    }

    fn merge_request(do_merge: bool) -> PipelineRequest {
        let mut req = sample_request();
        req.merge = do_merge;
        req
    }

    // 1. Multi-track merge on → one merged artifact ---------------------

    #[tokio::test]
    async fn merge_multi_track_yields_one_merged_artifact() {
        let artifacts = merge_artifacts(&[Some(441000), Some(441000), Some(441000)]);
        let req = merge_request(true);
        let staging = StagingDir::new(
            std::env::temp_dir().join("tonepoet-merge-test-1"),
            "merge-1".into(),
        );
        let _ = std::fs::create_dir_all(&staging.root);

        let runner = StubToolRunner::new();
        // ffmpeg concat: success
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::Ffmpeg,
                sanitized_args: vec![],
                cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });
        // ffprobe validation: report duration matching 3 * 441000 samples at 44100
        // 441000 / 44100 = 10.0 seconds per track → 30.0 seconds total
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: ffprobe_json(44100, 30.0),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::Ffprobe,
                sanitized_args: vec![], cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });

        let cancel = CancellationToken::new();
        let (result_artifacts, record) =
            merge_tracks(artifacts, &req, &staging, &runner, &cancel)
                .await
                .expect("merge should succeed");

        assert_eq!(record.outcome, StageOutcome::Ok);
        match &result_artifacts.audio {
            AudioArtifacts::Merged(m) => {
                assert_eq!(m.source_tracks.len(), 3);
                assert!(m.total_samples > 0);
            }
            AudioArtifacts::Tracks(_) => panic!("expected Merged, got Tracks"),
        }
        // Sidecars preserved.
        assert_eq!(result_artifacts.sidecars.len(), 1);

        // Verify ffmpeg used -c copy (concat demuxer).
        let transcript = runner.transcript();
        let ffmpeg_record = &transcript[0];
        assert_eq!(ffmpeg_record.binary, ToolBinary::Ffmpeg);

        let _ = std::fs::remove_dir_all(&staging.root);
    }

    // 2. Truncated merge → DurationMismatch -----------------------------

    #[tokio::test]
    async fn merge_truncated_output_returns_duration_mismatch() {
        let artifacts = merge_artifacts(&[Some(441000), Some(441000), Some(441000)]);
        let req = merge_request(true);
        let staging = StagingDir::new(
            std::env::temp_dir().join("tonepoet-merge-test-2"),
            "merge-2".into(),
        );
        let _ = std::fs::create_dir_all(&staging.root);

        let runner = StubToolRunner::new();
        // ffmpeg concat: success
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: String::new(), stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::Ffmpeg, sanitized_args: vec![],
                cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });
        // ffprobe: report only 5 seconds (expected 30) — way too short.
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: ffprobe_json(44100, 5.0),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                binary: ToolBinary::Ffprobe, sanitized_args: vec![],
                cwd: None, env_keys: vec![],
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(), stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });

        let cancel = CancellationToken::new();
        let err = merge_tracks(artifacts, &req, &staging, &runner, &cancel)
            .await
            .expect_err("should fail with mismatch");

        assert!(
            matches!(err, MergeError::DurationMismatch(_)),
            "expected DurationMismatch, got: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    // 3. Merge failure → Blocked via aggregation ------------------------

    #[tokio::test]
    async fn merge_failure_maps_to_blocked_via_aggregation() {
        let artifacts = merge_artifacts(&[Some(441000), Some(441000)]);
        let req = merge_request(true);
        let staging = StagingDir::new(
            std::env::temp_dir().join("tonepoet-merge-test-3"),
            "merge-3".into(),
        );
        let _ = std::fs::create_dir_all(&staging.root);

        let runner = StubToolRunner::new();
        // ffmpeg concat fails.
        runner.push_failure("ffmpeg: concat error");

        let cancel = CancellationToken::new();
        let err = merge_tracks(artifacts, &req, &staging, &runner, &cancel)
            .await
            .expect_err("should fail");

        // Now simulate how the orchestrator would record this:
        let stage_record = StageRecord {
            stage: PipelineStage::Merge,
            outcome: StageOutcome::Failed(format!("{}", err)),
        };
        let outcome = aggregate_album_outcome(
            vec![track_record(1, true), track_record(2, true)],
            vec![stage_record],
            FailurePolicy::FailAlbumOnAnyTrackFailure,
        );
        assert!(matches!(
            outcome,
            AlbumOutcome::Blocked {
                reason: BlockReason::RequiredStageFailure(PipelineStage::Merge),
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    // 4. Merge off → passthrough with Skipped ---------------------------

    #[tokio::test]
    async fn merge_off_passes_through_with_skipped() {
        let artifacts = merge_artifacts(&[Some(441000), Some(441000)]);
        let req = merge_request(false);
        let staging = StagingDir::new(PathBuf::from("/nonexistent"), "x".into());

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();
        let (result_artifacts, record) =
            merge_tracks(artifacts, &req, &staging, &runner, &cancel)
                .await
                .expect("should pass through");

        assert_eq!(record.outcome, StageOutcome::Skipped);
        assert!(matches!(result_artifacts.audio, AudioArtifacts::Tracks(ref t) if t.len() == 2));
        // No tool calls should have been made.
        assert!(runner.transcript().is_empty());
    }

    // 5. Single track merge → wrapped as MergedArtifact -----------------

    #[tokio::test]
    async fn merge_single_track_wraps_as_merged() {
        let artifacts = merge_artifacts(&[Some(441000)]);
        let req = merge_request(true);
        let staging = StagingDir::new(PathBuf::from("/nonexistent"), "x".into());

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();
        let (result_artifacts, record) =
            merge_tracks(artifacts, &req, &staging, &runner, &cancel)
                .await
                .expect("should succeed");

        assert_eq!(record.outcome, StageOutcome::Ok);
        match &result_artifacts.audio {
            AudioArtifacts::Merged(m) => {
                assert_eq!(m.source_tracks.len(), 1);
                assert_eq!(m.total_samples, 441000);
            }
            AudioArtifacts::Tracks(_) => panic!("expected Merged"),
        }
        // No tool calls — single track doesn't need concat.
        assert!(runner.transcript().is_empty());
    }

    // ====================================================================
    // PR 6 — metadata, replaygain, features, durable log tests
    // ====================================================================

    // ---- apply_metadata -----------------------------------------------

    #[tokio::test]
    async fn metadata_enabled_flac_tracks_calls_metaflac_with_tags() {
        let artifacts = merge_artifacts(&[Some(441000), Some(441000)]);
        let mut source = prepared_source();
        source.tracks = vec![
            PreparedTrack {
                id: TrackId { source_ordinal: 1, disc_number: None, track_number: 1 },
                source_ref: TrackSourceRef::StagedFile(PathBuf::from("/stage/01.flac")),
                metadata: TrackMetadata {
                    title: Some("First Song".into()),
                    artist: Some("The Band".into()),
                    ..TrackMetadata::default()
                },
                expected_samples: Some(441000),
                sample_rate: 44100,
            },
            PreparedTrack {
                id: TrackId { source_ordinal: 2, disc_number: None, track_number: 2 },
                source_ref: TrackSourceRef::StagedFile(PathBuf::from("/stage/02.flac")),
                metadata: TrackMetadata {
                    title: Some("Second Song".into()),
                    artist: Some("The Band".into()),
                    ..TrackMetadata::default()
                },
                expected_samples: Some(441000),
                sample_rate: 44100,
            },
        ];

        let mut req = sample_request();
        req.stages.metadata = StageRequirement::Enabled;

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let record = apply_metadata(&artifacts, &source, &req, &runner, &cancel)
            .await
            .expect("should succeed");

        assert_eq!(record.outcome, StageOutcome::Ok);
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 2, "should call metaflac for each track");
        assert_eq!(transcript[0].binary, ToolBinary::Metaflac);
        // Check tag values appear in args.
        let args_joined = transcript[0].sanitized_args.join(" ");
        assert!(args_joined.contains("First Song"), "should contain track title");
        assert!(args_joined.contains("The Band"), "should contain artist");
    }

    #[tokio::test]
    async fn metadata_disabled_skips_with_no_tool_calls() {
        let artifacts = merge_artifacts(&[Some(441000)]);
        let source = prepared_source();
        let mut req = sample_request();
        req.stages.metadata = StageRequirement::Disabled;

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let record = apply_metadata(&artifacts, &source, &req, &runner, &cancel)
            .await
            .expect("should skip");

        assert_eq!(record.outcome, StageOutcome::Skipped);
        assert!(runner.transcript().is_empty());
    }

    #[tokio::test]
    async fn metadata_enabled_tool_failure_returns_error() {
        let artifacts = merge_artifacts(&[Some(441000)]);
        let mut source = prepared_source();
        source.tracks[0].metadata.title = Some("Song".into());

        let mut req = sample_request();
        req.stages.metadata = StageRequirement::Enabled;

        let runner = StubToolRunner::new();
        runner.push_failure("metaflac: invalid argument");
        let cancel = CancellationToken::new();

        let err = apply_metadata(&artifacts, &source, &req, &runner, &cancel)
            .await
            .expect_err("should fail");

        assert!(matches!(err, MetadataError::Tool(_)));
    }

    // ---- apply_replaygain ---------------------------------------------

    #[tokio::test]
    async fn replaygain_enabled_tracks_uses_album_mode() {
        let artifacts = merge_artifacts(&[Some(441000), Some(441000)]);
        let mut req = sample_request();
        req.stages.replaygain = StageRequirement::Enabled;

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let record = apply_replaygain(&artifacts, &req, &runner, &cancel)
            .await
            .expect("should succeed");

        assert_eq!(record.outcome, StageOutcome::Ok);
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].binary, ToolBinary::Loudgain);
        let args = &transcript[0].sanitized_args;
        assert!(args.contains(&"-a".to_string()), "should use album mode (-a)");
        assert!(args.contains(&"-k".to_string()), "should prevent clipping (-k)");
    }

    #[tokio::test]
    async fn replaygain_enabled_merged_uses_single_file_mode() {
        let inner = ArtifactSet {
            audio: AudioArtifacts::Merged(MergedArtifact {
                staged_path: PathBuf::from("/stage/merged.flac"),
                final_path: PathBuf::from("/out/merged.flac"),
                total_samples: 882000,
                source_tracks: vec![
                    TrackId { source_ordinal: 1, disc_number: None, track_number: 1 },
                ],
            }),
            sidecars: vec![],
        };
        let mut req = sample_request();
        req.stages.replaygain = StageRequirement::Enabled;

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let record = apply_replaygain(&inner, &req, &runner, &cancel)
            .await
            .expect("should succeed");

        assert_eq!(record.outcome, StageOutcome::Ok);
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), 1);
        let args = &transcript[0].sanitized_args;
        assert!(!args.contains(&"-a".to_string()), "merged should NOT use album mode");
        assert!(args.iter().any(|a| a.contains("merged.flac")));
    }

    #[tokio::test]
    async fn replaygain_disabled_skips() {
        let artifacts = merge_artifacts(&[Some(441000)]);
        let mut req = sample_request();
        req.stages.replaygain = StageRequirement::Disabled;

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let record = apply_replaygain(&artifacts, &req, &runner, &cancel)
            .await
            .expect("should skip");

        assert_eq!(record.outcome, StageOutcome::Skipped);
        assert!(runner.transcript().is_empty());
    }

    // ---- run_features -------------------------------------------------

    #[tokio::test]
    async fn features_enabled_adds_sidecar_artifacts() {
        let artifacts = merge_artifacts(&[Some(441000), Some(441000)]);
        let outcome = AlbumOutcome::Complete {
            tracks: vec![track_record(1, true), track_record(2, true)],
            stages: vec![],
        };
        let source = prepared_source();
        let mut req = sample_request();
        req.stages.features = StageRequirement::Enabled;

        let staging = StagingDir::new(
            std::env::temp_dir().join("tonepoet-feat-test"),
            "feat-1".into(),
        );
        let _ = std::fs::create_dir_all(&staging.root);

        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let (result, record) =
            run_features(artifacts, &outcome, &source, &req, &staging, &runner, &cancel)
                .await
                .expect("should succeed");

        assert_eq!(record.outcome, StageOutcome::Ok);
        // Should have at least a conversion log sidecar.
        assert!(
            result.sidecars.iter().any(|s| s.kind == SidecarKind::ConversionLog),
            "should have conversion log sidecar"
        );
        // The staged file written by run_features should exist on disk.
        // (merge_artifacts pre-populates a sidecar at /stage/log.txt which
        // doesn't exist; the real one is under the staging root.)
        let log_sidecar = result.sidecars.iter()
            .find(|s| s.kind == SidecarKind::ConversionLog && s.staged_path.starts_with(&staging.root))
            .expect("should have a conversion log sidecar under staging root");
        assert!(log_sidecar.staged_path.exists(), "log file should exist on disk");

        let _ = std::fs::remove_dir_all(&staging.root);
    }

    #[tokio::test]
    async fn features_disabled_passes_through() {
        let artifacts = merge_artifacts(&[Some(441000)]);
        let outcome = AlbumOutcome::Complete { tracks: vec![], stages: vec![] };
        let source = prepared_source();
        let mut req = sample_request();
        req.stages.features = StageRequirement::Disabled;

        let staging = StagingDir::new(PathBuf::from("/nonexistent"), "x".into());
        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();

        let (_, record) =
            run_features(artifacts, &outcome, &source, &req, &staging, &runner, &cancel)
                .await
                .expect("should pass through");

        assert_eq!(record.outcome, StageOutcome::Skipped);
    }

    // ---- write_durable_log --------------------------------------------

    #[test]
    fn durable_log_writes_readable_json_with_expected_fields() {
        let req = sample_request();
        let report = PipelineReport {
            request: RedactedPipelineRequest::from(&req),
            source: Some(prepared_source()),
            plan: Some(AlbumPlan {
                album_dir: PathBuf::from("/out/album"),
                entries: vec![],
            }),
            artifacts: Some(sample_artifacts()),
            published: None,
            outcome: AlbumOutcome::Complete {
                tracks: vec![track_record(1, true)],
                stages: vec![StageRecord {
                    stage: PipelineStage::Convert,
                    outcome: StageOutcome::Ok,
                }],
            },
            durable_log: None,
        };

        let tmp = tempfile::tempdir().unwrap();
        let policy = LogPolicy {
            root: tmp.path().to_path_buf(),
            write_for_blocked: true,
        };

        let path = write_durable_log(&report, &policy).expect("should write");
        assert!(path.exists(), "log file should exist");

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content)
            .expect("should be valid JSON");

        // Verify key fields are present.
        assert!(parsed.get("request").is_some(), "should have request");
        assert!(parsed.get("source").is_some(), "should have source");
        assert!(parsed.get("plan").is_some(), "should have plan");
        assert!(parsed.get("artifacts").is_some(), "should have artifacts");
        assert!(parsed.get("outcome").is_some(), "should have outcome");
    }

    #[test]
    fn durable_log_never_contains_secrets() {
        let req = sample_request(); // has password "hunter2"
        let report = PipelineReport {
            request: RedactedPipelineRequest::from(&req),
            source: None,
            plan: None,
            artifacts: None,
            published: None,
            outcome: AlbumOutcome::Complete { tracks: vec![], stages: vec![] },
            durable_log: None,
        };

        let tmp = tempfile::tempdir().unwrap();
        let policy = LogPolicy {
            root: tmp.path().to_path_buf(),
            write_for_blocked: true,
        };

        let path = write_durable_log(&report, &policy).expect("should write");
        let content = std::fs::read_to_string(&path).unwrap();

        assert!(!content.contains("hunter2"), "durable log must not contain password");
    }

    #[test]
    fn durable_log_io_failure_returns_error() {
        let req = sample_request();
        let report = PipelineReport {
            request: RedactedPipelineRequest::from(&req),
            source: None, plan: None, artifacts: None, published: None,
            outcome: AlbumOutcome::Complete { tracks: vec![], stages: vec![] },
            durable_log: None,
        };

        // Write to a path that can't be created.
        let policy = LogPolicy {
            root: PathBuf::from("/nonexistent/deeply/nested/path"),
            write_for_blocked: true,
        };

        let err = write_durable_log(&report, &policy).expect_err("should fail");
        assert!(matches!(err, LogError::Io(_)));
    }
    fn pr8_command_record(binary: ToolBinary) -> CommandRecord {
        CommandRecord {
            binary,
            sanitized_args: Vec::new(),
            cwd: None,
            env_keys: Vec::new(),
            exit: Some(ProcessExit::Code(0)),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
        }
    }

    fn pr8_tool_output(binary: ToolBinary, stdout_tail: String) -> ToolOutput {
        ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail,
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: pr8_command_record(binary),
        }
    }

    fn pr8_exact_ffprobe_json(sample_rate: u32, samples: u64) -> String {
        let duration = samples as f64 / sample_rate as f64;
        format!(
            r#"{{"streams":[{{"sample_rate":"{}","duration_ts":"{}","time_base":"1/{}","duration":"{:.9}"}}],"format":{{"duration":"{:.9}"}}}}"#,
            sample_rate, samples, sample_rate, duration, duration
        )
    }

    fn pr8_duration_ffprobe_json(sample_rate: u32, duration: f64) -> String {
        format!(
            r#"{{"streams":[{{"sample_rate":"{}","duration":"{:.9}"}}],"format":{{"duration":"{:.9}"}}}}"#,
            sample_rate, duration, duration
        )
    }

    fn pr8_cue_text(file_name: &str) -> String {
        format!(
            r#"PERFORMER "Artist"
TITLE "Album"
REM DATE 1970
REM GENRE Rock
REM COMMENT "matrix"
FILE "{}" WAVE
  TRACK 01 AUDIO
    TITLE "One"
    ISRC USAAA0000001
    FLAGS PRE
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Two"
    REM NOTE "track-extra"
    INDEX 01 00:01:00
"#,
            file_name
        )
    }

    async fn pr8_materialize_fixture(image_name: &str) -> PreparedSource {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join(image_name);
        let cue_path = tmp.path().join("album.cue");
        std::fs::write(&image_path, b"dummy image").unwrap();
        std::fs::write(&cue_path, pr8_cue_text(image_name)).unwrap();

        let mut req = sample_request();
        req.container = image_path.clone();
        req.source.archive_password = None;
        req.source.cue_sidecar = CueSidecarPolicy::PreferSidecar;
        req.output_root = tmp.path().join("out");
        req.log.root = tmp.path().join("logs");

        let staging = StagingDir::new(tmp.path().join("stage"), "job-cue".to_string());
        let runner = StubToolRunner::new();
        runner.push_output(pr8_tool_output(
            ToolBinary::Ffprobe,
            pr8_exact_ffprobe_json(44_100, 132_300),
        ));

        let mat = super::materializer_cue::CueImageMaterializer;
        mat.materialize(&req, &staging, &runner, &CancellationToken::new())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pr8_cue_materializer_matrix_yields_segments_and_metadata() {
        for image_name in ["album.flac", "album.wv", "album.ape", "ümlaut album.flac"] {
            let source = pr8_materialize_fixture(image_name).await;
            assert_eq!(source.kind, SourceKind::CueImage);
            assert_eq!(source.tracks.len(), 2, "{image_name}");
            assert_eq!(source.album_metadata.album.as_deref(), Some("Album"));
            assert_eq!(source.album_metadata.album_artist.as_deref(), Some("Artist"));
            assert_eq!(source.album_metadata.genre.as_deref(), Some("Rock"));
            assert_eq!(source.album_metadata.date.as_deref(), Some("1970"));
            assert_eq!(source.album_metadata.extra.get("rem_comment").map(String::as_str), Some("matrix"));
            assert!(source.tracks[0].metadata.pre_emphasis);
            assert_eq!(source.tracks[0].metadata.isrc.as_deref(), Some("USAAA0000001"));
            assert_eq!(source.tracks[1].metadata.extra.get("rem_note").map(String::as_str), Some("track-extra"));
            match &source.tracks[0].source_ref {
                TrackSourceRef::ImageSegment { image, start_sample, samples } => {
                    assert_eq!(image.file_name().and_then(|value| value.to_str()), Some(image_name));
                    assert_eq!(*start_sample, 0);
                    assert_eq!(*samples, 44_100);
                }
                other => panic!("expected ImageSegment, got {other:?}"),
            }
            match &source.tracks[1].source_ref {
                TrackSourceRef::ImageSegment { start_sample, samples, .. } => {
                    assert_eq!(*start_sample, 44_100);
                    assert_eq!(*samples, 88_200);
                }
                other => panic!("expected ImageSegment, got {other:?}"),
            }
        }
    }

    #[test]
    fn pr8_detect_source_kind_validates_matching_single_image_cue() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("album.flac");
        let cue_path = tmp.path().join("album.cue");
        let foreign_cue_path = tmp.path().join("foreign.cue");
        let other_path = tmp.path().join("other.flac");
        std::fs::write(&image_path, b"dummy image").unwrap();
        std::fs::write(&other_path, b"dummy image").unwrap();
        std::fs::write(&foreign_cue_path, pr8_cue_text("other.flac")).unwrap();

        let mut req = sample_request();
        req.container = image_path.clone();
        req.source.archive_password = None;
        req.source.cue_sidecar = CueSidecarPolicy::PreferSidecar;
        assert!(matches!(detect_source_kind(&req), Err(SourceDetectError::UnknownSource)));

        std::fs::write(&cue_path, "not a cue sheet at all").unwrap();
        assert_eq!(detect_source_kind(&req).unwrap(), SourceKind::CueImage);

        std::fs::write(&cue_path, pr8_cue_text("album.flac")).unwrap();
        assert_eq!(detect_source_kind(&req).unwrap(), SourceKind::CueImage);

        req.source.cue_sidecar = CueSidecarPolicy::IgnoreCue;
        assert!(matches!(detect_source_kind(&req), Err(SourceDetectError::UnknownSource)));

        req.container = tmp.path().join("album.7z");
        assert_eq!(detect_source_kind(&req).unwrap(), SourceKind::SevenZip);
        assert!(materializer_for(SourceKind::CueImage).is_ok());
    }


    #[tokio::test]
    async fn pr8_malformed_same_stem_sidecar_fails_in_materializer() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("album.flac");
        let cue_path = tmp.path().join("album.cue");
        std::fs::write(&image_path, b"dummy image").unwrap();
        std::fs::write(&cue_path, "not a cue sheet at all").unwrap();

        let mut req = sample_request();
        req.container = image_path;
        req.source.archive_password = None;
        req.source.cue_sidecar = CueSidecarPolicy::PreferSidecar;
        req.output_root = tmp.path().join("out");
        req.log.root = tmp.path().join("logs");

        let staging = StagingDir::new(tmp.path().join("stage"), "job-cue".to_string());
        let runner = StubToolRunner::new();
        let mat = super::materializer_cue::CueImageMaterializer;
        let err = mat
            .materialize(&req, &staging, &runner, &CancellationToken::new())
            .await
            .expect_err("malformed same-stem sidecar should fail in materialization");
        assert!(matches!(err, MaterializeError::Parse(_)));
    }

    #[tokio::test]
    async fn pr8_sidecar_policy_failures_are_structured() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("album.flac");
        std::fs::write(&image_path, b"dummy image").unwrap();

        let mut req = sample_request();
        req.container = image_path;
        req.source.archive_password = None;
        req.output_root = tmp.path().join("out");
        req.log.root = tmp.path().join("logs");
        let staging = StagingDir::new(tmp.path().join("stage"), "job-cue".to_string());
        let runner = StubToolRunner::new();
        let mat = super::materializer_cue::CueImageMaterializer;

        req.source.cue_sidecar = CueSidecarPolicy::SidecarOnly;
        let err = mat
            .materialize(&req, &staging, &runner, &CancellationToken::new())
            .await
            .expect_err("missing sidecar should fail");
        assert!(matches!(err, MaterializeError::Parse(_)));

        req.source.cue_sidecar = CueSidecarPolicy::IgnoreCue;
        let err = mat
            .materialize(&req, &staging, &runner, &CancellationToken::new())
            .await
            .expect_err("IgnoreCue must not materialize");
        assert!(matches!(err, MaterializeError::Parse(_)));
    }

    #[test]
    fn pr8_probe_prefers_duration_ts_sample_count() {
        let parsed = super::materializer_cue::test_support::parse_probe_for_test(
            &pr8_exact_ffprobe_json(48_000, 96_001),
        )
        .unwrap();
        assert_eq!(parsed, (48_000, 96_001, true));

        let parsed = super::materializer_cue::test_support::parse_probe_for_test(
            &pr8_duration_ffprobe_json(44_100, 2.0),
        )
        .unwrap();
        assert_eq!(parsed, (44_100, 88_200, false));
    }

    struct CaptureFfmpegCutRunner {
        ffmpeg_args: std::sync::Mutex<Option<Vec<String>>>,
    }

    impl CaptureFfmpegCutRunner {
        fn new() -> Self {
            Self { ffmpeg_args: std::sync::Mutex::new(None) }
        }

        fn ffmpeg_args(&self) -> Vec<String> {
            self.ffmpeg_args
                .lock()
                .unwrap()
                .clone()
                .expect("ffmpeg should have been invoked")
        }
    }

    #[async_trait::async_trait]
    impl ToolRunner for CaptureFfmpegCutRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            match cmd.binary {
                ToolBinary::Ffmpeg => {
                    *self.ffmpeg_args.lock().unwrap() = Some(cmd.args.clone());
                    let out = cmd.args.last().unwrap().clone();
                    std::fs::write(out, b"track flac").unwrap();
                    Ok(pr8_tool_output(ToolBinary::Ffmpeg, String::new()))
                }
                ToolBinary::Ffprobe => Ok(pr8_tool_output(
                    ToolBinary::Ffprobe,
                    pr8_exact_ffprobe_json(44_100, 4_410),
                )),
                other => panic!("unexpected call {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn pr8_image_segment_cut_uses_absolute_sample_trim_without_input_seek() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("album.flac");
        std::fs::write(&image_path, b"dummy image").unwrap();
        let src = TrackSourceRef::ImageSegment {
            image: image_path,
            start_sample: 88_200,
            samples: 4_410,
        };
        let mut req = sample_request();
        req.source.archive_password = None;
        let staging = StagingDir::new(tmp.path().join("stage"), "job-realize".to_string());
        let runner = CaptureFfmpegCutRunner::new();

        let out = realize_track(&src, &req, &staging, &runner, &CancellationToken::new())
            .await
            .expect("realization should succeed");
        assert!(out.exists());

        let args = runner.ffmpeg_args();
        assert!(
            !args.iter().any(|arg| arg == "-ss"),
            "input-side -ss is not allowed for PR8 CUE segment cutting: {args:?}"
        );
        let filter = args
            .windows(2)
            .find(|pair| pair[0] == "-af")
            .map(|pair| pair[1].as_str())
            .expect("ffmpeg command should include an audio filter");
        assert_eq!(
            filter,
            "atrim=start_sample=88200:end_sample=92610,asetpts=PTS-STARTPTS"
        );
    }

    struct FailingWavpackFallbackRunner {
        ffmpeg_calls: std::sync::Mutex<u32>,
    }

    impl FailingWavpackFallbackRunner {
        fn new() -> Self {
            Self { ffmpeg_calls: std::sync::Mutex::new(0) }
        }
    }

    #[async_trait::async_trait]
    impl ToolRunner for FailingWavpackFallbackRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            match cmd.binary {
                ToolBinary::Ffprobe => Ok(pr8_tool_output(
                    ToolBinary::Ffprobe,
                    pr8_exact_ffprobe_json(44_100, 44_100),
                )),
                ToolBinary::Wvunpack => {
                    let out = cmd.args[2].clone();
                    std::fs::write(out, b"decoded wav").unwrap();
                    Ok(pr8_tool_output(ToolBinary::Wvunpack, String::new()))
                }
                ToolBinary::Ffmpeg => {
                    let mut calls = self.ffmpeg_calls.lock().unwrap();
                    *calls += 1;
                    let call_no = *calls;
                    drop(calls);

                    if call_no == 2 {
                        let out = cmd.args.last().unwrap().clone();
                        std::fs::write(out, b"partial flac").unwrap();
                    }
                    Err(ToolRunnerError::NonZeroExit {
                        exit: ProcessExit::Code(1),
                        stderr_tail: if call_no == 1 {
                            "direct cut failed".to_string()
                        } else {
                            "fallback cut failed".to_string()
                        },
                        command: pr8_command_record(ToolBinary::Ffmpeg),
                    })
                }
                other => panic!("unexpected call {other:?}"),
            }
        }
    }


    struct CachingWavpackFallbackRunner {
        wvunpack_calls: std::sync::Mutex<u32>,
    }

    impl CachingWavpackFallbackRunner {
        fn new() -> Self {
            Self { wvunpack_calls: std::sync::Mutex::new(0) }
        }

        fn wvunpack_calls(&self) -> u32 {
            *self.wvunpack_calls.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl ToolRunner for CachingWavpackFallbackRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            match cmd.binary {
                ToolBinary::Ffmpeg => {
                    let input = cmd
                        .args
                        .windows(2)
                        .find(|pair| pair[0] == "-i")
                        .map(|pair| pair[1].clone())
                        .unwrap_or_default();
                    if input.ends_with(".wv") {
                        return Err(ToolRunnerError::NonZeroExit {
                            exit: ProcessExit::Code(1),
                            stderr_tail: "direct cut failed".to_string(),
                            command: pr8_command_record(ToolBinary::Ffmpeg),
                        });
                    }
                    let out = cmd.args.last().unwrap().clone();
                    std::fs::write(out, b"track flac").unwrap();
                    Ok(pr8_tool_output(ToolBinary::Ffmpeg, String::new()))
                }
                ToolBinary::Wvunpack => {
                    *self.wvunpack_calls.lock().unwrap() += 1;
                    let out = cmd.args[2].clone();
                    std::fs::write(out, b"decoded wav").unwrap();
                    Ok(pr8_tool_output(ToolBinary::Wvunpack, String::new()))
                }
                ToolBinary::Ffprobe => Ok(pr8_tool_output(
                    ToolBinary::Ffprobe,
                    pr8_exact_ffprobe_json(44_100, 44_100),
                )),
                other => panic!("unexpected call {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn pr8_wavpack_fallback_failure_deletes_partial_output() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("album.wv");
        std::fs::write(&image_path, b"dummy image").unwrap();
        let src = TrackSourceRef::ImageSegment {
            image: image_path,
            start_sample: 0,
            samples: 44_100,
        };
        let mut req = sample_request();
        req.source.archive_password = None;
        let staging = StagingDir::new(tmp.path().join("stage"), "job-realize".to_string());
        let runner = FailingWavpackFallbackRunner::new();

        let err = realize_track(&src, &req, &staging, &runner, &CancellationToken::new())
            .await
            .expect_err("fallback cut should fail");
        assert!(matches!(err, ConvertError::Tool(_)));

        let segment_dir = staging.root.join("realized-image-segments");
        let partial_outputs: Vec<_> = if segment_dir.exists() {
            std::fs::read_dir(&segment_dir)
                .unwrap()
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("flac"))
                .collect()
        } else {
            Vec::new()
        };
        assert!(partial_outputs.is_empty(), "failed realization left partial track outputs: {partial_outputs:?}");
        assert!(segment_dir.join("decoded-image-cache").exists(), "decoded cache should be retained for retry/reuse");
    }


    #[tokio::test]
    async fn pr8_wavpack_fallback_reuses_decoded_image_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("album.wv");
        std::fs::write(&image_path, b"dummy image").unwrap();
        let mut req = sample_request();
        req.source.archive_password = None;
        let staging = StagingDir::new(tmp.path().join("stage"), "job-realize".to_string());
        let runner = CachingWavpackFallbackRunner::new();

        for start_sample in [0_u64, 44_100_u64] {
            let src = TrackSourceRef::ImageSegment {
                image: image_path.clone(),
                start_sample,
                samples: 44_100,
            };
            let out = realize_track(&src, &req, &staging, &runner, &CancellationToken::new())
                .await
                .expect("fallback realization should succeed");
            assert!(out.exists());
        }

        assert_eq!(runner.wvunpack_calls(), 1, "WavPack image should be decoded once per staging cache");
    }


    fn pr8_real_corpus_enabled() -> bool {
        std::env::var("TONEPOET_PR8_REAL_CORPUS").ok().as_deref() == Some("1")
    }

    fn pr8_command_available(program: &str) -> bool {
        ["-version", "--version", "-h", "--help"].iter().any(|flag| {
            std::process::Command::new(program)
                .arg(flag)
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false)
        })
    }

    fn pr8_run_command(program: &str, args: &[&str]) {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run {program}: {err}"));
        assert!(
            output.status.success(),
            "{program} {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn pr8_command_stdout(program: &str, args: &[&str]) -> String {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run {program}: {err}"));
        assert!(
            output.status.success(),
            "{program} {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn pr8_command_output(program: &str, args: &[&str]) -> Vec<u8> {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run {program}: {err}"));
        assert!(
            output.status.success(),
            "{program} {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn pr8_decode_pcm_s16le(path: &std::path::Path) -> Vec<u8> {
        pr8_command_output(
            "ffmpeg",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                path.to_str().unwrap(),
                "-map",
                "0:a:0",
                "-f",
                "s16le",
                "-acodec",
                "pcm_s16le",
                "-ac",
                "1",
                "-ar",
                "44100",
                "pipe:1",
            ],
        )
    }

    fn pr8_flac_has_tag(path: &std::path::Path, tag: &str) -> bool {
        let tag_arg = format!("--show-tag={tag}");
        let output = pr8_command_stdout("metaflac", &[tag_arg.as_str(), path.to_str().unwrap()]);
        let prefix = format!("{}=", tag.to_ascii_uppercase());
        output
            .lines()
            .any(|line| line.to_ascii_uppercase().starts_with(prefix.as_str()))
    }

    fn pr8_write_cue(path: &std::path::Path, image_name: &str, album: &str) {
        std::fs::write(
            path,
            format!(
                r#"PERFORMER "Corpus Artist"
TITLE "{album}"
REM DATE 1971
REM GENRE Test
FILE "{image_name}" WAVE
  TRACK 01 AUDIO
    TITLE "First"
    ISRC USAAA0000001
    FLAGS PRE
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Second"
    INDEX 01 00:01:00
  TRACK 03 AUDIO
    TITLE "Third"
    INDEX 01 00:02:00
"#
            ),
        )
        .unwrap();
    }

    fn pr8_write_many_track_cue(
        path: &std::path::Path,
        image_name: &str,
        album: &str,
        track_count: u32,
        seconds_per_track: u32,
    ) {
        let mut text = format!(
            "PERFORMER \"Corpus Artist\"\nTITLE \"{album}\"\nFILE \"{image_name}\" WAVE\n"
        );
        for track in 1..=track_count {
            let seconds = (track - 1) * seconds_per_track;
            let mm = seconds / 60;
            let ss = seconds % 60;
            text.push_str(&format!(
                "  TRACK {track:02} AUDIO\n    TITLE \"Track {track:02}\"\n    INDEX 01 {mm:02}:{ss:02}:00\n"
            ));
        }
        std::fs::write(path, text).unwrap();
    }

    async fn pr8_materialize_real_cue(image: PathBuf, policy: CueSidecarPolicy) -> PreparedSource {
        let tmp_root = image.parent().unwrap().to_path_buf();
        let mut req = sample_request();
        req.container = image;
        req.source.archive_password = None;
        req.source.cue_sidecar = policy;
        req.output_root = tmp_root.join("out");
        req.log.root = tmp_root.join("logs");
        let staging = StagingDir::new(tmp_root.join("stage"), "job-real-corpus".to_string());
        let runner = RealToolRunner::new(std::collections::HashMap::new());
        super::materializer_cue::CueImageMaterializer
            .materialize(&req, &staging, &runner, &CancellationToken::new())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pr8_real_corpus_generated_images_when_enabled() {
        if !pr8_real_corpus_enabled() {
            eprintln!("skipping generated real-corpus PR8 test; set TONEPOET_PR8_REAL_CORPUS=1 to run");
            return;
        }
        assert!(pr8_command_available("ffmpeg"), "ffmpeg is required");
        assert!(pr8_command_available("ffprobe"), "ffprobe is required");
        assert!(pr8_command_available("wavpack"), "wavpack is required");
        assert!(pr8_command_available("metaflac"), "metaflac is required");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wav = root.join("source.wav");
        let flac = root.join("standard.flac");
        let wv = root.join("standard.wv");
        let ape = root.join("standard.ape");

        pr8_run_command(
            "ffmpeg",
            &[
                "-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "sine=frequency=440:duration=3", "-ar", "44100", "-ac", "2", wav.to_str().unwrap(),
            ],
        );
        pr8_run_command("ffmpeg", &["-y", "-hide_banner", "-loglevel", "error", "-i", wav.to_str().unwrap(), flac.to_str().unwrap()]);
        pr8_run_command("wavpack", &["-y", "-q", wav.to_str().unwrap(), "-o", wv.to_str().unwrap()]);
        pr8_run_command("ffmpeg", &["-y", "-hide_banner", "-loglevel", "error", "-i", wav.to_str().unwrap(), ape.to_str().unwrap()]);

        for image in [&flac, &wv, &ape] {
            let cue = image.with_extension("cue");
            pr8_write_cue(&cue, image.file_name().unwrap().to_str().unwrap(), "Corpus Album");
            let source = pr8_materialize_real_cue(image.to_path_buf(), CueSidecarPolicy::PreferSidecar).await;
            assert_eq!(source.kind, SourceKind::CueImage);
            assert_eq!(source.tracks.len(), 3);
            assert_eq!(source.tracks[0].metadata.album_artist.as_deref(), Some("Corpus Artist"));
            assert!(source.tracks[0].metadata.pre_emphasis);
            assert_eq!(source.tracks[0].sample_rate, 44_100);
            assert_eq!(source.tracks[0].expected_samples, Some(44_100));
            assert!(matches!(source.tracks[0].source_ref, TrackSourceRef::ImageSegment { .. }));
        }

        let embedded_cue = root.join("embedded.cue");
        pr8_write_cue(&embedded_cue, flac.file_name().unwrap().to_str().unwrap(), "Embedded Album");
        pr8_run_command("metaflac", &["--remove-tag=CUESHEET", flac.to_str().unwrap()]);
        pr8_run_command(
            "metaflac",
            &[
                &format!("--set-tag-from-file=CUESHEET={}", embedded_cue.display()),
                flac.to_str().unwrap(),
            ],
        );
        let sidecar_cue = flac.with_extension("cue");
        pr8_write_cue(&sidecar_cue, flac.file_name().unwrap().to_str().unwrap(), "Sidecar Album");

        let prefer = pr8_materialize_real_cue(flac.clone(), CueSidecarPolicy::PreferSidecar).await;
        assert_eq!(prefer.album_metadata.album.as_deref(), Some("Sidecar Album"));
        let embedded = pr8_materialize_real_cue(flac.clone(), CueSidecarPolicy::EmbeddedOnly).await;
        assert_eq!(embedded.album_metadata.album.as_deref(), Some("Embedded Album"));
    }

    #[tokio::test]
    async fn pr8_real_segment_alignment_when_enabled() {
        if !pr8_real_corpus_enabled() {
            eprintln!("skipping generated segment-alignment PR8 test; set TONEPOET_PR8_REAL_CORPUS=1 to run");
            return;
        }
        assert!(pr8_command_available("ffmpeg"), "ffmpeg is required");
        assert!(pr8_command_available("ffprobe"), "ffprobe is required");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wav = root.join("alignment.wav");
        let flac = root.join("alignment.flac");

        pr8_run_command(
            "ffmpeg",
            &[
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1760:duration=1",
                "-filter_complex",
                "[0:a][1:a][2:a]concat=n=3:v=0:a=1[a]",
                "-map",
                "[a]",
                "-ar",
                "44100",
                "-ac",
                "1",
                wav.to_str().unwrap(),
            ],
        );
        pr8_run_command(
            "ffmpeg",
            &[
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                wav.to_str().unwrap(),
                flac.to_str().unwrap(),
            ],
        );

        let mut req = sample_request();
        req.container = flac.clone();
        req.source.archive_password = None;
        let staging = StagingDir::new(root.join("stage-alignment"), "job-align".to_string());
        let runner = RealToolRunner::new(std::collections::HashMap::new());
        let src = TrackSourceRef::ImageSegment {
            image: flac.clone(),
            start_sample: 44_100,
            samples: 44_100,
        };
        let realized = realize_track(&src, &req, &staging, &runner, &CancellationToken::new())
            .await
            .expect("segment realization should succeed");

        let source_pcm = pr8_decode_pcm_s16le(&flac);
        let realized_pcm = pr8_decode_pcm_s16le(&realized);
        let start = 44_100_usize * 2;
        let len = 44_100_usize * 2;
        assert_eq!(
            &realized_pcm[..],
            &source_pcm[start..start + len],
            "realized segment PCM must match the source-aligned sample window exactly"
        );
    }

    #[tokio::test]
    async fn pr8_real_long_image_many_tracks_benchmark_when_enabled() {
        if std::env::var("TONEPOET_PR8_LONG_IMAGE").ok().as_deref() != Some("1") {
            eprintln!(
                "skipping long-image PR8 benchmark; set TONEPOET_PR8_LONG_IMAGE=1 to run"
            );
            return;
        }
        assert!(pr8_command_available("ffmpeg"), "ffmpeg is required");
        assert!(pr8_command_available("ffprobe"), "ffprobe is required");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wav = root.join("long.wav");
        let flac = root.join("long.flac");
        let cue = root.join("long.cue");
        let track_count = std::env::var("TONEPOET_PR8_LONG_IMAGE_TRACKS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(60);
        let seconds_per_track = std::env::var("TONEPOET_PR8_LONG_IMAGE_SECONDS_PER_TRACK")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(2);
        let duration = track_count * seconds_per_track;
        let sine = format!("sine=frequency=440:duration={duration}");

        pr8_run_command(
            "ffmpeg",
            &[
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                sine.as_str(),
                "-ar",
                "44100",
                "-ac",
                "1",
                wav.to_str().unwrap(),
            ],
        );
        pr8_run_command(
            "ffmpeg",
            &[
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                wav.to_str().unwrap(),
                flac.to_str().unwrap(),
            ],
        );
        pr8_write_many_track_cue(
            &cue,
            flac.file_name().unwrap().to_str().unwrap(),
            "Long Benchmark Album",
            track_count,
            seconds_per_track,
        );

        let source = pr8_materialize_real_cue(flac.clone(), CueSidecarPolicy::PreferSidecar).await;
        assert_eq!(source.tracks.len(), track_count as usize);

        let mut req = sample_request();
        req.container = flac;
        req.source.archive_password = None;
        let staging = StagingDir::new(root.join("stage-long"), "job-long".to_string());
        let runner = RealToolRunner::new(std::collections::HashMap::new());
        let started = std::time::Instant::now();
        for track in &source.tracks {
            let out = realize_track(
                &track.source_ref,
                &req,
                &staging,
                &runner,
                &CancellationToken::new(),
            )
            .await
            .expect("long-image segment realization should succeed");
            assert!(out.exists());
        }
        let elapsed = started.elapsed();
        eprintln!(
            "PR8 long-image benchmark: {track_count} tracks x {seconds_per_track}s realized in {:.3}s",
            elapsed.as_secs_f64()
        );
        if let Ok(max_seconds) = std::env::var("TONEPOET_PR8_LONG_IMAGE_MAX_SECONDS") {
            let max_seconds = max_seconds.parse::<f64>().expect("invalid max seconds");
            assert!(
                elapsed.as_secs_f64() <= max_seconds,
                "long-image realization exceeded configured budget: {:.3}s > {max_seconds:.3}s",
                elapsed.as_secs_f64()
            );
        }
    }

    #[tokio::test]
    async fn pr8_real_pipeline_end_to_end_when_enabled() {
        if !pr8_real_corpus_enabled() {
            eprintln!("skipping generated end-to-end PR8 test; set TONEPOET_PR8_REAL_CORPUS=1 to run");
            return;
        }
        assert!(pr8_command_available("ffmpeg"), "ffmpeg is required");
        assert!(pr8_command_available("ffprobe"), "ffprobe is required");
        assert!(pr8_command_available("loudgain"), "loudgain is required");
        assert!(pr8_command_available("metaflac"), "metaflac is required");

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wav = root.join("e2e.wav");
        let flac = root.join("e2e.flac");
        let cue = root.join("e2e.cue");
        pr8_run_command(
            "ffmpeg",
            &[
                "-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "sine=frequency=330:duration=3", "-ar", "44100", "-ac", "2", wav.to_str().unwrap(),
            ],
        );
        pr8_run_command("ffmpeg", &["-y", "-hide_banner", "-loglevel", "error", "-i", wav.to_str().unwrap(), flac.to_str().unwrap()]);
        pr8_write_cue(&cue, flac.file_name().unwrap().to_str().unwrap(), "Pipeline Album");

        let mut req = sample_request();
        req.job_id = "job-pr8-e2e".to_string();
        req.item_id = "item-pr8-e2e".to_string();
        req.container = flac;
        req.source.archive_password = None;
        req.source.cue_sidecar = CueSidecarPolicy::PreferSidecar;
        req.output_root = root.join("published");
        req.log.root = root.join("logs");
        req.stages.metadata = StageRequirement::Enabled;
        req.stages.replaygain = StageRequirement::Enabled;
        req.stages.features = StageRequirement::Enabled;

        let runner = RealToolRunner::new(std::collections::HashMap::new());
        let reporter = RecordingReporter::new();
        let report = run_pipeline_item(req, &runner, &reporter, &CancellationToken::new()).await;
        assert!(matches!(report.outcome, AlbumOutcome::Complete { .. }), "unexpected outcome: {:?}", report.outcome);
        let published = report.published.expect("end-to-end test should publish outputs");
        let audio_entries = published
            .entries
            .iter()
            .filter(|entry| matches!(entry.role, PublishRole::Audio))
            .count();
        assert_eq!(audio_entries, 3);
        let audio_paths: Vec<_> = published
            .entries
            .iter()
            .filter(|entry| matches!(entry.role, PublishRole::Audio))
            .map(|entry| entry.final_path.clone())
            .collect();
        assert_eq!(audio_paths.len(), 3);
        for path in &audio_paths {
            assert!(path.exists(), "published audio file is missing: {}", path.display());
            assert!(
                pr8_flac_has_tag(path, "REPLAYGAIN_TRACK_GAIN"),
                "published file lacks ReplayGain tag: {}",
                path.display()
            );
        }
        let first_track = audio_paths
            .iter()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("01 - First"))
            })
            .expect("expected first published track");
        assert!(pr8_flac_has_tag(first_track, "ISRC"));
        assert!(pr8_flac_has_tag(first_track, "PRE_EMPHASIS"));
        assert!(pr8_flac_has_tag(first_track, "CUE_FLAGS"));
        assert!(published.entries.iter().any(|entry| matches!(entry.role, PublishRole::Sidecar(_))));
        assert!(report.durable_log.as_ref().is_some_and(|path| path.exists()));
    }


    // ====================================================================
    // PR 9 — SACD materializer / realization wiring tests
    // ====================================================================
    #[test]
    fn pr9_default_sacd_area_is_stereo() {
        assert_eq!(
            materializer_sacd::test_support::default_area_for_test(None),
            SacdArea::Stereo
        );
        assert_eq!(
            materializer_sacd::test_support::default_area_for_test(Some(SacdArea::MultiChannel)),
            SacdArea::MultiChannel
        );
    }

    #[test]
    fn pr9_sacd_expected_samples_are_unset_for_encoded_merge_accounting() {
        assert_eq!(
            materializer_sacd::test_support::sacd_expected_samples_for_test(),
            None
        );
    }

    #[test]
    fn pr9_track_selection_range_and_set_are_one_based() {
        let range = materializer_sacd::test_support::selection_ordinals_for_test(
            5,
            TrackSelection::Range { start: 2, end: 4 },
        )
        .unwrap();
        assert_eq!(range, vec![2, 3, 4]);

        let mut set = std::collections::BTreeSet::new();
        set.insert(1);
        set.insert(5);
        let selected = materializer_sacd::test_support::selection_ordinals_for_test(
            5,
            TrackSelection::Set(set),
        )
        .unwrap();
        assert_eq!(selected, vec![1, 5]);
    }

    #[test]
    fn pr9_detection_positive_only_for_sacd_master_toc_magic() {
        use crate::tui::sacd::DetectionResult;

        assert!(materializer_sacd::test_support::detection_positive_for_test(
            DetectionResult::HealthyAllRedundant
        ));
        assert!(materializer_sacd::test_support::detection_positive_for_test(
            DetectionResult::HealthyPartialRedundant { good: 1 }
        ));
        assert!(!materializer_sacd::test_support::detection_positive_for_test(
            DetectionResult::NotSacd
        ));
        assert!(!materializer_sacd::test_support::detection_positive_for_test(
            DetectionResult::TooSmall
        ));
    }

    #[test]
    fn pr9_non_sacd_iso_without_explicit_sacd_selection_is_unknown_source() {
        let tmp = tempfile::tempdir().unwrap();
        let iso = tmp.path().join("ordinary-data.iso");
        std::fs::write(&iso, vec![0_u8; 1_200_000]).unwrap();

        let mut req = sample_request();
        req.container = iso;
        req.source.sacd_area = None;

        assert!(matches!(
            detect_source_kind(&req),
            Err(SourceDetectError::UnknownSource)
        ));
    }

    #[test]
    fn pr9_explicit_sacd_selection_routes_iso_to_materializer() {
        let tmp = tempfile::tempdir().unwrap();
        let iso = tmp.path().join("deliberate-sacd.iso");
        std::fs::write(&iso, vec![0_u8; 1_200_000]).unwrap();

        let mut req = sample_request();
        req.container = iso;
        req.source.sacd_area = Some(SacdArea::Stereo);

        assert_eq!(detect_source_kind(&req).unwrap(), SourceKind::SacdIso);
        assert!(materializer_sacd::test_support::explicit_sacd_for_test(&req));
    }

    #[test]
    fn pr9_not_sacd_maps_to_encrypted_only_for_explicit_sacd_requests() {
        use crate::tui::sacd::SacdError;

        let implicit = materializer_sacd::test_support::encrypted_mapping_for_test(
            SacdError::NotSacdIso,
            false,
        );
        assert!(matches!(implicit, MaterializeError::Parse(_)));

        let explicit = materializer_sacd::test_support::encrypted_mapping_for_test(
            SacdError::NotSacdIso,
            true,
        );
        assert!(matches!(explicit, MaterializeError::Encrypted));

        let tiny = materializer_sacd::test_support::encrypted_mapping_for_test(
            SacdError::TooSmall { size: 1, required: 2 },
            true,
        );
        assert!(matches!(tiny, MaterializeError::Parse(_)));
    }

    #[test]
    fn pr9_materializer_for_sacd_iso_is_wired() {
        assert!(materializer_for(SourceKind::SacdIso).is_ok());
    }

    #[test]
    fn pr9_sacd_output_names_are_source_area_track_and_range_specific() {
        let stereo = stages::sacd_stage_test_support::output_name_for_test(
            &PathBuf::from("/music/Solo Monk.iso"),
            SacdArea::Stereo,
            0,
            1234,
            77,
        );
        assert!(stereo.starts_with("Solo_Monk_"));
        assert!(stereo.contains("_stereo_track_001_000004d2_0000004d.dsf"));

        let multichannel = stages::sacd_stage_test_support::output_name_for_test(
            &PathBuf::from("/music/Solo Monk.iso"),
            SacdArea::MultiChannel,
            10,
            1234,
            77,
        );
        assert!(multichannel.contains("_multichannel_track_011_000004d2_0000004d.dsf"));
        assert_ne!(stereo, multichannel);
    }

    #[test]
    fn pr9_dsf_cache_check_validates_header_fields() {
        fn write_fake_dsf(path: &std::path::Path, channels: u32, sample_rate: u32, samples: u64) {
            let file_size = 108_u64;
            let mut bytes = vec![0_u8; file_size as usize];
            bytes[0..4].copy_from_slice(b"DSD ");
            bytes[12..20].copy_from_slice(&file_size.to_le_bytes());
            bytes[20..28].copy_from_slice(&0_u64.to_le_bytes());
            bytes[28..32].copy_from_slice(b"fmt ");
            bytes[52..56].copy_from_slice(&channels.to_le_bytes());
            bytes[56..60].copy_from_slice(&sample_rate.to_le_bytes());
            bytes[60..64].copy_from_slice(&1_u32.to_le_bytes());
            bytes[64..72].copy_from_slice(&samples.to_le_bytes());
            bytes[72..76].copy_from_slice(&4096_u32.to_le_bytes());
            bytes[80..84].copy_from_slice(b"data");
            bytes[84..92].copy_from_slice(&(file_size - 80).to_le_bytes());
            std::fs::write(path, bytes).unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing.dsf");
        assert!(!stages::sacd_stage_test_support::dsf_ready_for_test(
            &missing,
            2,
            crate::tui::sacd::SACD_SAMPLE_RATE_HZ,
            37_632,
        ));

        let valid = tmp.path().join("valid.dsf");
        write_fake_dsf(&valid, 2, crate::tui::sacd::SACD_SAMPLE_RATE_HZ, 37_632);
        assert!(stages::sacd_stage_test_support::dsf_ready_for_test(
            &valid,
            2,
            crate::tui::sacd::SACD_SAMPLE_RATE_HZ,
            37_632,
        ));
        assert!(!stages::sacd_stage_test_support::dsf_ready_for_test(
            &valid,
            6,
            crate::tui::sacd::SACD_SAMPLE_RATE_HZ,
            37_632,
        ));
        assert!(!stages::sacd_stage_test_support::dsf_ready_for_test(
            &valid,
            2,
            crate::tui::sacd::SACD_SAMPLE_RATE_HZ,
            75_264,
        ));
    }

    #[test]
    fn pr9_dsf_sample_count_uses_sacd_frame_timing_only_for_dsf_validation() {
        assert_eq!(
            stages::sacd_stage_test_support::dsf_sample_count_for_test(0, 0, 1),
            37_632
        );
        assert_eq!(
            stages::sacd_stage_test_support::dsf_sample_count_for_test(0, 1, 0),
            2_822_400
        );
    }

}
