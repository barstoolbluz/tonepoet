use super::{LoudnessError, LoudnessProfile};

#[derive(Debug, Clone, Copy)]
pub(super) struct WindowClock {
    profile: LoudnessProfile,
    sample_rate_hz: u32,
    legacy_hop_frames: u64,
    integrated_window_frames: u64,
    lra_window_frames: u64,
    ring_frames: u64,
    next_integrated_index: u64,
    next_lra_index: u64,
}

impl WindowClock {
    pub(super) fn new(
        sample_rate_hz: u32,
        profile: LoudnessProfile,
    ) -> Result<Self, LoudnessError> {
        let rate = u64::from(sample_rate_hz);
        if rate == 0 {
            return Err(LoudnessError::InvalidSampleRate);
        }
        let legacy_hop_frames = rate
            .checked_add(5)
            .ok_or(LoudnessError::InputTooLong)?
            / 10;
        let integrated_window_frames = match profile {
            LoudnessProfile::Libebur128126 => legacy_hop_frames
                .checked_mul(4)
                .ok_or(LoudnessError::InputTooLong)?,
            // nearest integer to 0.4 * Fs; 2*Fs/5 can never be exactly x.5.
            LoudnessProfile::NativeEbu2023 => rate
                .checked_mul(2)
                .and_then(|value| value.checked_add(2))
                .ok_or(LoudnessError::InputTooLong)?
                / 5,
        };
        let lra_window_frames = match profile {
            LoudnessProfile::Libebur128126 => legacy_hop_frames
                .checked_mul(30)
                .ok_or(LoudnessError::InputTooLong)?,
            LoudnessProfile::NativeEbu2023 => rate
                .checked_mul(3)
                .ok_or(LoudnessError::InputTooLong)?,
        };
        let ring_frames = match profile {
            LoudnessProfile::Libebur128126 => {
                // libebur128 sizes its three-second audio_data ring from 3*Fs,
                // then rounds that storage up to a whole 100 ms block.  The
                // LRA observation itself remains exactly 30 blocks.  Those
                // quantities differ at some odd rates and the physical ring
                // geometry is observable in wrapped floating-point summation.
                let raw = rate.checked_mul(3).ok_or(LoudnessError::InputTooLong)?;
                let remainder = raw % legacy_hop_frames;
                if remainder == 0 {
                    raw
                } else {
                    raw.checked_add(legacy_hop_frames - remainder)
                        .ok_or(LoudnessError::InputTooLong)?
                }
            }
            LoudnessProfile::NativeEbu2023 => lra_window_frames,
        };
        Ok(Self {
            profile,
            sample_rate_hz,
            legacy_hop_frames,
            integrated_window_frames,
            lra_window_frames,
            ring_frames,
            next_integrated_index: 0,
            next_lra_index: 0,
        })
    }

    pub(super) const fn integrated_window_frames(&self) -> u64 {
        self.integrated_window_frames
    }

    pub(super) const fn lra_window_frames(&self) -> u64 {
        self.lra_window_frames
    }

    pub(super) const fn ring_frames(&self) -> u64 {
        self.ring_frames
    }

    fn native_offset(&self, index: u64) -> Result<u64, LoudnessError> {
        let numerator = u128::from(index)
            .checked_mul(u128::from(self.sample_rate_hz))
            .ok_or(LoudnessError::InputTooLong)?;
        u64::try_from(numerator / 10).map_err(|_| LoudnessError::InputTooLong)
    }

    pub(super) fn next_integrated_end(&self) -> Result<u64, LoudnessError> {
        match self.profile {
            LoudnessProfile::Libebur128126 => self
                .integrated_window_frames
                .checked_add(
                    self.next_integrated_index
                        .checked_mul(self.legacy_hop_frames)
                        .ok_or(LoudnessError::InputTooLong)?,
                )
                .ok_or(LoudnessError::InputTooLong),
            LoudnessProfile::NativeEbu2023 => self
                .integrated_window_frames
                .checked_add(self.native_offset(self.next_integrated_index)?)
                .ok_or(LoudnessError::InputTooLong),
        }
    }

    pub(super) fn next_lra_end(&self) -> Result<u64, LoudnessError> {
        match self.profile {
            LoudnessProfile::Libebur128126 => self
                .lra_window_frames
                .checked_add(
                    self.next_lra_index
                        .checked_mul(
                            self.legacy_hop_frames
                                .checked_mul(10)
                                .ok_or(LoudnessError::InputTooLong)?,
                        )
                        .ok_or(LoudnessError::InputTooLong)?,
                )
                .ok_or(LoudnessError::InputTooLong),
            LoudnessProfile::NativeEbu2023 => self
                .lra_window_frames
                .checked_add(self.native_offset(self.next_lra_index)?)
                .ok_or(LoudnessError::InputTooLong),
        }
    }

