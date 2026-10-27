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

/// The largest an avatar is published (one's own, a room's): what clients show, and
/// well under a server's stanza size limit (Prosody's is 256 KiB) once in base64.
const OWN_SIDE: u32 = 256;
const OWN_BYTES: usize = 96 * 1024;

/// The picture at `path` as an avatar (one's own, a room's): its middle square, at most [`OWN_SIDE`] a
/// side, as a PNG; smaller still while the PNG is too large. The PNG and its side.
pub fn own_avatar(path: &std::path::Path) -> Result<(Vec<u8>, u32), String> {
    let image = square(&image::open(path).map_err(|e| e.to_string())?);
    let mut side = image.width().min(OWN_SIDE);
    loop {
        let small = image.resize_exact(side, side, image::imageops::FilterType::Lanczos3);
        let mut png = std::io::Cursor::new(Vec::new());
        small
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        let png = png.into_inner();
        if png.len() <= OWN_BYTES || side <= 64 {
            return Ok((png, side));
        }
        side = side * 3 / 4;
    }
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

/// The icon of a desktop notification, made once into `dir` and kept: the person's
/// avatar round; from a room, the room's round with the person's small in its corner,
/// as messengers show it. Either alone when only one is known. Named by both hashes.
pub fn notice_icon(
    avatars: &std::path::Path,
    dir: &std::path::Path,
    person: Option<&str>,
    room: Option<&str>,
) -> Option<std::path::PathBuf> {
    const SIZE: u32 = 96;
    let name = match (room, person) {
        (None, None) => return None,
        (Some(room), Some(person)) => format!("{room}-{person}.png"),
        (Some(one), None) | (None, Some(one)) => format!("{one}.png"),
    };
    let path = dir.join(name);
    if path.is_file() {
        return Some(path);
    }
    let open = |hash: &str| -> Option<DynamicImage> {
        let image = image::ImageReader::open(avatars.join(hash))
            .ok()?
            .with_guessed_format()
            .ok()?;
        let image = image.decode().ok()?;
        Some(square(&image))
    };
    let round_at = |image: &DynamicImage, side: u32| {
        round(&image.resize_exact(side, side, image::imageops::FilterType::Lanczos3))
    };
    let icon = match (room.and_then(open), person.and_then(open)) {
        (Some(room), Some(person)) => {
            let mut icon = round_at(&room, SIZE);
            let small = round_at(&person, SIZE / 2);
            image::imageops::overlay(&mut icon, &small, (SIZE / 2) as i64, (SIZE / 2) as i64);
            icon
        }
        (Some(one), None) | (None, Some(one)) => round_at(&one, SIZE),
        (None, None) => return None,
    };
    std::fs::create_dir_all(dir).ok()?;
    icon.save(&path).ok()?;
    Some(path)
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
    fn ones_avatar_is_the_middle_square_small_enough_for_a_server() {
        let dir = tempfile::tempdir().unwrap();
        // A wide photo: its middle, 256 a side.
        let wide = dir.path().join("wide.png");
        RgbaImage::from_fn(800, 400, |x, _| Rgba([(x % 256) as u8, 80, 160, 255]))
            .save(&wide)
            .unwrap();
        let (png, side) = own_avatar(&wide).unwrap();
        let got = image::load_from_memory(&png).unwrap();
        assert_eq!((side, got.width(), got.height()), (256, 256, 256));
        // Noise does not compress: made smaller until it fits.
        let noisy = dir.path().join("noisy.png");
        let mut seed = 7u32;
        RgbaImage::from_fn(600, 600, |_, _| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            Rgba(seed.to_le_bytes())
        })
        .save(&noisy)
        .unwrap();
        let (png, side) = own_avatar(&noisy).unwrap();
        assert!(
            png.len() <= OWN_BYTES && side < 256,
            "{} bytes at {side}",
            png.len()
        );
        // A small one is not blown up.
        let small = dir.path().join("small.png");
        RgbaImage::from_pixel(48, 48, Rgba([1, 2, 3, 255]))
            .save(&small)
            .unwrap();
        assert_eq!(own_avatar(&small).unwrap().1, 48);
        assert!(own_avatar(&dir.path().join("none.png")).is_err());
    }

    #[test]
    fn a_room_notice_shows_the_room_with_the_writer_in_its_corner() {
        let dir = tempfile::tempdir().unwrap();
        let save = |name: &str, colour: [u8; 4]| {
            RgbaImage::from_pixel(40, 30, Rgba(colour))
                .save_with_format(dir.path().join(name), image::ImageFormat::Png)
                .unwrap()
        };
        save("room", [0, 0, 255, 255]);
        save("bob", [255, 0, 0, 255]);
        let out = dir.path().join("notify");
        let icon = notice_icon(dir.path(), &out, Some("bob"), Some("room")).expect("made");
        assert_eq!(icon, out.join("room-bob.png"));
        let icon = image::open(&icon).unwrap().to_rgba8();
        assert_eq!(icon.dimensions(), (96, 96));
        assert_eq!(icon.get_pixel(30, 30).0, [0, 0, 255, 255], "the room");
        assert_eq!(
            icon.get_pixel(72, 72).0,
            [255, 0, 0, 255],
            "the writer, in the corner"
        );
        assert_eq!(icon.get_pixel(1, 1).0[3], 0, "round");

        assert_eq!(
            notice_icon(dir.path(), &out, Some("bob"), None),
            Some(out.join("bob.png")),
            "a direct chat: the person alone"
        );
        assert_eq!(notice_icon(dir.path(), &out, None, None), None);
        assert_eq!(
            notice_icon(dir.path(), &out, Some("nobody"), None),
            None,
            "not fetched"
        );

        let command = crate::notify::desktop("bob@home.org", "bob", None, "hi", Some(&out.join("bob.png")));
        assert!(
            command
                .iter()
                .any(|a| a.starts_with("--icon=") && a.ends_with("bob.png")),
            "{command:?}"
        );
    }

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
