//! Keep terminal width disagreements local to a grapheme instead of shifting a row.
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Cell, CellDiffOption, CellWidth};
use ratatui::layout::{Position, Size};
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};
use std::io::{self, Write};
use std::process::Command;

/// Identify image support without sending a query through the keyboard input stream.
/// In particular, a late Kitty APC or cell-size CSI after the probe's status reply
/// would be decoded as keystrokes by crossterm.
pub fn image_picker() -> Picker {
    let term = std::env::var("TERM").unwrap_or_default();
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let tmux = program == "tmux" || term.starts_with("tmux") || std::env::var_os("TMUX").is_some();
    let mut font_size = crossterm::terminal::window_size().ok().and_then(|size| {
        measured_cell(
            size.width.checked_div(size.columns)?,
            size.height.checked_div(size.rows)?,
        )
    });

    let protocol = if tmux {
        // A long-running tmux server may have inherited the environment of a
        // different terminal. Ask tmux about the client displaying this pane.
        let mut command = Command::new("tmux");
        command.args(["display-message", "-p"]);
        if let Some(pane) = std::env::var_os("TMUX_PANE") {
            command.arg("-t").arg(pane);
        }
        let format = "#{client_termname}\t#{client_termtype}\t#{client_termfeatures}\t#{window_cell_width}\t#{window_cell_height}";
        command
            .arg(format)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|output| {
                let text = std::str::from_utf8(&output.stdout).ok()?;
                let mut fields = text.trim_end_matches(['\r', '\n']).split('\t');
                let protocol = image_protocol(fields.next()?, fields.next()?, fields.next()?);
                if let (Some(w), Some(h)) = (fields.next(), fields.next()) {
                    font_size = w
                        .parse()
                        .ok()
                        .zip(h.parse().ok())
                        .and_then(|(w, h)| measured_cell(w, h))
                        .or(font_size);
                }
                protocol
            })
    } else {
        image_protocol(&term, &program, "")
    };
    // This constructor retains the library's iTerm environment hints and tmux
    // passthrough setup while allowing the cell size to come from TIOCGWINSZ.
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font_size.unwrap_or(FontSize::new(10, 20)));
    picker.set_protocol_type(usable_protocol(
        protocol.unwrap_or(picker.protocol_type()),
        tmux,
        picker.tmux_detected(),
    ));
    picker
}

fn usable_protocol(protocol: ProtocolType, in_tmux: bool, picker_wraps_tmux: bool) -> ProtocolType {
    // With TERM=screen-256color, TMUX alone identifies the multiplexer to us.
    // The picker won't wrap graphics escapes for passthrough in that case.
    if in_tmux && !picker_wraps_tmux {
        ProtocolType::Halfblocks
    } else {
        protocol
    }
}

fn measured_cell(width: u16, height: u16) -> Option<FontSize> {
    (width > 0 && height > 0).then_some(FontSize::new(width, height))
}

fn image_protocol(term: &str, program: &str, features: &str) -> Option<ProtocolType> {
    let program = program.to_ascii_lowercase();
    if matches!(term, "xterm-kitty" | "xterm-ghostty")
        || program.starts_with("kitty")
        || program.starts_with("ghostty")
    {
        Some(ProtocolType::Kitty)
    } else if program.starts_with("iterm") || program.starts_with("wezterm") || term == "wezterm" {
        Some(ProtocolType::Iterm2)
    } else if term.starts_with("alacritty") || program.starts_with("alacritty") {
        Some(ProtocolType::Halfblocks)
    } else if term.starts_with("foot")
        || term == "mlterm"
        || term.contains("sixel")
        || features.split(',').any(|f| f == "sixel")
    {
        Some(ProtocolType::Sixel)
    } else {
        None
    }
}

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
        let mut covered = None;
        for (x, y, cell) in content {
            // Ratatui may emit trailing clears after a VS16 grapheme when it
            // replaces text. We clear before drawing it below; writing into its
            // second column afterwards would erase the whole glyph in tmux.
            if covered.is_some_and(|(end, row)| y == row && x < end) {
                continue;
            }
            covered = None;
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
            covered = Some((x.saturating_add(cell.cell_width()), y));
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

    #[test]
    fn graphics_are_selected_from_the_terminal_or_tmux_client_without_a_keyboard_probe() {
        use ProtocolType::*;
        for (term, program, features, expected) in [
            ("xterm-kitty", "", "", Some(Kitty)),
            ("xterm-256color", "ghostty 1.2", "", Some(Kitty)),
            ("alacritty", "", "", Some(Halfblocks)),
            ("xterm-256color", "WezTerm", "", Some(Iterm2)),
            ("xterm-256color", "", "RGB,sixel,usstyle", Some(Sixel)),
            ("foot-direct", "", "", Some(Sixel)),
            ("dumb", "", "", None),
        ] {
            assert_eq!(image_protocol(term, program, features), expected);
        }
    }

    #[test]
    fn graphics_inside_tmux_require_wrapped_output() {
        for graphics in [ProtocolType::Kitty, ProtocolType::Iterm2, ProtocolType::Sixel] {
            assert_eq!(usable_protocol(graphics, true, false), ProtocolType::Halfblocks);
            assert_eq!(usable_protocol(graphics, true, true), graphics);
            assert_eq!(usable_protocol(graphics, false, false), graphics);
        }
    }

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
    fn a_vs16_emoji_replacing_text_is_not_erased_by_a_trailing_diff_clear() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::Style;
        let area = Rect::new(0, 0, 3, 1);
        let mut previous = Buffer::empty(area);
        previous.set_string(0, 0, "AB│", Style::default());
        let mut next = Buffer::empty(area);
        next.set_string(0, 0, "❤️│", Style::default());
        let diff = previous.diff(&next);
        assert!(
            diff.iter().any(|(x, _, _)| *x == 1),
            "reproduce Ratatui's trailing clear"
        );
        let mut output = Vec::new();
        StableBackend::new(&mut output).draw(diff.into_iter()).unwrap();
        // In a wide terminal, a write to the covered column erases the whole
        // grapheme. There must be no such write after the heart was drawn.
        let text = String::from_utf8(output).unwrap();
        let after_heart = text.split_once("❤️").unwrap().1;
        assert!(!after_heart.contains("\x1b[1;2H"), "{text:?}");
        assert!(
            text[..text.find("❤️").unwrap()].contains("  "),
            "old text must be cleared before the glyph instead"
        );
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
