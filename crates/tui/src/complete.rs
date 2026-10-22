//! The commands, and Tab completion in the input line: command names, nicks in a room,
//! addresses, folder names. Pure functions over the line; the app keeps the state of a
//! completion in progress, the view shows the candidates.

pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub about: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/msg",
        args: "JID [ТЕКСТ]",
        about: "личный чат; с текстом — сразу отправить",
    },
    Command {
        name: "/join",
        args: "КОМНАТА [НИК]",
        about: "войти в комнату",
    },
    Command {
        name: "/leave",
        args: "",
        about: "выйти из текущей комнаты",
    },
    Command {
        name: "/add",
        args: "JID [ИМЯ]",
        about: "добавить контакт и попросить подписку",
    },
    Command {
        name: "/accept",
        args: "JID",
        about: "разрешить видеть ваш статус",
    },
    Command {
        name: "/refuse",
        args: "JID",
        about: "отказать в подписке",
    },
    Command {
        name: "/devices",
        args: "",
        about: "устройства OMEMO собеседника",
    },
    Command {
        name: "/services",
        args: "[АДРЕС]",
        about: "сервисы и команды сервера",
    },
    Command {
        name: "/folder",
        args: "[ПАПКА]",
        about: "положить чат в папку; без имени — вынуть",
    },
    Command {
        name: "/rename-folder",
        args: "СТАРАЯ НОВАЯ",
        about: "переименовать папку",
    },
    Command {
        name: "/help",
        args: "",
        about: "этот список",
    },
    Command {
        name: "/upload",
        args: "ПУТЬ",
        about: "отправить файл или картинку",
    },
    Command {
        name: "/keys",
        args: "[АДРЕС]",
        about: "ключи OMEMO: отпечатки, доверие, старые устройства",
    },
    Command {
        name: "/close",
        args: "",
        about: "закрыть чат; комнату — покинуть",
    },
    Command {
        name: "/quit",
        args: "",
        about: "выйти из omtalk",
    },
];

pub fn about(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// What can be completed where, gathered from the app.
#[derive(Debug, Default)]
pub struct Context {
    /// Nicks in the current room, if it is one.
    pub nicks: Vec<String>,
    /// Addresses of people: direct chats and room participants who show theirs.
    pub people: Vec<String>,
    pub rooms: Vec<String>,
    pub folders: Vec<String>,
    /// Servers and their services: the accounts' own, and those of open rooms.
    pub servers: Vec<String>,
}

/// A completion being cycled through with Tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Where the completed word starts, in characters.
    pub start: usize,
    /// How many characters the current candidate put in.
    pub inserted: usize,
    pub candidates: Vec<String>,
    pub index: usize,
}

/// The candidates for the word before `cursor` (in characters), and where it starts.
/// Each candidate is the whole replacement, with what follows it (a space, or ": " after a
/// nick that opens the line, as chat clients do).
pub fn candidates(input: &str, cursor: usize, context: &Context) -> Option<(usize, Vec<String>)> {
    let before: String = input.chars().take(cursor).collect();
    let first_end = before.find(char::is_whitespace);
    let command = first_end.map(|end| &before[..end]);
    // The folder's name is all the rest of the line, spaces and all.
    // A path: all the rest of the line, completed from the disk.
    if let (Some("/upload"), Some(end)) = (command, first_end) {
        let rest = &before[end..];
        let start = end + rest.len() - rest.trim_start().len();
        let found = paths(&before[start..]);
        return (!found.is_empty()).then(|| (before[..start].chars().count(), found));
    }
    if let (Some("/folder"), Some(end)) = (command, first_end) {
        let rest = &before[end..];
        let start = end + rest.len() - rest.trim_start().len();
        let typed = &before[start..];
        let found = matching(context.folders.iter().map(String::as_str), typed, "");
        return (!found.is_empty()).then(|| (before[..start].chars().count(), found));
    }
    let start_byte = before
        .char_indices()
        .rfind(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let typed = &before[start_byte..];
    let start = before[..start_byte].chars().count();
    let found = match command {
        None if typed.starts_with('/') => matching(COMMANDS.iter().map(|c| c.name), typed, " "),
        Some("/msg" | "/add" | "/accept" | "/refuse" | "/keys") if is_first_argument(&before, start_byte) => {
            matching(context.people.iter().map(String::as_str), typed, " ")
        }
        Some("/join") if is_first_argument(&before, start_byte) => {
            matching(context.rooms.iter().map(String::as_str), typed, " ")
        }
        Some("/services") if is_first_argument(&before, start_byte) => {
            matching(context.servers.iter().map(String::as_str), typed, "")
        }
        Some(c) if c.starts_with('/') => Vec::new(),
        // A nick: at the start of the line it addresses someone.
        _ if typed.is_empty() => Vec::new(),
        _ => matching(
            context.nicks.iter().map(String::as_str),
            typed,
            if start == 0 { ": " } else { " " },
        ),
    };
    (!found.is_empty()).then_some((start, found))
}

/// Files and folders beginning with `typed` (a path, `~/` for home), folders with `/`
/// after them so Tab goes on inside; hidden ones only when asked for with a dot.
pub fn paths(typed: &str) -> Vec<String> {
    let (dir, prefix) = match typed.rfind('/') {
        Some(at) => (&typed[..=at], &typed[at + 1..]),
        None => ("", typed),
    };
    let on_disk = match dir {
        "" => std::path::PathBuf::from("."),
        d if d.starts_with("~/") => match std::env::var_os("HOME") {
            Some(home) => std::path::PathBuf::from(home).join(&d[2..]),
            None => return Vec::new(),
        },
        d => std::path::PathBuf::from(d),
    };
    let Ok(entries) = std::fs::read_dir(&on_disk) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                return None;
            }
            let folder = e.file_type().is_ok_and(|t| t.is_dir());
            Some(format!("{dir}{name}{}", if folder { "/" } else { "" }))
        })
        .collect();
    found.sort();
    found
}

