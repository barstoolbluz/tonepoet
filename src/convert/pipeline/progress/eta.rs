//! Conservative remaining-time estimation for route-neutral progress messages.

use std::time::{Duration, Instant};

const MIN_WEIGHT: f64 = 1.0;
const MIN_MEASURED_PROGRESS: f32 = 0.02;
const MIN_HISTORY_ELAPSED: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
struct ActiveUnit {
    ordinal: usize,
    total: usize,
    weight: f64,
    total_weight: f64,
    started_at: Instant,
}

/// Conservative ETA estimator backed by completed unit history.
///
/// The estimator only returns an ETA after it has observed at least one completed
/// unit, except for measured tool progress where the caller supplies a reliable
/// progress fraction. A denominator change disables ETA for the current
/// operation, because the remaining-work model can no longer be trusted.
#[derive(Debug, Clone, Default)]
pub struct EtaEstimator {
    total_units: Option<usize>,
    total_weight: Option<f64>,
    active_unit: Option<ActiveUnit>,
    completed_units: usize,
    completed_weight: f64,
    completed_elapsed: Duration,
    disabled: bool,
}

impl EtaEstimator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_unit(
        &mut self,
        ordinal: usize,
        total: usize,
        unit_weight: Option<f64>,
        total_weight: Option<f64>,
        now: Instant,
    ) {
        if self.denominator_changed(total, total_weight) {
            self.disable();
            return;
        }

        if self.disabled || total == 0 || ordinal == 0 || ordinal > total {
            self.active_unit = None;
            return;
        }

        let weight = usable_weight(unit_weight);
        let total_weight = usable_total_weight(total_weight, total);
        self.total_units = Some(total);
        self.total_weight = Some(total_weight);
        self.active_unit = Some(ActiveUnit {
            ordinal,
            total,
            weight,
            total_weight,
            started_at: now,
        });
    }

    pub fn finish_unit(
        &mut self,
        ordinal: usize,
        total: usize,
        unit_weight: Option<f64>,
        total_weight: Option<f64>,
        now: Instant,
    ) {
        if self.denominator_changed(total, total_weight) {
            self.disable();
            return;
        }

        if self.disabled || total == 0 || ordinal == 0 || ordinal > total {
            return;
        }

        let active = match self.active_unit.take() {
            Some(active) if active.ordinal == ordinal && active.total == total => active,
            _ => return,
        };

        let elapsed = now.saturating_duration_since(active.started_at);
        let active_weight = active.weight;
        self.total_weight = Some(active.total_weight);

        if elapsed.is_zero() {
            return;
        }

        let weight = usable_weight(unit_weight).max(active_weight);
        self.total_units = Some(total);
        self.total_weight = Some(usable_total_weight(total_weight, total).max(weight));
        self.completed_units = self
            .completed_units
            .saturating_add(1)
            .max(ordinal.min(total));
        self.completed_weight += weight;
        self.completed_elapsed += elapsed;
    }

    pub fn suppress(&mut self) {
        self.disabled = true;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub(crate) fn force_active_unit_elapsed_for_tests(&mut self, elapsed: Duration, now: Instant) {
        if let Some(active) = self.active_unit.as_mut() {
            active.started_at = now.checked_sub(elapsed).unwrap_or(now);
        }
    }

    pub fn remaining_from_completed_units(&self) -> Option<Duration> {
        if self.disabled || self.completed_units == 0 || self.completed_weight <= 0.0 {
            return None;
        }

        if self.completed_elapsed < MIN_HISTORY_ELAPSED {
            return None;
        }

        let total_weight = self.total_weight?;
        let remaining_weight = (total_weight - self.completed_weight).max(0.0);
        if remaining_weight <= 0.0 {
            return None;
        }

        let elapsed_per_weight = self.completed_elapsed.as_secs_f64() / self.completed_weight;
        if !elapsed_per_weight.is_finite() || elapsed_per_weight <= 0.0 {
            return None;
        }

        Some(Duration::from_secs_f64(
            elapsed_per_weight * remaining_weight,
        ))
    }

    pub fn remaining_from_measured_progress(
        &self,
        progress: f32,
        elapsed_since_start: Duration,
    ) -> Option<Duration> {
        if self.disabled || elapsed_since_start < MIN_HISTORY_ELAPSED {
            return None;
        }

        if !(MIN_MEASURED_PROGRESS..1.0).contains(&progress) {
            return None;
        }

        let progress = f64::from(progress);
        let elapsed = elapsed_since_start.as_secs_f64();
        let remaining = elapsed * (1.0 - progress) / progress;
        if !remaining.is_finite() || remaining <= 0.0 {
            return None;
        }

        Some(Duration::from_secs_f64(remaining))
    }

    fn denominator_changed(&self, total: usize, total_weight: Option<f64>) -> bool {
        if total == 0 {
            return false;
        }

        if let Some(existing_total) = self.total_units {
            if existing_total != total {
                return true;
            }
        }

        if let (Some(existing_weight), Some(new_weight)) = (self.total_weight, total_weight) {
            if (existing_weight - usable_total_weight(Some(new_weight), total)).abs() > f64::EPSILON
            {
                return true;
            }
        }

        false
    }

    fn disable(&mut self) {
        self.active_unit = None;
        self.disabled = true;
    }
}

