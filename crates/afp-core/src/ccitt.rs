//! CCITT T.6 (Group 4 / IBM MMR) bilevel image decompression.
//!
//! IOCA compression X'01' (IBM MMR) is ITU-T T.6 Group-4 two-dimensional
//! coding with IBM framing: the first scan line is coded 1D (Modified
//! Huffman) after a leading EOL, a switch EOL follows, and the rest is 2D.
//!
//! [`decode_g4`] returns rows of `(width + 7) / 8` bytes, MSB first, with
//! 1 = black — the IOCA convention for an uncompressed bilevel image.

use std::collections::HashMap;
use std::sync::OnceLock;

const WHITE: &[(u32, &str)] = &[
    (0, "00110101"),
    (1, "000111"),
    (2, "0111"),
    (3, "1000"),
    (4, "1011"),
    (5, "1100"),
    (6, "1110"),
    (7, "1111"),
    (8, "10011"),
    (9, "10100"),
    (10, "00111"),
    (11, "01000"),
    (12, "001000"),
    (13, "000011"),
    (14, "110100"),
    (15, "110101"),
    (16, "101010"),
    (17, "101011"),
    (18, "0100111"),
    (19, "0001100"),
    (20, "0001000"),
    (21, "0010111"),
    (22, "0000011"),
    (23, "0000100"),
    (24, "0101000"),
    (25, "0101011"),
    (26, "0010011"),
    (27, "0100100"),
    (28, "0011000"),
    (29, "00000010"),
    (30, "00000011"),
    (31, "00011010"),
    (32, "00011011"),
    (33, "00010010"),
    (34, "00010011"),
    (35, "00010100"),
    (36, "00010101"),
    (37, "00010110"),
    (38, "00010111"),
    (39, "00101000"),
    (40, "00101001"),
    (41, "00101010"),
    (42, "00101011"),
    (43, "00101100"),
    (44, "00101101"),
    (45, "00000100"),
    (46, "00000101"),
    (47, "00001010"),
    (48, "00001011"),
    (49, "01010010"),
    (50, "01010011"),
    (51, "01010100"),
    (52, "01010101"),
    (53, "00100100"),
    (54, "00100101"),
    (55, "01011000"),
    (56, "01011001"),
    (57, "01011010"),
    (58, "01011011"),
    (59, "01001010"),
    (60, "01001011"),
    (61, "00110010"),
    (62, "00110011"),
    (63, "00110100"),
    (64, "11011"),
    (128, "10010"),
    (192, "010111"),
    (256, "0110111"),
    (320, "00110110"),
    (384, "00110111"),
    (448, "01100100"),
    (512, "01100101"),
    (576, "01101000"),
    (640, "01100111"),
    (704, "011001100"),
    (768, "011001101"),
    (832, "011010010"),
    (896, "011010011"),
    (960, "011010100"),
    (1024, "011010101"),
    (1088, "011010110"),
    (1152, "011010111"),
    (1216, "011011000"),
    (1280, "011011001"),
    (1344, "011011010"),
    (1408, "011011011"),
    (1472, "010011000"),
    (1536, "010011001"),
    (1600, "010011010"),
    (1664, "011000"),
    (1728, "010011011"),
];

