//! Pixel helpers for clipboard images: clipboard DIB <-> BGRA, PNG through
//! WIC (part of Windows, so no extra dependency), and thumbnails.

use windows::core::{Error, Result};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
use windows::Win32::UI::Shell::SHCreateMemStream;

/// Larger images are not recorded (about 190 MB of raw pixels).
const MAX_PIXELS: usize = 48_000_000;

/// Opaque top-down BGRA pixels.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Pixels {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// Reads a packed clipboard DIB (`CF_DIB`): 24 or 32 bits per pixel, uncompressed.
pub fn from_dib(dib: &[u8]) -> Option<Pixels> {
    let u32_at = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let header = u32_at(0)? as usize;
    if header < 40 {
        return None;
    }
    let (w, h) = (u32_at(4)? as i32, u32_at(8)? as i32);
    let bits = u16::from_le_bytes([*dib.get(14)?, *dib.get(15)?]) as usize;
    let compression = u32_at(16)?;
    let colours = u32_at(32)? as usize;
    // BI_RGB (0) or BI_BITFIELDS (3) with the usual blue-green-red order.
    if w <= 0 || h == 0 || !(bits == 24 || bits == 32) || !(compression == 0 || compression == 3) {
        return None;
    }
    let (w, rows, top_down) = (w as usize, h.unsigned_abs() as usize, h < 0);
    if w.checked_mul(rows)? > MAX_PIXELS {
        return None;
    }
    // A 40-byte header with BI_BITFIELDS is followed by three colour masks.
    let masks = if compression == 3 && header == 40 { 12 } else { 0 };
    let start = header + masks + colours * 4;
    let bpp = bits / 8;
    let stride = (w * bpp + 3) & !3;
    let data = dib.get(start..start + stride * rows)?;
    let mut bgra = Vec::with_capacity(w * rows * 4);
    for y in 0..rows {
        let src = if top_down { y } else { rows - 1 - y };
        for px in data[src * stride..src * stride + w * bpp].chunks_exact(bpp) {
            // Clipboard alpha is unreliable (often all zero): treat images as opaque.
            bgra.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    Some(Pixels { w: w as u32, h: rows as u32, bgra })
}

/// Packs pixels as a 32-bit bottom-up DIB for the clipboard.
pub fn to_dib(p: &Pixels) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + p.bgra.len());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(p.w as i32).to_le_bytes());
    out.extend_from_slice(&(p.h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(p.bgra.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    let stride = p.w as usize * 4;
    for row in p.bgra.chunks_exact(stride.max(1)).rev() {
        out.extend_from_slice(row);
    }
    out
}

/// A content hash that names the image on disk and spots repeated copies.
pub fn key(p: &Pixels) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ ((p.w as u64) << 32 | p.h as u64);
    let (chunks, rest) = p.bgra.as_chunks::<8>();
    for chunk in chunks {
        hash = (hash ^ u64::from_le_bytes(*chunk)).wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= hash >> 29;
    }
    for &byte in rest {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash.max(1)
}

/// Scales down to fit `max_w` x `max_h` (never up), averaging a few samples per pixel.
pub fn shrink(p: &Pixels, max_w: u32, max_h: u32) -> Pixels {
    if p.w == 0 || p.h == 0 {
        return Pixels::default();
    }
    let k = (max_w as f32 / p.w as f32).min(max_h as f32 / p.h as f32).min(1.0);
    let w = ((p.w as f32 * k).round() as u32).max(1);
    let h = ((p.h as f32 * k).round() as u32).max(1);
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let (y0, y1) = (y * p.h / h, ((y + 1) * p.h / h).max(y * p.h / h + 1));
        for x in 0..w {
            let (x0, x1) = (x * p.w / w, ((x + 1) * p.w / w).max(x * p.w / w + 1));
            // Up to 4 x 4 samples spread over the source block.
            let (sx, sy) = (((x1 - x0) / 4).max(1), ((y1 - y0) / 4).max(1));
            let (mut sum, mut count) = ([0u32; 3], 0u32);
            for yy in (y0..y1).step_by(sy as usize) {
                for xx in (x0..x1).step_by(sx as usize) {
                    let i = ((yy * p.w + xx) * 4) as usize;
                    for (total, &value) in sum.iter_mut().zip(&p.bgra[i..i + 3]) {
                        *total += value as u32;
                    }
                    count += 1;
                }
            }
            bgra.extend_from_slice(&[(sum[0] / count) as u8, (sum[1] / count) as u8, (sum[2] / count) as u8, 255]);
        }
    }
    Pixels { w, h, bgra }
}

/// Twice the size, each pixel repeated (small screen text reads better enlarged).
pub fn double(p: &Pixels) -> Pixels {
    let (w, h) = (p.w as usize, p.h as usize);
    let mut bgra = Vec::with_capacity(w * h * 16);
    for row in p.bgra.chunks_exact((w * 4).max(1)) {
        let start = bgra.len();
        for px in row.as_chunks::<4>().0 {
            bgra.extend_from_slice(px);
            bgra.extend_from_slice(px);
        }
        bgra.extend_from_within(start..);
    }
    Pixels { w: p.w * 2, h: p.h * 2, bgra }
}

/// A 32-bit BMP file (tests dump pictures this way).
pub fn to_bmp(p: &Pixels) -> Vec<u8> {
    let dib = to_dib(p);
    let mut out = Vec::with_capacity(14 + dib.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(14 + dib.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&dib);
    out
}

/// PNG bytes for the pixels. COM must be initialised on the calling thread.
pub fn encode_png(p: &Pixels) -> Option<Vec<u8>> {
    unsafe { encode(p, false).ok() }
}

/// JPEG bytes (WIC's default quality, 0.9). COM must be initialised on the calling thread.
pub fn encode_jpeg(p: &Pixels) -> Option<Vec<u8>> {
    unsafe { encode(p, true).ok() }
}

unsafe fn encode(p: &Pixels, jpeg: bool) -> Result<Vec<u8>> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = SHCreateMemStream(None).ok_or_else(Error::empty)?;
        let encoder = factory.CreateEncoder(if jpeg { &GUID_ContainerFormatJpeg } else { &GUID_ContainerFormatPng }, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let (mut frame, mut bag) = (None, None);
        encoder.CreateNewFrame(&mut frame, &mut bag)?;
        let frame = frame.ok_or_else(Error::empty)?;
        frame.Initialize(bag.as_ref())?;
        frame.SetSize(p.w, p.h)?;
        // JPEG has no alpha: it takes 24-bit pixels.
        let wanted = if jpeg { GUID_WICPixelFormat24bppBGR } else { GUID_WICPixelFormat32bppBGRA };
        let mut format = wanted;
        frame.SetPixelFormat(&mut format)?;
        if format != wanted {
            return Err(Error::empty());
        }
        if jpeg {
            let stride = (p.w * 3 + 3) & !3;
            let mut rows = vec![0u8; (stride * p.h) as usize];
            for (y, row) in p.bgra.chunks_exact((p.w * 4) as usize).enumerate() {
                for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
                    let at = y * stride as usize + x * 3;
                    rows[at..at + 3].copy_from_slice(&px[..3]);
                }
            }
            frame.WritePixels(p.h, stride, &rows)?;
        } else {
            frame.WritePixels(p.h, p.w * 4, &p.bgra)?;
        }
        frame.Commit()?;
        encoder.Commit()?;
        read_all(&stream)
    }
}