    pub(super) fn take_integrated_if_due(
        &mut self,
        frames_seen: u64,
    ) -> Result<bool, LoudnessError> {
        if frames_seen != self.next_integrated_end()? {
            return Ok(false);
        }
        self.next_integrated_index = self
            .next_integrated_index
            .checked_add(1)
            .ok_or(LoudnessError::InputTooLong)?;
        Ok(true)
    }

    pub(super) fn take_lra_if_due(&mut self, frames_seen: u64) -> Result<bool, LoudnessError> {
        if frames_seen != self.next_lra_end()? {
            return Ok(false);
        }
        self.next_lra_index = self
            .next_lra_index
            .checked_add(1)
            .ok_or(LoudnessError::InputTooLong)?;
        Ok(true)
    }

    pub(super) fn events_due_through(
        &self,
        target_frames: u64,
        include_integrated: bool,
        include_lra: bool,
    ) -> Result<(usize, usize), LoudnessError> {
        let integrated = if include_integrated {
            self.count_events(
                target_frames,
                self.next_integrated_index,
                self.integrated_window_frames,
                false,
            )?
        } else {
            0
        };
        let lra = if include_lra {
            self.count_events(target_frames, self.next_lra_index, self.lra_window_frames, true)?
        } else {
            0
        };
        Ok((integrated, lra))
    }

