//! Blu-ray title-timeline PTS normalization.
//!
//! This module maps raw PES PTS values onto title-level PTS before chapter
//! filtering. Multi-clip playlists must use segmented title mapping when the
//! backend can provide it, or fail before extraction when mapping is unavailable.

use std::cell::Cell;

use crate::disc::bluray_backend::{
    BlurayBackend, BlurayPtsCapability, BlurayPtsContinuitySegment, BlurayTitleKey,
};
use crate::disc::bluray_backend_libbluray::{
    BlurayBackendLibbluray, BlurayDisc, BlurayTitleSource,
};

use super::errors::ConvertError;

pub(crate) const PTS_CLOCK_HZ: u64 = 90_000;
pub(crate) const PTS_33BIT_MODULUS: u64 = 1u64 << 33;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlurayPtsMapMode {
    IdentityContinuous,
    Segmented(Vec<BlurayPtsContinuitySegment>),
    Unsupported,
}

/// Maps raw PES PTS values into title-level PTS before chapter filtering.
///
/// The segmented mode is intentionally stateful. Raw PTS alone cannot
/// disambiguate common multi-clip playlists where each clip restarts its PTS at
/// zero; when chapter seek lands inside a later clip, the mapper is primed from
/// the chapter's title PTS and then advances monotonically as PES packets are
/// read in title order.
#[derive(Debug, Clone)]
pub(crate) struct BlurayPtsMapper {
    mode: BlurayPtsMapMode,
    segment_hint: Cell<Option<usize>>,
    last_title_pts_90k: Cell<Option<u64>>,
    anchor_title_pts_90k: Cell<Option<u64>>,
}

impl BlurayPtsMapper {
    pub(crate) fn identity_continuous() -> Self {
        Self {
            mode: BlurayPtsMapMode::IdentityContinuous,
            segment_hint: Cell::new(None),
            last_title_pts_90k: Cell::new(None),
            anchor_title_pts_90k: Cell::new(None),
        }
    }

    pub(crate) fn unsupported() -> Self {
        Self {
            mode: BlurayPtsMapMode::Unsupported,
            segment_hint: Cell::new(None),
            last_title_pts_90k: Cell::new(None),
            anchor_title_pts_90k: Cell::new(None),
        }
    }

    pub(crate) fn segmented(mut segments: Vec<BlurayPtsContinuitySegment>) -> Result<Self, ConvertError> {
        if segments.is_empty() {
            return Ok(Self::identity_continuous());
        }
        segments.sort_by_key(|segment| (segment.title_start_pts_90k, segment.clip_ref));
        validate_pts_segments(&segments)?;
        Ok(Self {
            mode: BlurayPtsMapMode::Segmented(segments),
            segment_hint: Cell::new(Some(0)),
            last_title_pts_90k: Cell::new(None),
            anchor_title_pts_90k: Cell::new(None),
        })
    }

    pub(crate) fn is_unavailable(&self) -> bool {
        matches!(self.mode, BlurayPtsMapMode::Unsupported)
    }

    pub(crate) fn prime_for_title_pts(&self, title_pts_90k: u64) {
        self.anchor_title_pts_90k.set(Some(title_pts_90k));
        if let BlurayPtsMapMode::Segmented(segments) = &self.mode {
            if let Some(index) = segments.iter().position(|segment| {
                title_pts_90k >= segment.title_start_pts_90k
                    && title_pts_90k < segment.title_end_pts_90k
            }) {
                self.segment_hint.set(Some(index));
            } else if let Some(index) = segments
                .iter()
                .rposition(|segment| title_pts_90k >= segment.title_end_pts_90k)
            {
                self.segment_hint.set(Some(index));
            }
        }
    }

