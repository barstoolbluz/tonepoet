//! Behavioral contracts for the unified conversion orchestrator.
//!
//! This file deliberately contains no `include_str!`, source parsing, token
//! searches, or proximity assertions. Those mechanisms proved both brittle and
//! capable of passing while the named behavior was broken. Contracts that are
//! observable at the public boundary are exercised below.
//!
//! Purely architectural placement rules (for example, which source module owns
//! a subprocess boundary) are not externally observable behavior. They are
//! therefore intentionally not recreated here as text assertions; executable
//! ToolRunner and scheduler tests live beside the implementation that owns
//! those boundaries.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;
use tonepoet::convert::formats::{AudioFormat, ConversionOptions, FileFormat};
use tonepoet::convert::pipeline::{
    boxed_work, build_pipeline_request, AlbumCompletionTracker, AlbumReadiness,
    CueSidecarPolicy, SharedWorkerPool, TrackId, WorkKind, WorkUnit,
};
use tonepoet::convert::queue::{ConversionItem, ConversionQueue, ConversionStatus};
use tonepoet_pipeline::PipelineSettings;

fn input_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/tonepoet-chunk2-contract/{name}.flac"))
}

fn valid_settings() -> PipelineSettings {
    let mut settings = PipelineSettings::default();
    // Keep the sentinel valid while making accidental fallback to Default
    // observable at every handoff checked below.
    settings.force_encode = true;
    settings.flac.compression_level = 3;
    settings
}

fn item_with_settings(name: &str, settings: PipelineSettings) -> ConversionItem {
    ConversionItem::new_with_pipeline_settings(
        input_path(name),
        FileFormat::Audio(AudioFormat::Flac),
        ConversionOptions::default(),
        settings,
    )
}

#[test]
fn full_pipeline_settings_survive_item_to_request_without_legacy_projection() {
    let expected = valid_settings();
    let item = item_with_settings("settings", expected.clone());

    let request = build_pipeline_request(&item).expect("full settings build a request");

    assert_eq!(request.settings, expected);
}

#[test]
fn normal_request_builder_rejects_a_runnable_item_without_full_settings() {
    let item = ConversionItem::new(
        input_path("legacy-only"),
        FileFormat::Audio(AudioFormat::Flac),
        ConversionOptions::default(),
    );

    let error = build_pipeline_request(&item).expect_err("legacy-only item must not run");
    assert!(
        error.to_string().contains("missing full PipelineSettings"),
        "unexpected validation error: {error}"
    );
}

#[test]
fn prebuilt_request_is_the_executable_contract_and_preserves_worker_count() {
    let mut request = build_pipeline_request(&item_with_settings("prebuilt", valid_settings()))
        .expect("build seed request");
    request.worker_count = Some(7);
    request.settings.flac.compression_level = 2;
    let expected = request.clone();

    let item = ConversionItem::new_with_pipeline_request(
        input_path("prebuilt"),
        FileFormat::Audio(AudioFormat::Flac),
        ConversionOptions::default(),
        request,
        None,
    );
    let actual = build_pipeline_request(&item).expect("prebuilt request remains executable");

    assert_eq!(actual.worker_count, Some(7));
    assert_eq!(actual.settings, expected.settings);
    assert_eq!(actual.item_id, expected.item_id);
    assert_eq!(actual.job_id, expected.job_id);
}

#[test]
fn queue_time_cue_policy_override_is_applied_to_a_prebuilt_request() {
    let mut request = build_pipeline_request(&item_with_settings("cue-override", valid_settings()))
        .expect("build seed request");
    request.source.cue_sidecar = CueSidecarPolicy::IgnoreCue;

    let item = ConversionItem::new_with_pipeline_request(
        input_path("cue-override"),
        FileFormat::Audio(AudioFormat::Flac),
        ConversionOptions::default(),
        request,
        Some(CueSidecarPolicy::SidecarOnly),
    );

    let actual = build_pipeline_request(&item).expect("prebuilt request with queue override");
    assert_eq!(actual.source.cue_sidecar, CueSidecarPolicy::SidecarOnly);
}

#[test]
fn queue_validation_tracks_the_real_full_settings_handoff() {
    let mut queue = ConversionQueue::new();
    queue.add_item(
        input_path("queued"),
        FileFormat::Audio(AudioFormat::Flac),
        ConversionOptions::default(),
    );
    let item = queue
        .all_items_mut()
        .into_iter()
        .next()
        .expect("queued item exists");
    item.status = ConversionStatus::Queued;

    assert_eq!(queue.queued_items_missing_pipeline_settings().len(), 1);
    assert!(queue.validate_full_settings_handoff().is_err());

    queue
        .all_items_mut()
        .into_iter()
        .next()
        .expect("queued item remains")
        .set_pipeline_settings(valid_settings());

    assert!(queue.queued_items_missing_pipeline_settings().is_empty());
    queue
        .validate_full_settings_handoff()
        .expect("full settings make queue runnable");
}

