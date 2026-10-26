//! IOCA image segment decoding.
//!
//! An image object's IPD fields concatenate into one IOCA image segment: a
//! stream of self-defining fields (SDFs) in long format (code, u8 length,
//! params) or extended format (0xFE, code, u16 length, params):
//!
//! ```text
//! 0x94    Image Size      unit base, resolution, size in pixels
//! 0x95    Image Encoding  compression and recording algorithms
//! 0x96    IDE Size        bits per image data element
//! 0xFE92  Image Data      raw IDEs (may repeat; concatenated)
//! 0xFE9C  Band Image Data BANDNUM(1) reserved(2) then IDEs
//! ```
//!
//! Reference: IOCA Reference, AFPC-0003-09.

use std::collections::BTreeMap;

use crate::ccitt::decode_g4;
use crate::png::pack_png;
use crate::util::{be_uint, find_bytes, u16_at};

pub const COMPRESSION_MMR: u8 = 0x01;
pub const COMPRESSION_NONE: u8 = 0x03;
pub const COMPRESSION_LZW: u8 = 0x0D;
pub const COMPRESSION_JPEG: u8 = 0x83;

/// Largest image decoded, in pixels (about 9000 × 7000): larger sizes come
/// from corrupt or hostile size fields, not from real documents.
pub(crate) const MAX_IMAGE_PIXELS: usize = 64_000_000;

/// Decompress a TIFF-variant LZW stream (MSB-first, 9–12 bit codes, early
/// change), stopping after `max_out` bytes.
pub fn decode_lzw(data: &[u8], max_out: usize) -> Vec<u8> {
    const TABLE_SIZE: usize = 4096;
    const CLEAR: usize = 256;
    const EOI: usize = 257;
    let fresh = || -> (Vec<Vec<u8>>, usize, u32) {
        let mut t: Vec<Vec<u8>> = (0..256u16).map(|i| vec![i as u8]).collect();
        t.push(Vec::new());
        t.push(Vec::new());
        (t, 258, 9)
    };
    let (mut table, mut nxt, mut width) = fresh();
    let mut out = Vec::new();
    let mut prev: Option<usize> = None;
    let (mut acc, mut nbits, mut pos) = (0u64, 0u32, 0usize);
    loop {
        while nbits < width && pos < data.len() {
            acc = (acc << 8) | data[pos] as u64;
            pos += 1;
            nbits += 8;
        }
        if nbits < width {
            break;
        }
        nbits -= width;
        let code = ((acc >> nbits) & ((1 << width) - 1)) as usize;
        acc &= (1u64 << nbits) - 1;
        if code == EOI {
            break;
        }
        if code == CLEAR {
            (table, nxt, width) = fresh();
            prev = None;
            continue;
        }
        let entry = match prev {
            None => match table.get(code) {
                Some(e) => e.clone(),
                None => break,
            },
            Some(p) => {
                let entry = if code < nxt && !table[code].is_empty() {
                    table[code].clone()
                } else if code == nxt {
                    let mut e = table[p].clone();
                    if let Some(&f) = table[p].first() {
                        e.push(f);
                    }
                    e
                } else {
                    break; // corrupt stream
                };
                // A full table only grows again after a CLEAR code.
                if nxt < TABLE_SIZE {
                    let mut new = table[p].clone();
                    if let Some(&f) = entry.first() {
                        new.push(f);
                    }
                    table.push(new);
                    nxt += 1;
                }
                if nxt == (1 << width) - 1 && width < 12 {
                    width += 1; // TIFF early change
                }
                entry
            }
        };
        out.extend_from_slice(&entry);
        prev = Some(code);
        if out.len() >= max_out {
            out.truncate(max_out);
            break;
        }
    }
    out
}

/// One self-defining field: code (0xFExx for extended) and params.
pub fn parse_sdfs(data: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let (code, mut length);
        if data[pos] == 0xFE {
            if pos + 4 > data.len() {
                log::warn!("truncated extended SDF at offset {pos}");
                break;
            }
            code = 0xFE00 | data[pos + 1] as u16;
            length = u16_at(data, pos + 2) as usize;
            pos += 4;
        } else {
            if pos + 2 > data.len() {
                break;
            }
            code = data[pos] as u16;
            length = data[pos + 1] as usize;
            pos += 2;
        }
        if length == 0 && (code == 0xFE92 || code == 0xFE9C) {
            length = data.len() - pos;
        }
        out.push((code, crate::util::slice(data, pos, pos + length)));
        pos += length;
    }
    out
}

/// One decoded IOCA image segment's parameters and pixel data.
#[derive(Debug, Clone)]
pub struct IocaImage {
    pub width: usize,
    pub height: usize,
    pub hres: u32,
    pub vres: u32,
    pub compression: u8,
    pub bits: u8,
    pub data: Vec<u8>,
    pub bands: BTreeMap<u8, Vec<u8>>,
}

