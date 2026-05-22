//! DST (Direct Stream Transfer) decoder.
//!
//! Pure-Rust port of the DST frame path used by `libdstdec/` in
//! [Sound-Linux-More/sacd-extract][upstream]. Handles the "simple
//! segmentation" subset of DST syntax observed in real-world SACDs
//! (single segment, shared filter/probability maps); more complex
//! segmentation valid in the DST syntax is rejected with
//! `DstError::MalformedFrame`.
//!
//! Validation: byte-exact parity with `sacd_extract` for Solo Monk
//! (uncompressed stereo), Al Jarreau *All I Got* DST stereo, and
//! Al Jarreau *All I Got* DST 6-channel (70/70 tracks, ~20.5 GB).
//!
//! [upstream]: https://github.com/Sound-Linux-More/sacd-extract

#![forbid(unsafe_code)]

mod bitreader;
mod decoder;
mod tables;

pub use decoder::decode_frame;

/// DST decoder error. Every C `assert()` / failure path in the upstream
/// decoder maps to one of these variants — no panics on malformed input.
#[derive(Debug)]
pub enum DstError {
    /// Bit/byte reader ran past the end of the input frame.
    /// `consumed` is whole bytes consumed from `input` before exhaustion.
    UnexpectedEof { consumed: usize },
    /// Frame header or stream syntax violated the DST spec. Also used for
    /// invalid `channel_count` and for short output.
    MalformedFrame(&'static str),
    /// Decoder tried to emit more output than the fixed budget.
    /// `limit` is `channel_count * 4704`.
    OutputOverflow { limit: usize },
    /// Catch-all for upstream `return -1` paths that don't fit a more
    /// specific variant.
    InternalDecodeError(&'static str),
}

impl std::fmt::Display for DstError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DstError::UnexpectedEof { consumed } => {
                write!(f, "unexpected EOF in DST stream after {} bytes", consumed)
            }
            DstError::MalformedFrame(msg) => write!(f, "malformed DST frame: {}", msg),
            DstError::OutputOverflow { limit } => {
                write!(f, "decoded output exceeded budget ({} bytes)", limit)
            }
            DstError::InternalDecodeError(msg) => write!(f, "internal decoder error: {}", msg),
        }
    }
}

impl std::error::Error for DstError {}

#[cfg(test)]
mod fixture_tests {
    use super::decode_frame;

    fn pair(n: u8) -> (&'static [u8], &'static [u8]) {
        match n {
            1 => (
                include_bytes!("fixtures/frame_001.dst.bin"),
                include_bytes!("fixtures/frame_001.dsd.bin"),
            ),
            2 => (
                include_bytes!("fixtures/frame_002.dst.bin"),
                include_bytes!("fixtures/frame_002.dsd.bin"),
            ),
            3 => (
                include_bytes!("fixtures/frame_003.dst.bin"),
                include_bytes!("fixtures/frame_003.dsd.bin"),
            ),
            _ => unreachable!(),
        }
    }

    fn pair_6ch(n: u8) -> (&'static [u8], &'static [u8]) {
        match n {
            1 => (
                include_bytes!("fixtures/frame_001_6ch.dst.bin"),
                include_bytes!("fixtures/frame_001_6ch.dsd.bin"),
            ),
            2 => (
                include_bytes!("fixtures/frame_002_6ch.dst.bin"),
                include_bytes!("fixtures/frame_002_6ch.dsd.bin"),
            ),
            3 => (
                include_bytes!("fixtures/frame_003_6ch.dst.bin"),
                include_bytes!("fixtures/frame_003_6ch.dsd.bin"),
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn frame_1_byte_exact() {
        let (inp, expect) = pair(1);
        let got = decode_frame(inp, 2).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn frame_2_byte_exact() {
        let (inp, expect) = pair(2);
        let got = decode_frame(inp, 2).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn frame_3_byte_exact() {
        let (inp, expect) = pair(3);
        let got = decode_frame(inp, 2).expect("decode");
        assert_eq!(got, expect);
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(data))
    }

    /// Pin the bundled fixture hashes so a silent fixture corruption
    /// (e.g. wrong file accidentally copied) is caught loud rather
    /// than via opaque assert_eq! mismatches in the byte-exact tests.
    /// Hashes match `crates/sacd-rs/src/dst/fixtures/SHA256SUMS`.
    #[test]
    fn fixture_sha256_pins() {
        let cases = [
            (
                1,
                "a788eb38dd9cf5bf5313ed521dabca62107332e2ffa02bc0943384fe5b1e87e4",
                "4ba636974ba4217e348137a0ff9dda2df3f5ec1d80df03ad71c369a7a4f45ef7",
            ),
            (
                2,
                "fd77fa6f66e793eb309963fcda75fecbd5927dd816538d158d3440359c40efc9",
                "d138a1d886e52c6fcd741d3eaf7ffe482b1bcb460e7a10062fa78bdf6e48d913",
            ),
            (
                3,
                "9a788271fa0893b190ea180d47bf14019612b6f97c9680f71e80df41982ee921",
                "506f08c2eb6c82cd5ead58328f6b9a77c2676a391c49b90782ac3cda3fa4ff21",
            ),
        ];
        for (n, dst_h, dsd_h) in cases {
            let (dst, dsd) = pair(n);
            assert_eq!(sha256_hex(dst), dst_h, "frame {} DST input hash drift", n);
            assert_eq!(sha256_hex(dsd), dsd_h, "frame {} DSD output hash drift", n);
        }
    }

    /// 6-channel companion to `fixture_sha256_pins`. Hashes match the
    /// `*_6ch.*.bin` entries in `crates/sacd-rs/src/dst/fixtures/SHA256SUMS`.
    #[test]
    fn fixture_sha256_pins_6ch() {
        let cases = [
            (
                1,
                "59e54dd6a45fd543e96ad10d106ce254931b93a391424d82af5549f1528799bc",
                "f5a8b3f86aad452c166ab3f86a71d8355222f92735458f959328271f9f33765c",
            ),
            (
                2,
                "4931e8ebd786ec0a62d1290877e51796ba17ada62009fbc2c3fc5012f234bbd0",
                "43882f0757dddd92de08252057ed6d3c1ee2d75f25823bcae28490dee7994eea",
            ),
            (
                3,
                "7bcd510c01d54f8cd26677619cddc2c04cf83ff8077b534377536e5df54e95ba",
                "36fbd17cca9ff410070673f4870673d708fda5398ccebb0d8888c4ebebdbba06",
            ),
        ];
        for (n, dst_h, dsd_h) in cases {
            let (dst, dsd) = pair_6ch(n);
            assert_eq!(
                sha256_hex(dst),
                dst_h,
                "6ch frame {} DST input hash drift",
                n
            );
            assert_eq!(
                sha256_hex(dsd),
                dsd_h,
                "6ch frame {} DSD output hash drift",
                n
            );
        }
    }

    #[test]
    fn frame_1_6ch_byte_exact() {
        let (inp, expect) = pair_6ch(1);
        let got = decode_frame(inp, 6).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn frame_2_6ch_byte_exact() {
        let (inp, expect) = pair_6ch(2);
        let got = decode_frame(inp, 6).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn frame_3_6ch_byte_exact() {
        let (inp, expect) = pair_6ch(3);
        let got = decode_frame(inp, 6).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn invalid_channel_count_is_malformed() {
        let (inp, _) = pair(1);
        let err = decode_frame(inp, 5).expect_err("invalid channel count");
        assert!(matches!(
            err,
            super::DstError::MalformedFrame("invalid channel_count")
        ));
    }
}
