//! Opt-in CUE source-pass ReplayGain timing diagnostics.
//!
//! `TONEPOET_CUE_RG_PROFILE=1` AND the existing
//! `TONEPOET_PIPELINE_BASELINE_JSONL=<private-file>` are required. These
//! measurements are observational, never affect conversion decisions, and
//! never enter conversion/provenance logs. Timed I/O calls include runtime
//! scheduling and kernel time; they are NOT pure blocked-on-FIFO time.
//!
//! Source-pass observation and encoder writes overlap. Their durations are
//! deliberately not additive, nor are they equal to CPU utilization.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::json;

use super::baseline;

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("TONEPOET_CUE_RG_PROFILE")
            .as_deref() == Some(std::ffi::OsStr::new("1"))
            && baseline::enabled()
    })
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Expected source-pass participants for this exact CUE run. Emitted before
/// observer tasks launch, so a missing/crashed task cannot silently disappear
/// from the accepted evidence cohort.
pub(crate) fn emit_manifest(job_id: &str, track_ordinals: &[u32]) {
    baseline::cue_emit(job_id, "rg_diag_manifest", json!({
        "rg_diag_schema": "tonepoet.cue.rgdiag.v1",
        "track_count": track_ordinals.len(),
        "track_ordinals": track_ordinals,
    }));
}

#[derive(Debug, Clone)]
pub(crate) struct CueProfileKey {
    pub job_id: String,
    pub track_ordinal: u32,
}

#[derive(Debug, Default)]
pub(crate) struct TransferMetrics {
    pub chunks: u64,
    pub bytes: u64,
    pub decoder_read_ns: u64,
    pub decoder_read_max_ns: u64,
    pub encoder_write_ns: u64,
    pub encoder_write_max_ns: u64,
    pub mirror_write_ns: u64,
    pub mirror_write_max_ns: u64,
    pub fifo_open_ns: u64,
    pub header_write_ns: u64,
}

#[derive(Clone, Copy)]
pub(crate) enum TransferStage { DecoderRead, EncoderWrite, MirrorWrite, FifoOpen, HeaderWrite }

impl TransferMetrics {
    pub fn add(&mut self, stage: TransferStage, duration: Duration) {
        let elapsed = nanos(duration);
        let (total, maximum): (&mut u64, Option<&mut u64>) = match stage {
            TransferStage::DecoderRead => (&mut self.decoder_read_ns, Some(&mut self.decoder_read_max_ns)),
            TransferStage::EncoderWrite => (&mut self.encoder_write_ns, Some(&mut self.encoder_write_max_ns)),
            TransferStage::MirrorWrite => (&mut self.mirror_write_ns, Some(&mut self.mirror_write_max_ns)),
            TransferStage::FifoOpen => (&mut self.fifo_open_ns, None),
            TransferStage::HeaderWrite => (&mut self.header_write_ns, None),
        };
        *total = total.saturating_add(elapsed);
        if let Some(maximum) = maximum {
            *maximum = (*maximum).max(elapsed);
        }
    }
}

/// Emitted on Drop, including an open/prefix/transfer error or task cancellation.
/// Absence of a completed flag is never silently interpreted as success.
pub(crate) struct TransferGuard {
    pub metrics: TransferMetrics,
    key: CueProfileKey,
    start: Instant,
    complete: bool,
}

impl TransferGuard {
    pub fn new(key: CueProfileKey) -> Self {
        Self { metrics: TransferMetrics::default(), key, start: Instant::now(), complete: false }
    }