impl Default for IocaImage {
    fn default() -> Self {
        IocaImage {
            width: 0,
            height: 0,
            hres: 0,
            vres: 0,
            compression: COMPRESSION_NONE,
            bits: 1,
            data: Vec::new(),
            bands: BTreeMap::new(),
        }
    }
}

/// Parse concatenated IPD bytes into an image, or `None` if empty.
pub fn parse_image_segment(data: &[u8]) -> Option<IocaImage> {
    let mut img = IocaImage::default();
    let mut seen = false;
    for (code, p) in parse_sdfs(data) {
        match code {
            0x94 if p.len() >= 9 => {
                seen = true;
                img.hres = u16_at(p, 1) as u32;
                img.vres = u16_at(p, 3) as u32;
                img.width = u16_at(p, 5) as usize;
                img.height = u16_at(p, 7) as usize;
            }
            0xB6 if p.len() >= 8 => {
                seen = true;
                if img.width == 0 {
                    img.width = be_uint(p, 0, 4) as usize;
                }
                if img.height == 0 {
                    img.height = be_uint(p, 4, 4) as usize;
                }
            }
            0x95 if !p.is_empty() => {
                seen = true;
                img.compression = p[0];
            }
            0x96 if !p.is_empty() => img.bits = p[0],
            0xFE92 => {
                seen = true;
                img.data.extend_from_slice(p);
            }
            0xFE9C if p.len() >= 3 => {
                seen = true;
                img.bands.entry(p[0]).or_default().extend_from_slice(&p[3..]);
            }
            _ => {}
        }
    }
    if !seen {
        return None;
    }
    if img.data.is_empty() && img.bands.len() == 1 {
        // Single-band data is just image data delivered through the banded SDF.
        img.data = img.bands.values().next().unwrap().clone();
    }
    Some(img)
}

/// Turn a parsed image into (mime, bytes) a renderer can display, or `None`
/// for combinations not decoded (G3/T.6 fax, banded uncompressed color...).
pub fn image_blob(img: &IocaImage) -> Option<(&'static str, Vec<u8>)> {
    if img.compression == COMPRESSION_JPEG {
        let start = find_bytes(&img.data, &[0xFF, 0xD8])?;
        return Some(("image/jpeg", img.data[start..].to_vec()));
    }
    if img.width == 0 || img.height == 0 {
        return None;
    }
    let (w, h) = (img.width, img.height);
    if w.checked_mul(h).is_none_or(|pixels| pixels > MAX_IMAGE_PIXELS) {
        log::info!("skipping IOCA image: {w}x{h} exceeds the pixel guard");
        return None;
    }
    // Bytes of decoded data the image needs (bounded by the guard above).
    let need = match img.bits {
        1 => w.div_ceil(8) * h,
        8 => w * h,
        24 => w * 3 * h,
        _ => return None,
    };
    let raw = match img.compression {
        COMPRESSION_MMR => decode_g4(&img.data, w, h),
        COMPRESSION_LZW => decode_lzw(&img.data, need),
        COMPRESSION_NONE => img.data.clone(),
        _ => return None,
    };
    if raw.is_empty() {
        return None;
    }
    match img.bits {
        1 => {
            let row_bytes = w.div_ceil(8);
            if raw.len() < row_bytes * h {
                return None;
            }
            // IOCA bilevel: 1 = mark (black). PNG grayscale: 0 = black.
            let inverted: Vec<u8> = raw.iter().map(|b| b ^ 0xFF).collect();
            Some(("image/png", pack_png(w as u32, h as u32, 1, 0, row_bytes, &inverted)))
        }
        8 => {
            if raw.len() < w * h {
                return None;
            }
            Some(("image/png", pack_png(w as u32, h as u32, 8, 0, w, &raw)))
        }
        24 => {
            let row_bytes = w * 3;
            if raw.len() < row_bytes * h {
                return None;
            }
            Some(("image/png", pack_png(w as u32, h as u32, 8, 2, row_bytes, &raw)))
        }
        _ => None,
    }
}

fn has_cmyk_bands(img: &IocaImage) -> bool {
    img.bands.len() == 4 && (1..=4).all(|n| img.bands.contains_key(&n))
}

/// [C, M, Y, K] plane JPEGs from a 4-band JPEG image.
pub fn cmyk_jpeg_bands(img: &IocaImage) -> Option<Vec<Vec<u8>>> {
    if img.compression != COMPRESSION_JPEG || !has_cmyk_bands(img) {
        return None;
    }
    let bands: Vec<Vec<u8>> = (1..=4).map(|n| img.bands[&n].clone()).collect();
    if !bands.iter().all(|b| b.starts_with(&[0xFF, 0xD8])) {
        return None;
    }
    Some(bands)
}

const MAX_LZW_PIXELS: usize = 12_000_000;