    pub(crate) fn map_pes_pts_to_title_pts(&self, raw_pts_90k: u64) -> Option<u64> {
        match &self.mode {
            BlurayPtsMapMode::IdentityContinuous => Some(raw_pts_90k),
            BlurayPtsMapMode::Segmented(segments) => {
                if self.last_title_pts_90k.get().is_none() {
                    if let Some(mapped) = self.try_first_segment_candidate(segments, raw_pts_90k) {
                        return Some(mapped);
                    }
                }

                let hint = self.segment_hint.get().unwrap_or(0).min(segments.len() - 1);
                if let Some(mapped) = self.try_segment_candidate(segments, hint, raw_pts_90k) {
                    return Some(mapped);
                }

                let last = self.last_title_pts_90k.get();
                for index in hint.saturating_add(1)..segments.len() {
                    if let Some(mapped) = map_raw_pts_in_segment(&segments[index], raw_pts_90k) {
                        if last.map_or(true, |last| mapped >= last) {
                            self.accept_segment_candidate(index, mapped);
                            return Some(mapped);
                        }
                    }
                }
                for index in 0..hint {
                    if let Some(mapped) = map_raw_pts_in_segment(&segments[index], raw_pts_90k) {
                        if last.map_or(true, |last| mapped >= last) {
                            self.accept_segment_candidate(index, mapped);
                            return Some(mapped);
                        }
                    }
                }
                None
            }
            BlurayPtsMapMode::Unsupported => None,
        }
    }

    fn try_first_segment_candidate(
        &self,
        segments: &[BlurayPtsContinuitySegment],
        raw_pts_90k: u64,
    ) -> Option<u64> {
        let anchor = self.anchor_title_pts_90k.get();
        let mut best = None::<(usize, u64, u64)>;
        for (index, segment) in segments.iter().enumerate() {
            let Some(mapped) = map_raw_pts_in_segment(segment, raw_pts_90k) else {
                continue;
            };
            let score = anchor.map_or(0, |anchor| title_pts_abs_diff(mapped, anchor));
            if best.map_or(true, |(_, _, best_score)| score < best_score) {
                best = Some((index, mapped, score));
            }
        }
        let (index, mapped, _) = best?;
        self.accept_segment_candidate(index, mapped);
        Some(mapped)
    }

    fn try_segment_candidate(
        &self,
        segments: &[BlurayPtsContinuitySegment],
        index: usize,
        raw_pts_90k: u64,
    ) -> Option<u64> {
        let mapped = map_raw_pts_in_segment(&segments[index], raw_pts_90k)?;
        if self
            .last_title_pts_90k
            .get()
            .map_or(true, |last| mapped >= last)
        {
            self.accept_segment_candidate(index, mapped);
            Some(mapped)
        } else {
            None
        }
    }

    fn accept_segment_candidate(&self, index: usize, mapped_title_pts_90k: u64) {
        self.segment_hint.set(Some(index));
        self.last_title_pts_90k.set(Some(mapped_title_pts_90k));
    }
}

pub(crate) fn build_pts_mapper(
    disc: &BlurayDisc,
    title: BlurayTitleKey,
    title_source: &mut BlurayTitleSource,
) -> Result<BlurayPtsMapper, ConvertError> {
    match BlurayBackendLibbluray::pts_capability(disc, title, title_source).map_err(|err| {
        ConvertError::Realize(format!(
            "failed to query Blu-ray playlist {:05} PTS continuity capability: {err}",
            title.playlist_number()
        ))
    })? {
        BlurayPtsCapability::ContinuousTitleTimeline => Ok(BlurayPtsMapper::identity_continuous()),
        BlurayPtsCapability::SegmentedTitleTimeline(segments) => BlurayPtsMapper::segmented(segments),
        BlurayPtsCapability::Unavailable => Ok(BlurayPtsMapper::unsupported()),
    }
}

pub(crate) fn prepare_pts_mapper_for_realization(
    mapper: BlurayPtsMapper,
    clip_count: u32,
    playlist_number: u32,
) -> Result<BlurayPtsMapper, ConvertError> {
    if mapper.is_unavailable() {
        if clip_count > 1 {
            return Err(ConvertError::Realize(format!(
                "Blu-ray multi-clip LPCM extraction requires title PTS mapping, but this backend cannot provide usable clip-to-title timing for playlist {playlist_number:05} ({clip_count} clips)"
            )));
        }
        log::warn!(
            "Blu-ray playlist {playlist_number:05} backend did not expose title PTS mapping; accepting raw PES PTS because the title has {clip_count} clip"
        );
        return Ok(BlurayPtsMapper::identity_continuous());
    }
    Ok(mapper)
}