    pub fn mark_complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for TransferGuard {
    fn drop(&mut self) {
        let m = &self.metrics;
        baseline::cue_emit(&self.key.job_id, "rg_diag_transport", json!({
            "rg_diag_schema": "tonepoet.cue.rgdiag.v1",
            "track_ordinal": self.key.track_ordinal,
            "complete": self.complete,
            "elapsed_wall_ns": nanos(self.start.elapsed()),
            "chunks": m.chunks,
            "pcm_bytes": m.bytes,
            "decoder_read_call_ns": m.decoder_read_ns,
            "decoder_read_max_call_ns": m.decoder_read_max_ns,
            "encoder_write_call_ns": m.encoder_write_ns,
            "encoder_write_max_call_ns": m.encoder_write_max_ns,
            "mirror_write_call_ns": m.mirror_write_ns,
            "mirror_write_max_call_ns": m.mirror_write_max_ns,
            "fifo_open_call_ns": m.fifo_open_ns,
            "header_write_call_ns": m.header_write_ns,
            "warning": "call_wall_includes_sched_and_copy; not_pure_blocked_time",
        }));
    }
}

#[derive(Debug, Default)]
pub(crate) struct ObserverMetrics {
    pub fifo_open_ns: u64,
    pub header_read_ns: u64,
    pub pcm_read_ns: u64,
    pub pcm_decode_ns: u64,
    pub loudness_push_ns: u64,
    pub peak_push_ns: u64,
    pub loudness_finalize_ns: u64,
    pub peak_finalize_ns: u64,
    pub eof_wait_ns: u64,
    pub blocks: u64,
    pub pcm_bytes: u64,
    // would_block_poll_count counts EAGAIN reads, not poll(2) invocations.
    // poll_sleep_ns now covers only no-writer setup backoff on Unix; earlier
    // snapshots included sleeps on EAGAIN too. Use the new readiness fields
    // to compare wait behavior instead of comparing poll_sleep_ns directly.
    pub no_writer_poll_count: u64,
    pub would_block_poll_count: u64,
    pub poll_sleep_ns: u64,
    pub readiness_wait_count: u64,
    pub readiness_timeout_count: u64,
    pub readiness_wait_ns: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ObserverStage {
    FifoOpen,
    HeaderRead,
    PcmRead,
    PcmDecode,
    LoudnessPush,
    PeakPush,
    LoudnessFinalize,
    PeakFinalize,
    EofWait,
    PollSleep,
    ReadinessWait,
}

impl ObserverMetrics {
    pub fn add(&mut self, stage: ObserverStage, elapsed: Duration) {
        let bucket = match stage {
            ObserverStage::FifoOpen => &mut self.fifo_open_ns,
            ObserverStage::HeaderRead => &mut self.header_read_ns,
            ObserverStage::PcmRead => &mut self.pcm_read_ns,
            ObserverStage::PcmDecode => &mut self.pcm_decode_ns,
            ObserverStage::LoudnessPush => &mut self.loudness_push_ns,
            ObserverStage::PeakPush => &mut self.peak_push_ns,
            ObserverStage::LoudnessFinalize => &mut self.loudness_finalize_ns,
            ObserverStage::PeakFinalize => &mut self.peak_finalize_ns,
            ObserverStage::EofWait => &mut self.eof_wait_ns,
            ObserverStage::PollSleep => &mut self.poll_sleep_ns,
            ObserverStage::ReadinessWait => &mut self.readiness_wait_ns,
        };
        *bucket = bucket.saturating_add(nanos(elapsed));
    }
}

pub(crate) fn emit_observer(job_id: &str, track_ordinal: u32, success: bool, m: &ObserverMetrics) {
    baseline::cue_emit(job_id, "rg_diag_observer", json!({
        "rg_diag_schema": "tonepoet.cue.rgdiag.v1",
        "track_ordinal": track_ordinal,
        "complete": success,
        "blocks": m.blocks,
        "pcm_bytes": m.pcm_bytes,
        "fifo_open_call_ns": m.fifo_open_ns,
        "header_read_call_ns": m.header_read_ns,
        "pcm_read_call_ns": m.pcm_read_ns,
        "pcm_decode_call_ns": m.pcm_decode_ns,
        "loudness_push_call_ns": m.loudness_push_ns,
        "reporting_peak_push_call_ns": m.peak_push_ns,
        "loudness_finalize_call_ns": m.loudness_finalize_ns,
        "reporting_peak_finalize_call_ns": m.peak_finalize_ns,
        "eof_wait_call_ns": m.eof_wait_ns,
        "no_writer_poll_count": m.no_writer_poll_count,
        "would_block_poll_count": m.would_block_poll_count,
        "poll_sleep_call_ns": m.poll_sleep_ns,
        "readiness_wait_count": m.readiness_wait_count,
        "readiness_timeout_count": m.readiness_timeout_count,
        "readiness_wait_call_ns": m.readiness_wait_ns,
        "fifo_wait_strategy": if cfg!(unix) { "poll_50ms_plus_no_writer_backoff" } else { "legacy_non_unix" },
        "warning": "call_wall_includes_sched; overlapping_transport_not_additive",
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counters_saturate_and_retain_maximum() {
        let mut m = TransferMetrics::default();
        m.add(TransferStage::MirrorWrite, Duration::from_millis(2));
        m.add(TransferStage::MirrorWrite, Duration::from_millis(3));
        assert_eq!(m.mirror_write_ns, 5_000_000);
        assert_eq!(m.mirror_write_max_ns, 3_000_000);
        m.mirror_write_ns = u64::MAX;
        m.add(TransferStage::MirrorWrite, Duration::from_secs(1));
        assert_eq!(m.mirror_write_ns, u64::MAX);
    }
    #[test]
    fn observer_records_separate_stage_counters() {
        let mut m = ObserverMetrics::default();
        m.add(ObserverStage::LoudnessPush, Duration::from_millis(1));
        m.add(ObserverStage::PeakPush, Duration::from_millis(2));
        assert_eq!(m.loudness_push_ns, 1_000_000);
        assert_eq!(m.peak_push_ns, 2_000_000);
    }
}
