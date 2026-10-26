//! BCOCA bar code object decoding.
//!
//! A bar code object (BBC...EBC) carries a Bar Code Data Descriptor (BDD) —
//! units, symbology type/modifier, module width — and Bar Code Data (BDA) —
//! flags, the symbol origin, symbology-specific parameters and the data.
//!
//! Only QR Code (type X'20') is generated, packed as a bilevel PNG. Other
//! symbologies (Data Matrix X'1C', the 1D codes) are skipped rather than
//! drawn as the wrong symbol.
//!
//! Reference: BCOCA Reference, AFPC-0005-11.

use qrcode::{Color, EcLevel, QrCode, Version};

use crate::codepages::Codec;
use crate::png::pack_png;
use crate::util::{py_round, u16_at};

pub const TYPE_DATA_MATRIX: u8 = 0x1C;
pub const TYPE_QR: u8 = 0x20;

/// One decoded bar code object, ready to generate.
#[derive(Debug, Clone, PartialEq)]
pub struct BarCode {
    pub bc_type: u8,
    pub modifier: u8,
    /// BSD units per inch.
    pub upi: i64,
    /// Module width in thousandths of an inch.
    pub module_mils: i64,
    pub data: String,
    /// QR: desired symbol version, 0 = smallest.
    pub version: u8,
    /// QR: 0-3 = L M Q H.
    pub ec_level: u8,
    /// Symbol origin within the object area, in BSD units.
    pub x: i64,
    pub y: i64,
}

/// Combine BDD and BDA field data into a bar code, or `None` if malformed
/// or suppressed.
pub fn parse_barcode(bdd: &[u8], bda: &[u8]) -> Option<BarCode> {
    if bdd.len() < 18 || bda.len() < 5 {
        return None;
    }
    let xupub = u16_at(bdd, 2) as i64;
    let upi = if bdd[0] == 0x01 {
        py_round(xupub as f64 * 2.54 / 10.0) // units per 10 centimeters
    } else {
        xupub / 10 // units per 10 inches
    };
    let (bc_type, modifier) = (bdd[12], bdd[13]);
    let mut module_mils = bdd[17] as i64;
    if module_mils == 0x00 || module_mils == 0xFF {
        module_mils = 13; // device default
    }
    if bda[0] & 0x04 != 0 {
        return None; // suppress the bar code symbol
    }
    let (x, y) = (u16_at(bda, 1) as i64, u16_at(bda, 3) as i64);
    let (mut version, mut ec_level) = (0u8, 0u8);
    let mut payload = &bda[5..];
    let mut ebcdic = false;
    if bc_type == TYPE_QR && bda.len() >= 14 {
        ebcdic = bda[5] & 0x80 != 0;
        version = if bda[7] <= 40 { bda[7] } else { 0 };
        ec_level = if bda[8] <= 3 { bda[8] } else { 0 };
        if bda[10] != 0 {
            log::info!("QR structured append not supported; rendering symbol {}/{} alone", bda[9], bda[10]);
        }
        payload = &bda[14..];
    } else if bc_type == TYPE_DATA_MATRIX && bda.len() >= 15 {
        ebcdic = bda[5] & 0x80 != 0;
        payload = &bda[15..];
    }
    let data = if ebcdic {
        Codec::cp500().decode_replace(payload)
    } else {
        Codec::lookup("ascii")
            .unwrap()
            .decode_strict(payload)
            .unwrap_or_else(|| Codec::latin1().decode_replace(payload))
    };
    Some(BarCode {
        bc_type,
        modifier,
        upi: if upi != 0 { upi } else { 1440 },
        module_mils,
        data,
        version,
        ec_level,
        x,
        y,
    })
}

/// QR byte-mode payload: ISO-8859-1 when the text fits, else UTF-8.
fn qr_bytes(text: &str) -> Vec<u8> {
    if text.chars().all(|c| (c as u32) < 0x100) {
        text.chars().map(|c| c as u8).collect()
    } else {
        text.as_bytes().to_vec()
    }
}