const BLACK: &[(u32, &str)] = &[
    (0, "0000110111"),
    (1, "010"),
    (2, "11"),
    (3, "10"),
    (4, "011"),
    (5, "0011"),
    (6, "0010"),
    (7, "00011"),
    (8, "000101"),
    (9, "000100"),
    (10, "0000100"),
    (11, "0000101"),
    (12, "0000111"),
    (13, "00000100"),
    (14, "00000111"),
    (15, "000011000"),
    (16, "0000010111"),
    (17, "0000011000"),
    (18, "0000001000"),
    (19, "00001100111"),
    (20, "00001101000"),
    (21, "00001101100"),
    (22, "00000110111"),
    (23, "00000101000"),
    (24, "00000010111"),
    (25, "00000011000"),
    (26, "000011001010"),
    (27, "000011001011"),
    (28, "000011001100"),
    (29, "000011001101"),
    (30, "000001101000"),
    (31, "000001101001"),
    (32, "000001101010"),
    (33, "000001101011"),
    (34, "000011010010"),
    (35, "000011010011"),
    (36, "000011010100"),
    (37, "000011010101"),
    (38, "000011010110"),
    (39, "000011010111"),
    (40, "000001101100"),
    (41, "000001101101"),
    (42, "000011011010"),
    (43, "000011011011"),
    (44, "000001010100"),
    (45, "000001010101"),
    (46, "000001010110"),
    (47, "000001010111"),
    (48, "000001100100"),
    (49, "000001100101"),
    (50, "000001010010"),
    (51, "000001010011"),
    (52, "000000100100"),
    (53, "000000110111"),
    (54, "000000111000"),
    (55, "000000100111"),
    (56, "000000101000"),
    (57, "000001011000"),
    (58, "000001011001"),
    (59, "000000101011"),
    (60, "000000101100"),
    (61, "000001011010"),
    (62, "000001100110"),
    (63, "000001100111"),
    (64, "0000001111"),
    (128, "000011001000"),
    (192, "000011001001"),
    (256, "000001011011"),
    (320, "000000110011"),
    (384, "000000110100"),
    (448, "000000110101"),
    (512, "0000001101100"),
    (576, "0000001101101"),
    (640, "0000001001010"),
    (704, "0000001001011"),
    (768, "0000001001100"),
    (832, "0000001001101"),
    (896, "0000001110010"),
    (960, "0000001110011"),
    (1024, "0000001110100"),
    (1088, "0000001110101"),
    (1152, "0000001110110"),
    (1216, "0000001110111"),
    (1280, "0000001010010"),
    (1344, "0000001010011"),
    (1408, "0000001010100"),
    (1472, "0000001010101"),
    (1536, "0000001011010"),
    (1600, "0000001011011"),
    (1664, "0000001100100"),
    (1728, "0000001100101"),
];

/// Extended make-up codes, common to both colors (T.4 Table 3).
const EXT: &[(u32, &str)] = &[
    (1792, "00000001000"),
    (1856, "00000001100"),
    (1920, "00000001101"),
    (1984, "000000010010"),
    (2048, "000000010011"),
    (2112, "000000010100"),
    (2176, "000000010101"),
    (2240, "000000010110"),
    (2304, "000000010111"),
    (2368, "000000011100"),
    (2432, "000000011101"),
    (2496, "000000011110"),
    (2560, "000000011111"),
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Pass,
    Horizontal,
    Vertical(i32),
}

const MODES: &[(&str, Mode)] = &[
    ("0001", Mode::Pass),
    ("001", Mode::Horizontal),
    ("1", Mode::Vertical(0)),
    ("011", Mode::Vertical(1)),
    ("010", Mode::Vertical(-1)),
    ("000011", Mode::Vertical(2)),
    ("000010", Mode::Vertical(-2)),
    ("0000011", Mode::Vertical(3)),
    ("0000010", Mode::Vertical(-3)),
];

type CodeTable = HashMap<(usize, u32), u32>;

struct Tables {
    white: CodeTable,
    white_max: usize,
    black: CodeTable,
    black_max: usize,
    modes: HashMap<(usize, u32), Mode>,
    mode_max: usize,
}

fn build(codes: &[&[(u32, &str)]]) -> (CodeTable, usize) {
    let mut table = HashMap::new();
    let mut longest = 0;
    for set in codes {
        for &(run, bits) in *set {
            table.insert((bits.len(), u32::from_str_radix(bits, 2).unwrap()), run);
            longest = longest.max(bits.len());
        }
    }
    (table, longest)
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let (white, white_max) = build(&[WHITE, EXT]);
        let (black, black_max) = build(&[BLACK, EXT]);
        let modes = MODES.iter().map(|&(b, m)| ((b.len(), u32::from_str_radix(b, 2).unwrap()), m)).collect();
        let mode_max = MODES.iter().map(|(b, _)| b.len()).max().unwrap();
        Tables { white, white_max, black, black_max, modes, mode_max }
    })
}

/// MSB-first bit reader.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        if self.pos >= self.end {
            return None;
        }
        let b = (self.data[self.pos >> 3] >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Some(b as u32)
    }
}

fn read_mode(bits: &mut Bits) -> Option<Mode> {
    let t = tables();
    let mut val = 0;
    for length in 1..=t.mode_max {
        val = (val << 1) | bits.bit()?;
        if let Some(m) = t.modes.get(&(length, val)) {
            return Some(*m);
        }
    }
    None
}

/// Longest leading-zero prefix of any valid line-start code is 6; a longer
/// zero run then a 1 marks an inter-line EOL / fill.
const EOL_MIN_ZEROS: usize = 7;

fn maybe_skip_eol(bits: &mut Bits) -> bool {
    let save = bits.pos;
    let mut zeros = 0;
    loop {
        match bits.bit() {
            None => {
                bits.pos = save;
                return false;
            }
            Some(0) => zeros += 1,
            Some(_) => {
                if zeros >= EOL_MIN_ZEROS {
                    return true;
                }
                bits.pos = save;
                return false;
            }
        }
    }
}

