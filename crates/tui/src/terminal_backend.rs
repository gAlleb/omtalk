//! Keep terminal width disagreements local to a grapheme instead of shifting a row.
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Cell, CellDiffOption, CellWidth};
use ratatui::layout::{Position, Size};
use std::io::{self, Write};

pub struct StableBackend<W: Write> {
    inner: CrosstermBackend<W>,
}

impl<W: Write> StableBackend<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
        }
    }
}

impl<W: Write> Backend for StableBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut run = Vec::new();
        for (x, y, cell) in content {
            // Image protocols declare their own width and may carry escape sequences.
            // Keep those cells in the original backend's output path.
            let guard = !matches!(cell.diff_option, CellDiffOption::ForcedWidth(_))
                && (cell.cell_width() > 1 || cell.symbol().chars().count() > 1)
                && !cell.symbol().contains('\x1b');
            if !guard {
                run.push((x, y, cell));
                continue;
            }
            if !run.is_empty() {
                self.inner.draw(run.drain(..))?;
            }
            // Diffing skips the covered columns of a wide cell. Explicitly paint
            // them before its glyph, so a narrower terminal rendering leaves blanks,
            // not characters from the previous frame. Separate draw calls also force
            // an absolute cursor position before the glyph and the following run.
            let mut blank = cell.clone();
            blank.set_symbol(" ");
            self.inner
                .draw((0..cell.cell_width()).map(|offset| (x.saturating_add(offset), y, &blank)))?;
            self.inner.draw(std::iter::once((x, y, cell)))?;
        }
        if !run.is_empty() {
            self.inner.draw(run.into_iter())?;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Model a terminal that draws the heart sequence in one column even though
    // the application's buffer reserves two. Only printable text and CSI from
    // the real backend output are interpreted; SGR does not move the cursor.
    fn narrow_terminal(bytes: &[u8]) -> Vec<char> {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let mut chars = text.chars().peekable();
        let mut cells = vec!['A', 'B', '│'];
        let mut x = 0;
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                assert_eq!(chars.next(), Some('['));
                let mut params = String::new();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        if next == 'H' {
                            x = params.rsplit(';').next().unwrap().parse::<usize>().unwrap() - 1;
                        }
                        break;
                    }
                    params.push(next);
                }
            } else if c != '\u{fe0f}' {
                if let Some(cell) = cells.get_mut(x) {
                    *cell = c;
                }
                x += 1;
            }
        }
        cells
    }

    #[test]
    fn width_disagreement_cannot_leave_old_text_under_an_emoji() {
        let glyph = Cell::new("❤️");
        let border = Cell::new("│");
        let cells = [(0, 0, &glyph), (2, 0, &border)];
        let mut baseline = Vec::new();
        CrosstermBackend::new(&mut baseline)
            .draw(cells.into_iter())
            .unwrap();
        assert_eq!(
            narrow_terminal(&baseline),
            ['❤', 'B', '│'],
            "reproduce the stale covered column"
        );
        let mut fixed = Vec::new();
        StableBackend::new(&mut fixed).draw(cells.into_iter()).unwrap();
        assert_eq!(narrow_terminal(&fixed), ['❤', ' ', '│']);
    }

    #[test]
    fn image_protocol_cells_keep_the_original_output_path() {
        let mut image = Cell::new("\x1b_Gi=1;data\x1b\\\u{10eeee}");
        image.set_diff_option(CellDiffOption::ForcedWidth(std::num::NonZeroU16::new(1).unwrap()));
        let text = Cell::new("x");
        let cells = [(0, 0, &image), (1, 0, &text)];
        let mut baseline = Vec::new();
        CrosstermBackend::new(&mut baseline)
            .draw(cells.into_iter())
            .unwrap();
        let mut fixed = Vec::new();
        StableBackend::new(&mut fixed).draw(cells.into_iter()).unwrap();
        assert_eq!(fixed, baseline);
    }
}
