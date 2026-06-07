// SPDX-License-Identifier: GPL-2.0-or-later
// Pure-Rust port of bit-reading helpers from libdstdec/ in
// https://github.com/Sound-Linux-More/sacd-extract (GPL-2.0).

use super::DstError;

#[derive(Clone, Debug)]
pub(crate) struct BitReader<'a> {
    input: &'a [u8],
    bit_pos: usize,
    zero_pad_after_eof: bool,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            bit_pos: 0,
            zero_pad_after_eof: false,
        }
    }

    pub(crate) fn set_zero_pad_after_eof(&mut self, yes: bool) {
        self.zero_pad_after_eof = yes;
    }

    pub(crate) fn read_bit(&mut self) -> Result<u8, DstError> {
        let total_bits = self.input.len().checked_mul(8).ok_or(
            DstError::InternalDecodeError("bit reader input length overflow"),
        )?;
        if self.bit_pos >= total_bits {
            if self.zero_pad_after_eof {
                self.bit_pos = self.bit_pos.checked_add(1).ok_or(
                    DstError::InternalDecodeError("bit reader position overflow"),
                )?;
                return Ok(0);
            }
            return Err(DstError::UnexpectedEof {
                consumed: self.input.len(),
            });
        }

        let byte = self.input[self.bit_pos / 8];
        let shift = 7 - (self.bit_pos % 8);
        self.bit_pos = self.bit_pos.checked_add(1).ok_or(
            DstError::InternalDecodeError("bit reader position overflow"),
        )?;
        Ok((byte >> shift) & 1)
    }

    pub(crate) fn read_bits(&mut self, n: usize) -> Result<u32, DstError> {
        if n > 32 {
            return Err(DstError::InternalDecodeError("bit read width too large"));
        }

        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }

    pub(crate) fn read_signed(&mut self, n: usize) -> Result<i32, DstError> {
        if n == 0 || n >= 32 {
            return Err(DstError::InternalDecodeError(
                "signed bit read width too large",
            ));
        }

        let raw = self.read_bits(n)? as i32;
        let shift = 32 - n;
        Ok((raw << shift) >> shift)
    }
}