fn read_run(bits: &mut Bits, black: bool) -> Option<u32> {
    let t = tables();
    let (table, longest) = if black { (&t.black, t.black_max) } else { (&t.white, t.white_max) };
    let mut total: u32 = 0;
    loop {
        let mut val = 0;
        let mut run = None;
        for length in 1..=longest {
            val = (val << 1) | bits.bit()?;
            if let Some(r) = table.get(&(length, val)) {
                run = Some(*r);
                break;
            }
        }
        let run = run?;
        total = total.saturating_add(run);
        if run < 64 {
            return Some(total);
        }
    }
}

fn decode_1d_line(bits: &mut Bits, width: i64) -> Vec<i64> {
    let mut cur = Vec::new();
    let mut a0 = 0i64;
    let mut black = false;
    while a0 < width {
        let Some(run) = read_run(bits, black) else { break };
        a0 = (a0 + run as i64).min(width);
        cur.push(a0);
        black = !black;
    }
    cur
}

fn decode_2d_line(bits: &mut Bits, refl: &[i64], width: i64) -> Vec<i64> {
    let mut cur = Vec::new();
    let mut a0 = -1i64;
    let mut color = 0usize; // 0 = white, 1 = black
    while a0 < width {
        let mut i = 0;
        while i < refl.len() && refl[i] <= a0 {
            i += 1;
        }
        if i < refl.len() && (i & 1) != color {
            i += 1;
        }
        let b1 = refl.get(i).copied().unwrap_or(width);
        let b2 = refl.get(i + 1).copied().unwrap_or(width);

        let Some(mode) = read_mode(bits) else { break };
        let start = a0.max(0);
        match mode {
            Mode::Pass => a0 = b2,
            Mode::Vertical(delta) => {
                let a1 = (b1 + delta as i64).clamp(0, width);
                cur.push(a1);
                a0 = a1;
                color ^= 1;
            }
            Mode::Horizontal => {
                let run1 = read_run(bits, color == 1);
                let run2 = read_run(bits, color != 1);
                let (Some(run1), Some(run2)) = (run1, run2) else { break };
                let a1 = (start + run1 as i64).min(width);
                let a2 = (a1 + run2 as i64).min(width);
                cur.push(a1);
                cur.push(a2);
                a0 = a2;
            }
        }
    }
    cur
}

