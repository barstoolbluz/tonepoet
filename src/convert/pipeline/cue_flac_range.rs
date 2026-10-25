//! IP-02: inert, sample-indexed FLAC range producer for S3 feasibility tests.
//!
//! `prepare` reads the same FLAC STREAMINFO authority as the CUE materializer,
//! checks an exact half-open range, and returns a typed FFmpeg command and an
//! independent byte-count verifier. It DOES NOT spawn, execute, alter routing,
//! or admit parallel work. The caller of any future execution path must use
//! the existing configured ToolRunner/cancellation and count **all** producer
//! stdout bytes before accepting an artifact. A successful FFmpeg exit alone
//! does not establish the sample count; atrim can succeed after short EOF.
//!
//! Qualified decoder *versions* and full supervised execution are separate
//! gates. See docs/cue_parallel_ip01/ip02_verification/.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tonepoet_pipeline::CommandEnvironmentPolicy;

use super::materializer_cue::cue_exact_flac_streaminfo;
use super::tool::{ToolBinary, ToolCommand};
use super::types::{CueSegmentCarrier, TrackSourceRef};

#[derive(Debug, Error)]
pub(crate) enum CueFlacRangeError {
    #[error("source is not an exact CUE stream segment")]
    NotCueStreamSegment,
    #[error("FLAC range requires the existing signed s32le carrier and qualified PCM facts")]
    UnsupportedFormat,
    #[error("invalid zero, reversed, or out-of-image half-open sample range")]
    InvalidRange,
    #[error("range byte count does not fit in u64")]
    ExtentOverflow,
    #[error("FLAC source path cannot be resolved: {0}")]
    SourcePath(#[source] std::io::Error),
    #[error("source path cannot be represented losslessly for the tool runner")]
    SourcePathEncoding,
    #[error("source is not a valid exact FLAC STREAMINFO prefix")]
    SourceNotExactFlac,
    #[error("CUE facts do not match current FLAC STREAMINFO")]
    SourceFactsDrift,
    #[error("ranged decoder yielded {observed} bytes but required {expected} bytes")]
    ByteCount { expected: u64, observed: u128 },
}

/// Independent-source request. The caller supplies the CUE materializer's
/// sample facts, *not* floating-point seconds. An independent STREAMINFO read
/// must agree before the command is returned.
#[derive(Debug, Clone)]
pub(crate) struct CueFlacRangeRequest {
    pub source_path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub source_bits_per_sample: u16,
    pub carrier: CueSegmentCarrier,
    pub image_samples: u64,
    pub start_sample: u64,
    pub end_sample: u64,
}

impl CueFlacRangeRequest {
    /// Adapt an existing typed CUE track; callers still supply the upstream
    /// source's sample rate and *original* integer depth. The carrier is s32le
    /// for both 16-bit and 24-bit FLAC, not packed s16/s24.
    pub fn from_segment(
        source: &TrackSourceRef,
        sample_rate: u32,
        source_bits_per_sample: u16,
    ) -> Result<Self, CueFlacRangeError> {
        let TrackSourceRef::CueStreamSegment {
            decode_path,
            start_sample,
            samples,
            image_samples,
            carrier,
            channels,
            ..
        } = source
        else {
            return Err(CueFlacRangeError::NotCueStreamSegment);
        };
        let end_sample = start_sample
            .checked_add(*samples)
            .ok_or(CueFlacRangeError::InvalidRange)?;
        Ok(Self {
            source_path: decode_path.clone(),
            sample_rate,
            channels: *channels,
            source_bits_per_sample,
            carrier: *carrier,
            image_samples: *image_samples,
            start_sample: *start_sample,
            end_sample,
        })
    }
}

/// Verified immutable command configuration; no runtime tool/version binding
/// is asserted here. IP-03/04 must gate the installed executable separately.
#[derive(Debug, Clone)]
pub(crate) struct CueFlacRangeDecoder {
    source_path: PathBuf,
    start_sample: u64,
    end_sample: u64,
    expected_bytes: u64,
    seek_seconds: u64,
    trim_start_sample: u64,
    trim_end_sample: u64,
}

/// Count an arbitrarily chunked stream without retaining any PCM. `finish`
/// rejects early EOF, while `observe_bytes` rejects surplus before forwarding.
#[derive(Debug, Clone)]
pub(crate) struct ExactPcmExtent {
    expected: u64,
    observed: u128,
}

impl ExactPcmExtent {
    pub fn observe_bytes(&mut self, amount: usize) -> Result<(), CueFlacRangeError> {
        let next = self
            .observed
            .checked_add(amount as u128)
            .ok_or(CueFlacRangeError::ExtentOverflow)?;
        if next > u128::from(self.expected) {
            return Err(CueFlacRangeError::ByteCount {
                expected: self.expected,
                observed: next,
            });
        }
        self.observed = next;
        Ok(())
    }