/// Whether the word starting at `start` is the command's first argument.
fn is_first_argument(before: &str, start: usize) -> bool {
    before[..start].split_whitespace().count() == 1
}

/// Candidates beginning with `typed`, ignoring case, in their order, each with `suffix`.
fn matching<'a>(all: impl Iterator<Item = &'a str>, typed: &str, suffix: &str) -> Vec<String> {
    let typed = typed.to_lowercase();
    let mut found: Vec<String> = Vec::new();
    for candidate in all {
        let full = format!("{candidate}{suffix}");
        if candidate.to_lowercase().starts_with(&typed) && !found.contains(&full) {
            found.push(full);
        }
    }
    found
}

/// The commands that begin with what is typed, while only the command is being typed.
pub fn commands_for(input: &str) -> Vec<&'static Command> {
    if !input.starts_with('/') || input.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMANDS.iter().filter(|c| c.name.starts_with(input)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context {
            nicks: vec!["Алиса".into(), "alex".into(), "bob".into()],
            people: vec!["anna@home.org".into(), "boss@work.org".into()],
            rooms: vec!["team@muc.home.org".into()],
            folders: vec!["Работа".into(), "Рабочие дела".into()],
            servers: vec!["home.org".into(), "muc.home.org".into()],
        }
    }

    fn complete(input: &str) -> Option<(usize, Vec<String>)> {
        candidates(input, input.chars().count(), &context())
    }

    #[test]
    fn commands_complete_and_are_listed_while_typed() {
        assert_eq!(complete("/se"), Some((0, vec!["/services ".into()])));
        assert_eq!(complete("/re").unwrap().1, ["/refuse ", "/rename-folder "]);
        assert_eq!(
            commands_for("/f").iter().map(|c| c.name).collect::<Vec<_>>(),
            ["/folder"]
        );
        assert_eq!(commands_for("/").len(), COMMANDS.len());
        assert!(
            commands_for("/msg bob").is_empty(),
            "the list goes once the command is typed"
        );
        assert!(commands_for("привет").is_empty());
    }

    #[test]
    fn nicks_complete_with_the_address_mark_at_the_start_only() {
        assert_eq!(complete("al"), Some((0, vec!["alex: ".into()])));
        assert_eq!(
            complete("а"),
            Some((0, vec!["Алиса: ".into()])),
            "case and Cyrillic"
        );
        assert_eq!(complete("спасибо, b"), Some((9, vec!["bob ".into()])));
        assert_eq!(complete("A").unwrap().1, ["alex: "], "Latin A is not Cyrillic А");
        assert_eq!(complete("привет "), None, "nothing typed, nothing guessed");
    }

    #[test]
    fn addresses_rooms_and_folders_complete_after_their_commands() {
        assert_eq!(complete("/msg b"), Some((5, vec!["boss@work.org ".into()])));
        assert_eq!(complete("/join t"), Some((6, vec!["team@muc.home.org ".into()])));
        assert_eq!(
            complete("/msg boss@work.org al"),
            None,
            "the text after the address is free"
        );
        assert_eq!(complete("/services m"), Some((10, vec!["muc.home.org".into()])));
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("фото")).unwrap();
        std::fs::write(dir.path().join("фото").join("кот.png"), b"").unwrap();
        std::fs::write(dir.path().join(".скрытый"), b"").unwrap();
        let base = dir.path().display().to_string();
        assert_eq!(
            complete(&format!("/upload {base}/ф")).unwrap().1,
            [format!("{base}/фото/")]
        );
        assert_eq!(
            complete(&format!("/upload {base}/фото/")).unwrap().1,
            [format!("{base}/фото/кот.png")]
        );
        assert_eq!(
            complete(&format!("/upload {base}/")).unwrap().1,
            [format!("{base}/фото/")],
            "hidden ones not unasked"
        );
        assert_eq!(complete("/services ").unwrap().1, ["home.org", "muc.home.org"]);
        assert_eq!(
            complete("/folder Раб").unwrap(),
            (8, vec!["Работа".into(), "Рабочие дела".into()])
        );
        assert_eq!(
            complete("/folder Рабочие д"),
            Some((8, vec!["Рабочие дела".into()]))
        );
        // The cursor decides, not the end of the line.
        assert_eq!(
            candidates("/se что-то", 3, &context()),
            Some((0, vec!["/services ".into()]))
        );
    }
}
