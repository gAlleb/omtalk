//! The settings window: `[ui]` and `[notify]` of the config, changed in place. A change
//! takes effect at once and is written to the config file, its comments kept. A model
//! only; the view draws it, the app applies a change, the runtime writes it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// When a signal sounds, as `[notify]` writes it, and as it is said.
pub const WHEN: &[(&str, &str)] = &[
    ("private+mention", "личные и упоминания"),
    ("private", "личные"),
    ("mention", "упоминания"),
    ("all", "все"),
    ("none", "никогда"),
];
const MODIFIERS: &[(&str, &str)] = &[
    ("ctrl", "Ctrl"),
    ("alt", "Alt"),
    ("super", "Super"),
    ("none", "нет, только префикс"),
];
const ICONS: &[(&str, &str)] = &[("nerd", "Nerd Font"), ("text", "текст")];

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    /// One of the written words; the index into them.
    Choice(&'static [(&'static str, &'static str)], usize),
    Number {
        value: u16,
        min: u16,
        max: u16,
    },
    Text(String),
}

impl Value {
    /// As the window shows it.
    pub fn words(&self) -> String {
        match self {
            Value::Bool(true) => "да".into(),
            Value::Bool(false) => "нет".into(),
            Value::Choice(options, at) => options[*at].1.into(),
            Value::Number { value, .. } => value.to_string(),
            Value::Text(text) if text.is_empty() => "—".into(),
            Value::Text(text) => text.clone(),
        }
    }

    /// As the config file has it.
    pub fn written(&self) -> Written {
        match self {
            Value::Bool(on) => Written::Bool(*on),
            Value::Choice(options, at) => Written::Text(options[*at].0.into()),
            Value::Number { value, .. } => Written::Number(*value as i64),
            Value::Text(text) => Written::Text(text.clone()),
        }
    }

    /// The text of a choice or a text value.
    pub fn text(&self) -> String {
        match self.written() {
            Written::Text(text) => text,
            Written::Bool(on) => on.to_string(),
            Written::Number(n) => n.to_string(),
        }
    }

    fn step(&mut self, by: i32) {
        match self {
            Value::Bool(on) => *on = !*on,
            Value::Choice(options, at) => {
                *at = (*at as i32 + by).rem_euclid(options.len() as i32) as usize;
            }
            Value::Number { value, min, max } => {
                *value = (*value as i32 + by).clamp(*min as i32, *max as i32) as u16;
            }
            Value::Text(_) => {}
        }
    }
}

/// A value for the config file.
#[derive(Debug, Clone, PartialEq)]
pub enum Written {
    Bool(bool),
    Number(i64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    /// The table it is in: "ui" or "notify".
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub value: Value,
}

/// What a key or a click did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Stay,
    Close,
    /// The setting at this row changed.
    Changed(usize),
}

pub struct Settings {
    pub rows: Vec<Setting>,
    pub cursor: usize,
    /// The text being typed for a text setting, and where the cursor is in it (chars).
    pub editing: Option<(String, usize)>,
    /// Why the last change did not take, or where it went.
    pub problem: Option<String>,
}

/// The settings as they stand now, to start the window from.
pub struct Current<'a> {
    pub ui: &'a crate::config::Ui,
    pub notify: &'a crate::notify::NotifyConfig,
}

fn choice(options: &'static [(&'static str, &'static str)], written: &str) -> Value {
    let at = options
        .iter()
        .position(|(w, _)| *w == written.trim())
        .unwrap_or(0);
    Value::Choice(options, at)
}

/// "mention+private" is written either way; the window knows one.
fn when(written: &str) -> Value {
    let parsed = crate::notify::When::parse(written).unwrap_or(crate::notify::When::NEVER);
    let word = WHEN
        .iter()
        .map(|(w, _)| *w)
        .find(|w| crate::notify::When::parse(w).ok() == Some(parsed))
        .unwrap_or("none");
    choice(WHEN, word)
}

