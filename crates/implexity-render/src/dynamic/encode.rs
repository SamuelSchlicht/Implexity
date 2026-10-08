// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::io::Write;

use implexity_mesh::raster::RgbImage;

use crate::RenderError;

fn bound(e: impl std::fmt::Display) -> RenderError {
    RenderError::Invalid(e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationFormat {
    Webp,
    Gif,
    Mp4,
    PngSequence,
}

impl AnimationFormat {


    pub fn parse(s: &str) -> Result<Self, RenderError> {
        match s {
            "webp" => Ok(Self::Webp),
            "gif" => Ok(Self::Gif),
            "mp4" => Ok(Self::Mp4),
            "png_sequence" => Ok(Self::PngSequence),
            other => Err(RenderError::Invalid(format!(
                "animation format must be webp, gif, mp4 or png_sequence, not {other:?}"
            ))),
        }
    }

    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
            Self::Mp4 => "video/mp4",
            Self::PngSequence => "application/zip",
        }
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Gif => "gif",
            Self::Mp4 => "mp4",
            Self::PngSequence => "zip",
        }
    }
}

enum State {
    Gif(Box<gif::Encoder<Vec<u8>>>),
    Webp(Vec<u8>),
    Mp4 { enc: Option<less_avc::LessEncoder>, sps: Vec<u8>, pps: Vec<u8>, samples: Vec<u8>, sizes: Vec<u32> },
    Png(Vec<(String, Vec<u8>, implexity_mesh::zip::ZipMethod)>),
}

pub struct Animation {
    format: AnimationFormat,
    width: usize,
    height: usize,
    fps: u32,
    frames: usize,
    bytes: usize,
    limit: usize,
    state: State,
}

