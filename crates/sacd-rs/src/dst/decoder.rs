use super::bitreader::BitReader;
use super::tables::{
    log2_floor_u32, log2_floor_usize, prob_dst_x_bit, FilterTable, Table,
    FSETS_CODE_PRED_COEFF, FRAME_BITS_PER_CHANNEL, FRAME_BYTES_PER_CHANNEL, MAX_CHANNELS,
    MAX_ELEMENTS, MAX_TABLE_LEN, PROBS_CODE_PRED_COEFF,
};
use super::DstError;

pub fn decode_frame(input: &[u8], channel_count: u8) -> Result<Vec<u8>, DstError> {
    let channels = match channel_count {
        2 | 6 => usize::from(channel_count),
        _ => return Err(DstError::MalformedFrame("invalid channel_count")),
    };

    if input.is_empty() {
        return Err(DstError::UnexpectedEof { consumed: 0 });
    }

    let mut reader = BitReader::new(input);
    let mut out = vec![0u8; channels * FRAME_BYTES_PER_CHANNEL];

    if reader.read_bit()? == 0 {
        decode_uncompressed_dst_payload(&mut reader, &mut out)?;
    } else {
        decode_compressed_dst_payload(&mut reader, &mut out, channels)?;
    }

    Ok(out)
}

fn decode_uncompressed_dst_payload(reader: &mut BitReader<'_>, out: &mut [u8]) -> Result<(), DstError> {
    let _marker = reader.read_bit()?;
    let reserved = reader.read_bits(6)?;
    if reserved != 0 {
        return Err(DstError::MalformedFrame("invalid uncompressed DST header"));
    }

    for byte in out.iter_mut() {
        *byte = reader.read_bits(8)? as u8;
    }

    Ok(())
}

fn decode_compressed_dst_payload(
    reader: &mut BitReader<'_>,
    out: &mut [u8],
    channels: usize,
) -> Result<(), DstError> {
    // These three bits are the simple segmentation case used by SACD DST streams
    // observed by sacd_extract/FFmpeg: probability segments share filter segments,
    // the segmentation is shared by all channels, and the first segment runs to the
    // end of the frame. More complex segment layouts are valid in the DST syntax,
    // but are not present in the staged SACD fixtures or the Al Jarreau stereo area.
    if reader.read_bit()? == 0 {
        return Err(DstError::MalformedFrame("unsupported DST probability segmentation"));
    }
    if reader.read_bit()? == 0 {
        return Err(DstError::MalformedFrame("unsupported per-channel DST segmentation"));
    }
    if reader.read_bit()? == 0 {
        return Err(DstError::MalformedFrame("unsupported multi-segment DST frame"));
    }

    let same_probability_map = reader.read_bit()? != 0;

    let mut fsets = Table::default();
    let filter_map = read_map(reader, &mut fsets, channels)?;

    let mut probs = Table::default();
    let probability_map = if same_probability_map {
        probs.elements = fsets.elements;
        filter_map
    } else {
        read_map(reader, &mut probs, channels)?
    };

    let mut half_probability = [false; MAX_CHANNELS];
    for slot in half_probability.iter_mut().take(channels) {
        *slot = reader.read_bit()? != 0;
    }

    read_table(reader, &mut fsets, &FSETS_CODE_PRED_COEFF, 7, 9, true, 0)?;
    read_table(reader, &mut probs, &PROBS_CODE_PRED_COEFF, 6, 7, false, 1)?;

    if reader.read_bit()? != 0 {
        return Err(DstError::MalformedFrame("invalid arithmetic-code start bit"));
    }

    let mut arithmetic = ArithmeticCoder::new(reader)?;
    let filters = build_filters(&fsets)?;
    let mut status = [[0xAAu8; 16]; MAX_CHANNELS];

    // The reference decoder consumes one warm-up x-bit before the first DSD
    // sample. Its value is intentionally discarded.
    let first_probability = prob_dst_x_bit(fsets.coeff[0][0]);
    let _ = arithmetic.get(reader, first_probability)?;

    for bit_index in 0..FRAME_BITS_PER_CHANNEL {
        let bit_in_byte = 7 - (bit_index & 7);
        let byte_base = (bit_index >> 3) * channels;

        for ch in 0..channels {
            let filter_element = filter_map[ch];
            if filter_element >= fsets.elements {
                return Err(DstError::MalformedFrame("invalid filter table map"));
            }

            let mut predict = 0i32;
            for tap in 0..16 {
                predict += i32::from(filters[filter_element][tap][usize::from(status[ch][tap])]);
            }

            let probability = if !half_probability[ch] || bit_index >= fsets.length[filter_element] {
                let probability_element = probability_map[ch];
                if probability_element >= probs.elements {
                    return Err(DstError::MalformedFrame("invalid probability table map"));
                }
                let length = probs.length[probability_element];
                if length == 0 {
                    return Err(DstError::MalformedFrame("empty probability table"));
                }
                let abs_predict = if predict < 0 { (-predict) as usize } else { predict as usize };
                let mut idx = abs_predict >> 3;
                if idx >= length {
                    idx = length - 1;
                }
                probs.coeff[probability_element][idx]
            } else {
                128
            };

            let residual = i32::from(arithmetic.get(reader, probability)?);
            let dsd_bit = ((predict >> 15) ^ residual) & 1;
            if dsd_bit != 0 {
                out[byte_base + ch] |= 1u8 << bit_in_byte;
            }

            push_status_bit(&mut status[ch], dsd_bit as u8);
        }
    }

    Ok(())
}

