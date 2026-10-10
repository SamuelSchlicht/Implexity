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


#![allow(clippy::cast_possible_wrap)]

use implexity_mesh::raster::RgbImage;

use super::draw::{Canvas, text_width};
use super::plane::{MAX_SIDE, Theme};
use crate::RenderError;



pub fn sheet(tiles: &[(RgbImage, String)], columns: usize, theme: Theme) -> Result<RgbImage, RenderError> {
    if tiles.is_empty() || columns == 0 {
        return Err(RenderError::Invalid("a sheet needs at least one tile and one column".into()));
    }
    let cw = tiles.iter().map(|t| t.0.width).max().unwrap_or(1);
    let ch = tiles.iter().map(|t| t.0.height).max().unwrap_or(1);
    let cols = columns.min(tiles.len());
    let rows = tiles.len().div_ceil(cols);
    let (gap, cap) = (6, 14);
    let width = cols * cw + (cols + 1) * gap;
    let height = rows * (ch + cap) + (rows + 1) * gap;
    if width > 2 * MAX_SIDE || height > 2 * MAX_SIDE {
        return Err(RenderError::Invalid(format!(
            "the sheet would be {width}x{height} pixels (at most {} per side)",
            2 * MAX_SIDE
        )));
    }
    let mut canvas = Canvas::new(width, height, theme.background());
    for (k, (img, caption)) in tiles.iter().enumerate() {
        let (r, c) = (k / cols, k % cols);
        let x = i64::try_from(gap + c * (cw + gap)).unwrap_or(0);
        let y = i64::try_from(gap + r * (ch + cap + gap)).unwrap_or(0);
        let text: String = caption.chars().take(cw / 6).collect();
        let tw = i64::try_from(text_width(&text, 1)).unwrap_or(0);
        let cwi = i64::try_from(cw).unwrap_or(0);
        canvas.text(x + (cwi - tw) / 2, y + 2, &text, theme.text(), 1);
        canvas.blit(
            img,
            x + (cwi - i64::try_from(img.width).unwrap_or(0)) / 2,
            y + i64::try_from(cap).unwrap_or(0),
        );
    }
    Ok(canvas.img)
}