pub fn append_eta(message: &str, eta: Option<Duration>) -> String {
    match eta.and_then(format_eta_coarse) {
        Some(eta) => format!("{message} · {eta}"),
        None => message.to_string(),
    }
}

/// Format an ETA with intentionally coarse precision.
///
/// The output never contains seconds, so estimates do not look more certain than
/// the underlying model supports.
pub fn format_eta_coarse(remaining: Duration) -> Option<String> {
    if remaining.is_zero() {
        return None;
    }

    let minutes = ceil_div(remaining.as_secs(), 60).max(1);
    let rounded_minutes = if minutes <= 10 {
        minutes
    } else if minutes <= 60 {
        ceil_to(minutes, 5)
    } else if minutes <= 180 {
        ceil_to(minutes, 15)
    } else {
        ceil_to(minutes, 60)
    };

    if rounded_minutes < 60 {
        return Some(format!("about {rounded_minutes}m remaining"));
    }

    let hours = rounded_minutes / 60;
    let minutes = rounded_minutes % 60;
    if minutes == 0 {
        Some(format!("about {hours}h remaining"))
    } else {
        Some(format!("about {hours}h {minutes:02}m remaining"))
    }
}

fn ceil_div(value: u64, divisor: u64) -> u64 {
    if value == 0 {
        0
    } else {
        1 + (value - 1) / divisor
    }
}

fn ceil_to(value: u64, quantum: u64) -> u64 {
    ceil_div(value, quantum) * quantum
}

fn usable_weight(weight: Option<f64>) -> f64 {
    weight
        .filter(|weight| weight.is_finite() && *weight > 0.0)
        .unwrap_or(MIN_WEIGHT)
}

fn usable_total_weight(total_weight: Option<f64>, total_units: usize) -> f64 {
    total_weight
        .filter(|weight| weight.is_finite() && *weight > 0.0)
        .unwrap_or(total_units.max(1) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant() -> Instant {
        Instant::now()
    }

    #[test]
    fn formats_eta_without_seconds() {
        assert_eq!(
            format_eta_coarse(Duration::from_secs(6 * 60 + 43)).as_deref(),
            Some("about 7m remaining")
        );
        assert_eq!(
            format_eta_coarse(Duration::from_secs(61)).as_deref(),
            Some("about 2m remaining")
        );
        assert_eq!(
            format_eta_coarse(Duration::from_secs(67 * 60)).as_deref(),
            Some("about 1h 15m remaining")
        );
    }

    #[test]
    fn hides_eta_without_completed_history() {
        let now = instant();
        let mut eta = EtaEstimator::new();
        eta.start_unit(1, 3, None, None, now);
        assert!(eta.remaining_from_completed_units().is_none());
    }

    #[test]
    fn appears_after_first_completed_unit() {
        let now = instant();
        let mut eta = EtaEstimator::new();
        eta.start_unit(1, 3, None, None, now);
        eta.finish_unit(1, 3, None, None, now + Duration::from_secs(60));
        assert_eq!(
            eta.remaining_from_completed_units(),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn uses_weighted_duration_when_available() {
        let now = instant();
        let mut eta = EtaEstimator::new();
        eta.start_unit(1, 3, Some(1_000.0), Some(6_000.0), now);
        eta.finish_unit(
            1,
            3,
            Some(1_000.0),
            Some(6_000.0),
            now + Duration::from_secs(60),
        );
        assert_eq!(
            eta.remaining_from_completed_units(),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn falls_back_to_item_count_without_weights() {
        let now = instant();
        let mut eta = EtaEstimator::new();
        eta.start_unit(1, 4, None, None, now);
        eta.finish_unit(1, 4, None, None, now + Duration::from_secs(30));
        assert_eq!(
            eta.remaining_from_completed_units(),
            Some(Duration::from_secs(90))
        );
    }

    #[test]
    fn hides_eta_after_failure_or_cancellation_suppression() {
        let now = instant();
        let mut eta = EtaEstimator::new();
        eta.start_unit(1, 2, None, None, now);
        eta.finish_unit(1, 2, None, None, now + Duration::from_secs(30));
        eta.suppress();
        assert!(eta.remaining_from_completed_units().is_none());
    }

    #[test]
    fn hides_eta_after_denominator_change() {
        let now = instant();
        let mut eta = EtaEstimator::new();
        eta.start_unit(1, 3, None, None, now);
        eta.finish_unit(1, 3, None, None, now + Duration::from_secs(30));
        eta.start_unit(2, 2, None, None, now + Duration::from_secs(31));
        assert!(eta.remaining_from_completed_units().is_none());
    }

    #[test]
    fn measured_progress_can_estimate_during_first_unit() {
        let eta = EtaEstimator::new();
        assert_eq!(
            eta.remaining_from_measured_progress(0.25, Duration::from_secs(60)),
            Some(Duration::from_secs(180))
        );
    }
}
