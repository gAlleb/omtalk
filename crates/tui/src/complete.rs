//! The commands, and Tab completion in the input line: command names, nicks in a room,
//! addresses, folder names. Pure functions over the line; the app keeps the state of a
//! completion in progress, the view shows the candidates.

pub struct Command {
    pub name: &'static str,
    /// In English and in Russian; [`Command::args`] picks.
    pub args: [&'static str; 2],
    pub about: [&'static str; 2],
}

impl Command {
    pub fn args(&self) -> &'static str {
        self.args[usize::from(crate::lang::russian())]
    }

    pub fn about(&self) -> &'static str {
        self.about[usize::from(crate::lang::russian())]
    }
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/emoji",
        args: ["", ""],
        about: ["insert an emoji", "вставить эмодзи"],
    },
    Command {
        name: "/room",
        args: ["", ""],
        about: [
            "room configuration, members and roles",
            "настройки комнаты, участники и права",
        ],
    },
    Command {
        name: "/np",
        args: ["", ""],
        about: ["share the playing MPRIS track", "отправить текущий трек MPRIS"],
    },
    Command {
        name: "/msg",
        args: ["JID [TEXT]", "JID [ТЕКСТ]"],
        about: [
            "a direct chat; with text, sends it at once",
            "личный чат; с текстом — сразу отправить",
        ],
    },
    Command {
        name: "/join",
        args: ["ROOM [NICK]", "КОМНАТА [НИК]"],
        about: ["join a room", "войти в комнату"],
    },
    Command {
        name: "/mute",
        args: ["", ""],
        about: [
            "mute all notifications from this chat",
            "выключить все уведомления этого чата",
        ],
    },
    Command {
        name: "/unmute",
        args: ["", ""],
        about: [
            "restore notifications from this chat",
            "вернуть уведомления этого чата",
        ],
    },
    Command {
        name: "/leave",
        args: ["", ""],
        about: ["leave the current room", "выйти из текущей комнаты"],
    },
    Command {
        name: "/add",
        args: ["JID [NAME]", "JID [ИМЯ]"],
        about: [
            "add a contact and ask to see their status",
            "добавить контакт и попросить подписку",
        ],
    },
    Command {
        name: "/accept",
        args: ["JID", "JID"],
        about: ["let them see your status", "разрешить видеть ваш статус"],
    },
    Command {
        name: "/refuse",
        args: ["JID", "JID"],
        about: ["refuse a subscription request", "отказать в подписке"],
    },
    Command {
        name: "/devices",
        args: ["", ""],
        about: ["the contact's OMEMO devices", "устройства OMEMO собеседника"],
    },
    Command {
        name: "/services",
        args: ["[ADDRESS]", "[АДРЕС]"],
        about: ["the server's services and commands", "сервисы и команды сервера"],
    },
    Command {
        name: "/folder",
        args: ["[FOLDER]", "[ПАПКА]"],
        about: [
            "put the chat in a folder; without a name, take it out",
            "положить чат в папку; без имени — вынуть",
        ],
    },
    Command {
        name: "/folder-up",
        args: ["", ""],
        about: ["move the open chat's folder up", "поднять папку открытого чата"],
    },
    Command {
        name: "/folder-down",
        args: ["", ""],
        about: [
            "move the open chat's folder down",
            "опустить папку открытого чата",
        ],
    },
    Command {
        name: "/rename-folder",
        args: ["OLD NEW", "СТАРАЯ НОВАЯ"],
        about: ["rename a folder", "переименовать папку"],
    },
    Command {
        name: "/help",
        args: ["", ""],
        about: ["this list", "этот список"],
    },
    Command {
        name: "/upload",
        args: ["PATH", "ПУТЬ"],
        about: ["send a file or a picture", "отправить файл или картинку"],
    },
    Command {
        name: "/me",
        args: ["TEXT", "ТЕКСТ"],
        about: [
            "say what you do: * nick text, as in IRC",
            "о себе в третьем лице: * ник текст, как в IRC",
        ],
    },
    Command {
        name: "/reply",
        args: ["", ""],
        about: [
            "reply to the selected or the last message",
            "ответить на выделенное или последнее сообщение",
        ],
    },
    Command {
        name: "/search",
        args: ["WORDS", "СЛОВА"],
        about: [
            "find words in this chat's history; Enter shows the message",
            "найти слова в истории этого чата; Enter покажет сообщение",
        ],
    },
    Command {
        name: "/away",
        args: ["[TEXT]", "[ТЕКСТ]"],
        about: [
            "status: away, with words of your own",
            "статус: отошёл, можно со своими словами",
        ],
    },
    Command {
        name: "/dnd",
        args: ["[TEXT]", "[ТЕКСТ]"],
        about: ["status: do not disturb", "статус: не беспокоить"],
    },
    Command {
        name: "/online",
        args: ["[TEXT]", "[ТЕКСТ]"],
        about: ["status: online again", "статус: снова в сети"],
    },
    Command {
        name: "/react",
        args: ["EMOJI", "ЭМОДЗИ"],
        about: [
            "react to the selected or the last message; again, and it is taken back",
            "реакция на выделенное или последнее сообщение; повторно — снять",
        ],
    },
    Command {
        name: "/reactions",
        args: ["", ""],
        about: [
            "who reacted to the selected message, else the last with reactions",
            "кто отреагировал на выбранное сообщение или последнее с реакциями",
        ],
    },
    Command {
        name: "/edit",
        args: ["", ""],
        about: [
            "correct your last message here",
            "исправить своё последнее сообщение здесь",
        ],
    },
    Command {
        name: "/commands",
        args: ["", ""],
        about: [
            "this chat's commands (a room's, a bridge's, a contact's client's)",
            "команды этого чата (комнаты, моста, клиента собеседника)",
        ],
    },
    Command {
        name: "/settings",
        args: ["[chat]", "[chat]"],
        about: ["settings", "настройки"],
    },
    Command {
        name: "/keys",
        args: ["[ADDRESS]", "[АДРЕС]"],
        about: [
            "OMEMO keys: fingerprints, trust, old devices",
            "ключи OMEMO: отпечатки, доверие, старые устройства",
        ],
    },
    Command {
        name: "/create",
        args: ["NAME [TITLE]", "ИМЯ [НАЗВАНИЕ]"],
        about: [
            "a new closed room (for OMEMO)",
            "новая закрытая комната (для OMEMO)",
        ],
    },
    Command {
        name: "/invite",
        args: ["JID", "JID"],
        about: [
            "invite to the room (make a member)",
            "позвать в комнату (сделать участником)",
        ],
    },
    Command {
        name: "/nick",
        args: ["[NICK]", "[НИК]"],
        about: [
            "your nick in the room; none: back to the default",
            "свой ник в комнате; без ника — обычный",
        ],
    },
    Command {
        name: "/avatar",
        args: ["[PATH]", "[ПУТЬ]"],
        about: [
            "your avatar from a picture; none: take it away",
            "своя аватарка из картинки; без пути — убрать",
        ],
    },
    Command {
        name: "/nickname",
        args: ["[NAME]", "[ИМЯ]"],
        about: [
            "the name you give yourself, seen by your contacts; none: take it away",
            "своё имя, которое видят контакты; без имени — убрать",
        ],
    },
    Command {
        name: "/topic",
        args: ["[TEXT]", "[ТЕКСТ]"],
        about: [
            "the room's topic: show or change it",
            "тема комнаты: показать или сменить",
        ],
    },
    Command {
        name: "/name",
        args: ["[TITLE]", "[НАЗВАНИЕ]"],
        about: [
            "the room's title (owner only); a contact's name for you, none: take it away",
            "название комнаты (только владелец); имя контакта для вас, без имени — убрать",
        ],
    },
    Command {
        name: "/close",
        args: ["", ""],
        about: [
            "close the chat; a room is left",
            "закрыть чат; комнату — покинуть",
        ],
    },
    Command {
        name: "/quit",
        args: ["", ""],
        about: ["quit omtalk", "выйти из omtalk"],
    },
    Command {
        name: "/remove",
        args: ["[JID]", "[JID]"],
        about: [
            "remove a contact (the open chat's, or the address given)",
            "удалить контакт (открытого чата или по адресу)",
        ],
    },
    Command {
        name: "/record",
        args: ["", ""],
        about: ["record a voice message", "записать голосовое сообщение"],
    },
    Command {
        name: "/play",
        args: ["", ""],
        about: [
            "play/pause selected or latest audio",
            "играть/пауза выбранного или последнего аудио",
        ],
    },
    Command {
        name: "/stop",
        args: ["", ""],
        about: ["stop audio playback", "остановить аудио"],
    },
    Command {
        name: "/seek",
        args: ["SECONDS", "СЕКУНДЫ"],
        about: [
            "seek in selected or latest audio",
            "перемотать выбранное или последнее аудио",
        ],
    },
];