fn validate_pts_segments(segments: &[BlurayPtsContinuitySegment]) -> Result<(), ConvertError> {
    let mut previous_title_end = None;
    for segment in segments {
        if segment.title_end_pts_90k <= segment.title_start_pts_90k {
            return Err(ConvertError::TrackValidation(format!(
                "invalid Blu-ray PTS continuity segment for clip {}: title range {}..{} is empty or reversed",
                segment.clip_ref, segment.title_start_pts_90k, segment.title_end_pts_90k
            )));
        }
        let title_duration = segment.title_end_pts_90k - segment.title_start_pts_90k;
        let clip_duration = pts_forward_distance(segment.clip_start_pts_90k, segment.clip_end_pts_90k);
        if clip_duration == 0 {
            return Err(ConvertError::TrackValidation(format!(
                "invalid Blu-ray PTS continuity segment for clip {}: clip range {}..{} has zero 33-bit duration",
                segment.clip_ref, segment.clip_start_pts_90k, segment.clip_end_pts_90k
            )));
        }
        if title_duration != clip_duration {
            return Err(ConvertError::TrackValidation(format!(
                "invalid Blu-ray PTS continuity segment for clip {}: title duration {} does not match clip PTS duration {}",
                segment.clip_ref, title_duration, clip_duration
            )));
        }
        if let Some(previous_title_end) = previous_title_end {
            if segment.title_start_pts_90k < previous_title_end {
                return Err(ConvertError::TrackValidation(format!(
                    "invalid Blu-ray PTS continuity segments: title range starting at {} overlaps prior end {}",
                    segment.title_start_pts_90k, previous_title_end
                )));
            }
        }
        previous_title_end = Some(segment.title_end_pts_90k);
    }
    Ok(())
}

fn map_raw_pts_in_segment(
    segment: &BlurayPtsContinuitySegment,
    raw_pts_90k: u64,
) -> Option<u64> {
    let duration = pts_forward_distance(segment.clip_start_pts_90k, segment.clip_end_pts_90k);
    if duration == 0 {
        return None;
    }
    let delta = pts_forward_distance(segment.clip_start_pts_90k, raw_pts_90k);
    if delta >= duration {
        return None;
    }
    segment.title_start_pts_90k.checked_add(delta).and_then(|title_pts| {
        (title_pts < segment.title_end_pts_90k).then_some(title_pts)
    })
}

fn title_pts_abs_diff(a: u64, b: u64) -> u64 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

fn pts_forward_distance(start_pts_90k: u64, end_pts_90k: u64) -> u64 {
    let start = start_pts_90k % PTS_33BIT_MODULUS;
    let end = end_pts_90k % PTS_33BIT_MODULUS;
    if end >= start {
        end - start
    } else {
        PTS_33BIT_MODULUS - start + end
    }
}



pub(crate) fn samples_to_pts90(samples: u64, sample_rate: u32) -> Result<u64, ConvertError> {
    let pts = u128::from(samples)
        .checked_mul(u128::from(PTS_CLOCK_HZ))
        .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM sample-to-PTS overflow".to_string()))?
        / u128::from(sample_rate);
    u64::try_from(pts).map_err(|_| {
        ConvertError::Realize("Blu-ray LPCM sample-to-PTS result exceeds u64".to_string())
    })
}

pub(crate) fn ceil_pts_to_samples(delta_pts_90k: u64, sample_rate: u32) -> Result<u64, ConvertError> {
    let numerator = u128::from(delta_pts_90k)
        .checked_mul(u128::from(sample_rate))
        .and_then(|value| value.checked_add(u128::from(PTS_CLOCK_HZ - 1)))
        .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM ceil PTS-to-samples overflow".to_string()))?;
    let samples = numerator / u128::from(PTS_CLOCK_HZ);
    u64::try_from(samples).map_err(|_| {
        ConvertError::Realize("Blu-ray LPCM ceil PTS-to-samples result exceeds u64".to_string())
    })
}

