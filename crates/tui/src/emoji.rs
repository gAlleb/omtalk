//! An offline Unicode emoji catalog and a keyboard/mouse picker model.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::OnceLock;

pub const CELL_WIDTH: u16 = 5;
pub const CATEGORY_ICONS: [&str; 10] = ["*", "😀", "👋", "🐻", "🍎", "🚗", "⚽", "💡", "🔣", "🏁"];

pub fn category_name(index: usize) -> &'static str {
    [
        tr!("All", "Все"),
        tr!("Smileys & emotion", "Смайлы и эмоции"),
        tr!("People & body", "Люди и жесты"),
        tr!("Animals & nature", "Животные и природа"),
        tr!("Food & drink", "Еда и напитки"),
        tr!("Travel & places", "Путешествия и места"),
        tr!("Activities", "Занятия"),
        tr!("Objects", "Предметы"),
        tr!("Symbols", "Символы"),
        tr!("Flags", "Флаги"),
    ][index]
}

pub struct Emoji {
    pub group: usize,
    pub symbol: &'static str,
    pub name: &'static str,
    pub russian: &'static str,
    search: String,
}

impl Emoji {
    pub fn label(&self) -> &'static str {
        if crate::lang::russian() && !self.russian.is_empty() {
            self.russian
        } else {
            self.name
        }
    }
}

pub fn catalog() -> &'static [Emoji] {
    static CATALOG: OnceLock<Vec<Emoji>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        include_str!("../data/emoji.tsv")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|line| {
                let parts: Vec<_> = line.split('\t').collect();
                Emoji {
                    group: parts[0].parse().expect("generated emoji group"),
                    symbol: parts[1],
                    name: parts[2],
                    russian: parts[3],
                    search: parts[1..].join(" ").to_lowercase(),
                }
            })
            .collect()
    })
}

pub struct Picker {
    pub category: usize,
    pub query: String,
    pub cursor: usize,
    pub columns: usize,
    pub rows: usize,
    pub matches: Vec<usize>,
}

pub enum Outcome {
    Stay,
    Close,
    Insert(&'static str),
}

impl Picker {
    pub fn new() -> Self {
        let mut picker = Self {
            category: 0,
            query: String::new(),
            cursor: 0,
            columns: 12,
            rows: 10,
            matches: Vec::new(),
        };
        picker.filter();
        picker
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        self.matches = catalog()
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                (self.category == 0 || self.category == e.group + 1)
                    && query.split_whitespace().all(|word| e.search.contains(word))
            })
            .map(|(i, _)| i)
            .collect();
        self.cursor = 0;
    }

    pub fn category(&mut self, category: usize) {
        self.category = category.min(CATEGORY_ICONS.len() - 1);
        self.filter();
    }

    pub fn paste(&mut self, text: &str) {
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        self.filter();
    }

    pub fn pick(&self) -> Outcome {
        self.matches
            .get(self.cursor)
            .map_or(Outcome::Stay, |i| Outcome::Insert(catalog()[*i].symbol))
    }

    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        let last = self.matches.len().saturating_sub(1);
        let columns = self.columns.max(1);
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => return self.pick(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(columns),
            KeyCode::Down => self.cursor = (self.cursor + columns).min(last),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(columns * self.rows.max(1)),
            KeyCode::PageDown => self.cursor = (self.cursor + columns * self.rows.max(1)).min(last),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = last,
            KeyCode::Tab => self.category((self.category + 1) % CATEGORY_ICONS.len()),
            KeyCode::BackTab => {
                self.category((self.category + CATEGORY_ICONS.len() - 1) % CATEGORY_ICONS.len())
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.filter();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                    && !c.is_control() =>
            {
                self.query.push(c);
                self.filter();
            }
            _ => {}
        }
        Outcome::Stay
    }
}

impl Default for Picker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn catalog_keeps_whole_sequences_and_searches_russian_and_english() {
        let entries = catalog();
        assert!(entries.len() > 3000, "a full catalog, not only common reactions");
        let unique: std::collections::BTreeSet<_> = entries.iter().map(|e| e.symbol).collect();
        assert_eq!(unique.len(), entries.len());
        for symbol in ["👨‍👩‍👧‍👦", "👍🏽", "🇷🇺", "❤️"] {
            assert!(unique.contains(symbol));
        }
        let mut picker = Picker::new();
        picker.paste("кошка");
        assert!(picker.matches.iter().any(|i| entries[*i].symbol == "🐈"));
        picker.query.clear();
        picker.paste("cat");
        assert!(picker.matches.iter().any(|i| entries[*i].symbol == "🐈"));
        picker.category(9);
        assert!(picker.matches.iter().all(|i| entries[*i].group == 8));
    }

    #[test]
    fn grid_navigation_categories_and_empty_search_are_bounded() {
        let mut picker = Picker::new();
        picker.columns = 7;
        picker.rows = 4;
        picker.key(key(KeyCode::Down));
        assert_eq!(picker.cursor, 7);
        picker.key(key(KeyCode::Right));
        assert_eq!(picker.cursor, 8);
        picker.key(key(KeyCode::PageDown));
        assert_eq!(picker.cursor, 36);
        picker.key(key(KeyCode::End));
        assert_eq!(picker.cursor, picker.matches.len() - 1);
        picker.key(key(KeyCode::Tab));
        assert_eq!(picker.category, 1);
        picker.key(key(KeyCode::BackTab));
        assert_eq!(picker.category, 0);
        picker.paste("no-such-emoji-12345");
        assert!(picker.matches.is_empty());
        picker.key(key(KeyCode::Down));
        assert_eq!(picker.cursor, 0);
        assert!(matches!(picker.key(key(KeyCode::Enter)), Outcome::Stay));
        assert!(matches!(picker.key(key(KeyCode::Esc)), Outcome::Close));
    }
}
