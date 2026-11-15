//! PCM container writer for the native disc extraction path (#88).
//!
//! Decode to a private raw 16-/24-bit integer stream and write a single
//! container whose header declares the *source* sample rate, channel count
//! and valid bits. The in-memory state is bounded, regardless of disc size.
//! No seek-through-the-audio or whole-file read is needed, including RF64/W64.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tokio_util::sync::CancellationToken;

use super::native_extract::NativeWavContainer;

const PCM_GUID: [u8; 16] = [1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
const W64_RIFF: [u8; 16] = [114,105,102,102,46,145,207,17,165,214,40,219,4,193,0,0];
const W64_WAVE: [u8; 16] = [119,97,118,101,243,172,211,17,140,209,0,192,79,142,219,138];
const W64_FMT: [u8; 16]  = [102,109,116,32,243,172,211,17,140,209,0,192,79,142,219,138];
const W64_DATA: [u8; 16] = [100,97,116,97,243,172,211,17,140,209,0,192,79,142,219,138];

#[derive(Debug, Clone, Copy)]
pub(super) struct PcmInfo {
    pub rate: u32,
    pub channels: u16,
    pub mask: Option<u32>,
}

/// Read no more than the 40-byte `fmt ` payload of a realized WAV. Seek over
/// arbitrarily large chunks rather than allocating or scanning audio samples.
pub(super) fn inspect_carrier(path: &Path) -> Result<PcmInfo, String> {
    let mut file = File::open(path).map_err(|e| format!("Open carrier: {e}"))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len < 44 { return Err("PCM carrier has a truncated WAV header".into()); }
    let mut head = [0u8; 12];
    file.read_exact(&mut head).map_err(|e| e.to_string())?;
    if &head[8..12] != b"WAVE" || (&head[0..4] != b"RIFF" && &head[0..4] != b"RF64") {
        return Err("Native carrier is not a RIFF/RF64 WAVE".into());
    }
    let mut offset = 12u64;
    let mut fmt: Option<PcmInfo> = None;
    let mut found_data = false;
    while offset.checked_add(8).is_some_and(|end| end <= len) {
        file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let mut ch = [0u8; 8];
        file.read_exact(&mut ch).map_err(|e| e.to_string())?;
        let size = u64::from(u32::from_le_bytes(ch[4..8].try_into().unwrap()));
        let body = offset.checked_add(8).ok_or("WAV chunk offset overflow")?;
        if &ch[..4] == b"fmt " {
            if fmt.is_some() || size < 16 || size > 256 || body.checked_add(size).is_none_or(|end| end > len) {
                return Err("Invalid PCM WAV fmt chunk".into());
            }
            let mut bytes = [0u8; 40];
            let count = usize::try_from(size.min(40)).map_err(|e| e.to_string())?;
            file.read_exact(&mut bytes[..count]).map_err(|e| e.to_string())?;
            let tag = u16::from_le_bytes([bytes[0],bytes[1]]);
            if !matches!(tag, 1 | 0xFFFE) { return Err("Native carrier is not integer PCM".into()); }
            if tag == 0xFFFE && (size < 40 || bytes[24..40] != PCM_GUID) {
                return Err("Native extensible PCM carrier has an invalid subformat".into());
            }
            let channels = u16::from_le_bytes([bytes[2],bytes[3]]);
            let rate = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            if channels == 0 || channels > 32 || !(8000..=768_000).contains(&rate) {
                return Err("Invalid carrier channel count or sample rate".into());
            }
            let mask = if tag == 0xFFFE && size >= 40 {
                Some(u32::from_le_bytes(bytes[20..24].try_into().unwrap()))
            } else { None };
            fmt = Some(PcmInfo { rate, channels, mask });
        }
        if &ch[..4] == b"data" { found_data = true; break; }
        let next = body.checked_add(size).and_then(|end| end.checked_add(size & 1))
            .ok_or("WAV chunk length overflow")?;
        if next > len { return Err("WAV chunk exceeds actual file size".into()); }
        offset = next;
    }
    if !found_data { return Err("PCM carrier has no audio data chunk".into()); }
    fmt.ok_or_else(|| "PCM carrier has no format chunk".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NativePcmFormat {
    pub rate: u32,
    pub channels: u16,
    pub container_bits: u16,
    pub valid_bits: u16,
    pub channel_mask: u32,
}

impl NativePcmFormat {
    pub fn block_align(self) -> Result<u16, String> {
        if self.channels == 0 || self.channels > 32 || !matches!(self.container_bits, 16 | 24)
            || !matches!(self.valid_bits, 16 | 20 | 24) || self.valid_bits > self.container_bits {
            return Err("Unsupported native PCM format".into());
        }
        self.channels.checked_mul(self.container_bits / 8).ok_or("PCM block alignment overflow".into())
    }
    pub fn byte_rate(self) -> Result<u32, String> {
        self.rate.checked_mul(u32::from(self.block_align()?)).ok_or("PCM byte rate overflow".into())
    }
}

/// Preallocate an RF64 carrier header for native-only DVD/Blu-ray LPCM
/// realization. Convert still uses its original RIFF header. Every rewrite
/// has the same byte offset, regardless of whether audio exceeds 4 GiB.
/// Unlike the completed-file writer, this accepts an unknown final length on
/// first write and uses the sample count when finalizing the temporary file.
#[derive(Debug, Clone, Copy)]
pub(super) struct NativeCarrierFormat {
    pub rate: u32,
    pub channels: u16,
    pub container_bits: u16,
    pub valid_bits: u16,
    pub channel_mask: Option<u32>,
    pub extensible: bool,
}

pub(super) fn write_native_rf64_carrier_header(
    output: &mut File, fmt: NativeCarrierFormat, bytes: u64,
) -> Result<(), String> {
    let align = fmt.channels.checked_mul(fmt.container_bits / 8)
        .filter(|align| *align != 0).ok_or("RF64 carrier block alignment overflow")?;
    if bytes % u64::from(align) != 0 {
        return Err("RF64 carrier data is not sample-frame aligned".into());
    }
    let byte_rate = fmt.rate.checked_mul(u32::from(align))
        .ok_or("RF64 carrier byte-rate overflow")?;
    if fmt.rate == 0 || fmt.channels == 0 || fmt.valid_bits == 0
        || fmt.valid_bits > fmt.container_bits
        || !matches!(fmt.container_bits, 16 | 24 | 32) {
        return Err("Invalid RF64 carrier sample format".into());
    }
    let format_bytes = if fmt.extensible { 40u64 } else { 16u64 };
    let padding = bytes & 1;
    let size = bytes.checked_add(56 + format_bytes + padding)
        .ok_or("RF64 carrier size overflow")?;
    output.write_all(b"RF64\xff\xff\xff\xffWAVEds64")
        .map_err(|e|e.to_string())?;
    output.write_all(&28u32.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&size.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&bytes.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&(bytes / u64::from(align)).to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&0u32.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(b"fmt ").map_err(|e|e.to_string())?;
    output.write_all(&(format_bytes as u32).to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&(if fmt.extensible { 0xfffeu16 } else { 1u16 }).to_le_bytes())
        .map_err(|e|e.to_string())?;
    output.write_all(&fmt.channels.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&fmt.rate.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&byte_rate.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&align.to_le_bytes()).map_err(|e|e.to_string())?;
    output.write_all(&fmt.container_bits.to_le_bytes()).map_err(|e|e.to_string())?;
    if fmt.extensible {
        output.write_all(&22u16.to_le_bytes()).map_err(|e|e.to_string())?;
        output.write_all(&fmt.valid_bits.to_le_bytes()).map_err(|e|e.to_string())?;
        output.write_all(&fmt.channel_mask.unwrap_or(0).to_le_bytes()).map_err(|e|e.to_string())?;
        output.write_all(&PCM_GUID).map_err(|e|e.to_string())?;
    }
    output.write_all(b"data\xff\xff\xff\xff").map_err(|e|e.to_string())?;
    Ok(())
}

fn put_fmt<W: Write>(w: &mut W, f: NativePcmFormat) -> io::Result<()> {
    // Write extensible even for mono/stereo: valid depth and speaker assignment
    // are explicit for every output, including 20-in-24.
    w.write_all(&0xFFFEu16.to_le_bytes())?;
    w.write_all(&f.channels.to_le_bytes())?;
    w.write_all(&f.rate.to_le_bytes())?;
    w.write_all(&f.byte_rate().map_err(io_other)?.to_le_bytes())?;
    w.write_all(&f.block_align().map_err(io_other)?.to_le_bytes())?;
    w.write_all(&f.container_bits.to_le_bytes())?;
    w.write_all(&22u16.to_le_bytes())?;
    w.write_all(&f.valid_bits.to_le_bytes())?;
    w.write_all(&f.channel_mask.to_le_bytes())?;
    w.write_all(&PCM_GUID)?;
    Ok(())
}
fn io_other(msg: String) -> io::Error { io::Error::new(io::ErrorKind::InvalidInput, msg) }
fn write_header(out: &mut File, f: NativePcmFormat, bytes: u64, container: NativeWavContainer) -> Result<(u64, u64), String> {
    let pad2 = bytes & 1;
    let pad8 = (8 - bytes % 8) % 8;
    let chosen = match container {
        NativeWavContainer::Automatic => {
            if bytes.checked_add(60 + pad2).is_some_and(|total| total <= u64::from(u32::MAX)) {
                NativeWavContainer::Riff
            } else { NativeWavContainer::Rf64 }
        }
        other => other,
    };
    let data_offset;
    let tail_pad;
    match chosen {
        NativeWavContainer::Riff => {
            let size = bytes.checked_add(60 + pad2).ok_or("RIFF size overflow")?;
            let riff_size = u32::try_from(size).map_err(|_| "Selected RIFF WAV exceeds 4 GiB; use automatic or RF64".to_string())?;
            let data_size = u32::try_from(bytes).map_err(|_| "Selected RIFF WAV has an oversized data chunk".to_string())?;
            out.write_all(b"RIFF").map_err(|e| e.to_string())?;
            out.write_all(&riff_size.to_le_bytes()).map_err(|e| e.to_string())?;
            out.write_all(b"WAVEfmt ").map_err(|e| e.to_string())?;
            out.write_all(&40u32.to_le_bytes()).map_err(|e| e.to_string())?;
            put_fmt(out,f).map_err(|e| e.to_string())?;
            out.write_all(b"data").map_err(|e| e.to_string())?;
            out.write_all(&data_size.to_le_bytes()).map_err(|e| e.to_string())?;
            data_offset = 68; tail_pad = pad2;
        }
        NativeWavContainer::Rf64 => {
            let size = bytes.checked_add(104 + pad2).ok_or("RF64 size overflow")?;
            out.write_all(b"RF64\xff\xff\xff\xffWAVEds64").map_err(|e| e.to_string())?;
            out.write_all(&28u32.to_le_bytes()).map_err(|e| e.to_string())?;
            out.write_all(&(size - 8).to_le_bytes()).map_err(|e| e.to_string())?;
            out.write_all(&bytes.to_le_bytes()).map_err(|e| e.to_string())?;
            out.write_all(&(bytes / u64::from(f.block_align()?)).to_le_bytes()).map_err(|e| e.to_string())?;
            out.write_all(&0u32.to_le_bytes()).map_err(|e| e.to_string())?;
            out.write_all(b"fmt ").map_err(|e| e.to_string())?;
            out.write_all(&40u32.to_le_bytes()).map_err(|e| e.to_string())?;
            put_fmt(out,f).map_err(|e| e.to_string())?;
            out.write_all(b"data\xff\xff\xff\xff").map_err(|e| e.to_string())?;
            data_offset = 104; tail_pad = pad2;
        }
        NativeWavContainer::W64 => {
            let size = bytes.checked_add(128 + pad8).ok_or("W64 size overflow")?;
            out.write_all(&W64_RIFF).map_err(|e| e.to_string())?;
            out.write_all(&size.to_le_bytes()).map_err(|e| e.to_string())?;
            out.write_all(&W64_WAVE).map_err(|e| e.to_string())?;
            out.write_all(&W64_FMT).map_err(|e| e.to_string())?;
            out.write_all(&64u64.to_le_bytes()).map_err(|e| e.to_string())?;
            put_fmt(out,f).map_err(|e| e.to_string())?;
            out.write_all(&W64_DATA).map_err(|e| e.to_string())?;
            out.write_all(&(bytes + 24).to_le_bytes()).map_err(|e| e.to_string())?;
            data_offset = 128; tail_pad = pad8;
        }
        NativeWavContainer::Automatic => unreachable!(),
    }
    Ok((data_offset, tail_pad))
}

/// Writes only inside working storage. The output must not exist on entry.
/// Raw PCM is kept in a separate private file so its length is known before
/// writing any header, removing all 32-bit-size wrap risk.
#[cfg_attr(not(test), allow(dead_code))] // Non-controlled compatibility wrapper used by tests.
pub(super) fn package_raw_pcm(
    raw: &Path, output: &Path, format: NativePcmFormat,
    container: NativeWavContainer, cancel: &CancellationToken,
) -> Result<(), String> {
    package_raw_pcm_checked(raw, output, format, container, cancel, || Ok(()))
}

/// Extraction-only pause checkpoint. Ordinary callers retain the original
/// signature and do not pay for any extra shared-state polling.
pub(super) fn package_raw_pcm_checked(
    raw: &Path, output: &Path, format: NativePcmFormat,
    container: NativeWavContainer, cancel: &CancellationToken,
    mut checkpoint: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let size = fs::metadata(raw).map_err(|e| format!("Raw PCM not found: {e}"))?.len();
    let align = u64::from(format.block_align()?);
    if size == 0 || size % align != 0 { return Err("Raw audio is empty or has an incomplete sample frame".into()); }
    let mut input = File::open(raw).map_err(|e| e.to_string())?;
    let mut output_file = OpenOptions::new().write(true).create_new(true).open(output)
        .map_err(|e| format!("Create private WAV: {e}"))?;
    let (header_bytes, padding) = write_header(&mut output_file,format,size,container)?;
    if output_file.stream_position().map_err(|e| e.to_string())? != header_bytes {
        return Err("Native WAV header byte count mismatch".into());
    }
    let chunk_bytes = 1024 * 1024 / align as usize * align as usize;
    let mut buffer = vec![0u8; chunk_bytes];
    let mut remaining = size;
    while remaining != 0 {
        checkpoint()?;
        if cancel.is_cancelled() { return Err("Extraction cancelled while packaging PCM".into()); }
        let want = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|e|e.to_string())?;
        input.read_exact(&mut buffer[..want]).map_err(|e| format!("Read raw PCM: {e}"))?;
        if format.valid_bits == 20 {
            // WAVEFORMATEXTENSIBLE stores 20 significant bits left-aligned
            // in a 24-bit container. Claiming 20 bits while carrying nonzero
            // padding bits is not lossless. Fail instead of fabricating depth.
            let frame_aligned = want - want % align as usize;
            for sample in buffer[..frame_aligned].chunks_exact(3) {
                if sample[0] & 0x0f != 0 {
                    return Err("20-bit source has nonzero padding bits after decoding; refusing to label it 20-bit".into());
                }
            }
        }
        output_file.write_all(&buffer[..want]).map_err(|e| format!("Write native WAV: {e}"))?;
        remaining -= want as u64;
    }
    checkpoint()?;
    output_file.write_all(&[0; 8][..padding as usize]).map_err(|e| e.to_string())?;
    output_file.sync_all().map_err(|e| e.to_string())?;
    let actual = output_file.metadata().map_err(|e|e.to_string())?.len();
    if actual != header_bytes + size + padding { return Err("Native PCM publication refused: size mismatch".into()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> NativePcmFormat {
        NativePcmFormat { rate: 96000, channels: 2, container_bits: 24, valid_bits: 20, channel_mask: 3 }
    }
    #[test]
    fn riff_20bit_explicit_valid_depth_and_chunk_lengths() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("samples.pcm");
        fs::write(&raw, vec![0u8; 30]).unwrap();
        let wave = dir.path().join("samples.wav");
        package_raw_pcm(&raw, &wave, sample(),NativeWavContainer::Riff,&CancellationToken::new()).unwrap();
        let bytes=fs::read(&wave).unwrap();
        assert_eq!(&bytes[..4],b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len()-8);
        assert_eq!(u16::from_le_bytes(bytes[38..40].try_into().unwrap()), 20);
        assert_eq!(&bytes[60..64],b"data");
        assert_eq!(bytes.len(),98);
    }
    #[test]
    fn rf64_explicit_64bit_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("samples.pcm");
        fs::write(&raw, vec![0u8; 30]).unwrap();
        let wave = dir.path().join("samples.wav");
        package_raw_pcm(&raw, &wave,sample(),NativeWavContainer::Rf64,&CancellationToken::new()).unwrap();
        let bytes=fs::read(&wave).unwrap();
        assert_eq!(&bytes[..4],b"RF64");
        assert_eq!(u64::from_le_bytes(bytes[20..28].try_into().unwrap()),(bytes.len()-8) as u64);
        assert_eq!(u64::from_le_bytes(bytes[28..36].try_into().unwrap()),30);
        assert_eq!(&bytes[96..100],b"data");
        assert_eq!(bytes.len(),134);
    }
    #[test]
    fn wave64_chunks_are_eight_byte_aligned() {
        let dir=tempfile::tempdir().unwrap();
        let raw=dir.path().join("samples.pcm");
        fs::write(&raw,vec![0u8; 30]).unwrap();
        let wave=dir.path().join("samples.w64");
        package_raw_pcm(&raw,&wave,sample(),NativeWavContainer::W64,&CancellationToken::new()).unwrap();
        let bytes=fs::read(&wave).unwrap();
        assert_eq!(&bytes[..16],&W64_RIFF);
        assert_eq!(u64::from_le_bytes(bytes[16..24].try_into().unwrap()),bytes.len() as u64);
        assert_eq!(&bytes[104..120],&W64_DATA);
        assert_eq!(bytes.len()%8,0);
    }
    #[test]
    fn rf64_native_intermediate_sparse_above_four_gib_is_valid_and_bounded() {
        use super::super::bluray_wav_validate::read_wav_info;
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("sparse-native.rf64.wav");
        let mut file = File::create(&wav).unwrap();
        let f = NativeCarrierFormat {
            rate: 96000, channels: 6, container_bits: 24,
            valid_bits: 24, channel_mask: Some(0x3f), extensible: true,
        };
        // Both sizes are whole 18-byte six-channel sample frames. A sparse
        // file tests the full parser/size math without allocating audio RAM.
        let length = ((1u64 << 32) / 18 + 1) * 18;
        write_native_rf64_carrier_header(&mut file, f, length).unwrap();
        file.set_len(104 + length).unwrap();
        drop(file);
        let observed = read_wav_info(&wav).unwrap();
        assert_eq!(observed.data_size, length);
        assert_eq!(observed.channel_mask, Some(0x3f));
        assert_eq!(observed.channels, 6);
        assert_eq!(observed.sample_rate, 96000);
        assert_eq!(observed.valid_bits_per_sample, Some(24));
        assert_eq!(inspect_carrier(&wav).unwrap().mask,Some(0x3f));
    }
    #[test]
    fn native_intermediate_rejects_float_pretending_to_be_pcm() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("float-carrier.wav");
        let mut file = File::create(&wav).unwrap();
        write_native_rf64_carrier_header(&mut file, NativeCarrierFormat {
            rate: 48000, channels: 2, container_bits: 32, valid_bits: 32,
            channel_mask: None, extensible: false,
        }, 0).unwrap();
        file.seek(SeekFrom::Start(56)).unwrap();
        file.write_all(&3u16.to_le_bytes()).unwrap(); // WAVE_FORMAT_IEEE_FLOAT
        assert!(inspect_carrier(&wav).unwrap_err().contains("integer PCM"));
    }
    #[test]
    fn explicit_riff_refuses_oversize_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("too-big.wav");
        let mut file = File::create(&path).unwrap();
        let beyond = ((1u64 << 32) / 6 + 1) * 6;
        let error = write_header(&mut file, sample(), beyond, NativeWavContainer::Riff).unwrap_err();
        assert!(error.contains("exceeds") || error.contains("too large"));
    }
    #[test]
    fn reject_false_20_bit_claim_with_nonzero_padding() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("samples.pcm");
        let mut samples = vec![0u8; 30];
        samples[0] = 1;
        fs::write(&raw, samples).unwrap();
        let wave = dir.path().join("samples.wav");
        let error = package_raw_pcm(&raw, &wave, sample(), NativeWavContainer::Automatic,
            &CancellationToken::new()).unwrap_err();
        assert!(error.contains("nonzero padding bits"));
        // The incomplete file remains only in temporary working storage;
        // the publication path must never be reached after this error.
    }

    #[test]
    fn reject_partial_sample_frames_without_creating_file() {
        let dir=tempfile::tempdir().unwrap();
        let raw=dir.path().join("samples.pcm");
        fs::write(&raw,vec![0u8; 31]).unwrap();
        let wave=dir.path().join("samples.wav");
        assert!(package_raw_pcm(&raw,&wave,sample(),NativeWavContainer::Automatic,&CancellationToken::new()).is_err());
        assert!(!wave.exists());
    }
}

