//! Fixed-parameter, per-channel Float64 DC subtraction and conservative
//! observation. This is deliberately a *single effect*, not a DSP framework.
//!
//! The correction signal is the exact interleaved headerless Float64 carrier
//! after all registered upstream transforms (including a stereo fold). Auto
//! makes one complete-reader observation, resolves its policy, then makes at
//! most one bounded-memory processing traversal. Manual makes no observation.
//! No filter, limiter, dither, precision narrowing, or automatic gain exists
//! here; final peak/quantization policy stays with the existing terminal.

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub const POLICY_VERSION: u32 = 1;
const WINDOWS: usize = 16;
const TARGET_BUFFER_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { Auto, Manual { offset_nanounits: i64 } }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition { Apply, Negligible, Unstable, Indeterminate, ContinuousScopeUnavailable, ManualZero }

impl Disposition {
    pub fn label(self) -> &'static str {
        match self {
            Self::Apply => "offset subtracted",
            Self::Negligible => "analyzed, no change: offset negligible relative to programme level",
            Self::Unstable => "analyzed, no change: baseline varies across the recording",
            Self::Indeterminate => "analyzed, no change: recording too short or signal insufficient",
            Self::ContinuousScopeUnavailable => "analyzed, no change: a continuous CUE programme requires an album-wide estimate",
            Self::ManualZero => "manual zero: no subtraction needed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChannelResult {
    /// Observed arithmetic mean (not a diagnosis of an audio defect).
    pub observed_mean: Option<f64>,
    pub ac_rms: Option<f64>,
    pub input_peak: Option<f64>,
    pub maximum_window_deviation: Option<f64>,
    /// *Positive means subtract positive* (UI convention).
    pub subtraction: f64,
    pub disposition: Disposition,
    /// Mean and peak accumulated during the corrective write, not a third pass.
    pub output_mean: Option<f64>,
    pub output_peak: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ResultRecord {
    pub mode: Mode,
    pub frames: u64,
    pub sample_rate_hz: u32,
    pub channels: Vec<ChannelResult>,
    pub output_written: bool,
    /// SHA-256 of the exact raw interleaved Float64 bytes observed/processed.
    pub input_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NativeDcReceipt {
    pub input: PathBuf,
    pub output: PathBuf,
    pub policy_version: u32,
    /// Bound before the observation/decision completed, not projected afterward.
    pub typed_dependency: Option<super::plan_bridge::DcEffectDependency>,
    pub result: ResultRecord,
}


impl ResultRecord {
    pub fn changed(&self) -> bool {
        self.channels.iter().any(|channel| channel.disposition == Disposition::Apply)
    }
}

#[derive(Default, Clone, Copy)]
struct Compensated {
    sum: f64,
    compensation: f64,
}
impl Compensated {
    fn add(&mut self, value: f64) {
        let t = self.sum + value;
        if self.sum.abs() >= value.abs() {
            self.compensation += (self.sum - t) + value;
        } else {
            self.compensation += (value - t) + self.sum;
        }
        self.sum = t;
    }
    fn total(self) -> f64 { self.sum + self.compensation }
}

#[derive(Default, Clone, Copy)]
struct RunningChannel {
    sum: Compensated,
    mean: f64,
    m2: f64,
    peak: f64,
}

fn geometry(path: &Path, rate: u32, channels: u16) -> Result<(u64, usize), String> {
    if rate == 0 || channels == 0 || channels > 64 {
        return Err("DC correction requires valid nonzero PCM rate and 1..64 channels".into());
    }
    let bytes_per_frame = usize::from(channels) * 8;
    let length = fs::metadata(path).map_err(|e| format!("DC carrier stat failed: {e}"))?.len();
    if length == 0 || length % bytes_per_frame as u64 != 0 {
        return Err("DC carrier is empty or has an incomplete interleaved Float64 frame".into());
    }
    Ok((length / bytes_per_frame as u64, bytes_per_frame))
}

/// Iterate over whole frames, checking cancellation at each bounded chunk and
/// refusing nonfinite input instead of laundering it into a plausible result.
fn visit_f64le(
    path: &Path,
    channels: u16,
    frames: u64,
    cancel: &CancellationToken,
    mut visit: impl FnMut(u64, usize, f64) -> Result<(), String>,
) -> Result<String, String> {
    let mut sha = Sha256::new();
    let frame_bytes = usize::from(channels) * 8;
    let buffer_bytes = (TARGET_BUFFER_BYTES / frame_bytes).max(1) * frame_bytes;
    let mut buffer = vec![0_u8; buffer_bytes];
    let mut reader = BufReader::with_capacity(TARGET_BUFFER_BYTES,
        File::open(path).map_err(|e| format!("DC carrier open failed: {e}"))?);
    let mut frame = 0_u64;
    while frame < frames {
        if cancel.is_cancelled() { return Err("DC processing cancelled".into()); }
        let count = (frames - frame).min((buffer_bytes / frame_bytes) as u64) as usize;
        let used = count * frame_bytes;
        reader.read_exact(&mut buffer[..used])
            .map_err(|e| format!("DC carrier read failed: {e}"))?;
        sha.update(&buffer[..used]);
        for (local_frame, data) in buffer[..used].chunks_exact(frame_bytes).enumerate() {
            for (channel, sample_bytes) in data.chunks_exact(8).enumerate() {
                let sample = f64::from_le_bytes(sample_bytes.try_into().expect("one Float64 sample"));
                if !sample.is_finite() {
                    return Err(format!("DC input is nonfinite at frame {} channel {}", frame + local_frame as u64, channel));
                }
                visit(frame + local_frame as u64, channel, sample)?;
            }
        }
        frame += count as u64;
    }
    Ok(format!("{:x}", sha.finalize()))
}

fn observe(
    input: &Path,
    rate: u32,
    channels: u16,
    frames: u64,
    cancel: &CancellationToken,
) -> Result<(Vec<ChannelResult>, String), String> {
    let n = usize::from(channels);
    let mut stats = vec![RunningChannel::default(); n];
    let mut window_sums = vec![[Compensated::default(); WINDOWS]; n];
    let mut window_counts = [0_u64; WINDOWS];
    let digest = visit_f64le(input, channels, frames, cancel, |frame, ch, sample| {
        // Each temporal window has a deterministic sample/frame membership,
        // irrespective of file I/O block size or iterator chunking.
        let window = ((u128::from(frame) * WINDOWS as u128) / u128::from(frames)) as usize;
        if ch == 0 { window_counts[window] += 1; }
        window_sums[ch][window].add(sample);
        let s = &mut stats[ch];
        s.sum.add(sample);
        let count = (frame + 1) as f64;
        let delta = sample - s.mean;
        s.mean += delta / count;
        s.m2 += delta * (sample - s.mean);
        s.peak = s.peak.max(sample.abs());
        Ok(())
    })?;
    let minimum_frames = u64::from(rate).saturating_mul(2).max(32_768);
    let results = stats.iter().enumerate().map(|(ch, s)| {
        let mean = s.sum.total() / frames as f64;
        let ac_rms = (s.m2.max(0.0) / frames as f64).sqrt();
        let max_delta = window_counts.iter().enumerate().filter(|(_, count)| **count != 0)
            .map(|(w, count)| (window_sums[ch][w].total() / *count as f64 - mean).abs())
            .fold(0.0_f64, f64::max);
        if !mean.is_finite() || !ac_rms.is_finite() || !s.peak.is_finite()
            || !max_delta.is_finite()
        {
            return Err(format!("DC statistics exceeded finite Float64 range for channel {ch}"));
        }
        let disposition = if frames < minimum_frames || s.peak < 1e-8 || ac_rms < 1e-8 {
            Disposition::Indeterminate
        } else if mean.abs() < 1e-9 || mean.abs() / ac_rms < 0.015 {
            Disposition::Negligible
        } else if max_delta > (mean.abs() * 0.20).max(ac_rms * 0.02) {
            Disposition::Unstable
        } else {
            Disposition::Apply
        };
        Ok(ChannelResult {
            observed_mean: Some(mean), ac_rms: Some(ac_rms),
            input_peak: Some(s.peak), maximum_window_deviation: Some(max_delta),
            subtraction: if disposition == Disposition::Apply { mean } else { 0.0 },
            disposition, output_mean: None, output_peak: None,
        })
    }).collect::<Result<Vec<_>, String>>()?;
    Ok((results, digest))
}

/// Resolved, execution-authoritative fixed subtraction values. An Auto
/// instance exists only after the full typed observation has finished; the
/// physical writer accepts this immutable decision rather than a mode or a
/// hidden placeholder offset.
#[derive(Debug, Clone)]
pub struct ResolvedCorrection {
    mode: Mode,
    frames: u64,
    rate: u32,
    channels: u16,
    records: Vec<ChannelResult>,
    observed_hash: Option<String>,
}

impl ResolvedCorrection {
    pub fn changed(&self) -> bool {
        self.records.iter().any(|record| record.subtraction != 0.0)
    }
}

/// Observe the exact correction-input signal and resolve every channel's
/// fixed subtraction *before* performing any physical sample modification.
/// Manual never scans the input; only its geometry is checked.
pub fn resolve(
    input: &Path,
    rate: u32,
    channels: u16,
    mode: Mode,
    allow_auto_subtraction: bool,
    cancel: &CancellationToken,
) -> Result<ResolvedCorrection, String> {
    let (frames, _) = geometry(input, rate, channels)?;
    if cancel.is_cancelled() { return Err("DC processing cancelled".into()); }
    let (records, observed_hash) = match mode {
        Mode::Auto => {
            let (mut measured_channels, digest) = observe(input, rate, channels, frames, cancel)?;
            if !allow_auto_subtraction {
                for ch in &mut measured_channels {
                    if ch.disposition == Disposition::Apply {
                        ch.disposition = Disposition::ContinuousScopeUnavailable;
                        ch.subtraction = 0.0;
                    }
                }
            }
            (measured_channels, Some(digest))
        },
        Mode::Manual { offset_nanounits } => {
            if !(-1_000_000_000..=1_000_000_000).contains(&offset_nanounits) {
                return Err("manual DC offset is outside normalized amplitude bounds".into());
            }
            let subtraction = offset_nanounits as f64 / 1_000_000_000.0;
            ((0..channels).map(|_| ChannelResult {
                observed_mean: None, ac_rms: None, input_peak: None,
                maximum_window_deviation: None, subtraction,
                disposition: if subtraction == 0.0 { Disposition::ManualZero } else { Disposition::Apply },
                output_mean: None, output_peak: None,
            }).collect(), None)
        }
    };
    Ok(ResolvedCorrection { mode, frames, rate, channels, records, observed_hash })
}

/// Apply exactly the values from a previously completed observation/decision.
/// Recheck content identity during the write to refuse stale observations.
/// No backend filter, integer format conversion, limiter, or dither is used.
pub fn apply_resolved(
    input: &Path,
    output: &Path,
    resolved: ResolvedCorrection,
    cancel: &CancellationToken,
) -> Result<ResultRecord, String> {
    if input == output { return Err("DC input and output must be distinct staging paths".into()); }
    let ResolvedCorrection { mode, frames, rate, channels, mut records, observed_hash } = resolved;
    let (actual_frames, frame_bytes) = geometry(input, rate, channels)?;
    if actual_frames != frames {
        return Err("DC correction-input frame count changed since its decision".into());
    }
    if cancel.is_cancelled() { return Err("DC processing cancelled".into()); }
    let any_subtraction = records.iter().any(|record| record.subtraction != 0.0);
    // Auto skips still require content-identity verification, but avoid a
    // corrective sample write. These staging carriers are read-only afterward.
    if !any_subtraction {
        // A skipped Auto decision still describes a specific observed carrier.
        // Verify that identity before publishing the no-change result; Manual
        // zero has no observation and retains its zero-traversal fast path.
        if let Some(expected_hash) = observed_hash.as_deref() {
            let actual_hash = visit_f64le(input, channels, frames, cancel, |_, _, _| Ok(()))?;
            if actual_hash != expected_hash {
                return Err("DC input changed after automatic no-op decision; refusing stale observation".into());
            }
        }
        let input_digest = observed_hash;
        if cancel.is_cancelled() { return Err("DC processing cancelled".into()); }
        if fs::hard_link(input, output).is_err() {
            fs::copy(input, output).map_err(|e| format!("DC unchanged carrier copy failed: {e}"))?;
        }
        return Ok(ResultRecord { mode, frames, sample_rate_hz: rate, channels: records,
            output_written: false, input_sha256: input_digest });
    }

    let partial_path = output.with_extension("dcshift-writing.tmp");
    let _ = fs::remove_file(&partial_path);
    let mut realized_input_hash = String::new();
    let process = (|| -> Result<(), String> {
        let mut reader = BufReader::with_capacity(TARGET_BUFFER_BYTES,
            File::open(input).map_err(|e| format!("DC processing input failed: {e}"))?);
        let mut writer = BufWriter::with_capacity(TARGET_BUFFER_BYTES,
            OpenOptions::new().write(true).create_new(true).open(&partial_path)
                .map_err(|e| format!("DC processing output failed: {e}"))?);
        let chunk_bytes = (TARGET_BUFFER_BYTES / frame_bytes).max(1) * frame_bytes;
        let mut buf = vec![0_u8; chunk_bytes];
        let mut input_sha = Sha256::new();
        let mut sums = vec![Compensated::default(); usize::from(channels)];
        let mut peaks = vec![0.0_f64; usize::from(channels)];
        let mut remaining = frames;
        while remaining != 0 {
            if cancel.is_cancelled() { return Err("DC subtraction cancelled".into()); }
            let count = remaining.min((chunk_bytes / frame_bytes) as u64);
            let used = count as usize * frame_bytes;
            reader.read_exact(&mut buf[..used]).map_err(|e| format!("DC subtraction input read failed: {e}"))?;
            input_sha.update(&buf[..used]);
            for frame in buf[..used].chunks_exact_mut(frame_bytes) {
                for (channel, slot) in frame.chunks_exact_mut(8).enumerate() {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(slot);
                    let sample = f64::from_le_bytes(bytes);
                    if !sample.is_finite() { return Err("nonfinite DC input encountered".into()); }
                    let changed = sample - records[channel].subtraction;
                    if !changed.is_finite() { return Err("DC subtraction produced nonfinite output".into()); }
                    slot.copy_from_slice(&changed.to_le_bytes());
                    sums[channel].add(changed);
                    peaks[channel] = peaks[channel].max(changed.abs());
                }
            }
            writer.write_all(&buf[..used]).map_err(|e| format!("DC subtraction output write failed: {e}"))?;
            remaining -= count;
        }
        realized_input_hash = format!("{:x}", input_sha.finalize());
        if observed_hash.as_deref().is_some_and(|digest| digest != realized_input_hash) {
            return Err("DC input changed between measurement and correction; refusing stale subtraction".into());
        }
        writer.flush().map_err(|e| format!("DC output flush failed: {e}"))?;
        writer.get_ref().sync_all().map_err(|e| format!("DC output sync failed: {e}"))?;
        for (channel, result) in records.iter_mut().enumerate() {
            result.output_mean = Some(sums[channel].total() / frames as f64);
            result.output_peak = Some(peaks[channel]);
        }
        Ok(())
    })();
    if let Err(error) = process {
        let _ = fs::remove_file(&partial_path);
        return Err(error);
    }
    if cancel.is_cancelled() {
        let _ = fs::remove_file(&partial_path);
        return Err("DC correction cancelled before committing carrier".into());
    }
    if let Err(error) = fs::rename(&partial_path, output) {
        let _ = fs::remove_file(&partial_path);
        return Err(format!("DC carrier commit failed: {error}"));
    }
    Ok(ResultRecord { mode, frames, sample_rate_hz: rate, channels: records,
        output_written: true, input_sha256: Some(realized_input_hash) })
}

/// Compatibility entry point for focused numeric tests. Production planning
/// calls resolve() and apply_resolved() separately, preserving the typed
/// observation -> decision -> physical operation dependency.
pub fn run(
    input: &Path,
    output: &Path,
    rate: u32,
    channels: u16,
    mode: Mode,
    cancel: &CancellationToken,
) -> Result<ResultRecord, String> {
    run_scoped(input, output, rate, channels, mode, true, cancel)
}

pub fn run_scoped(
    input: &Path,
    output: &Path,
    rate: u32,
    channels: u16,
    mode: Mode,
    allow_auto_subtraction: bool,
    cancel: &CancellationToken,
) -> Result<ResultRecord, String> {
    let decision = resolve(input, rate, channels, mode, allow_auto_subtraction, cancel)?;
    apply_resolved(input, output, decision, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn write_pcm(path: &Path, channels: usize, frames: usize,
        make: impl Fn(usize, usize) -> f64) {
        let mut file = File::create(path).unwrap();
        for f in 0..frames { for ch in 0..channels {
            file.write_all(&make(f, ch).to_le_bytes()).unwrap();
        }}
    }
    fn read_pcm(path: &Path) -> Vec<f64> {
        fs::read(path).unwrap().chunks_exact(8)
            .map(|bytes| f64::from_le_bytes(bytes.try_into().unwrap())).collect()
    }
    #[test]
    fn resolved_auto_decision_is_authoritative_before_physical_subtraction() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.f64le");
        let output = temp.path().join("corrected.f64le");
        write_pcm(&input, 3, 144_000, |frame, channel| {
            0.025 * (TAU * (frame % 128) as f64 / 128.0).sin()
                + [0.003, -0.007, 0.012][channel]
        });
        let cancel = CancellationToken::new();
        let decision = resolve(&input, 48_000, 3, Mode::Auto, true, &cancel).unwrap();
        assert!(!output.exists(), "observation/decision cannot write samples");
        assert_eq!(decision.records.len(), 3);
        for (channel, expected) in [0.003, -0.007, 0.012].iter().enumerate() {
            assert_eq!(decision.records[channel].disposition, Disposition::Apply);
            assert!((decision.records[channel].subtraction - *expected).abs() < 1e-11);
        }
        let before = read_pcm(&input);
        let outcome = apply_resolved(&input, &output, decision, &cancel).unwrap();
        assert!(outcome.changed());
        for (frame, after) in read_pcm(&output).into_iter().enumerate() {
            assert!((before[frame] - [0.003, -0.007, 0.012][frame % 3] - after).abs() < 1e-12);
        }
    }

    #[test]
    fn resolved_decision_rejects_changed_carrier_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("in.f64le");
        let output = temp.path().join("out.f64le");
        write_pcm(&input, 1, 144_000, |frame, _| {
            0.02 * (TAU * (frame % 160) as f64 / 160.0).sin() + 0.007
        });
        let cancel = CancellationToken::new();
        let decision = resolve(&input, 48_000, 1, Mode::Auto, true, &cancel).unwrap();
        assert!(decision.changed());
        let mut original = fs::read(&input).unwrap();
        original[1000] ^= 1;
        fs::write(&input, original).unwrap();
        assert!(apply_resolved(&input, &output, decision, &cancel).is_err());
        assert!(!output.exists(), "stale observation must never publish samples");
    }

    #[test]
    fn skipped_auto_decision_rejects_changed_input_without_claiming_observation() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("in.f64le");
        let output = temp.path().join("out.f64le");
        write_pcm(&input, 1, 144_000, |frame, _| {
            0.04 * (TAU * (frame % 160) as f64 / 160.0).sin()
        });
        let cancel = CancellationToken::new();
        let decision = resolve(&input, 48_000, 1, Mode::Auto, true, &cancel).unwrap();
        assert!(!decision.changed());
        let mut raw = fs::read(&input).unwrap();
        raw[1000] ^= 1;
        fs::write(&input, raw).unwrap();
        assert!(apply_resolved(&input, &output, decision, &cancel).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn manual_sign_and_stereo_physical_samples_no_scan() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.f64le");
        let output = temp.path().join("output.f64le");
        write_pcm(&input, 2, 256, |f, ch| f as f64 / 8192.0 + ch as f64 / 100.0);
        let result = run(&input, &output, 48_000, 2,
            Mode::Manual { offset_nanounits: 3_000_000 }, &CancellationToken::new()).unwrap();
        assert!(result.output_written);
        assert!(result.channels.iter().all(|r| r.observed_mean.is_none()));
        let src = read_pcm(&input);
        let dst = read_pcm(&output);
        for (a,b) in src.iter().zip(dst) { assert!((a - 0.003 - b).abs() < 1e-14); }
    }
    #[test]
    fn independent_channel_biases_and_no_integer_clamping() {
        let temp = tempfile::tempdir().unwrap();
        let (input,output) = (temp.path().join("in.f64le"), temp.path().join("out.f64le"));
        let frames=160_000;
        write_pcm(&input, 6, frames, |f,ch| {
            let phase=TAU * ((f%160) as f64)/160.0;
            (ch as f64 - 2.0)*0.004 + phase.sin() * 0.025
        });
        let res=run(&input,&output,48_000,6,Mode::Auto,&CancellationToken::new()).unwrap();
        assert_eq!(res.channels[2].disposition, Disposition::Negligible);
        assert!(res.channels.iter().enumerate().all(|(ch, r)|
            ch == 2 || r.disposition == Disposition::Apply));
        for (ch,r) in res.channels.iter().enumerate() {
            assert!((r.subtraction - (ch as f64 - 2.0)*0.004).abs()<1e-10);
            assert!(r.output_mean.unwrap().abs()<1e-10);
        }
    }
    #[test]
    fn nonzero_programme_mean_and_baseline_drift_do_not_force_a_shift() {
        let temp = tempfile::tempdir().unwrap();
        let frames=192_000;
        let a=temp.path().join("asymmetric.f64le");
        let b=temp.path().join("drift.f64le");
        write_pcm(&a,1,frames,|f,_| (TAU*(f as f64)/(frames as f64)*0.5).sin()*0.3);
        write_pcm(&b,1,frames,|f,_| 0.015*(f as f64/frames as f64) +
            0.025*(TAU*(f%160) as f64/160.0).sin());
        for source in [&a,&b] {
            let output=source.with_extension("out");
            let res=run(source,&output,48_000,1,Mode::Auto,&CancellationToken::new()).unwrap();
            assert!(!res.changed());
            assert_eq!(fs::read(source).unwrap(), fs::read(&output).unwrap());
        }
    }
    #[test]
    fn silence_and_short_clips_are_indeterminate() {
        let temp=tempfile::tempdir().unwrap();
        let input=temp.path().join("in.f64le");
        let out=temp.path().join("out.f64le");
        write_pcm(&input,1,96000,|_,_|0.001);
        let result=run(&input,&out,48000,1,Mode::Auto,&CancellationToken::new()).unwrap();
        assert_eq!(result.channels[0].disposition,Disposition::Indeterminate);
        write_pcm(&input,1,256,|f,_|f as f64/10_000.0+0.01);
        let short=run(&input,&out.with_extension("short"),48000,1,Mode::Auto,&CancellationToken::new()).unwrap();
        assert_eq!(short.channels[0].disposition,Disposition::Indeterminate);
    }
    #[test]
    fn contiguous_cue_scope_avoids_boundary_steps_independent_tracks_can_correct() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("continuous_cue_member.f64le");
        write_pcm(&input, 2, 192_000, |f, channel| {
            0.02 * (TAU * (f % 160) as f64 / 160.0).sin()
                + if channel == 0 { 0.01 } else { -0.008 }
        });
        let preserved = temp.path().join("continuous-no-change.f64le");
        let result = run_scoped(&input, &preserved, 48_000, 2,
            Mode::Auto, false, &CancellationToken::new()).unwrap();
        assert!(!result.changed());
        assert_eq!(result.channels[0].disposition, Disposition::ContinuousScopeUnavailable);
        assert_eq!(result.channels[1].disposition, Disposition::ContinuousScopeUnavailable);
        assert_eq!(fs::read(&input).unwrap(), fs::read(&preserved).unwrap());
        let corrected = temp.path().join("independent.f64le");
        let independent = run_scoped(&input, &corrected, 48_000, 2,
            Mode::Auto, true, &CancellationToken::new()).unwrap();
        assert!(independent.changed());
        let output = read_pcm(&corrected);
        assert!((output[0] - (read_pcm(&input)[0] - 0.01)).abs() < 1e-8);
    }

    #[test]
    fn cancelled_measurement_never_publishes() {
        let temp=tempfile::tempdir().unwrap();
        let input=temp.path().join("in.f64le");
        let out=temp.path().join("out.f64le");
        write_pcm(&input,1,128,|_,_|0.1);
        let token=CancellationToken::new();token.cancel();
        assert!(run(&input,&out,48000,1,Mode::Auto,&token).is_err());
        assert!(!out.exists());
    }
}