unsafe fn read_all(stream: &IStream) -> Result<Vec<u8>> {
    unsafe {
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = vec![0u8; stat.cbSize as usize];
        let mut got = 0u32;
        stream.Read(bytes.as_mut_ptr() as *mut _, bytes.len() as u32, Some(&mut got)).ok()?;
        bytes.truncate(got as usize);
        Ok(bytes)
    }
}

/// Decodes a PNG (or any format WIC knows). COM must be initialised on the calling thread.
pub fn decode_png(bytes: &[u8]) -> Option<Pixels> {
    unsafe { decode(bytes).ok() }
}

unsafe fn decode(bytes: &[u8]) -> Result<Pixels> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = SHCreateMemStream(Some(bytes)).ok_or_else(Error::empty)?;
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let source = WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA, &frame)?;
        let (mut w, mut h) = (0u32, 0u32);
        source.GetSize(&mut w, &mut h)?;
        if w == 0 || h == 0 || w as usize * h as usize > MAX_PIXELS {
            return Err(Error::empty());
        }
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        source.CopyPixels(std::ptr::null(), w * 4, &mut bgra)?;
        for px in bgra.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Ok(Pixels { w, h, bgra })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    fn sample(w: u32, h: u32) -> Pixels {
        let mut bgra = Vec::new();
        for y in 0..h {
            for x in 0..w {
                bgra.extend_from_slice(&[(x * 7) as u8, (y * 11) as u8, (x + y) as u8, 255]);
            }
        }
        Pixels { w, h, bgra }
    }

    #[test]
    fn dib_round_trip_and_24_bit() {
        let p = sample(5, 3);
        assert_eq!(from_dib(&to_dib(&p)).unwrap(), p);
        // 24-bit, bottom-up, rows padded to 4 bytes: 2 x 2 pixels.
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        dib.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]); // bottom row
        dib.extend_from_slice(&[7, 8, 9, 10, 11, 12, 0, 0]); // top row
        let p = from_dib(&dib).unwrap();
        assert_eq!(p.bgra, [7, 8, 9, 255, 10, 11, 12, 255, 1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(from_dib(&dib[..50]), None);
        assert_eq!(from_dib(b"junk"), None);
    }

    #[test]
    fn png_round_trip_shrink_and_key() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let p = sample(64, 40);
        let png = encode_png(&p).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(decode_png(&png).unwrap(), p);
        assert_eq!(decode_png(b"not an image"), None);
        // JPEG is lossy: same size, colours of a smooth picture close.
        let smooth = Pixels { w: 64, h: 40, bgra: (0..40u32).flat_map(|y| (0..64u32).flat_map(move |x| [(x * 3) as u8, (y * 4) as u8, 120, 255])).collect() };
        let jpeg = encode_jpeg(&smooth).unwrap();
        assert_eq!(&jpeg[..2], [0xff, 0xd8]);
        let back = decode_png(&jpeg).unwrap();
        assert_eq!((back.w, back.h), (64, 40));
        let error = back.bgra.iter().zip(&smooth.bgra).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(error < 40, "jpeg error {error}");
        let small = shrink(&p, 16, 16);
        assert_eq!((small.w, small.h), (16, 10));
        assert_eq!(small.bgra.len(), 16 * 10 * 4);
        assert_eq!(shrink(&small, 100, 100), small);
        assert_eq!(key(&p), key(&p.clone()));
        assert_ne!(key(&p), key(&small));
    }
}
