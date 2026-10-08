// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;

fn chunk(out: &mut Vec<u8>, tag: [u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&tag);
    out.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(&tag);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

fn encode(pixels: &[u8], width: usize, height: usize, channels: usize, colour_type: u8) -> Vec<u8> {
    let row = width * channels;
    let mut raw = Vec::with_capacity(height * (row + 1));
    for y in 0..height {
        raw.push(0);
        raw.extend_from_slice(&pixels[y * row..(y + 1) * row]);
    }
    let mut z = ZlibEncoder::new(Vec::with_capacity(raw.len() / 2 + 64), Compression::new(6));

    let compressed: Vec<u8> = z.write_all(&raw).and_then(|()| z.finish()).unwrap_or_default();
    let mut out = Vec::with_capacity(compressed.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&u32::try_from(width).unwrap_or(u32::MAX).to_be_bytes());
    ihdr.extend_from_slice(&u32::try_from(height).unwrap_or(u32::MAX).to_be_bytes());
    ihdr.extend_from_slice(&[8, colour_type, 0, 0, 0]);
    chunk(&mut out, *b"IHDR", &ihdr);
    chunk(&mut out, *b"IDAT", &compressed);
    chunk(&mut out, *b"IEND", &[]);
    out
}

#[must_use]
pub fn write_png(pixels: &[u8], width: usize, height: usize) -> Vec<u8> {
    encode(pixels, width, height, 3, 2)
}

#[must_use]
pub fn write_png_rgba(pixels: &[u8], width: usize, height: usize) -> Vec<u8> {
    encode(pixels, width, height, 4, 6)
}



pub fn read_png(bytes: &[u8]) -> Result<(usize, usize, usize, Vec<u8>), String> {
    use std::io::Read as _;
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("not a PNG".into());
    }
    let mut pos = 8;
    let (mut width, mut height, mut channels) = (0usize, 0usize, 0usize);
    let mut idat = Vec::new();
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        let tag = &bytes[pos + 4..pos + 8];
        let data = bytes.get(pos + 8..pos + 8 + len).ok_or("truncated chunk")?;
        match tag {
            b"IHDR" => {
                width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
                height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
                if data[8] != 8 || data[12] != 0 {
                    return Err("only 8-bit non-interlaced PNG".into());
                }
                channels = match data[9] {
                    2 => 3,
                    6 => 4,
                    _ => return Err("only RGB/RGBA PNG".into()),
                };
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let row = width * channels;
    if raw.len() != height * (row + 1) {
        return Err("IDAT size mismatch".into());
    }
    let mut out = vec![0u8; height * row];
    for y in 0..height {
        let filter = raw[y * (row + 1)];
        let src = &raw[y * (row + 1) + 1..(y + 1) * (row + 1)];
        for x in 0..row {
            let a = if x >= channels { out[y * row + x - channels] } else { 0 };
            let b = if y > 0 { out[(y - 1) * row + x] } else { 0 };
            let c = if y > 0 && x >= channels { out[(y - 1) * row + x - channels] } else { 0 };
            let pred = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => u8::try_from(u16::midpoint(u16::from(a), u16::from(b))).unwrap_or(u8::MAX),
                4 => {
                    let p = i16::from(a) + i16::from(b) - i16::from(c);
                    let (pa, pb, pc) =
                        ((p - i16::from(a)).abs(), (p - i16::from(b)).abs(), (p - i16::from(c)).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => return Err("bad filter".into()),
            };
            out[y * row + x] = src[x].wrapping_add(pred);
        }
    }
    Ok((width, height, channels, out))
}