#[test]
fn failed_album_accounting_continues_through_every_terminal_track_record() {
    let mut tracker = AlbumCompletionTracker::default();
    tracker.register_album("album".to_string(), 3, false);

    assert_eq!(
        tracker.mark_track_finished("album", false),
        AlbumReadiness::Failed {
            finished: 1,
            expected: 3,
            failed: 1,
        }
    );
    assert_eq!(
        tracker.mark_track_finished("album", true),
        AlbumReadiness::Failed {
            finished: 2,
            expected: 3,
            failed: 1,
        }
    );
    assert_eq!(
        tracker.mark_track_finished("album", false),
        AlbumReadiness::Failed {
            finished: 3,
            expected: 3,
            failed: 2,
        }
    );
}

#[test]
fn partial_album_mode_waits_for_all_tracks_before_postprocessing() {
    let mut tracker = AlbumCompletionTracker::default();
    tracker.register_album("album".to_string(), 2, true);

    assert_eq!(
        tracker.mark_track_finished("album", false),
        AlbumReadiness::Waiting {
            finished: 1,
            expected: 2,
        }
    );
    assert_eq!(
        tracker.mark_track_finished("album", true),
        AlbumReadiness::ReadyForPostProcess
    );
}

fn work_kind_track(index: u32) -> TrackId {
    TrackId {
        source_ordinal: index,
        disc_number: None,
        track_number: index + 1,
    }
}

#[tokio::test]
async fn one_shared_worker_pool_executes_source_track_and_album_work_kinds() {
    let cancel = CancellationToken::new();
    let pool = SharedWorkerPool::<usize>::new(Some(3), cancel);
    let mut run = pool.start();

    let expected = vec![
        WorkKind::SingleFile,
        WorkKind::ArchiveExtract,
        WorkKind::CueSplitTrack {
            track_id: work_kind_track(0),
        },
        WorkKind::SacdExtractTrack {
            track_id: work_kind_track(1),
        },
        WorkKind::EncodeTrack {
            track_id: work_kind_track(2),
        },
        WorkKind::AlbumPostProcess,
    ];

    for (index, kind) in expected.iter().cloned().enumerate() {
        pool.submit(WorkUnit {
            job_id: if index % 2 == 0 { "album-a" } else { "album-b" }.to_string(),
            unit_id: format!("unit-{index}"),
            kind,
            task: boxed_work(move |_cancel| async move { Ok(index) }),
        })
        .await;
    }

    let mut seen = Vec::new();
    while seen.len() < expected.len() {
        let result = run.results.recv().await.expect("scheduler result");
        assert_eq!(result.outcome.expect("work succeeds") < expected.len(), true);
        seen.push(result.kind);
    }
    run.shutdown().await;

    for kind in expected {
        assert!(seen.contains(&kind), "shared pool did not execute {kind:?}");
    }
}

#[tokio::test]
async fn shared_scheduler_reports_successes_and_failures_as_terminal_results() {
    let cancel = CancellationToken::new();
    let pool = SharedWorkerPool::<usize>::new(Some(2), cancel);
    let mut run = pool.start();

    pool.submit(WorkUnit {
        job_id: "ok".to_string(),
        unit_id: "ok-0".to_string(),
        kind: WorkKind::SingleFile,
        task: boxed_work(|_cancel| async move { Ok(1) }),
    })
    .await;
    pool.submit(WorkUnit {
        job_id: "fail".to_string(),
        unit_id: "fail-0".to_string(),
        kind: WorkKind::EncodeTrack {
            track_id: work_kind_track(0),
        },
        task: boxed_work(|_cancel| async move { Err("synthetic failure".to_string()) }),
    })
    .await;

    let first = run.results.recv().await.expect("first terminal result");
    let second = run.results.recv().await.expect("second terminal result");
    run.shutdown().await;

    let results = [first, second];
    assert!(results.iter().any(|result| result.outcome.as_ref() == Ok(&1)));
    assert!(results.iter().any(|result| {
        result
            .outcome
            .as_ref()
            .err()
            .is_some_and(|error| error == "synthetic failure")
    }));
}
