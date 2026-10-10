// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

