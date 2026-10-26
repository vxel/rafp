//! Crafted hostile inputs: each must be decoded quickly and with bounded
//! memory, without panicking or aborting, and still yield a valid SVG.

use afp_core::codepages::Codec;
use afp_core::inspect::inspect;
use afp_core::render_svg::page_to_svg;

/// One structured field: 0x5A, length, id, flags and sequence number, data.
fn sf(id: u32, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x5A];
    out.extend_from_slice(&((data.len() + 8) as u16).to_be_bytes());
    out.extend_from_slice(&id.to_be_bytes()[1..]);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(data);
    out
}

fn ebcdic(name: &str) -> Vec<u8> {
    // Digits and capital letters are all that these names use.
    name.bytes()
        .map(|b| match b {
            b'0'..=b'9' => 0xF0 + (b - b'0'),
            b'A'..=b'I' => 0xC1 + (b - b'A'),
            b'J'..=b'R' => 0xD1 + (b - b'J'),
            b'S'..=b'Z' => 0xE2 + (b - b'S'),
            _ => 0x40,
        })
        .collect()
}

/// Inspect with and without embedded small fonts, render every page, and
/// check the SVG is well-formed XML.
fn survives(data: &[u8]) -> usize {
    let mut pages = 0;
    for embed in [false, true] {
        let Ok(ins) = inspect(data, Codec::cp500(), embed) else { continue };
        for page in &ins.pages {
            let svg = page_to_svg(page);
            assert!(!svg.chars().any(|c| c < ' ' && !matches!(c, '\t' | '\n' | '\r')), "control character in the SVG");
            #[cfg(feature = "raster")]
            afp_core::raster::Rasterizer::new().parse(&svg).expect("well-formed SVG");
        }
        pages = ins.pages.len();
    }
    pages
}

/// An IOCA image object whose image segment is `segment`.
fn ioca(segment: &[u8]) -> Vec<u8> {
    let name = [0x40; 8];
    [sf(0xD3A8FB, &name), sf(0xD3EEFB, segment), sf(0xD3A9FB, &name)].concat()
}

/// Begin segment, image size (0xB6) `w` × `h`, compression, IDE size, and a
/// few bytes of image data.
fn image_segment(w: u32, h: u32, compression: u8, ide_size: u8) -> Vec<u8> {
    let mut seg = vec![0x70, 0x00, 0xB6, 0x08];
    seg.extend_from_slice(&w.to_be_bytes());
    seg.extend_from_slice(&h.to_be_bytes());
    seg.extend_from_slice(&[0x95, 0x02, compression, 0x01, 0x96, 0x01, ide_size]);
    seg.extend_from_slice(&[0xFE, 0x92, 0x00, 0x04, 0x00, 0x10, 0x01, 0xFF]);
    seg
}

#[test]
fn huge_mmr_image_size_is_refused() {
    // 268 million × 268 million pixels from 4 bytes of data.
    survives(&ioca(&image_segment(0x1000_0000, 0x1000_0000, 0x01, 0x01)));
}

#[test]
fn overflowing_uncompressed_image_size_is_refused() {
    survives(&ioca(&image_segment(u32::MAX, u32::MAX, 0x03, 0x18)));
}

#[test]
fn utf16_control_characters_keep_the_svg_valid() {
    let name = [0x40; 8];
    // TRN (0xDA) with U+0001 then "ABC" in UTF-16BE.
    let ptx = [0x2B, 0xD3, 10, 0xDA, 0x00, 0x01, 0x00, 0x41, 0x00, 0x42, 0x00, 0x43];
    let file = [sf(0xD3A8AF, &name), sf(0xD3EE9B, &ptx), sf(0xD3A9AF, &name)].concat();
    assert_eq!(survives(&file), 1);
}

#[test]
fn nested_overlays_stay_bounded() {
    // Overlay k includes overlay k-1 twice: 2^30 copies without a cap.
    let ipo = |name: &[u8]| sf(0xD3AFD8, &[name, &[0; 6]].concat());
    let ptx = [0x2B, 0xD3, 4, 0xDA, 0xC1, 0xC2];
    let ov = |k: usize| ebcdic(&format!("OV{k:06}"));
    let mut file = [sf(0xD3A8DF, &ov(0)), sf(0xD3EE9B, &ptx), sf(0xD3A9DF, &ov(0))].concat();
    for k in 1..30 {
        file.extend([sf(0xD3A8DF, &ov(k)), ipo(&ov(k - 1)), ipo(&ov(k - 1)), sf(0xD3A9DF, &ov(k))].concat());
    }
    let name = [0x40; 8];
    file.extend(
        [sf(0xD3A8A8, &name), sf(0xD3A8AF, &name), ipo(&ov(29)), sf(0xD3A9AF, &name), sf(0xD3A9A8, &name)].concat(),
    );
    let ins = inspect(&file, Codec::cp500(), false).unwrap();
    let page = ins.pages.last().unwrap();
    assert!(page.truncated);
    assert!(page.texts.len() <= afp_core::model::MAX_RUNS_PER_PAGE);
    survives(&file);
}

/// Type 1 eexec / charstring encryption.
fn type1_encrypt(plain: &[u8], mut r: u16) -> Vec<u8> {
    plain
        .iter()
        .map(|&p| {
            let c = p ^ (r >> 8) as u8;
            r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
            c
        })
        .collect()
}

#[test]
fn self_referencing_seac_glyph_fails_cleanly() {
    // Glyph "A" = seac using "A" as both base and accent: endless recursion.
    let num = |v: u8| v + 139;
    let cs = [num(0), num(0), num(0), num(65), num(65), 12, 6];
    let ecs = type1_encrypt(&[&[0u8; 4][..], &cs].concat(), 4330);
    let mut private = b"dup /Private 8 dict dup begin /lenIV 4 def\n/CharStrings 2 dict dup begin\n".to_vec();
    private.extend_from_slice(format!("/A {} RD ", ecs.len()).as_bytes());
    private.extend_from_slice(&ecs);
    private.extend_from_slice(b" ND\nend\n");
    let mut program =
        b"%!PS-AdobeFont-1.0: T\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\ncurrentfile eexec\n".to_vec();
    program.extend(type1_encrypt(&[&[0u8; 4][..], &private].concat(), 55665));

    let gcgid = ebcdic("LA020000");
    let mut fnc = [0u8; 28];
    fnc[1] = 0x1E;
    fnc[15] = 28;
    let fni = [&gcgid[..], &[0; 20]].concat();
    let pool_offset: u32 = 2 + 12 + 12;
    let fnn = [&[0, 0][..], &gcgid, &pool_offset.to_be_bytes(), &[0; 12], &[2], b"A"].concat();
    let name = ebcdic("C0TEST00");
    let file = [
        sf(0xD3A889, &name),
        sf(0xD3A789, &fnc),
        sf(0xD38C89, &fni),
        sf(0xD3AB89, &fnn),
        sf(0xD3EE89, &program),
        sf(0xD3A989, &name),
    ]
    .concat();
    survives(&file);
}