pub(crate) fn floor_pts_to_samples(delta_pts_90k: u64, sample_rate: u32) -> Result<u64, ConvertError> {
    let samples = u128::from(delta_pts_90k)
        .checked_mul(u128::from(sample_rate))
        .ok_or_else(|| ConvertError::Realize("Blu-ray LPCM floor PTS-to-samples overflow".to_string()))?
        / u128::from(PTS_CLOCK_HZ);
    u64::try_from(samples).map_err(|_| {
        ConvertError::Realize("Blu-ray LPCM floor PTS-to-samples result exceeds u64".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_clip_restart_segments() -> Vec<BlurayPtsContinuitySegment> {
        vec![
            BlurayPtsContinuitySegment {
                title_start_pts_90k: 0,
                title_end_pts_90k: 100,
                clip_ref: 0,
                clip_start_pts_90k: 0,
                clip_end_pts_90k: 100,
            },
            BlurayPtsContinuitySegment {
                title_start_pts_90k: 100,
                title_end_pts_90k: 200,
                clip_ref: 1,
                clip_start_pts_90k: 0,
                clip_end_pts_90k: 100,
            },
        ]
    }

    #[test]
    fn pts_mapper_maps_two_clip_restart_to_continuous_title_pts() {
        let mapper = BlurayPtsMapper::segmented(two_clip_restart_segments()).unwrap();
        mapper.prime_for_title_pts(120);

        assert_eq!(mapper.map_pes_pts_to_title_pts(10), Some(110));
        assert_eq!(mapper.map_pes_pts_to_title_pts(25), Some(125));
    }

    #[test]
    fn pts_mapper_keeps_adjacent_chapters_disjoint_at_clip_boundary() {
        let first_chapter = BlurayPtsMapper::segmented(two_clip_restart_segments()).unwrap();
        first_chapter.prime_for_title_pts(95);
        assert_eq!(first_chapter.map_pes_pts_to_title_pts(95), Some(95));
        assert_eq!(first_chapter.map_pes_pts_to_title_pts(0), Some(100));

        let second_chapter = BlurayPtsMapper::segmented(two_clip_restart_segments()).unwrap();
        second_chapter.prime_for_title_pts(100);
        assert_eq!(second_chapter.map_pes_pts_to_title_pts(0), Some(100));
        assert_eq!(second_chapter.map_pes_pts_to_title_pts(10), Some(110));
    }

    #[test]
    fn pts_mapper_uses_anchor_to_choose_later_clip_when_raw_ranges_repeat() {
        let mapper = BlurayPtsMapper::segmented(two_clip_restart_segments()).unwrap();
        mapper.prime_for_title_pts(150);

        assert_eq!(mapper.map_pes_pts_to_title_pts(50), Some(150));
    }

    #[test]
    fn pts_mapper_handles_chapter_crossing_clip_boundary_monotonically() {
        let mapper = BlurayPtsMapper::segmented(two_clip_restart_segments()).unwrap();
        mapper.prime_for_title_pts(95);

        assert_eq!(mapper.map_pes_pts_to_title_pts(95), Some(95));
        assert_eq!(mapper.map_pes_pts_to_title_pts(99), Some(99));
        assert_eq!(mapper.map_pes_pts_to_title_pts(0), Some(100));
        assert_eq!(mapper.map_pes_pts_to_title_pts(5), Some(105));
    }

    #[test]
    fn pts_mapper_handles_33_bit_clip_pts_wrap() {
        let mapper = BlurayPtsMapper::segmented(vec![BlurayPtsContinuitySegment {
            title_start_pts_90k: 1_000,
            title_end_pts_90k: 1_030,
            clip_ref: 0,
            clip_start_pts_90k: PTS_33BIT_MODULUS - 10,
            clip_end_pts_90k: 20,
        }])
        .unwrap();

        assert_eq!(
            mapper.map_pes_pts_to_title_pts(PTS_33BIT_MODULUS - 5),
            Some(1_005)
        );
        assert_eq!(mapper.map_pes_pts_to_title_pts(5), Some(1_015));
    }

    #[test]
    fn unsupported_single_clip_backend_falls_back_to_identity_pts() {
        let mapper = prepare_pts_mapper_for_realization(BlurayPtsMapper::unsupported(), 1, 800)
            .unwrap();

        assert_eq!(mapper.map_pes_pts_to_title_pts(42), Some(42));
    }

    #[test]
    fn unsupported_multi_clip_backend_errors_before_extraction() {
        let err = prepare_pts_mapper_for_realization(BlurayPtsMapper::unsupported(), 2, 800)
            .unwrap_err();
        assert!(err.to_string().contains("multi-clip LPCM extraction requires title PTS mapping"));
    }
}
