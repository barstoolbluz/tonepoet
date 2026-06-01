//! Contract tests for Chunk 2 orchestrator unification.
//!
//! These tests intentionally check architectural invariants in the checked-in
//! source, so they can run before tool-dependent audio fixtures exist. They do
//! not replace end-to-end conversion tests, but they prevent regressions that
//! would split the unified path, bypass ToolRunner, or fake metadata ownership.

#[test]
fn single_files_enter_the_shared_pool_as_immediate_work_units() {
    let processor = include_str!("../src/convert/processor.rs");
    assert!(processor.contains("WorkKind::SingleFile"));
    assert!(processor.contains("build_single_file_work("));
    assert!(processor.contains("run_pipeline_item_with_tool_paths"));
    assert!(processor.contains("run_single_item_with_shared_scheduler"));
    assert!(processor.contains("run_queue_with_shared_orchestrator("));

    let single_branch = processor
        .find("Some(SourceKind::SingleFile)")
        .expect("processor must branch on SourceKind::SingleFile");
    let branch_tail = &processor[single_branch..processor.len().min(single_branch + 500)];
    assert!(branch_tail.contains("build_single_file_work"));
    assert!(!branch_tail.contains("prepare_pipeline_item_for_scheduler"));
}

#[test]
fn direct_process_item_uses_the_same_shared_scheduler_graph_as_queue_processing() {
    let processor = include_str!("../src/convert/processor.rs");
    let process_item_pos = processor
        .find(concat!("pub async ", "f", "n process_item("))
        .expect("process_item exists");
    let process_item_tail = &processor[process_item_pos..];
    assert!(process_item_tail.contains("run_single_item_with_shared_scheduler"));
    assert!(processor.contains("run_queue_with_shared_orchestrator(
        vec![item]"));
    assert!(!process_item_tail.contains("run_pipeline_item_with_tool_paths("));
}

#[test]
fn normal_request_builder_requires_full_pipeline_settings_handoff() {
    let unified = include_str!("../src/convert/pipeline/unified_request.rs");
    let build_pos = unified
        .find(concat!("pub ", "f", "n build_pipeline_request(item"))
        .expect("build_pipeline_request exists");
    let build_end = unified[build_pos..]
        .find("/// Attach exact")
        .map(|offset| build_pos + offset)
        .expect("handoff helper follows builder");
    let build_body = &unified[build_pos..build_end];
    assert!(build_body.contains("prebuilt PipelineRequest with full PipelineSettings"));
    assert!(!build_body.contains("legacy_pipeline_settings_for_item"));
    assert!(unified.contains("attach_full_pipeline_settings"));
    assert!(unified.contains("build_pipeline_request_from_legacy_options"));
    assert!(include_str!("../src/convert/processor.rs").contains("process_item_with_pipeline_settings"));
}

#[test]
fn scheduler_failure_accounting_waits_for_all_terminal_track_records() {
    let processor = include_str!("../src/convert/processor.rs");
    let scheduler = include_str!("../src/convert/pipeline/scheduler.rs");
    assert!(processor.contains("cancel_requested"));
    assert!(processor.contains("pending.finished >= pending.expected"));
    assert!(processor.contains("pending.outputs.sort_by_key(|output| output.index)"));
    assert!(!processor.contains("pending_albums.remove(&job_id) {
                                    pending.job_cancel.cancel()"));
    assert!(scheduler.contains("failed_album_keeps_waiting_until_every_expected_track_has_terminal_accounting"));
    assert!(scheduler.contains("fail_fast_cancellation_still_collects_all_terminal_track_records"));
}

#[test]
fn scheduler_has_one_shared_work_graph_for_source_and_track_units() {
    let processor = include_str!("../src/convert/processor.rs");
    let scheduler = include_str!("../src/convert/pipeline/scheduler.rs");
    let stages = include_str!("../src/convert/pipeline/stages.rs");

    assert!(processor.contains("SharedWorkerPool::<QueueWorkOutput>::new"));
    assert!(!stages.contains("SharedWorkerPool::<"));
    assert!(!processor.contains("JoinSet"));

    for kind in [
        "SingleFile",
        "ArchiveExtract",
        "CueSplitTrack",
        "SacdExtractTrack",
        "EncodeTrack",
        "AlbumPostProcess",
    ] {
        assert!(processor.contains(kind), "processor missing work kind {kind}");
        assert!(scheduler.contains(kind), "scheduler missing work kind {kind}");
    }

    assert!(processor.contains("AlbumReadiness::Failed"));
    assert!(processor.contains("job_cancel.cancel()"));
}

#[test]
fn planner_metadata_disposition_is_consulted_after_topology_planning() {
    let bridge = include_str!("../src/convert/pipeline/plan_bridge.rs");
    let executor = include_str!("../src/convert/pipeline/track_executor.rs");
    let stages = include_str!("../src/convert/pipeline/stages.rs");

    assert!(bridge.contains("orchestrator_metadata_stage_required"));
    assert!(!bridge.contains("settings.metadata.transfer_tags = false"));
    assert!(executor.contains("plan_topology"));
    assert!(executor.contains("ToolRegistry::with_builtin_tools"));
    assert!(executor.contains("metadata_disposition_for_step"));
    assert!(executor.contains("PlanOperation::MetadataTransfer"));
    assert!(executor.contains("PlanOperation::StoreSourceAudioMd5"));
    assert!(stages.contains("orchestrator_metadata_stage_required"));
}

#[test]
fn legacy_compat_pipeline_settings_cover_the_legacy_option_surface_explicitly() {
    let unified = include_str!("../src/convert/pipeline/unified_request.rs");
    for token in [
        "target_format",
        "target_sample_rate",
        "target_bit_depth",
        "resample_quality",
        "nyquist_transition",
        "dither_type",
        "preferred_tool",
        "force_encode",
        "flac.verify",
        "mp3.mode",
        "mp3.bitrate_kbps",
        "mp3.vbr_quality",
        "aac.profile",
        "aac.bitrate_kbps",
        "opus.content_type",
        "opus.bitrate_kbps",
        "opus.complexity",
        "wavpack.mode",
        "wavpack.hybrid",
        "wavpack.hybrid_bitrate_kbps",
        "wavpack.correction_file",
        "ssrc.force",

        "ssrc.insane_mode",
        "ssrc.profile",
        "settings.dsd = DsdSettings::default()",
        "metadata.transfer_tags",
        "metadata.preserve_artwork",
        "metadata.store_source_audio_md5",
        "verification.verify_after_encode",
        "verification.prefer_native_flac_verify",
        "replay_gain.mode",
        "replay_gain.prevent_clipping",
    ] {
        assert!(unified.contains(token), "settings builder missing {token}");
    }
}

#[test]
fn every_external_process_boundary_runs_through_tool_runner_modules() {
    for (path, contents) in [
        ("processor.rs", include_str!("../src/convert/processor.rs")),
        ("stages.rs", include_str!("../src/convert/pipeline/stages.rs")),
        ("track_executor.rs", include_str!("../src/convert/pipeline/track_executor.rs")),
        ("planned_adapter.rs", include_str!("../src/convert/pipeline/planned_adapter.rs")),
    ] {
        assert!(!contents.contains("std::process::Command"), "{path} spawns directly");
        assert!(!contents.contains("tokio::process::Command"), "{path} spawns directly");
    }
}


#[test]
fn compatibility_orchestrator_metadata_gate_matches_scheduler_gate() {
    let stages = include_str!("../src/convert/pipeline/stages.rs");
    assert!(
        stages.matches("planner_metadata_already_satisfied(artifacts.as_ref().expect(\"artifacts present\"), &req)").count() >= 2,
        "both scheduler and compatibility orchestrator paths must honor planner metadata completion"
    );
}

#[test]
fn queue_items_carry_full_pipeline_settings_without_legacy_projection() {
    let formats = include_str!("../src/convert/formats.rs");
    let queue = include_str!("../src/convert/queue.rs");
    let unified = include_str!("../src/convert/pipeline/unified_request.rs");
    let processor = include_str!("../src/convert/processor.rs");

    assert!(formats.contains("pub pipeline_settings: Option<tonepoet_pipeline::PipelineSettings>"));
    assert!(formats.contains("pipeline_settings: None"));
    assert!(queue.contains("pub pipeline_settings: Option<tonepoet_pipeline::PipelineSettings>"));
    assert!(queue.contains("new_with_pipeline_settings"));
    assert!(queue.contains("set_pipeline_settings"));
    assert!(queue.contains("add_item_with_pipeline_settings"));
    assert!(unified.contains("item.pipeline_settings.clone()"));
    assert!(unified.contains("item.options.pipeline_settings.clone()"));
    assert!(unified.contains("item.options.pipeline_settings = Some(settings.clone())"));
    assert!(unified.contains("item.pipeline_settings = Some(settings.clone())"));
    assert!(processor.contains("item.options.pipeline_settings = Some(settings.clone())"));
    assert!(processor.contains("item.pipeline_settings = Some(settings.clone())"));
}

#[test]
fn pipeline_request_literals_include_worker_count() {
    for (path, contents) in [
        ("stages.rs", include_str!("../src/convert/pipeline/stages.rs")),
        ("mod.rs", include_str!("../src/convert/pipeline/mod.rs")),
        ("unified_request.rs", include_str!("../src/convert/pipeline/unified_request.rs")),
    ] {
        let mut cursor = 0;
        while let Some(offset) = contents[cursor..].find("PipelineRequest {") {
            let start = cursor + offset;
            let line_start = contents[..start].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
            let before = &contents[line_start..start];
            if before.contains("struct ") || before.contains("fn ") || before.contains("impl ") || before.contains("->") {
                cursor = start + "PipelineRequest {".len();
                continue;
            }
            let literal_tail = &contents[start..contents.len().min(start + 1400)];
            assert!(
                literal_tail.contains("worker_count:"),
                "{path} has a PipelineRequest literal missing worker_count near byte {start}"
            );
            cursor = start + "PipelineRequest {".len();
        }
    }
}
