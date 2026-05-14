pub(crate) const MAX_CHANNELS: usize = 6;
pub(crate) const MAX_ELEMENTS: usize = 2 * MAX_CHANNELS;
pub(crate) const MAX_TABLE_LEN: usize = 128;

pub(crate) const FRAME_BYTES_PER_CHANNEL: usize = 4704;
pub(crate) const FRAME_BITS_PER_CHANNEL: usize = FRAME_BYTES_PER_CHANNEL * 8;

pub(crate) const FSETS_CODE_PRED_COEFF: [[i32; 3]; 3] = [
    [-8, 0, 0],
    [-16, 8, 0],
    [-9, -5, 6],
];

pub(crate) const PROBS_CODE_PRED_COEFF: [[i32; 3]; 3] = [
    [-8, 0, 0],
    [-16, 8, 0],
    [-24, 24, -8],
];

#[derive(Clone, Debug)]
pub(crate) struct Table {
    pub(crate) elements: usize,
    pub(crate) length: [usize; MAX_ELEMENTS],
    pub(crate) coeff: [[i32; MAX_TABLE_LEN]; MAX_ELEMENTS],
}

impl Default for Table {
    fn default() -> Self {
        Self {
            elements: 1,
            length: [0; MAX_ELEMENTS],
            coeff: [[0; MAX_TABLE_LEN]; MAX_ELEMENTS],
        }
    }
}

pub(crate) type FilterTable = [[[i16; 256]; 16]; MAX_ELEMENTS];

pub(crate) fn log2_floor_usize(value: usize) -> usize {
    debug_assert!(value > 0);
    usize::BITS as usize - 1 - value.leading_zeros() as usize
}

pub(crate) fn log2_floor_u32(value: u32) -> u32 {
    debug_assert!(value > 0);
    u32::BITS - 1 - value.leading_zeros()
}

pub(crate) fn prob_dst_x_bit(coeff: i32) -> i32 {
    let low = (coeff & 0x7f) as u8;
    i32::from((low.reverse_bits() >> 1) + 1)
}