    pub fn finish(self) -> Result<(), CueFlacRangeError> {
        if self.observed == u128::from(self.expected) {
            Ok(())
        } else {
            Err(CueFlacRangeError::ByteCount {
                expected: self.expected,
                observed: self.observed,
            })
        }
    }
}

fn checked_pcm_bytes(
    samples: u64,
    channels: u16,
    carrier_bytes: u64,
) -> Result<u64, CueFlacRangeError> {
    let count = u128::from(samples)
        .checked_mul(u128::from(channels))
        .and_then(|n| n.checked_mul(u128::from(carrier_bytes)))
        .ok_or(CueFlacRangeError::ExtentOverflow)?;
    u64::try_from(count).map_err(|_| CueFlacRangeError::ExtentOverflow)
}

impl CueFlacRangeDecoder {
    pub fn prepare(request: &CueFlacRangeRequest) -> Result<Self, CueFlacRangeError> {
        // This narrow capability is intentional: it is not a generic FLAC,
        // float, surround or resampling decoder selection mechanism.
        if !matches!(request.sample_rate, 44_100 | 48_000 | 96_000)
            || !matches!(request.channels, 1 | 2)
            || !matches!(request.source_bits_per_sample, 16 | 24)
            || request.carrier != CueSegmentCarrier::PcmS32LeWav
        {
            return Err(CueFlacRangeError::UnsupportedFormat);
        }
        if request.start_sample >= request.end_sample
            || request.end_sample > request.image_samples
            || request.image_samples == 0
        {
            return Err(CueFlacRangeError::InvalidRange);
        }
        let samples = request.end_sample - request.start_sample;
        let expected_bytes = checked_pcm_bytes(
            samples,
            request.channels,
            request.carrier.bytes_per_sample(),
        )?;

        // The *physical* input is used for execution, independently of the
        // authored CUE track title and logical provenance identity.
        let canonical_path = fs::canonicalize(&request.source_path)
            .map_err(CueFlacRangeError::SourcePath)?;
        if canonical_path.to_str().is_none() {
            return Err(CueFlacRangeError::SourcePathEncoding);
        }
        let (actual_rate, actual_channels, actual_depth, actual_total) =
            cue_exact_flac_streaminfo(&canonical_path)
                .ok_or(CueFlacRangeError::SourceNotExactFlac)?;
        if (actual_rate, actual_channels, actual_depth, actual_total)
            != (
                request.sample_rate,
                request.channels,
                request.source_bits_per_sample,
                request.image_samples,
            )
        {
            return Err(CueFlacRangeError::SourceFactsDrift);
        }

        // FFmpeg's input `-ss` runs the FLAC seek path; an integer-second
        // anchor avoids decimal/rational timestamp rounding. `atrim` counts
        // samples relative to the accurately sought decoder output, not PTS.
        // At most (rate - 1) samples of preroll precede the exact start.
        // Do not replace input `-ss` with a decode-from-zero `atrim`.
        let seek_seconds = request.start_sample / u64::from(request.sample_rate);
        let trim_start_sample = request.start_sample % u64::from(request.sample_rate);
        let trim_end_sample = trim_start_sample
            .checked_add(samples)
            .ok_or(CueFlacRangeError::ExtentOverflow)?;

        Ok(Self {
            source_path: canonical_path,
            start_sample: request.start_sample,
            end_sample: request.end_sample,
            expected_bytes,
            seek_seconds,
            trim_start_sample,
            trim_end_sample,
        })
    }

