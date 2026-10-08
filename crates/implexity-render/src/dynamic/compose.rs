// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