fn read_map(
    reader: &mut BitReader<'_>,
    table: &mut Table,
    channels: usize,
) -> Result<[usize; MAX_CHANNELS], DstError> {
    let mut map = [0usize; MAX_CHANNELS];
    table.elements = 1;

    if reader.read_bit()? == 0 {
        for slot in map.iter_mut().take(channels).skip(1) {
            let bits = log2_floor_usize(table.elements) + 1;
            let value = reader.read_bits(bits)? as usize;

            if value == table.elements {
                table.elements += 1;
                if table.elements > MAX_ELEMENTS {
                    return Err(DstError::MalformedFrame("too many DST tables"));
                }
            } else if value > table.elements {
                return Err(DstError::MalformedFrame("invalid DST table number"));
            }

            *slot = value;
        }
    }

    Ok(map)
}

fn read_table(
    reader: &mut BitReader<'_>,
    table: &mut Table,
    code_pred_coeff: &[[i32; 3]; 3],
    length_bits: usize,
    coeff_bits: usize,
    signed: bool,
    offset: i32,
) -> Result<(), DstError> {
    for element in 0..table.elements {
        let length = reader.read_bits(length_bits)? as usize + 1;
        if length > MAX_TABLE_LEN {
            return Err(DstError::MalformedFrame("DST table length too large"));
        }
        table.length[element] = length;

        if reader.read_bit()? == 0 {
            for idx in 0..length {
                table.coeff[element][idx] = read_uncoded_coeff(reader, coeff_bits, signed, offset)?;
            }
        } else {
            let method = reader.read_bits(2)? as usize;
            if method >= code_pred_coeff.len() {
                return Err(DstError::MalformedFrame("invalid coefficient predictor"));
            }

            let warmup = method + 1;
            if warmup > length {
                return Err(DstError::MalformedFrame("coefficient predictor exceeds table length"));
            }

            for idx in 0..warmup {
                table.coeff[element][idx] = read_uncoded_coeff(reader, coeff_bits, signed, offset)?;
            }

            let lsb_size = reader.read_bits(3)? as usize;
            for idx in warmup..length {
                let mut predicted = 0i32;
                for tap in 0..warmup {
                    predicted += code_pred_coeff[method][tap] * table.coeff[element][idx - tap - 1];
                }

                let mut coeff = get_sr_golomb_dst(reader, lsb_size)?;
                if predicted >= 0 {
                    coeff -= (predicted + 4) / 8;
                } else {
                    coeff += (-predicted + 3) / 8;
                }

                validate_coeff(coeff, coeff_bits, signed, offset)?;
                table.coeff[element][idx] = coeff;
            }
        }

        for idx in length..MAX_TABLE_LEN {
            table.coeff[element][idx] = 0;
        }
    }

    Ok(())
}

