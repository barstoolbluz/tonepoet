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
pub mod label_resolver;
pub mod materializer_7z;
pub mod materializer_cue;
pub mod materializer_sacd;
pub mod materializer_single;
pub mod planned_adapter;
pub mod plan_bridge;
pub mod scheduler;
pub mod track_executor;
pub mod unified_request;
pub mod manifest;
pub mod manifest_builder;
pub mod orchestrator_rerun_gate;
pub mod rerun;
pub mod transactional_state;
pub mod progress;
pub mod reporter;
pub mod stages;
pub mod tool;
pub mod types;

pub use errors::*;
pub use label_resolver::*;
pub use materializer_single::*;
pub use planned_adapter::*;
pub use plan_bridge::*;
pub use scheduler::*;
pub use track_executor::*;
pub use unified_request::*;
pub use progress::*;
pub use reporter::*;
pub use stages::*;
pub use tool::*;
pub use types::*;


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn sample_request() -> PipelineRequest {
        PipelineRequest {
            job_id: "job-1".into(),
            item_id: "item-1".into(),
            container: PathBuf::from("/tmp/in.flac"),
            source: SourceOptions {
                archive_password: Some(SecretString::new("hunter2")),
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::IgnoreCue,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: Some(2),
            merge: false,
            output_root: PathBuf::from("/tmp/out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".into(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
            },
            log: LogPolicy {
                root: PathBuf::from("/tmp/logs"),
                write_for_blocked: true,
                write_json_log: false,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        }
    }

    fn track_record(ordinal: u32, ok: bool) -> TrackRecord {
        TrackRecord {
            track_id: TrackId {
                source_ordinal: ordinal,
                disc_number: None,
                track_number: ordinal,
            },
            outcome: if ok { TrackOutcome::Ok } else { TrackOutcome::Err("encode failed".into()) },
            source_ref: TrackSourceRef::StagedFile(PathBuf::from(format!("/s/{ordinal}.flac"))),
            realized_input: None,
            output_file: None,
            commands: Vec::new(),
            bytes_in: None,
            bytes_out: None,
            duration: None,
        }
    }

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
        assert!(!json.contains("hunter2"));
    }

    #[test]
    fn aggregate_track_failure_default_policy_is_blocked() {
        let out = aggregate_album_outcome(
            vec![track_record(1, true), track_record(2, false)],
            vec![],
            FailurePolicy::FailAlbumOnAnyTrackFailure,
        );
        assert!(matches!(out, AlbumOutcome::Blocked { reason: BlockReason::TrackFailures, .. }));
    }

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
        assert_eq!(transcript[0].sanitized_args[1], "<redacted>");
        let json = serde_json::to_string(&transcript[0]).unwrap();
        assert!(!json.contains("hunter2"));
        assert!(!json.contains("s3cr3t"));
    }
}
