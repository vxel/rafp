//! Minimal PNG writer (filter 0 on every row, one IDAT), shared by the IOCA,
//! IM image, FOCA glyph and bar code decoders.

use std::cell::RefCell;

use flate2::{Compress, Compression, FlushCompress, Status};

thread_local! {
    // A compressor's state is large; reuse one per thread instead of
    // allocating it for each of the thousands of small glyph bitmaps.
    static COMPRESSOR: RefCell<Compress> = RefCell::new(Compress::new(Compression::fast(), true));
}

fn zlib(data: &[u8]) -> Vec<u8> {
    COMPRESSOR.with(|c| {
        let mut c = c.borrow_mut();
        c.reset();
        let mut out = Vec::with_capacity(data.len() / 2 + 64);
        loop {
            let consumed = c.total_in() as usize;
            if out.capacity() - out.len() < 64 {
                out.reserve(out.capacity().max(1024));
            }
            match c.compress_vec(&data[consumed..], &mut out, FlushCompress::Finish) {
                Ok(Status::StreamEnd) => break,
                Ok(_) => {}
                Err(e) => panic!("in-memory zlib failed: {e}"),
            }
        }
        out
    })
}

fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(tag);
    crc.update(body);
    out.extend_from_slice(tag);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

/// Pack raw scanlines into a PNG.
///
/// `pixels` is the raster as PNG expects it: `row_bytes` bytes per scanline,
/// gray 0 = black. Short input is padded with zero bytes.
pub fn pack_png(width: u32, height: u32, bit_depth: u8, color_type: u8, row_bytes: usize, pixels: &[u8]) -> Vec<u8> {
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);

    let mut raster = Vec::with_capacity((row_bytes + 1) * height as usize);
    for row in 0..height as usize {
        raster.push(0);
        let start = (row * row_bytes).min(pixels.len());
        let end = ((row + 1) * row_bytes).min(pixels.len());
        raster.extend_from_slice(&pixels[start..end]);
        raster.resize(raster.len() + row_bytes - (end - start), 0);
    }
    let idat = zlib(&raster);

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Decode the raw (unfiltered) rows back from a PNG written by [`pack_png`];
/// test helper.
#[cfg(test)]
pub fn unpack_rows(png: &[u8]) -> (u32, u32, Vec<Vec<u8>>) {
    use std::io::Read;
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
    let idat = &png[41..41 + idat_len];
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(idat).read_to_end(&mut raw).unwrap();
    let row = raw.len() / h as usize;
    let rows = raw.chunks(row).map(|r| r[1..].to_vec()).collect();
    (w, h, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let png = pack_png(9, 2, 1, 0, 2, &[0xFF, 0x80, 0x00, 0x00]);
        assert!(png.starts_with(b"\x89PNG"));
        let (w, h, rows) = unpack_rows(&png);
        assert_eq!((w, h), (9, 2));
        assert_eq!(rows, vec![vec![0xFF, 0x80], vec![0, 0]]);
    }
}