fn read_uncoded_coeff(
    reader: &mut BitReader<'_>,
    coeff_bits: usize,
    signed: bool,
    offset: i32,
) -> Result<i32, DstError> {
    if signed {
        reader.read_signed(coeff_bits)
    } else {
        Ok(reader.read_bits(coeff_bits)? as i32 + offset)
    }
}

fn validate_coeff(coeff: i32, coeff_bits: usize, signed: bool, offset: i32) -> Result<(), DstError> {
    if signed {
        let min = -(1i32 << (coeff_bits - 1));
        let max = (1i32 << (coeff_bits - 1)) - 1;
        if coeff < min || coeff > max {
            return Err(DstError::MalformedFrame("signed coefficient out of range"));
        }
    } else {
        let min = offset;
        let max = offset + (1i32 << coeff_bits) - 1;
        if coeff < min || coeff > max {
            return Err(DstError::MalformedFrame("probability coefficient out of range"));
        }
    }

    Ok(())
}

fn get_sr_golomb_dst(reader: &mut BitReader<'_>, k: usize) -> Result<i32, DstError> {
    if k > 30 {
        return Err(DstError::MalformedFrame("Rice code width too large"));
    }

    let mut prefix = 0i32;
    while reader.read_bit()? == 0 {
        prefix += 1;
        if prefix > 1_000_000 {
            return Err(DstError::MalformedFrame("runaway Rice code"));
        }
    }

    let mut value = (prefix << k) + reader.read_bits(k)? as i32;
    if value != 0 && reader.read_bit()? != 0 {
        value = -value;
    }

    Ok(value)
}

fn build_filters(fsets: &Table) -> Result<FilterTable, DstError> {
    let mut filters = [[[0i16; 256]; 16]; MAX_ELEMENTS];

    for element in 0..fsets.elements {
        for byte_tap in 0..16 {
            let base = byte_tap * 8;
            let available = fsets.length[element].saturating_sub(base).min(8);

            for history in 0..256usize {
                let mut total = 0i32;
                for bit in 0..available {
                    let history_bit = if ((history >> bit) & 1) != 0 { 1 } else { -1 };
                    total += history_bit * fsets.coeff[element][base + bit];
                }

                if total < i32::from(i16::MIN) || total > i32::from(i16::MAX) {
                    return Err(DstError::MalformedFrame("filter table entry out of range"));
                }

                filters[element][byte_tap][history] = total as i16;
            }
        }
    }

    Ok(filters)
}

fn push_status_bit(status: &mut [u8; 16], bit: u8) {
    let mut carry = bit & 1;
    for byte in status.iter_mut() {
        let next_carry = (*byte >> 7) & 1;
        *byte = (*byte << 1) | carry;
        carry = next_carry;
    }
}

struct ArithmeticCoder {
    a: u32,
    c: u32,
}

impl ArithmeticCoder {
    fn new(reader: &mut BitReader<'_>) -> Result<Self, DstError> {
        let c = reader.read_bits(12)?;
        // DST arithmetic renormalization can request a few padding bits after the
        // encoded payload is exhausted. The reference bit readers deliver zeros in
        // that situation; syntax/header parsing above remains strict.
        reader.set_zero_pad_after_eof(true);
        Ok(Self { a: 4095, c })
    }

    fn get(&mut self, reader: &mut BitReader<'_>, probability: i32) -> Result<u8, DstError> {
        if !(1..=128).contains(&probability) {
            return Err(DstError::MalformedFrame("invalid arithmetic probability"));
        }

        let p = probability as u32;
        let k = (self.a >> 8) | ((self.a >> 7) & 1);
        let q = k * p;
        let a_minus_q = self.a.saturating_sub(q);

        let bit = if self.c < a_minus_q {
            self.a = a_minus_q;
            1u8
        } else {
            self.a = q;
            self.c -= a_minus_q;
            0u8
        };

        if self.a == 0 {
            return Err(DstError::MalformedFrame("arithmetic coder interval collapsed"));
        }

        if self.a < 2048 {
            let n = 11 - log2_floor_u32(self.a);
            self.a <<= n;
            self.c = (self.c << n) | reader.read_bits(n as usize)?;
        }

        Ok(bit)
    }
}