/// Validate a *generated* PCM WAV (the native writer's fixed RIFF/RF64/W64
/// headers) and locate its sample payload without reading it into memory.
/// This is deliberately not a general-purpose WAV parser.
fn generated_wave_span(path: &Path) -> Result<(NativePcmFormat, u64, u64), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut bytes = [0u8; 128];
    file.read_exact(&mut bytes[..(len.min(128) as usize)])
        .map_err(|e| format!("Native PCM header is truncated: {e}"))?;
    let (fmt_start, data_start, data_size, pad) = if &bytes[0..4] == b"RIFF" && len >= 68 {
        let declared = u64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap())) + 8;
        let size = u64::from(u32::from_le_bytes(bytes[64..68].try_into().unwrap()));
        if declared != len || &bytes[12..16] != b"fmt " || &bytes[60..64] != b"data" {
            return Err("Invalid generated RIFF header".into());
        }
        (20usize, 68u64, size, size & 1)
    } else if &bytes[0..4] == b"RF64" && len >= 104 {
        let declared = u64::from_le_bytes(bytes[20..28].try_into().unwrap()) + 8;
        let size = u64::from_le_bytes(bytes[28..36].try_into().unwrap());
        if declared != len || &bytes[48..52] != b"fmt " || &bytes[96..100] != b"data" {
            return Err("Invalid generated RF64 header".into());
        }
        (56usize, 104u64, size, size & 1)
    } else if &bytes[..16] == &W64_RIFF && len >= 128 {
        let declared = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let chunk = u64::from_le_bytes(bytes[120..128].try_into().unwrap());
        if declared != len || &bytes[40..56] != &W64_FMT || &bytes[104..120] != &W64_DATA || chunk < 24 {
            return Err("Invalid generated Wave64 header".into());
        }
        let size = chunk - 24;
        (64usize, 128u64, size, (8 - size % 8) % 8)
    } else { return Err("Not a generated PCM WAV".into()); };
    if data_start.checked_add(data_size).and_then(|v|v.checked_add(pad)) != Some(len) {
        return Err("Generated PCM data length does not match the media file".into());
    }
    let u16_at = |offset:usize| u16::from_le_bytes(bytes[offset..offset+2].try_into().unwrap());
    let u32_at = |offset:usize| u32::from_le_bytes(bytes[offset..offset+4].try_into().unwrap());
    if u16_at(fmt_start) != 0xfffe { return Err("Not a native extensible PCM header".into()); }
    let fmt = NativePcmFormat {
        channels: u16_at(fmt_start+2), rate: u32_at(fmt_start+4),
        container_bits: u16_at(fmt_start+14), valid_bits: u16_at(fmt_start+18),
        channel_mask: u32_at(fmt_start+20),
    };
    let align = u64::from(fmt.block_align()?);
    if align == 0 || data_size == 0 || data_size % align != 0 {
        return Err("Generated PCM does not contain whole sample frames".into());
    }
    Ok((fmt, data_start, data_size))
}