/// Decode an IBM MMR (IOCA X'01') stream to a 1-bpp raster, 1 = black.
///
/// Always returns `(width + 7) / 8 * height` bytes; decoding stops early
/// and pads with white on malformed data rather than failing.
pub fn decode_g4(data: &[u8], width: usize, height: usize) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let w = width as i64;
    let mut bits = Bits { data, pos: 0, end: data.len() * 8 };
    let row_bytes = width.div_ceil(8);
    let mut out = Vec::with_capacity(row_bytes * height);
    let mut refl: Vec<i64> = vec![w, w];

    for _ in 0..height {
        let mut one_d = false;
        if maybe_skip_eol(&mut bits) {
            one_d = bits.bit() == Some(1);
        }
        if bits.pos >= bits.end {
            break;
        }
        let cur = if one_d { decode_1d_line(&mut bits, w) } else { decode_2d_line(&mut bits, &refl, w) };
        let mut rowbuf = vec![0u8; row_bytes];
        let mut pos = 0i64;
        let mut black = false;
        let paint = |from: i64, to: i64, row: &mut Vec<u8>| {
            for x in from..to {
                row[(x >> 3) as usize] |= 0x80 >> (x & 7);
            }
        };
        for &t in &cur {
            let t = t.clamp(0, w);
            if black {
                paint(pos, t, &mut rowbuf);
            }
            pos = t;
            black = !black;
        }
        if black {
            paint(pos, w, &mut rowbuf);
        }
        out.extend_from_slice(&rowbuf);
        refl = cur;
        refl.push(w);
        refl.push(w);
    }
    out.resize(row_bytes * height, 0);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn changing_elements(row: &[u8]) -> Vec<i64> {
        let mut ce = Vec::new();
        let mut prev = 0;
        for (x, &px) in row.iter().enumerate() {
            if px != prev {
                ce.push(x as i64);
                prev = px;
            }
        }
        ce
    }

    fn run_bits(mut run: i64, white: bool) -> String {
        let table = if white { WHITE } else { BLACK };
        let codes: Vec<(u32, &str)> = table.iter().chain(EXT.iter()).copied().collect();
        let mut bits = String::new();
        while run >= 64 {
            let makeup = codes.iter().map(|c| c.0 as i64).filter(|&m| m >= 64 && m <= run).max().unwrap();
            bits += codes.iter().find(|c| c.0 as i64 == makeup).unwrap().1;
            run -= makeup;
        }
        bits + codes.iter().find(|c| c.0 as i64 == run).unwrap().1
    }

    fn mode_bits(m: Mode) -> &'static str {
        MODES.iter().find(|(_, mm)| *mm == m).unwrap().0
    }

    /// Test-only IBM MMR encoder (port of readAFP's `_encode_g4`).
    pub fn encode_g4(rows: &[Vec<u8>], width: usize) -> Vec<u8> {
        let w = width as i64;
        let mut bits = String::from("0000000000011");
        let mut pos = 0;
        let mut color = 0;
        for x in changing_elements(&rows[0]) {
            bits += &run_bits(x - pos, color == 0);
            pos = x;
            color ^= 1;
        }
        bits += &run_bits(w - pos, color == 0);
        bits += "0000000000010";
        let mut refl = changing_elements(&rows[0]);
        refl.extend([w, w]);
        for row in &rows[1..] {
            let mut cur = changing_elements(row);
            cur.extend([w, w]);
            let (mut a0, mut color, mut ci) = (-1i64, 0usize, 0usize);
            while a0 < w {
                let mut i = 0;
                while i < refl.len() && refl[i] <= a0 {
                    i += 1;
                }
                if i < refl.len() && (i & 1) != color {
                    i += 1;
                }
                let b1 = refl.get(i).copied().unwrap_or(w);
                let b2 = refl.get(i + 1).copied().unwrap_or(w);
                while ci < cur.len() && cur[ci] <= a0 {
                    ci += 1;
                }
                let a1 = cur.get(ci).copied().unwrap_or(w);
                if b2 < a1 {
                    bits += mode_bits(Mode::Pass);
                    a0 = b2;
                } else if (a1 - b1).abs() <= 3 {
                    bits += mode_bits(Mode::Vertical((a1 - b1) as i32));
                    a0 = a1;
                    color ^= 1;
                } else {
                    let a2 = cur.get(ci + 1).copied().unwrap_or(w);
                    let start = a0.max(0);
                    bits += mode_bits(Mode::Horizontal);
                    bits += &run_bits(a1 - start, color == 0);
                    bits += &run_bits(a2 - a1, color == 1);
                    a0 = a2;
                }
            }
            refl = changing_elements(row);
            refl.extend([w, w]);
        }
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        bits.as_bytes().chunks(8).map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap()).collect()
    }

    fn to_rows(raw: &[u8], width: usize, height: usize) -> Vec<Vec<u8>> {
        let rb = width.div_ceil(8);
        (0..height).map(|y| (0..width).map(|x| (raw[y * rb + (x >> 3)] >> (7 - (x & 7))) & 1).collect()).collect()
    }

    #[test]
    fn round_trips_a_patterned_image() {
        let (width, height) = (24, 12);
        let rows: Vec<Vec<u8>> = (0..height)
            .map(|y| {
                if y == 0 || y == height - 1 {
                    vec![1; width]
                } else {
                    let mut r = vec![0; width];
                    r[0] = 1;
                    r[width - 1] = 1;
                    r[y] = 1;
                    r[width - 1 - y] = 1;
                    r
                }
            })
            .collect();
        let raw = decode_g4(&encode_g4(&rows, width), width, height);
        assert_eq!(to_rows(&raw, width, height), rows);
    }

    #[test]
    fn first_line_has_content() {
        let (width, height) = (40, 20);
        let rows: Vec<Vec<u8>> = (0..height)
            .map(|y| {
                let mut r = vec![0; width];
                for x in (0..width).step_by(if y == 0 { 2 } else { 7 }) {
                    r[x] = 1;
                }
                r
            })
            .collect();
        let raw = decode_g4(&encode_g4(&rows, width), width, height);
        assert_eq!(to_rows(&raw, width, height), rows);
    }

    #[test]
    fn all_white_and_all_black() {
        let (width, height) = (16, 4);
        for fill in [0, 1] {
            let rows = vec![vec![fill; width]; height];
            let raw = decode_g4(&encode_g4(&rows, width), width, height);
            assert_eq!(to_rows(&raw, width, height), rows);
        }
    }

    #[test]
    fn truncated_stream_pads_white() {
        assert_eq!(decode_g4(&[0x00, 0x01], 32, 10).len(), 40);
    }
}