impl std::fmt::Debug for Animation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Animation")
            .field("format", &self.format)
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl Animation {


    pub fn new(
        format: AnimationFormat,
        width: usize,
        height: usize,
        fps: u32,
        limit: usize,
    ) -> Result<Self, RenderError> {
        if !(1..=60).contains(&fps) || width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(RenderError::Invalid(
                "animations need 1..60 frames per second and 1..4096 pixels per side".into(),
            ));
        }
        if format == AnimationFormat::Mp4 && (width % 2 == 1 || height % 2 == 1) {
            return Err(RenderError::Invalid("MP4 (4:2:0) frames need an even width and height".into()));
        }
        let state = match format {
            AnimationFormat::Gif => {
                let (w, h) = (u16::try_from(width).map_err(bound)?, u16::try_from(height).map_err(bound)?);
                let mut enc = gif::Encoder::new(Vec::new(), w, h, &[]).map_err(bound)?;
                enc.set_repeat(gif::Repeat::Infinite).map_err(bound)?;
                State::Gif(Box::new(enc))
            }
            AnimationFormat::Webp => State::Webp(Vec::new()),
            AnimationFormat::Mp4 => State::Mp4 {
                enc: None,
                sps: Vec::new(),
                pps: Vec::new(),
                samples: Vec::new(),
                sizes: Vec::new(),
            },
            AnimationFormat::PngSequence => State::Png(Vec::new()),
        };
        Ok(Self { format, width, height, fps, frames: 0, bytes: 0, limit, state })
    }

    #[must_use]
    pub const fn format(&self) -> AnimationFormat {
        self.format
    }

    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn check_limit(
        format: AnimationFormat,
        limit: usize,
        frames: usize,
        bytes: usize,
    ) -> Result<(), RenderError> {
        if bytes > limit {
            return Err(RenderError::Invalid(format!(
                "the {} animation would exceed its byte limit of {limit} bytes after {frames} frames",
                format.extension(),
            )));
        }
        Ok(())
    }



    #[allow(clippy::cast_possible_truncation)]
    pub fn push(&mut self, img: &RgbImage) -> Result<(), RenderError> {
        if (img.width, img.height) != (self.width, self.height) {
            return Err(RenderError::Invalid("every animation frame must have the size of the first".into()));
        }
        let (w, h) = (self.width, self.height);
        let delay_ms = 1000 / self.fps;
        let (format, limit, frames) = (self.format, self.limit, self.frames);
        let check = |bytes: usize| Self::check_limit(format, limit, frames, bytes);
        match &mut self.state {
            State::Gif(enc) => {
                let mut frame = gif::Frame::from_rgb_speed(w as u16, h as u16, &img.data, 10);
                frame.delay = u16::try_from(delay_ms / 10).unwrap_or(10).max(2);
                enc.write_frame(&frame).map_err(bound)?;
                self.bytes = enc.get_ref().len();
                check(self.bytes)?;
            }
            State::Webp(chunks) => {
                let mut single = Vec::new();
                image_webp::WebPEncoder::new(&mut single)
                    .encode(&img.data, w as u32, h as u32, image_webp::ColorType::Rgb8)
                    .map_err(bound)?;
                let vp8l = riff_chunk(&single, *b"VP8L")
                    .ok_or_else(|| bound("the WebP encoder produced no VP8L chunk"))?;
                let mut payload = Vec::with_capacity(16 + vp8l.len() + 8);
                payload.extend_from_slice(&u24(0));
                payload.extend_from_slice(&u24(0));
                payload.extend_from_slice(&u24((w - 1) as u32));
                payload.extend_from_slice(&u24((h - 1) as u32));
                payload.extend_from_slice(&u24(delay_ms));
                payload.push(0b10);
                push_chunk(&mut payload, *b"VP8L", vp8l);
                let before = chunks.len();
                push_chunk(chunks, *b"ANMF", &payload);
                self.bytes += chunks.len() - before;
                check(self.bytes)?;
            }
            State::Mp4 { enc, sps, pps, samples, sizes } => {
                let (yuv, pw, ph) = to_ycbcr420(img);
                let (y, rest) = yuv.split_at(pw * ph);
                let (cb, cr) = rest.split_at(pw * ph / 4);
                let depth = less_avc::BitDepth::Depth8;
                let image = less_avc::ycbcr_image::YCbCrImage {
                    planes: less_avc::ycbcr_image::Planes::YCbCr((
                        less_avc::ycbcr_image::DataPlane { data: y, stride: pw, bit_depth: depth },
                        less_avc::ycbcr_image::DataPlane { data: cb, stride: pw / 2, bit_depth: depth },
                        less_avc::ycbcr_image::DataPlane { data: cr, stride: pw / 2, bit_depth: depth },
                    )),
                    width: w as u32,
                    height: h as u32,
                };
                let nal = if let Some(e) = enc.as_mut() {
                    e.encode(&image).map_err(bound)?
                } else {
                    let (initial, e) = less_avc::LessEncoder::new(&image).map_err(bound)?;
                    *sps = initial.sps.to_nal_unit();
                    *pps = initial.pps.to_nal_unit();
                    *enc = Some(e);
                    initial.frame
                };
                let unit = nal.to_nal_unit();
                check(self.bytes + unit.len() + 4)?;
                samples.extend_from_slice(&u32::try_from(unit.len()).map_err(bound)?.to_be_bytes());
                samples.extend_from_slice(&unit);
                sizes.push(u32::try_from(unit.len() + 4).map_err(bound)?);
                self.bytes = samples.len();
            }
            State::Png(entries) => {
                let png = img.to_png();
                check(self.bytes + png.len() + 128)?;
                self.bytes += png.len() + 128;
                entries.push((
                    format!("frame_{:05}.png", self.frames),
                    png,
                    implexity_mesh::zip::ZipMethod::Stored,
                ));
            }
        }
        self.frames += 1;
        Ok(())
    }



    #[allow(clippy::cast_possible_truncation)]
    pub fn finish(self) -> Result<Vec<u8>, RenderError> {
        if self.frames == 0 {
            return Err(RenderError::Invalid("an animation needs at least one frame".into()));
        }
        let out = match self.state {
            State::Gif(enc) => enc.into_inner().map_err(bound)?,
            State::Webp(chunks) => {
                let mut vp8x = vec![0b0000_0010, 0, 0, 0];
                vp8x.extend_from_slice(&u24((self.width - 1) as u32));
                vp8x.extend_from_slice(&u24((self.height - 1) as u32));
                let mut body = b"WEBP".to_vec();
                push_chunk(&mut body, *b"VP8X", &vp8x);
                push_chunk(&mut body, *b"ANIM", &[0, 0, 0, 255, 0, 0]);
                body.extend_from_slice(&chunks);
                let mut out = b"RIFF".to_vec();
                out.extend_from_slice(&u32::try_from(body.len()).map_err(bound)?.to_le_bytes());
                out.extend_from_slice(&body);
                out
            }
            State::Mp4 { sps, pps, samples, sizes, .. } => {
                mp4(&sps, &pps, &samples, &sizes, self.width, self.height, self.fps)?
            }
            State::Png(entries) => implexity_mesh::zip::write_zip(&entries)?,
        };
        if out.len() > self.limit {
            return Err(RenderError::Invalid(format!(
                "the animation exceeds its byte limit of {} bytes",
                self.limit
            )));
        }
        Ok(out)
    }
}

