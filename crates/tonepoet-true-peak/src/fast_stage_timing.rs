//! Commissioning-only Fast066 stage timing.
//!
//! This module is compiled only with the `fast-stage-timing` feature. Normal
//! library builds therefore carry no timing state or clock reads in Fast066.

use std::time::Instant;

#[derive(Debug)]
pub(crate) struct StageTimer(Instant);

impl StageTimer {
    #[inline]
    pub(crate) fn start() -> Self {
        Self(Instant::now())
    }

    #[inline]
    pub(crate) fn finish(self, accumulator_nanos: &mut u64) {
        let nanos = self.0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        *accumulator_nanos = accumulator_nanos.saturating_add(nanos);
    }
}