impl Settings {
    pub fn open(current: Current) -> Self {
        let ui = current.ui;
        let notify = current.notify;
        let modifier = match ui.modifier {
            crate::config::Modifier::Ctrl => "ctrl",
            crate::config::Modifier::Alt => "alt",
            crate::config::Modifier::Super => "super",
            crate::config::Modifier::None => "none",
        };
        let icons = match ui.icons {
            crate::config::IconSet::Nerd => "nerd",
            crate::config::IconSet::Text => "text",
        };
        let row = |section, key, label, value| Setting {
            section,
            key,
            label,
            value,
        };
        // The older switch: bell = false is bell_on = "none".
        let bell_on = if ui.bell { notify.bell_on.as_str() } else { "none" };
        let rows = vec![
            row(
                "ui",
                "modifier",
                "Модификатор сочетаний",
                choice(MODIFIERS, modifier),
            ),
            row("ui", "leader", "Префикс", Value::Text(ui.leader.clone())),
            row("ui", "icons", "Значки", choice(ICONS, icons)),
            row(
                "ui",
                "left",
                "Ширина списка чатов",
                Value::Number {
                    value: ui.left,
                    min: crate::app::MIN_PANE,
                    max: 80,
                },
            ),
            row(
                "ui",
                "right",
                "Ширина правой панели",
                Value::Number {
                    value: ui.right,
                    min: crate::app::MIN_PANE,
                    max: 80,
                },
            ),
            row("ui", "mouse", "Мышь в omtalk", Value::Bool(ui.mouse)),
            row(
                "ui",
                "read_markers",
                "Отправлять «прочитано»",
                Value::Bool(ui.read_markers),
            ),
            row("ui", "typing", "Отправлять «печатает»", Value::Bool(ui.typing)),
            row(
                "ui",
                "auto_download",
                "Скачивать файлы сразу",
                Value::Bool(ui.auto_download),
            ),
            row(
                "ui",
                "viewer",
                "Чем открывать файлы",
                Value::Text(ui.viewer.clone()),
            ),
            row("notify", "sound_on", "Звук", when(&notify.sound_on)),
            row(
                "notify",
                "sound",
                "Команда звука",
                Value::Text(notify.sound.clone()),
            ),
            row(
                "notify",
                "desktop_on",
                "Всплывающие (notify-send)",
                when(&notify.desktop_on),
            ),
            row("notify", "bell_on", "Звонок терминала", when(bell_on)),
        ];
        Self {
            rows,
            cursor: 0,
            editing: None,
            problem: None,
        }
    }

    pub fn selected(&self) -> &Setting {
        &self.rows[self.cursor]
    }

    /// The value of `section.key`, for applying the others along with one.
    pub fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.rows
            .iter()
            .find(|s| s.section == section && s.key == key)
            .map(|s| &s.value)
    }

    /// `[notify]` as the window has it now.
    pub fn notify(&self) -> crate::notify::NotifyConfig {
        let text = |key| self.get("notify", key).map(Value::text).unwrap_or_default();
        crate::notify::NotifyConfig {
            sound: text("sound"),
            sound_on: text("sound_on"),
            desktop_on: text("desktop_on"),
            bell_on: text("bell_on"),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if self.editing.is_some() {
            return self.edit_key(key);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                Outcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.rows.len() - 1);
                Outcome::Stay
            }
            KeyCode::Left | KeyCode::Char('h') => self.step(-1),
            KeyCode::Right | KeyCode::Char('l') => self.step(1),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            _ => Outcome::Stay,
        }
    }

    /// A click on a row: it is selected, and a click on the one selected changes it.
    pub fn click_row(&mut self, row: usize) -> Outcome {
        if row >= self.rows.len() {
            return Outcome::Stay;
        }
        if self.editing.is_some() && row != self.cursor {
            self.editing = None;
        }
        if row == self.cursor && self.editing.is_none() {
            return self.activate();
        }
        self.cursor = row;
        Outcome::Stay
    }

    /// Enter: a switch flips, a choice goes to the next, text is typed.
    fn activate(&mut self) -> Outcome {
        match &self.selected().value {
            Value::Text(text) => {
                self.editing = Some((text.clone(), text.chars().count()));
                Outcome::Stay
            }
            Value::Number { .. } => Outcome::Stay,
            _ => self.step(1),
        }
    }

    fn step(&mut self, by: i32) -> Outcome {
        let before = self.rows[self.cursor].value.clone();
        self.rows[self.cursor].value.step(by);
        if self.rows[self.cursor].value == before {
            Outcome::Stay
        } else {
            Outcome::Changed(self.cursor)
        }
    }

    fn edit_key(&mut self, key: KeyEvent) -> Outcome {
        let Some((text, at)) = self.editing.as_mut() else {
            return Outcome::Stay;
        };
        let byte = |s: &str, at: usize| s.char_indices().nth(at).map_or(s.len(), |(i, _)| i);
        match key.code {
            KeyCode::Esc => self.editing = None,
            KeyCode::Enter => {
                let text = self
                    .editing
                    .take()
                    .map(|(t, _)| t.trim().to_owned())
                    .unwrap_or_default();
                if Value::Text(text.clone()) != self.rows[self.cursor].value {
                    self.rows[self.cursor].value = Value::Text(text);
                    return Outcome::Changed(self.cursor);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                text.clear();
                *at = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                text.insert(byte(text, *at), c);
                *at += 1;
            }
            KeyCode::Backspace if *at > 0 => {
                *at -= 1;
                text.remove(byte(text, *at));
            }
            KeyCode::Delete if *at < text.chars().count() => {
                text.remove(byte(text, *at));
            }
            KeyCode::Left => *at = at.saturating_sub(1),
            KeyCode::Right => *at = (*at + 1).min(text.chars().count()),
            KeyCode::Home => *at = 0,
            KeyCode::End => *at = text.chars().count(),
            _ => {}
        }
        Outcome::Stay
    }
}