fn u24(v: u32) -> [u8; 3] {
    let b = v.to_le_bytes();
    [b[0], b[1], b[2]]
}

fn push_chunk(out: &mut Vec<u8>, tag: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&tag);
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}

fn riff_chunk(file: &[u8], tag: [u8; 4]) -> Option<&[u8]> {
    if file.len() < 12 || &file[0..4] != b"RIFF" || &file[8..12] != b"WEBP" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= file.len() {
        let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().ok()?) as usize;
        let data = file.get(at + 8..at + 8 + len)?;
        if file[at..at + 4] == tag {
            return Some(data);
        }
        at += 8 + len + len % 2;
    }
    None
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_ycbcr420(img: &RgbImage) -> (Vec<u8>, usize, usize) {
    let pw = img.width.div_ceil(16) * 16;
    let ph = img.height.div_ceil(16) * 16;
    let px = |x: usize, y: usize| -> [f64; 3] {
        let p = img.get(y.min(img.height - 1), x.min(img.width - 1));
        p.map(|c| f64::from(c) / 255.0)
    };
    let mut out = vec![0u8; pw * ph * 3 / 2];
    for y in 0..ph {
        for x in 0..pw {
            let [r, g, b] = px(x, y);
            out[y * pw + x] = (255.0 * (0.299 * r + 0.587 * g + 0.114 * b)).round().clamp(0.0, 255.0) as u8;
        }
    }
    let (cw, ch) = (pw / 2, ph / 2);
    for y in 0..ch {
        for x in 0..cw {
            let mut acc = [0.0; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = px(2 * x + dx, 2 * y + dy);
                for k in 0..3 {
                    acc[k] += 0.25 * p[k];
                }
            }
            let [r, g, b] = acc;
            out[pw * ph + y * cw + x] =
                (128.0 + 255.0 * (-0.168_736 * r - 0.331_264 * g + 0.5 * b)).round().clamp(0.0, 255.0) as u8;
            out[pw * ph + cw * ch + y * cw + x] =
                (128.0 + 255.0 * (0.5 * r - 0.418_688 * g - 0.081_312 * b)).round().clamp(0.0, 255.0) as u8;
        }
    }
    (out, pw, ph)
}

fn boxed(tag: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&u32::try_from(body.len() + 8).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&tag);
    out.extend_from_slice(body);
    out
}