/// Generate the symbol as (PNG bytes, modules per side). Only QR is
/// generated; other symbologies return `None`.
pub fn barcode_png(bar: &BarCode) -> Option<(Vec<u8>, usize)> {
    if bar.bc_type != TYPE_QR {
        if bar.bc_type == TYPE_DATA_MATRIX {
            log::info!("Data Matrix symbol not supported; skipping");
        } else {
            log::info!("bar code type 0x{:02X} not supported", bar.bc_type);
        }
        return None;
    }
    let ec =
        [EcLevel::L, EcLevel::M, EcLevel::Q, EcLevel::H].get(usize::from(bar.ec_level)).copied().unwrap_or(EcLevel::L);
    let data = qr_bytes(&bar.data);
    let code = if bar.version > 0 {
        // A too-small requested version grows to the smallest that fits.
        QrCode::with_version(&data, Version::Normal(bar.version as i16), ec)
            .or_else(|_| QrCode::with_error_correction_level(&data, ec))
    } else {
        QrCode::with_error_correction_level(&data, ec)
    };
    let code = match code {
        Ok(c) => c,
        Err(e) => {
            log::warn!("cannot encode QR data: {e}");
            return None;
        }
    };
    let n = code.width();
    let colors = code.to_colors();
    let row_bytes = n.div_ceil(8);
    let mut raster = vec![0xFFu8; row_bytes * n]; // PNG gray: 1 = white
    for r in 0..n {
        for c in 0..n {
            if colors[r * n + c] == Color::Dark {
                raster[r * row_bytes + c / 8] &= !(0x80 >> (c % 8));
            }
        }
    }
    Some((pack_png(n as u32, n as u32, 1, 0, row_bytes, &raster), n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bdd(unit_base: u8, xupub: u16, bc_type: u8, mils: u8) -> Vec<u8> {
        let mut d = vec![0u8; 18];
        d[0] = unit_base;
        d[2..4].copy_from_slice(&xupub.to_be_bytes());
        d[12] = bc_type;
        d[17] = mils;
        d
    }

    fn qr_bda(version: u8, data: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8, 0, 1, 0, 1, 0, 0, version, 0, 0, 0, 0, 0, 0];
        d.extend(data);
        d
    }

    #[test]
    fn qr_version_is_honored_and_grows() {
        let bar = parse_barcode(&bdd(0, 14400, TYPE_QR, 20), &qr_bda(18, b"hello")).unwrap();
        assert_eq!(bar.upi, 1440);
        assert_eq!((bar.x, bar.y), (1, 1));
        assert_eq!(barcode_png(&bar).unwrap().1, 89);
        let long = vec![b'x'; 200];
        let bar = parse_barcode(&bdd(0, 14400, TYPE_QR, 20), &qr_bda(1, &long)).unwrap();
        assert!(barcode_png(&bar).unwrap().1 > 21);
        let huge = vec![b'7'; 8000];
        let bar = parse_barcode(&bdd(0, 14400, TYPE_QR, 20), &qr_bda(0, &huge)).unwrap();
        assert!(barcode_png(&bar).is_none());
    }

    #[test]
    fn other_symbologies_and_flags() {
        let bar = parse_barcode(&bdd(0, 14400, 0x01, 0), &[0, 0, 0, 0, 0, 0xC1]).unwrap();
        assert_eq!(bar.module_mils, 13);
        assert!(barcode_png(&bar).is_none());
        assert!(parse_barcode(&bdd(0, 14400, TYPE_QR, 20), &[0x04, 0, 0, 0, 0]).is_none());
        let bar = parse_barcode(&bdd(1, 3937, TYPE_QR, 20), &qr_bda(0, b"a")).unwrap();
        assert_eq!(bar.upi, 1000);
    }
}
