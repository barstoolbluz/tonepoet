//! Heartbeat support for opaque long-running operations.
//!
//! The heartbeat wrapper does not manufacture progress. It re-emits the
//! tracker's last known phase progress with an `Unknown` confidence message so
//! users can see that an operation is still alive while an in-process or opaque
//! child operation is running.

use std::future::Future;
use std::time::Duration;

use tokio::time::{self, Instant};

use super::operation::OperationProgressTracker;

/// Default cadence for opaque-operation liveness updates.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Run `future` while periodically emitting unknown-progress liveness updates.
///
/// The first heartbeat is emitted only after `interval` has elapsed. Completion
/// returns immediately without waiting for another tick.
pub async fn run_with_heartbeat<'a, F, T>(
    future: F,
    tracker: &mut OperationProgressTracker<'a>,
    material_key: impl AsRef<str>,
    message: impl AsRef<str>,
    interval: Duration,
) -> T
where
    F: Future<Output = T>,
{
    let material_key = material_key.as_ref().to_string();
    let message = message.as_ref().to_string();
    tokio::pin!(future);

    let sleep = time::sleep(interval);
    tokio::pin!(sleep);

    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = &mut sleep => {
                tracker
                    .unknown_alive_with_key(&material_key, &message)
                    .await;
                sleep.as_mut().reset(Instant::now() + interval);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::pipeline::reporter::{PipelineEvent, RecordingReporter};
    use crate::convert::pipeline::types::PipelineStage;

    fn progress_events(reporter: &RecordingReporter) -> Vec<PipelineEvent> {
        reporter
            .events()
            .into_iter()
            .filter(|event| matches!(event, PipelineEvent::Progress { .. }))
            .collect()
    }

    fn progress_values(reporter: &RecordingReporter) -> Vec<f32> {
        progress_events(reporter)
            .into_iter()
            .map(|event| match event {
                PipelineEvent::Progress { phase_progress, .. } => phase_progress,
                _ => unreachable!(),
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_emits_after_interval() {
        // Drive run_with_heartbeat to completion and verify at least one
        // heartbeat fired. The tracker's throttle uses std::time::Instant
        // (real clock) while this test uses tokio mock time, so repeated
        // heartbeats at the same wall-clock instant may be coalesced.
        // In production the 10-second interval provides enough real-time
        // separation. Here we verify the mechanism works for at least one
        // heartbeat emission.
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.25, "Converting").await;

        let result = run_with_heartbeat(
            async {
                time::sleep(Duration::from_secs(15)).await;
                7_u32
            },
            &mut tracker,
            "opaque-work",
            "still running",
            Duration::from_secs(10),
        )
        .await;

        assert_eq!(result, 7);
        let events = progress_events(&reporter);
        // At least 2: the initial estimated() + at least 1 heartbeat.
        assert!(
            events.len() >= 2,
            "expected at least 2 progress events (1 estimated + 1 heartbeat), got {}",
            events.len()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_does_not_advance_progress() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.40, "Converting").await;

        let result = run_with_heartbeat(
            async {
                time::sleep(Duration::from_secs(11)).await;
                Ok::<_, ()>(())
            },
            &mut tracker,
            "opaque-work",
            "still running",
            Duration::from_secs(10),
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(progress_values(&reporter), vec![0.40, 0.40]);
    }
}