/// [C, M, Y, K] plane PNGs from a 4-band LZW image (8-bit ink values).
pub fn lzw_cmyk_bands(img: &IocaImage) -> Option<Vec<Vec<u8>>> {
    if img.compression != COMPRESSION_LZW || !has_cmyk_bands(img) {
        return None;
    }
    if img.width == 0 || img.height == 0 {
        return None;
    }
    let need = img.width.saturating_mul(img.height);
    if need > MAX_LZW_PIXELS {
        log::info!("skipping LZW CMYK image: {}x{} exceeds pixel guard", img.width, img.height);
        return None;
    }
    let mut planes = Vec::new();
    for n in 1..=4u8 {
        let raw = decode_lzw(&img.bands[&n], need);
        if raw.len() < need {
            return None;
        }
        planes.push(pack_png(img.width as u32, img.height as u32, 8, 0, img.width, &raw[..need]));
    }
    Some(planes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal TIFF-variant LZW encoder (port of readAFP's test encoder).
    pub fn encode_lzw(data: &[u8]) -> Vec<u8> {
        use std::collections::HashMap;
        let mut table: HashMap<Vec<u8>, usize> = (0..256usize).map(|i| (vec![i as u8], i)).collect();
        let (mut nxt, mut width) = (258usize, 9usize);
        let mut bits = String::new();
        let emit = |code: usize, w: usize| format!("{code:0w$b}");
        bits += &emit(256, width);
        let mut w: Vec<u8> = Vec::new();
        for &b in data {
            let mut wc = w.clone();
            wc.push(b);
            if table.contains_key(&wc) {
                w = wc;
            } else {
                bits += &emit(table[&w], width);
                table.insert(wc, nxt);
                nxt += 1;
                if nxt == (1 << width) && width < 12 {
                    width += 1;
                }
                w = vec![b];
            }
        }
        if !w.is_empty() {
            bits += &emit(table[&w], width);
        }
        bits += &emit(257, width);
        while !bits.len().is_multiple_of(8) {
            bits.push('0');
        }
        bits.as_bytes().chunks(8).map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap()).collect()
    }

    #[test]
    fn lzw_round_trips() {
        let mut big = Vec::new();
        for _ in 0..4 {
            big.extend(0..=255u8);
        }
        let ab: Vec<u8> = b"ABABABABAB".repeat(20);
        for data in [&b""[..], b"A", b"AAAAAAAA", &ab, &big, &[0u8; 1000]] {
            assert_eq!(decode_lzw(&encode_lzw(data), usize::MAX), data);
        }
    }

    pub fn segment(w: u16, h: u16, compression: u8, bits: u8, data: &[u8]) -> Vec<u8> {
        let mut seg = vec![0x70, 0x00];
        seg.extend([0x94, 0x09, 0x00]);
        seg.extend(720u16.to_be_bytes());
        seg.extend(720u16.to_be_bytes());
        seg.extend(w.to_be_bytes());
        seg.extend(h.to_be_bytes());
        seg.extend([0x95, 0x02, compression, 0x01]);
        seg.extend([0x96, 0x01, bits]);
        seg.extend([0xFE, 0x92]);
        seg.extend((data.len() as u16).to_be_bytes());
        seg.extend(data);
        seg.extend([0x93, 0x00]);
        seg.extend([0x71, 0x00]);
        seg
    }

    #[test]
    fn sdf_framing() {
        let sdfs = parse_sdfs(&[0x94, 0x01, 0xAA, 0xFE, 0x92, 0x00, 0x02, 1, 2]);
        assert_eq!(sdfs, vec![(0x94, &[0xAA][..]), (0xFE92, &[1, 2][..])]);
        // zero length on image data consumes the rest of the stream
        let sdfs = parse_sdfs(&[0xFE, 0x92, 0x00, 0x00, 9, 9, 9]);
        assert_eq!(sdfs, vec![(0xFE92, &[9, 9, 9][..])]);
    }

    #[test]
    fn bilevel_is_inverted() {
        let img = parse_image_segment(&segment(8, 1, COMPRESSION_NONE, 1, &[0xF0])).unwrap();
        assert_eq!((img.width, img.height, img.hres), (8, 1, 720));
        let (mime, png) = image_blob(&img).unwrap();
        assert_eq!(mime, "image/png");
        let (_, _, rows) = crate::png::unpack_rows(&png);
        assert_eq!(rows, vec![vec![0x0F]]);
    }

    #[test]
    fn jpeg_passthrough_and_unknown_compression() {
        let img = parse_image_segment(&segment(1, 1, COMPRESSION_JPEG, 8, &[0, 0xFF, 0xD8, 0xFF])).unwrap();
        assert_eq!(image_blob(&img).unwrap(), ("image/jpeg", vec![0xFF, 0xD8, 0xFF]));
        let img = parse_image_segment(&segment(8, 1, 0x82, 1, &[0xF0])).unwrap();
        assert!(image_blob(&img).is_none());
    }

    #[test]
    fn short_data_is_none() {
        let img = parse_image_segment(&segment(8, 4, COMPRESSION_NONE, 8, &[0; 3])).unwrap();
        assert!(image_blob(&img).is_none());
    }
}
