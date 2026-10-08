// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
}

impl Channels {
    #[must_use]
    pub fn count(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::GrayAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    fn color(self) -> png::ColorType {
        match self {
            Self::Gray => png::ColorType::Grayscale,
            Self::GrayAlpha => png::ColorType::GrayscaleAlpha,
            Self::Rgb => png::ColorType::Rgb,
            Self::Rgba => png::ColorType::Rgba,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub channels: Channels,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PngError(pub String);


pub fn encode(image: &Image) -> Result<Vec<u8>, PngError> {
    let expected = (image.width as usize) * (image.height as usize) * image.channels.count();
    if image.pixels.len() != expected {
        return Err(PngError(format!(
            "{} samples for a {}x{}x{} image",
            image.pixels.len(),
            image.width,
            image.height,
            image.channels.count()
        )));
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, image.width, image.height);
        enc.set_color(image.channels.color());
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| PngError(e.to_string()))?;
        writer.write_image_data(&image.pixels).map_err(|e| PngError(e.to_string()))?;
        writer.finish().map_err(|e| PngError(e.to_string()))?;
    }
    Ok(out)
}


pub fn encode_rgb(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, PngError> {
    encode(&Image { width, height, channels: Channels::Rgb, pixels: pixels.to_vec() })
}


pub fn decode(bytes: &[u8]) -> Result<Image, PngError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| PngError(e.to_string()))?;
    let size = reader.output_buffer_size().ok_or_else(|| PngError("image too large".into()))?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| PngError(e.to_string()))?;
    buf.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Grayscale => Channels::Gray,
        png::ColorType::GrayscaleAlpha => Channels::GrayAlpha,
        png::ColorType::Rgb | png::ColorType::Indexed => Channels::Rgb,
        png::ColorType::Rgba => Channels::Rgba,
    };
    Ok(Image { width: info.width, height: info.height, channels, pixels: buf })
}

#[must_use]
pub fn pixel_sha256(image: &Image) -> String {
    let mut h = Sha256::new();
    h.update(format!("{}x{}x{}\n", image.width, image.height, image.channels.count()).as_bytes());
    h.update(&image.pixels);
    hex::encode(h.finalize())
}

