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

#![deny(unsafe_code)]

pub(crate) mod dvda_channel_layout;
pub(crate) mod dvda_demux;
pub(crate) mod dvda_lpcm;
pub(crate) mod dvda_mlp;
pub(crate) mod dvda_mlp_native;
pub(crate) mod dvda_realize;
pub(crate) mod dvdv_realize;
pub(crate) mod bluray_pts;
pub(crate) mod bluray_ts_demux;
pub(crate) mod bluray_lpcm;
pub(crate) mod bluray_wav_validate;
pub(crate) mod bluray_realize;
pub mod errors;
pub mod label_resolver;
pub mod manifest;
pub mod manifest_builder;
pub mod materializer_archive;
pub mod materializer_cue;
pub mod materializer_dvda;
pub mod materializer_dvdv;
pub mod materializer_bluray;
pub mod materializer_sacd;
pub mod materializer_single;
pub mod orchestrator_rerun_gate;
pub mod plan_bridge;
pub mod planned_adapter;
pub mod progress;
pub mod reporter;
pub mod rerun;
pub mod scheduler;
pub mod source_heuristics;
pub mod stages;
pub mod tool;
pub mod track_executor;
pub mod transactional_state;
pub mod types;
pub mod unified_request;

pub use errors::*;
pub use label_resolver::*;
pub use materializer_single::*;
pub use plan_bridge::*;
pub use planned_adapter::*;
pub use progress::*;
pub use reporter::*;
pub use scheduler::*;
pub use stages::*;
pub use tool::*;
pub use track_executor::*;
pub use types::*;
pub use unified_request::*;

// Re-export DVD-Audio probe items for dvda-info CLI subcommand.
pub use dvda_demux::{
    parse_private_stream_1_packets, parse_private_stream_1_packets_with_mode, DvdaSubHeaderMode,
    DvdaSubstreamKind,
};
pub use dvda_mlp::{probe_mlp_major_sync, MlpMajorSyncInfo};

#[cfg(test)]
mod tests {
    use super::*;
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
                dvda_group: None,
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                dvdv_vts: None,
                dvdv_title: None,
                dvdv_audio_stream: None,
                dvdv_angle: None,
                bluray_playlist: None,
                bluray_audio_pid: None,
                bluray_audio_stream: None,
                bluray_angle: None,
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
                write_manifest: false,
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
            album_batch: None,
            album_batch_track: None,
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            suppress_incremental_conversion_log_append: false,
            expected_album_track_count: None,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
            companion: CompanionCopyPolicy::default(),
        }
    }

    fn track_record(ordinal: u32, ok: bool) -> TrackRecord {
        TrackRecord {
            track_id: TrackId {
                source_ordinal: ordinal,
                disc_number: None,
                track_number: ordinal,
            },
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
            dsd_dst_stats: None,
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
        assert_eq!(
            redacted.source.archive_password.as_deref(),
            Some("<redacted>")
        );
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
        assert!(matches!(
            out,
            AlbumOutcome::Blocked {
                reason: BlockReason::TrackFailures,
                ..
            }
        ));
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
