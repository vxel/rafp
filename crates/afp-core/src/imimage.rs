//! IM Image Object (IM/1) decoder.
//!
//! The IM image object is MO:DCA's legacy bilevel raster, carried by
//! BII / IOC / IID / ICP / IRD / EII fields. An image is either *simple*
//! (one IRD covering the whole image) or *celled*: a grid of (ICP, IRD)
//! pairs, each cell optionally replicated across a fill rectangle. Every
//! cell is reassembled into one XSize×YSize bitmap and packed as a PNG.
//!
//! Reference: MO:DCA Reference, AFPC-0004-10, Appendix C.

use crate::png::pack_png;
use crate::util::{be_uint, u16_at};

const MAX_IM_PIXELS: usize = 8_000_000;
const FFFF: usize = 0xFFFF;

/// A decoded IM image: a bilevel PNG plus its geometry.
#[derive(Debug, Clone)]
pub struct ImImage {
    /// XSize, in image points (pels).
    pub width: usize,
    pub height: usize,
    /// Image points per inch (XUnits / 10).
    pub resolution: i64,
    /// Packed 1-bpp grayscale PNG (0 = black).
    pub png: Vec<u8>,
}

/// The object-area origin (XoaOset, YoaOset) from an IOC field, in image
/// points.
pub fn parse_ioc_origin(ioc: &[u8]) -> (i64, i64) {
    if ioc.len() < 6 {
        return (0, 0);
    }
    (be_uint(ioc, 0, 3) as i64, be_uint(ioc, 3, 3) as i64)
}

/// IID fixed layout: (xsize, ysize, resolution, xcsized, ycsized).
fn parse_iid(iid: &[u8]) -> Option<(usize, usize, i64, usize, usize)> {
    if iid.len() < 32 {
        return None;
    }
    Some((
        u16_at(iid, 18) as usize,
        u16_at(iid, 20) as usize,
        u16_at(iid, 14) as i64 / 10,
        u16_at(iid, 28) as usize,
        u16_at(iid, 30) as usize,
    ))
}

/// Reassemble an IM image object into a bilevel PNG. `cells` is the
/// sequence of (ICP data or `None`, IRD data) pairs between BII and EII.
pub fn decode_im_image(iid: &[u8], cells: &[(Option<Vec<u8>>, Vec<u8>)]) -> Option<ImImage> {
    let (xsize, ysize, resolution, xcsized, ycsized) = parse_iid(iid)?;
    if xsize == 0 || ysize == 0 {
        return None;
    }
    if xsize * ysize > MAX_IM_PIXELS {
        log::info!("skipping oversized IM image: {xsize}×{ysize} pels");
        return None;
    }
    let row_bytes = xsize.div_ceil(8);
    let mut bitmap = vec![0u8; row_bytes * ysize];
    // Pixels painted so far: overlapping cells could otherwise repaint the
    // image without end.
    let mut work = 0usize;

    for (icp, ird) in cells {
        let (xco, yco, xcs, ycs, xfil, yfil) = match icp {
            Some(icp) if icp.len() >= 8 => {
                let (xco, yco) = (u16_at(icp, 0) as usize, u16_at(icp, 2) as usize);
                let mut xcs = u16_at(icp, 4) as usize;
                let mut ycs = u16_at(icp, 6) as usize;
                if xcs == FFFF {
                    xcs = xcsized;
                }
                if ycs == FFFF {
                    ycs = ycsized;
                }
                let mut xfil = if icp.len() >= 10 { u16_at(icp, 8) as usize } else { xcs };
                let mut yfil = if icp.len() >= 12 { u16_at(icp, 10) as usize } else { ycs };
                if xfil == FFFF {
                    xfil = xcs;
                }
                if yfil == FFFF {
                    yfil = ycs;
                }
                (xco, yco, xcs, ycs, xfil, yfil)
            }
            _ => (0, 0, xsize, ysize, xsize, ysize),
        };
        if xcs == 0 || ycs == 0 {
            continue;
        }
        work += xfil.min(xsize.saturating_sub(xco)) * yfil.min(ysize.saturating_sub(yco));
        if work > 4 * MAX_IM_PIXELS {
            log::info!("IM image: stopping after {work} painted pels");
            break;
        }
        let cell_row_bytes = xcs.div_ceil(8);
        for fy in 0..yfil {
            let dy = yco + fy;
            if dy >= ysize {
                break;
            }
            let src_row = (fy % ycs) * cell_row_bytes;
            let dst_row = dy * row_bytes;
            for fx in 0..xfil {
                let dx = xco + fx;
                if dx >= xsize {
                    break;
                }
                let sx = fx % xcs;
                let Some(&byte) = ird.get(src_row + (sx >> 3)) else { continue };
                if (byte >> (7 - (sx & 7))) & 1 != 0 {
                    bitmap[dst_row + (dx >> 3)] |= 0x80 >> (dx & 7);
                }
            }
        }
    }
    // IM pel 1 = toned (black); PNG grayscale 0 = black — invert.
    let inverted: Vec<u8> = bitmap.iter().map(|b| b ^ 0xFF).collect();
    let png = pack_png(xsize as u32, ysize as u32, 1, 0, row_bytes, &inverted);
    Some(ImImage { width: xsize, height: ysize, resolution, png })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iid(xsize: u16, ysize: u16, xcs: u16, ycs: u16, units: u16) -> Vec<u8> {
        let mut d = vec![0u8; 32];
        d[14..16].copy_from_slice(&units.to_be_bytes());
        d[18..20].copy_from_slice(&xsize.to_be_bytes());
        d[20..22].copy_from_slice(&ysize.to_be_bytes());
        d[28..30].copy_from_slice(&xcs.to_be_bytes());
        d[30..32].copy_from_slice(&ycs.to_be_bytes());
        d
    }

    fn icp(vals: &[u16]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn rows(img: &ImImage) -> Vec<Vec<u8>> {
        crate::png::unpack_rows(&img.png).2
    }

    #[test]
    fn simple_image_polarity() {
        let im = decode_im_image(&iid(8, 2, 0, 0, 3000), &[(None, vec![0xF0, 0x0F])]).unwrap();
        assert_eq!((im.width, im.height, im.resolution), (8, 2, 300));
        assert_eq!(rows(&im), vec![vec![0x0F], vec![0xF0]]);
    }

    #[test]
    fn cell_fill_replicates_and_clips() {
        // A 2x1 cell "10" replicated over a 6x2 fill, clipped to a 5x2 image.
        let cells = vec![(Some(icp(&[0, 0, 2, 1, 6, 2])), vec![0x80])];
        let im = decode_im_image(&iid(5, 2, 0, 0, 3000), &cells).unwrap();
        // pels 0,2,4 black → bits 10101 → 0xA8; inverted → 0x57
        assert_eq!(rows(&im), vec![vec![0x57], vec![0x57]]);
    }

    #[test]
    fn ffff_uses_iid_defaults() {
        let cells = vec![(Some(icp(&[0, 0, 0xFFFF, 0xFFFF])), vec![0xFF])];
        let im = decode_im_image(&iid(8, 1, 8, 1, 3000), &cells).unwrap();
        assert_eq!(rows(&im), vec![vec![0x00]]);
    }

    #[test]
    fn zero_size_and_ioc() {
        assert!(decode_im_image(&iid(0, 1, 0, 0, 3000), &[]).is_none());
        assert_eq!(parse_ioc_origin(&[0, 0, 10, 0, 1, 0]), (10, 256));
    }
}