    fn count_events(
        &self,
        target_frames: u64,
        next_index: u64,
        window_frames: u64,
        lra: bool,
    ) -> Result<usize, LoudnessError> {
        if target_frames < window_frames {
            return Ok(0);
        }
        let max_index = match self.profile {
            LoudnessProfile::Libebur128126 => {
                let hop = if lra {
                    self.legacy_hop_frames
                        .checked_mul(10)
                        .ok_or(LoudnessError::InputTooLong)?
                } else {
                    self.legacy_hop_frames
                };
                (target_frames - window_frames) / hop
            }
            LoudnessProfile::NativeEbu2023 => {
                // floor(j*Fs/10) <= target-window.  The largest j satisfying
                // this is floor((10*(x+1)-1)/Fs).
                let x = u128::from(target_frames - window_frames);
                let numerator = x
                    .checked_add(1)
                    .and_then(|value| value.checked_mul(10))
                    .and_then(|value| value.checked_sub(1))
                    .ok_or(LoudnessError::InputTooLong)?;
                u64::try_from(numerator / u128::from(self.sample_rate_hz))
                    .map_err(|_| LoudnessError::InputTooLong)?
            }
        };
        if max_index < next_index {
            return Ok(0);
        }
        usize::try_from(max_index - next_index + 1).map_err(|_| LoudnessError::InputTooLong)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_clock_matches_reference_geometry() {
        let mut clock = WindowClock::new(48_000, LoudnessProfile::Libebur128126).unwrap();
        assert_eq!(clock.integrated_window_frames(), 19_200);
        assert_eq!(clock.lra_window_frames(), 144_000);
        assert_eq!(clock.ring_frames(), 144_000);
        assert_eq!(clock.next_integrated_end().unwrap(), 19_200);
        assert!(clock.take_integrated_if_due(19_200).unwrap());
        assert_eq!(clock.next_integrated_end().unwrap(), 24_000);
        assert_eq!(clock.next_lra_end().unwrap(), 144_000);
        assert!(clock.take_lra_if_due(144_000).unwrap());
        assert_eq!(clock.next_lra_end().unwrap(), 192_000);
    }


    #[test]
    fn legacy_odd_rate_ring_rounds_up_without_changing_lra_window() {
        let clock = WindowClock::new(44_101, LoudnessProfile::Libebur128126).unwrap();
        // H=(44101+5)/10=4410.  The LRA window is 30H, but libebur128's
        // physical three-second ring starts from 3*Fs and rounds to a whole H.
        assert_eq!(clock.lra_window_frames(), 132_300);
        assert_eq!(clock.ring_frames(), 136_710);
        assert_eq!(clock.next_lra_end().unwrap(), 132_300);
    }

    #[test]
    fn native_odd_rate_schedule_does_not_drift() {
        let mut clock = WindowClock::new(44_101, LoudnessProfile::NativeEbu2023).unwrap();
        assert_eq!(clock.integrated_window_frames(), 17_640);
        assert_eq!(clock.next_integrated_end().unwrap(), 17_640);
        assert!(clock.take_integrated_if_due(17_640).unwrap());
        assert_eq!(clock.next_integrated_end().unwrap(), 22_050);
        assert!(clock.take_integrated_if_due(22_050).unwrap());
        assert_eq!(clock.next_integrated_end().unwrap(), 26_460);
        assert_eq!(clock.lra_window_frames(), 132_303);
    }

    #[test]
    fn event_preflight_counts_match_due_events() {
        let clock = WindowClock::new(48_000, LoudnessProfile::Libebur128126).unwrap();
        assert_eq!(clock.events_due_through(240_000, true, true).unwrap(), (47, 3));
        let native = WindowClock::new(44_101, LoudnessProfile::NativeEbu2023).unwrap();
        let end = 44_101 * 10;
        assert_eq!(native.events_due_through(end, true, true).unwrap(), (97, 71));
    }
    #[test]
    fn required_integrated_and_lra_boundary_matrix_is_exact() {
        const H: u64 = 4_800;
        for profile in [LoudnessProfile::Libebur128126, LoudnessProfile::NativeEbu2023] {
            let clock = WindowClock::new(48_000, profile).unwrap();
            for (frames, expected) in [
                (4 * H - 1, 0),
                (4 * H, 1),
                (5 * H - 1, 1),
                (5 * H, 2),
                (5 * H + H / 2, 2),
            ] {
                assert_eq!(clock.events_due_through(frames, true, false).unwrap().0, expected, "{profile:?} integrated at {frames}");
            }
        }

        let legacy = WindowClock::new(48_000, LoudnessProfile::Libebur128126).unwrap();
        for (frames, expected) in [
            (144_000 - 1, 0),
            (144_000, 1),
            (148_800, 1),
            (187_200, 1),
            (192_000, 2),
        ] {
            assert_eq!(legacy.events_due_through(frames, false, true).unwrap().1, expected, "legacy LRA at {frames}");
        }

        let native = WindowClock::new(48_000, LoudnessProfile::NativeEbu2023).unwrap();
        for (frames, expected) in [
            (144_000 - 1, 0),
            (144_000, 1),
            (148_800, 2),
            (187_200, 10),
            (192_000, 11),
        ] {
            assert_eq!(native.events_due_through(frames, false, true).unwrap().1, expected, "native LRA at {frames}");
        }
    }

    #[test]
    fn odd_rate_clock_state_advances_without_push_boundary_reset() {
        let mut clock = WindowClock::new(44_101, LoudnessProfile::NativeEbu2023).unwrap();
        for expected in [17_640, 22_050, 26_460, 30_870] {
            assert_eq!(clock.next_integrated_end().unwrap(), expected);
            assert!(clock.take_integrated_if_due(expected).unwrap());
        }
        for expected in [132_303, 136_713, 141_123] {
            assert_eq!(clock.next_lra_end().unwrap(), expected);
            assert!(clock.take_lra_if_due(expected).unwrap());
        }
    }

    #[test]
    fn qualified_clock_boundary_matrix_is_exact() {
        let fixtures = [
            (32_000, LoudnessProfile::Libebur128126, 12_800, 16_000, 96_000, 128_000, 96_000),
            (44_100, LoudnessProfile::Libebur128126, 17_640, 22_050, 132_300, 176_400, 132_300),
            (44_101, LoudnessProfile::Libebur128126, 17_640, 22_050, 132_300, 176_400, 136_710),
            (48_000, LoudnessProfile::Libebur128126, 19_200, 24_000, 144_000, 192_000, 144_000),
            (96_000, LoudnessProfile::Libebur128126, 38_400, 48_000, 288_000, 384_000, 288_000),
            (192_000, LoudnessProfile::Libebur128126, 76_800, 96_000, 576_000, 768_000, 576_000),
            (32_000, LoudnessProfile::NativeEbu2023, 12_800, 16_000, 96_000, 99_200, 96_000),
            (44_100, LoudnessProfile::NativeEbu2023, 17_640, 22_050, 132_300, 136_710, 132_300),
            (44_101, LoudnessProfile::NativeEbu2023, 17_640, 22_050, 132_303, 136_713, 132_303),
            (48_000, LoudnessProfile::NativeEbu2023, 19_200, 24_000, 144_000, 148_800, 144_000),
            (96_000, LoudnessProfile::NativeEbu2023, 38_400, 48_000, 288_000, 297_600, 288_000),
            (192_000, LoudnessProfile::NativeEbu2023, 76_800, 96_000, 576_000, 595_200, 576_000),
        ];
        for (rate, profile, first_i, second_i, first_lra, second_lra, ring) in fixtures {
            let mut clock = WindowClock::new(rate, profile).unwrap();
            assert_eq!(clock.integrated_window_frames(), first_i, "{rate} {profile:?} integrated window");
            assert_eq!(clock.lra_window_frames(), first_lra, "{rate} {profile:?} LRA window");
            assert_eq!(clock.ring_frames(), ring, "{rate} {profile:?} ring");
            assert_eq!(clock.next_integrated_end().unwrap(), first_i);
            assert!(!clock.take_integrated_if_due(first_i - 1).unwrap());
            assert!(clock.take_integrated_if_due(first_i).unwrap());
            assert_eq!(clock.next_integrated_end().unwrap(), second_i);
            assert_eq!(clock.next_lra_end().unwrap(), first_lra);
            assert!(!clock.take_lra_if_due(first_lra - 1).unwrap());
            assert!(clock.take_lra_if_due(first_lra).unwrap());
            assert_eq!(clock.next_lra_end().unwrap(), second_lra);
        }
    }

}