pub fn about(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// Commands that can choose an account independently of the currently open chat.
pub fn allows_account(command: &str) -> bool {
    matches!(
        command,
        "/msg"
            | "/join"
            | "/add"
            | "/remove"
            | "/accept"
            | "/refuse"
            | "/create"
            | "/keys"
            | "/services"
            | "/nickname"
            | "/avatar"
    )
}

/// What can be completed where, gathered from the app.
#[derive(Debug, Default)]
pub struct Context {
    /// Full JIDs of the configured accounts, for --account.
    pub accounts: Vec<String>,
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
    if let (Some(command), Some(end)) = (command, first_end)
        && allows_account(command)
    {
        let rest = before[end..].trim_start();
        if rest.split_whitespace().next() == Some("--account") {
            if rest == "--account" {
                return Some((before.chars().count(), vec![" ".into()]));
            }
            let value = rest["--account".len()..].trim_start();
            if let Some(end) = value.find(char::is_whitespace) {
                let tail = value[end..].trim_start();
                let normalized = format!("{command} {tail}");
                let skipped = before.chars().count() - normalized.chars().count();
                return candidates(&normalized, normalized.chars().count(), context)
                    .map(|(start, found)| (start + skipped, found));
            }
            let start = before.chars().count() - value.chars().count();
            let found = matching(context.accounts.iter().map(String::as_str), value, " ");
            return (!found.is_empty()).then_some((start, found));
        }
        if !rest.contains(char::is_whitespace) && rest.starts_with('-') {
            let start = before.chars().count() - rest.chars().count();
            let found = matching(["--account"].into_iter(), rest, " ");
            return (!found.is_empty()).then_some((start, found));
        }
    }
    // The folder's name is all the rest of the line, spaces and all.
    // A path: all the rest of the line, completed from the disk.
    if let (Some("/upload" | "/avatar"), Some(end)) = (command, first_end) {
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
        Some("/msg" | "/add" | "/remove" | "/accept" | "/refuse" | "/keys" | "/invite")
            if is_first_argument(&before, start_byte) =>
        {
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
            accounts: vec!["me@home.org".into(), "me@work.org".into()],
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
    fn account_options_and_their_following_targets_complete() {
        assert_eq!(complete("/msg --a"), Some((5, vec!["--account ".into()])));
        assert_eq!(
            complete("/msg --account me@w"),
            Some((15, vec!["me@work.org ".into()]))
        );
        for (prefix, tail, expected) in [
            ("/msg --account me@work.org ", "b", "boss@work.org "),
            ("/join --account me@work.org ", "t", "team@muc.home.org "),
            ("/services --account me@work.org ", "m", "muc.home.org"),
            ("/remove --account me@work.org ", "a", "anna@home.org "),
            ("/msg --account я@home.org ", "b", "boss@work.org "),
        ] {
            assert_eq!(
                complete(&format!("{prefix}{tail}")),
                Some((prefix.chars().count(), vec![expected.into()]))
            );
        }
        assert!(
            complete("/msg boss@work.org --a").is_none(),
            "message body is not an option"
        );
        assert!(complete("/msg --account unknown").is_none());
    }

    #[test]
    fn commands_complete_and_are_listed_while_typed() {
        assert_eq!(complete("/ser"), Some((0, vec!["/services ".into()])));
        assert_eq!(
            complete("/re").unwrap().1,
            [
                "/refuse ",
                "/rename-folder ",
                "/reply ",
                "/react ",
                "/reactions ",
                "/remove ",
                "/record "
            ]
        );
        assert_eq!(
            commands_for("/f").iter().map(|c| c.name).collect::<Vec<_>>(),
            ["/folder", "/folder-up", "/folder-down"]
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
        // A picture for one's avatar, after the account named too.
        assert_eq!(
            complete(&format!("/avatar {base}/фото/")).unwrap().1,
            [format!("{base}/фото/кот.png")]
        );
        let named = format!("/avatar --account me@home.org {base}/фото/к");
        let (start, found) = complete(&named).unwrap();
        assert_eq!(found, [format!("{base}/фото/кот.png")]);
        assert_eq!(start, "/avatar --account me@home.org ".chars().count());
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
            candidates("/ser что-то", 4, &context()),
            Some((0, vec!["/services ".into()]))
        );
    }
}