/// Writes `section.key = value` into the config file, keeping everything else in it as
/// it was (comments too). The file is made if there is none. Setting `[notify]
/// bell_on` drops the older `[ui] bell`, which would otherwise override it on start.
pub fn write(path: &std::path::Path, section: &str, key: &str, value: &Written) -> Result<(), String> {
    use toml_edit::{DocumentMut, Item, Table, value as v};
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    if !doc.contains_table(section) {
        doc.insert(section, Item::Table(Table::new()));
    }
    let table = doc[section]
        .as_table_mut()
        .ok_or_else(|| format!("{}: [{section}] — не таблица", path.display()))?;
    let item = match value {
        Written::Bool(on) => v(*on),
        Written::Number(n) => v(*n),
        Written::Text(text) => v(text.as_str()),
    };
    // The comment after the old value stays with the new one.
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            let decor = old.decor().clone();
            let mut new = item.into_value().expect("a value");
            *new.decor_mut() = decor;
            *old = new;
        }
        None => {
            table.insert(key, item);
        }
    }
    if section == "notify"
        && key == "bell_on"
        && let Some(ui) = doc.get_mut("ui").and_then(Item::as_table_mut)
    {
        ui.remove("bell");
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Whole or not at all: a new file next to it, then put in its place.
    let fresh = path.with_extension("toml.new");
    std::fs::write(&fresh, doc.to_string()).map_err(|e| format!("{}: {e}", fresh.display()))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&fresh, meta.permissions());
    }
    std::fs::rename(&fresh, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::open(Current {
            ui: &crate::config::Ui::default(),
            notify: &crate::notify::NotifyConfig {
                sound_on: "mention + private".into(),
                ..Default::default()
            },
        })
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn at(settings: &Settings, key: &str) -> usize {
        settings.rows.iter().position(|s| s.key == key).unwrap()
    }

    #[test]
    fn switches_flip_choices_go_round_and_text_is_typed() {
        let mut s = settings();
        assert_eq!(
            s.get("notify", "sound_on").unwrap().words(),
            "личные и упоминания"
        );

        s.cursor = at(&s, "mouse");
        assert_eq!(s.on_key(key(KeyCode::Enter)), Outcome::Changed(s.cursor));
        assert_eq!(s.selected().value, Value::Bool(false));

        s.cursor = at(&s, "sound_on");
        assert_eq!(
            s.on_key(key(KeyCode::Left)),
            Outcome::Changed(s.cursor),
            "round to the last"
        );
        assert_eq!(s.selected().value.text(), "none");
        assert_eq!(s.notify().sound_on, "none");

        s.cursor = at(&s, "left");
        let before = s.selected().value.clone();
        assert_eq!(s.on_key(key(KeyCode::Right)), Outcome::Changed(s.cursor));
        assert_ne!(s.selected().value, before);
        s.rows[s.cursor].value = Value::Number {
            value: crate::app::MIN_PANE,
            min: crate::app::MIN_PANE,
            max: 80,
        };
        assert_eq!(
            s.on_key(key(KeyCode::Left)),
            Outcome::Stay,
            "not narrower than the least"
        );

        s.cursor = at(&s, "viewer");
        assert_eq!(s.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert!(s.editing.is_some());
        s.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in "imv".chars() {
            s.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.on_key(key(KeyCode::Char('q'))),
            Outcome::Stay,
            "q is typed, not a close"
        );
        s.on_key(key(KeyCode::Backspace));
        assert_eq!(s.on_key(key(KeyCode::Enter)), Outcome::Changed(s.cursor));
        assert_eq!(s.selected().value, Value::Text("imv".into()));
        assert_eq!(s.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn a_change_is_written_with_the_files_comments_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[account]]\njid = \"me@home.org\"\n\n[ui]\nmouse = true   # мышь\nbell = false\n",
        )
        .unwrap();
        write(&path, "ui", "mouse", &Written::Bool(false)).unwrap();
        write(&path, "notify", "bell_on", &Written::Text("mention".into())).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("mouse = false   # мышь"), "{text}");
        assert!(text.contains("jid = \"me@home.org\""), "{text}");
        assert!(!text.contains("bell = false"), "the older switch goes: {text}");
        assert!(text.contains("[notify]\nbell_on = \"mention\""), "{text}");
        crate::config::load(&path).expect("still a valid config");

        let none = dir.path().join("sub/new.toml");
        write(&none, "ui", "left", &Written::Number(30)).unwrap();
        assert!(std::fs::read_to_string(&none).unwrap().contains("left = 30"));
    }
}