fn full(tag: [u8; 4], version_flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = version_flags.to_be_bytes().to_vec();
    b.extend_from_slice(body);
    boxed(tag, &b)
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
fn moov(
    sps: &[u8],
    pps: &[u8],
    sizes: &[u32],
    width: usize,
    height: usize,
    fps: u32,
    chunk_offset: u32,
) -> Vec<u8> {
    let n = sizes.len() as u32;
    let timescale = fps * 1000;
    let duration_ms = n * 1000 / fps;
    let be32 = |v: u32| v.to_be_bytes();

    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&be32(0));
    mvhd.extend_from_slice(&be32(0));
    mvhd.extend_from_slice(&be32(1000));
    mvhd.extend_from_slice(&be32(duration_ms));
    mvhd.extend_from_slice(&be32(0x0001_0000));
    mvhd.extend_from_slice(&[0x01, 0x00, 0, 0]);
    mvhd.extend_from_slice(&[0; 8]);
    for m in MATRIX {
        mvhd.extend_from_slice(&be32(m));
    }
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&be32(2));
    let mvhd = full(*b"mvhd", 0, &mvhd);

    let mut tkhd = Vec::new();
    tkhd.extend_from_slice(&be32(0));
    tkhd.extend_from_slice(&be32(0));
    tkhd.extend_from_slice(&be32(1));
    tkhd.extend_from_slice(&be32(0));
    tkhd.extend_from_slice(&be32(duration_ms));
    tkhd.extend_from_slice(&[0; 8]);
    tkhd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    for m in MATRIX {
        tkhd.extend_from_slice(&be32(m));
    }
    tkhd.extend_from_slice(&be32((width as u32) << 16));
    tkhd.extend_from_slice(&be32((height as u32) << 16));
    let tkhd = full(*b"tkhd", 3, &tkhd);

    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&be32(0));
    mdhd.extend_from_slice(&be32(0));
    mdhd.extend_from_slice(&be32(timescale));
    mdhd.extend_from_slice(&be32(n * 1000));
    mdhd.extend_from_slice(&[0x55, 0xC4, 0, 0]);
    let mdhd = full(*b"mdhd", 0, &mdhd);
    let mut hdlr = vec![0, 0, 0, 0];
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.extend_from_slice(b"Implexity dynamic results\0");
    let hdlr = full(*b"hdlr", 0, &hdlr);

    let mut avcc = vec![
        1,
        sps.get(1).copied().unwrap_or(66),
        sps.get(2).copied().unwrap_or(0),
        sps.get(3).copied().unwrap_or(30),
        0xFF,
        0xE1,
    ];
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1);
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(pps);
    let avcc = boxed(*b"avcC", &avcc);
    let mut avc1 = vec![0; 6];
    avc1.extend_from_slice(&1u16.to_be_bytes());
    avc1.extend_from_slice(&[0; 16]);
    avc1.extend_from_slice(&(width as u16).to_be_bytes());
    avc1.extend_from_slice(&(height as u16).to_be_bytes());
    avc1.extend_from_slice(&be32(0x0048_0000));
    avc1.extend_from_slice(&be32(0x0048_0000));
    avc1.extend_from_slice(&be32(0));
    avc1.extend_from_slice(&1u16.to_be_bytes());
    let mut name = [0u8; 32];
    let label = b"less-avc PCM";
    name[0] = label.len() as u8;
    name[1..=label.len()].copy_from_slice(label);
    avc1.extend_from_slice(&name);
    avc1.extend_from_slice(&0x0018u16.to_be_bytes());
    avc1.extend_from_slice(&0xFFFFu16.to_be_bytes());
    avc1.extend_from_slice(&avcc);
    let avc1 = boxed(*b"avc1", &avc1);
    let mut stsd = be32(1).to_vec();
    stsd.extend_from_slice(&avc1);
    let stsd = full(*b"stsd", 0, &stsd);
    let mut stts = be32(1).to_vec();
    stts.extend_from_slice(&be32(n));
    stts.extend_from_slice(&be32(1000));
    let stts = full(*b"stts", 0, &stts);
    let mut stsc = be32(1).to_vec();
    stsc.extend_from_slice(&be32(1));
    stsc.extend_from_slice(&be32(n));
    stsc.extend_from_slice(&be32(1));
    let stsc = full(*b"stsc", 0, &stsc);
    let mut stsz = be32(0).to_vec();
    stsz.extend_from_slice(&be32(n));
    for s in sizes {
        stsz.extend_from_slice(&be32(*s));
    }
    let stsz = full(*b"stsz", 0, &stsz);
    let mut stco = be32(1).to_vec();
    stco.extend_from_slice(&be32(chunk_offset));
    let stco = full(*b"stco", 0, &stco);
    let stbl = boxed(*b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
    let vmhd = full(*b"vmhd", 1, &[0; 8]);
    let dref = full(*b"dref", 0, &[be32(1).to_vec(), full(*b"url ", 1, &[])].concat());
    let dinf = boxed(*b"dinf", &dref);
    let minf = boxed(*b"minf", &[vmhd, dinf, stbl].concat());
    let mdia = boxed(*b"mdia", &[mdhd, hdlr, minf].concat());
    let trak = boxed(*b"trak", &[tkhd, mdia].concat());
    boxed(*b"moov", &[mvhd, trak].concat())
}

fn mp4(
    sps: &[u8],
    pps: &[u8],
    samples: &[u8],
    sizes: &[u32],
    width: usize,
    height: usize,
    fps: u32,
) -> Result<Vec<u8>, RenderError> {
    let mut ftyp = b"isom".to_vec();
    ftyp.extend_from_slice(&0x200u32.to_be_bytes());
    for b in [b"isom", b"iso2", b"avc1", b"mp41"] {
        ftyp.extend_from_slice(b);
    }
    let ftyp = boxed(*b"ftyp", &ftyp);

    let probe = moov(sps, pps, sizes, width, height, fps, 0);
    let offset = ftyp.len() + probe.len() + 8;
    let offset = u32::try_from(offset).map_err(bound)?;
    let moov = moov(sps, pps, sizes, width, height, fps, offset);
    let mut out = Vec::with_capacity(offset as usize + samples.len());
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&u32::try_from(samples.len() + 8).map_err(bound)?.to_be_bytes());
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(samples);
    Ok(out)
}

#[must_use]
pub fn png(img: &RgbImage) -> Vec<u8> {
    img.to_png()
}



pub fn write_all(w: &mut dyn Write, bytes: &[u8]) -> Result<(), RenderError> {
    w.write_all(bytes).map_err(bound)
}