    pub fn expected_pcm_bytes(&self) -> u64 {
        self.expected_bytes
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn sample_bounds(&self) -> (u64, u64) {
        (self.start_sample, self.end_sample)
    }

    pub fn extent_verifier(&self) -> ExactPcmExtent {
        ExactPcmExtent {
            expected: self.expected_bytes,
            observed: 0,
        }
    }

    /// Check FLAC frame CRC: `explode` alone does NOT enable CRC verification.
    /// Produce the exact legacy s32le carrier over stdout, never a WAV header,
    /// transient PCM file, or a second codec's packed native-depth carrier.
    ///
    /// The `ToolRunner` supervisor must reap the child even if byte
    /// verification rejects the stream. A nonzero tool exit always fails, even
    /// when its prefix happened to contain the required number of bytes.
    pub fn command(&self) -> ToolCommand {
        let filter = format!(
            "atrim=start_sample={}:end_sample={},asetpts=N/SR/TB",
            self.trim_start_sample, self.trim_end_sample,
        );
        ToolCommand {
            binary: ToolBinary::Ffmpeg,
            args: vec![
                "-v".into(),
                "error".into(),
                "-hide_banner".into(),
                "-nostdin".into(),
                "-xerror".into(),
                "-accurate_seek".into(),
                "-ss".into(),
                self.seek_seconds.to_string(),
                "-err_detect".into(),
                "crccheck+explode".into(),
                "-i".into(),
                self.source_path.display().to_string(),
                "-map".into(),
                "0:a:0".into(),
                "-vn".into(),
                "-sn".into(),
                "-dn".into(),
                "-af".into(),
                filter,
                "-f".into(),
                "s32le".into(),
                "-c:a".into(),
                "pcm_s32le".into(),
                "pipe:1".into(),
            ],
            secret_args: vec![],
            cwd: None,
            environment_policy: CommandEnvironmentPolicy::InheritAndSet,
            env: vec![],
            timeout: std::time::Duration::from_secs(6 * 60 * 60), // S1 default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> CueFlacRangeRequest {
        CueFlacRangeRequest {
            source_path: PathBuf::from("not-needed-for-geometry.flac"),
            sample_rate: 44_100,
            channels: 2,
            source_bits_per_sample: 24,
            carrier: CueSegmentCarrier::PcmS32LeWav,
            image_samples: 1_000_001,
            start_sample: 588,
            end_sample: 589,
        }
    }

    #[test]
    fn cue_ip02_flac_checked_u128_pcm_extent_and_limits() {
        assert_eq!(checked_pcm_bytes(1, 2, 4).unwrap(), 8);
        assert_eq!(checked_pcm_bytes(588, 1, 4).unwrap(), 2352);
        assert!(matches!(
            checked_pcm_bytes(u64::MAX, u16::MAX, 8),
            Err(CueFlacRangeError::ExtentOverflow)
        ));
    }

    #[test]
    fn cue_ip02_flac_exact_extent_detects_early_eof_and_surplus() {
        let mut exact = ExactPcmExtent {
            expected: 8,
            observed: 0,
        };
        exact.observe_bytes(1).unwrap();
        exact.observe_bytes(7).unwrap();
        exact.finish().unwrap();
        let mut short = ExactPcmExtent {
            expected: 8,
            observed: 0,
        };
        short.observe_bytes(7).unwrap();
        assert!(matches!(
            short.finish(),
            Err(CueFlacRangeError::ByteCount { .. })
        ));
        let mut surplus = ExactPcmExtent {
            expected: 8,
            observed: 0,
        };
        assert!(matches!(
            surplus.observe_bytes(9),
            Err(CueFlacRangeError::ByteCount { .. })
        ));
        // Refusal doesn't mutate a verifier into a false success.
        assert!(matches!(
            surplus.finish(),
            Err(CueFlacRangeError::ByteCount { .. })
        ));
    }

    #[test]
    fn cue_ip02_flac_invalid_range_fails_before_source_open() {
        for (start, end, total) in [
            (0, 0, 10),
            (1, 1, 10),
            (5, 4, 10),
            (9, 11, 10),
            (0, 1, 0),
            (u64::MAX - 1, u64::MAX, 10),
        ] {
            let mut req = request();
            req.start_sample = start;
            req.end_sample = end;
            req.image_samples = total;
            assert!(matches!(
                CueFlacRangeDecoder::prepare(&req),
                Err(CueFlacRangeError::InvalidRange)
            ));
        }
    }

    #[test]
    fn cue_ip02_flac_unsupported_source_fails_before_source_open() {
        let mut req = request();
        for (rate, channels, depth, carrier) in [
            (44_100, 3, 24, CueSegmentCarrier::PcmS32LeWav),
            (44_100, 2, 20, CueSegmentCarrier::PcmS32LeWav),
            (192_000, 2, 24, CueSegmentCarrier::PcmS32LeWav),
            (44_100, 2, 24, CueSegmentCarrier::PcmF32LeWav),
        ] {
            req.sample_rate = rate;
            req.channels = channels;
            req.source_bits_per_sample = depth;
            req.carrier = carrier;
            assert!(matches!(
                CueFlacRangeDecoder::prepare(&req),
                Err(CueFlacRangeError::UnsupportedFormat)
            ));
        }
    }

    #[test]
    fn cue_ip02_flac_75fps_boundary_arithmetic_is_integer() {
        for rate in [44_100_u32, 48_000, 96_000] {
            let at_frame = |frame: u64| (u128::from(frame) * u128::from(rate) / 75) as u64;
            let frame_one = u64::from(rate / 75);
            assert_eq!(at_frame(1), frame_one);
            assert_eq!(at_frame(75), u64::from(rate));
            assert_eq!(at_frame(1) + 1, frame_one + 1);
            assert_eq!(at_frame(1) - 1, frame_one - 1);
        }
    }

    #[test]
    fn cue_ip02_flac_from_segment_rejects_addition_overflow() {
        let source = TrackSourceRef::CueStreamSegment {
            fallback_path: PathBuf::from("unused.wav"),
            source_image: PathBuf::from("album.flac"),
            decode_path: PathBuf::from("album.flac"),
            start_sample: u64::MAX,
            samples: 2,
            image_samples: u64::MAX,
            carrier: CueSegmentCarrier::PcmS32LeWav,
            channels: 2,
        };
        assert!(matches!(
            CueFlacRangeRequest::from_segment(&source, 44_100, 16),
            Err(CueFlacRangeError::InvalidRange)
        ));
    }

    #[test]
    fn cue_ip02_flac_streaminfo_agreement_and_command_are_typed() {
        // Build a complete 42-byte FLAC prefix with 16-bit mono 44.1 kHz,
        // two samples. The adapter never launches a decoder on this fixture.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.flac");
        let mut raw = vec![0_u8; 42];
        raw[..4].copy_from_slice(b"fLaC");
        raw[4] = 0x80; // last metadata block, STREAMINFO
        raw[7] = 34;
        let packed = ((44_100_u64) << 44) | (15_u64 << 36) | 2;
        raw[18..26].copy_from_slice(&packed.to_be_bytes());
        fs::write(&path, raw).unwrap();
        let mut req = request();
        req.source_path = path.clone();
        req.sample_rate = 44_100;
        req.channels = 1;
        req.source_bits_per_sample = 16;
        req.image_samples = 2;
        req.start_sample = 1;
        req.end_sample = 2;
        let decoder = CueFlacRangeDecoder::prepare(&req).unwrap();
        assert_eq!(decoder.expected_pcm_bytes(), 4);
        assert_eq!(decoder.sample_bounds(), (1, 2));
        assert_eq!(decoder.source_path().to_path_buf(), fs::canonicalize(&path).unwrap());
        let cmd = decoder.command();
        assert_eq!(cmd.binary, ToolBinary::Ffmpeg);
        assert!(cmd.args.windows(2).any(|v| v[0] == "-ss" && v[1] == "0"));
        assert!(cmd
            .args
            .windows(2)
            .any(|v| v[0] == "-err_detect" && v[1] == "crccheck+explode"));
        assert!(cmd
            .args
            .windows(2)
            .any(|v| v[0] == "-af" && v[1] == "atrim=start_sample=1:end_sample=2,asetpts=N/SR/TB"));
        assert_eq!(cmd.args.last().map(String::as_str), Some("pipe:1"));
        assert!(!cmd.args.windows(2).any(|v| v[0] == "-c:a" && v[1] == "copy"));
        // Large, non-frame-aligned start proves integer input seeking and
        // sample trim independently. No native FLAC decoding is claimed here.
        let total = 3_000_000_u64;
        let packed = ((44_100_u64) << 44) | (15_u64 << 36) | total;
        let mut larger = fs::read(&path).unwrap();
        larger[18..26].copy_from_slice(&packed.to_be_bytes());
        fs::write(&path, larger).unwrap();
        req.image_samples = total;
        req.start_sample = 44_100 * 17 + 589;
        req.end_sample = req.start_sample + 587;
        let decoder = CueFlacRangeDecoder::prepare(&req).unwrap();
        let args = decoder.command().args;
        assert!(args.windows(2).any(|v| v[0] == "-ss" && v[1] == "17"));
        assert!(args.windows(2).any(|v| {
            v[0] == "-af" && v[1] == "atrim=start_sample=589:end_sample=1176,asetpts=N/SR/TB"
        }));
        assert_eq!(decoder.expected_pcm_bytes(), 587 * 4);
        req.source_bits_per_sample = 24;
        assert!(matches!(
            CueFlacRangeDecoder::prepare(&req),
            Err(CueFlacRangeError::SourceFactsDrift)
        ));
    }
}
