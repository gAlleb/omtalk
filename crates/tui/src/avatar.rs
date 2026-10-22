//! Avatars drawn round. Kitty takes the transparency of a round picture as it is; the
//! block characters of other terminals know no transparency, so there the picture is
//! drawn here, a half cell at a time, and what is outside the circle is left alone.

use image::{DynamicImage, Rgba, RgbaImage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// The middle square of `image`: avatars are square, but not every one sent is.
pub fn square(image: &DynamicImage) -> DynamicImage {
    let side = image.width().min(image.height());
    let x = (image.width() - side) / 2;
    let y = (image.height() - side) / 2;
    image.crop_imm(x, y, side, side)
}

/// `image` with what lies outside the inscribed circle (of an ellipse, if it is not
/// square) made transparent; the edge softened by a pixel.
pub fn round(image: &DynamicImage) -> RgbaImage {
    let mut out = image.to_rgba8();
    let (w, h) = (out.width() as f32, out.height() as f32);
    let (cx, cy) = (w / 2.0, h / 2.0);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        // Distance from the middle in pixels of the shorter side, at the pixel's centre.
        let dx = (x as f32 + 0.5 - cx) / cx;
        let dy = (y as f32 + 0.5 - cy) / cy;
        let from_edge = (1.0 - (dx * dx + dy * dy).sqrt()) * cx.min(cy);
        let cover = from_edge.clamp(0.0, 1.0);
        pixel.0[3] = (pixel.0[3] as f32 * cover) as u8;
    }
    out
}

/// Draws `image` round into `area` with half blocks: two pixels a cell, one above the
/// other. Half cells outside the circle keep the terminal's background.
pub fn draw_blocks(image: &DynamicImage, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let small = image.resize_exact(
        area.width as u32,
        area.height as u32 * 2,
        image::imageops::FilterType::Triangle,
    );
    let small = round(&small);
    let colour = |p: &Rgba<u8>| (p.0[3] >= 128).then(|| Color::Rgb(p.0[0], p.0[1], p.0[2]));
    for row in 0..area.height {
        for column in 0..area.width {
            let top = colour(small.get_pixel(column as u32, row as u32 * 2));
            let bottom = colour(small.get_pixel(column as u32, row as u32 * 2 + 1));
            let Some(cell) = buffer.cell_mut((area.x + column, area.y + row)) else {
                continue;
            };
            match (top, bottom) {
                (Some(top), Some(bottom)) => drop(cell.set_char('▀').set_fg(top).set_bg(bottom)),
                (Some(top), None) => drop(cell.set_char('▀').set_fg(top).set_bg(Color::Reset)),
                (None, Some(bottom)) => drop(cell.set_char('▄').set_fg(bottom).set_bg(Color::Reset)),
                (None, None) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_cut_and_the_middle_kept() {
        let red = DynamicImage::ImageRgba8(RgbaImage::from_pixel(20, 10, Rgba([255, 0, 0, 255])));
        let squared = square(&red);
        assert_eq!((squared.width(), squared.height()), (10, 10));
        let round = round(&squared);
        assert_eq!(round.get_pixel(0, 0).0[3], 0, "a corner");
        assert_eq!(round.get_pixel(5, 5).0[3], 255, "the middle");

        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 3));
        draw_blocks(&squared, Rect::new(0, 0, 6, 3), &mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), " ", "the corner is left alone");
        assert_eq!(buffer[(3, 1)].symbol(), "▀");
        assert_eq!(buffer[(3, 1)].fg, Color::Rgb(255, 0, 0));
        assert_eq!(buffer[(3, 1)].bg, Color::Rgb(255, 0, 0));
    }
}
