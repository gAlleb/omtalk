//! Message styling (XEP-0393), as Conversations, Gajim, Monal and Dino show it:
//! `*strong*`, `_emphasis_`, `~strike~`, `` `code` `` within a line, and blocks of
//! preformatted text (between ```` ``` ```` lines) and quotes (`>` lines). The
//! directives stay in the text, dimmed: copied, it is what was written.

/// How one character is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mark {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    /// A styling directive itself (`*`, `_`, …): dimmed.
    pub directive: bool,
}

/// One line of a message, as a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block<'a> {
    /// Text whose spans are styled.
    Text(&'a str),
    /// A quote line: how deep, and its text after the `>`s.
    Quote(usize, &'a str),
    /// A line inside a preformatted block: shown as it is.
    Pre(&'a str),
    /// The ```` ``` ```` line that opens or closes a preformatted block.
    Fence(&'a str),
}

/// The lines of a message as blocks. A preformatted block runs from a line starting
/// with ```` ``` ```` to a line that is exactly ```` ``` ````, or to the end.
pub fn blocks(text: &str) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut pre = false;
    for line in text.split('\n') {
        if pre {
            if line.trim_end() == "```" {
                pre = false;
                out.push(Block::Fence(line));
            } else {
                out.push(Block::Pre(line));
            }
        } else if line.starts_with("```") {
            pre = true;
            out.push(Block::Fence(line));
        } else if line.starts_with('>') {
            let mut depth = 0;
            let mut rest = line;
            while let Some(after) = rest.strip_prefix('>') {
                depth += 1;
                rest = after.strip_prefix(' ').unwrap_or(after);
            }
            out.push(Block::Quote(depth, rest));
        } else {
            out.push(Block::Text(line));
        }
    }
    out
}

/// How each character of `line` is shown, one mark a character. Spans do not cross
/// lines: an opening directive at the start or after whitespace (or another opening
/// directive), followed by a non-space; the closing one after a non-space; never
/// empty. Nothing is styled inside `code`.
pub fn marks(line: &str) -> Vec<Mark> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = vec![Mark::default(); chars.len()];
    let closers = Closers::of(&chars);
    span(&chars, &closers, 0, chars.len(), Mark::default(), &mut out);
    out
}

/// Where each directive could close (after a non-space), and where lines break: found
/// once, so that looking for a span's end is a search, not a walk to the end of the
/// line for every opening (a line of "*a *a *a …" took seconds).
struct Closers {
    by_directive: [Vec<usize>; 4],
    breaks: Vec<usize>,
}

const DIRECTIVES: [char; 4] = ['*', '_', '~', '`'];

impl Closers {
    fn of(chars: &[char]) -> Self {
        let mut by_directive: [Vec<usize>; 4] = Default::default();
        let mut breaks = Vec::new();
        for (j, c) in chars.iter().enumerate() {
            if *c == '\n' {
                breaks.push(j);
            }
            if let Some(d) = DIRECTIVES.iter().position(|d| d == c)
                && j > 0
                && !chars[j - 1].is_whitespace()
            {
                by_directive[d].push(j);
            }
        }
        Self { by_directive, breaks }
    }

    /// The first place from `from` on, before `end` and before the line breaks, where
    /// directive `c` closes.
    fn close(&self, c: char, from: usize, end: usize) -> Option<usize> {
        let d = DIRECTIVES.iter().position(|d| *d == c)?;
        let line_end = self.breaks[self.breaks.partition_point(|&b| b < from)..]
            .first()
            .copied()
            .unwrap_or(usize::MAX);
        let places = &self.by_directive[d];
        let at = places[places.partition_point(|&j| j < from)..].first().copied()?;
        (at < end && at < line_end).then_some(at)
    }
}

fn span(chars: &[char], closers: &Closers, start: usize, end: usize, mark: Mark, out: &mut [Mark]) {
    let mut i = start;
    while i < end {
        let c = chars[i];
        // At the start of a span's text (right after its opening directive) another may
        // open too, as in `*_both_*`.
        let may_open = i == start || chars[i - 1].is_whitespace();
        if matches!(c, '*' | '_' | '~' | '`')
            && may_open
            && i + 1 < end
            && !chars[i + 1].is_whitespace()
            && let Some(close) = closers.close(c, i + 2, end)
        {
            let directive = Mark {
                directive: true,
                ..mark
            };
            out[i] = directive;
            out[close] = directive;
            let inner = match c {
                '*' => Mark { strong: true, ..mark },
                '_' => Mark {
                    emphasis: true,
                    ..mark
                },
                '~' => Mark { strike: true, ..mark },
                _ => Mark { code: true, ..mark },
            };
            if c == '`' {
                out[i + 1..close].iter_mut().for_each(|m| *m = inner);
            } else {
                span(chars, closers, i + 1, close, inner, out);
            }
            i = close + 1;
            continue;
        }
        out[i] = mark;
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line with each styled stretch in brackets: [s:…] strong, [e:…] emphasis,
    /// [x:…] strike, [c:…] code; directives as `|`.
    fn show(line: &str) -> String {
        let mut out = String::new();
        for (c, m) in line.chars().zip(marks(line)) {
            if m.directive {
                out.push('|');
                continue;
            }
            let tag = match (m.code, m.strong, m.emphasis, m.strike) {
                (true, ..) => "c",
                (_, true, true, _) => "se",
                (_, true, ..) => "s",
                (_, _, true, _) => "e",
                (_, _, _, true) => "x",
                _ => "",
            };
            if tag.is_empty() {
                out.push(c);
            } else {
                out.push_str(&format!("[{tag}:{c}]"));
            }
        }
        out
    }

    #[test]
    fn spans_follow_the_rules_of_the_specification() {
        assert_eq!(show("*жир*"), "|[s:ж][s:и][s:р]|");
        assert_eq!(show("_a_ ~b~ `c`"), "|[e:a]| |[x:b]| |[c:c]|");
        // Examples from XEP-0393.
        assert_eq!(show("*not strong *"), "*not strong *", "closing after a space");
        assert_eq!(show("* not strong*"), "* not strong*", "opening before a space");
        assert_eq!(show("**"), "**", "never empty");
        assert_eq!(show("a*b*"), "a*b*", "opening only at the start or after a space");
        assert_eq!(show("`*x*`"), "|[c:*][c:x][c:*]|", "nothing styled inside code");
        assert_eq!(show("*_y_*"), "||[se:y]||", "spans nest");
        assert!(
            !show("*a\nb*").contains('|'),
            "not across lines (one line at a time)"
        );
        assert_eq!(show("2*3*4 = 24"), "2*3*4 = 24");
        assert_eq!(show("snake_case_name"), "snake_case_name");
    }

    #[test]
    fn a_long_line_of_openings_is_read_at_once() {
        let line = "*a ".repeat(50_000);
        let started = std::time::Instant::now();
        let marks = marks(&line);
        assert_eq!(marks.len(), line.chars().count());
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn blocks_are_quotes_and_preformatted_text() {
        let text = "привет\n> цитата\n>> глубже\n```rust\nlet *a* = 1;\n```\nконец";
        assert_eq!(
            blocks(text),
            [
                Block::Text("привет"),
                Block::Quote(1, "цитата"),
                Block::Quote(2, "глубже"),
                Block::Fence("```rust"),
                Block::Pre("let *a* = 1;"),
                Block::Fence("```"),
                Block::Text("конец"),
            ]
        );
        assert_eq!(
            blocks("```\nне закрыт"),
            [Block::Fence("```"), Block::Pre("не закрыт")],
            "to the end"
        );
    }
}