/// Assemble a continuous native PCM presentation from fully completed private
/// per-item containers. Returns exact source sample-frame counts for CUE.
pub(super) fn concatenate_generated_waves_checked(
    inputs: &[std::path::PathBuf], output: &Path,
    container: NativeWavContainer, cancel: &CancellationToken,
    mut checkpoint: impl FnMut() -> Result<(), String>,
) -> Result<(Vec<u64>, u32), String> {
    if inputs.is_empty() { return Err("No selected chapters to concatenate".into()); }
    let spool = output.with_extension("native-join-pcm");
    let mut raw = OpenOptions::new().create_new(true).write(true).open(&spool)
        .map_err(|e| format!("Create private join spool: {e}"))?;
    let mut common = None;
    let mut samples = Vec::with_capacity(inputs.len());
    let mut buf = vec![0u8; 1024*1024];
    for path in inputs {
        checkpoint()?;
        if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
        let (format, start, bytes) = generated_wave_span(path)?;
        if let Some(previous) = common {
            if previous != format { return Err("Chapters have unequal PCM formats or channel layouts; cannot concatenate them natively".into()); }
        } else { common = Some(format); }
        samples.push(bytes / u64::from(format.block_align()?));
        let mut f = File::open(path).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        let mut remaining = bytes;
        while remaining > 0 {
            checkpoint()?;
            if cancel.is_cancelled() { return Err("Extraction cancelled".into()); }
            let count = usize::try_from(remaining.min(buf.len() as u64)).map_err(|e|e.to_string())?;
            f.read_exact(&mut buf[..count]).map_err(|e| e.to_string())?;
            raw.write_all(&buf[..count]).map_err(|e| e.to_string())?;
            remaining -= count as u64;
        }
    }
    raw.sync_all().map_err(|e| e.to_string())?;
    drop(raw);
    let format = common.ok_or("No native PCM format found")?;
    package_raw_pcm_checked(&spool, output, format, container, cancel, &mut checkpoint)?;
    fs::remove_file(&spool).map_err(|e| e.to_string())?;
    Ok((samples, format.rate))
}
