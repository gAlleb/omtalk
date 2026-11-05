use irc_proto::{Command, Message};
use omtalk_core::MessageKind;

pub(crate) fn atom(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(':')
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Keep extension arguments opaque. irc-proto's typed MODE discards arguments for
/// custom PREFIX modes, and typed BATCH uppercases vendor batch names. A neutral
/// command lets its ordinary parser handle the complete envelope, tags, escaping
/// and parameters without applying that lossy command-specific coercion.
pub(crate) fn parse(line: &str) -> Result<Message, String> {
    // Some servers send CRCRLF; accepting the redundant terminator must not
    // introduce a carriage return into a displayed body (senpai 2132570).
    let line = line.trim_end_matches(['\r', '\n']);
    let mut start = 0;
    for marker in ['@', ':'] {
        if line[start..].starts_with(marker) {
            start += line[start..].find(' ').ok_or("IRC: malformed envelope")? + 1;
            while line[start..].starts_with(' ') {
                start += 1;
            }
        }
    }
    let end = start
        + line[start..]
            .find([' ', '\r', '\n'])
            .unwrap_or(line.len() - start);
    let name = &line[start..end];
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("IRC: malformed command".into());
    }
    let normalized = format!("{}OMTALKRAW{}", &line[..start], &line[end..]);
    let mut message: Message = normalized
        .parse()
        .map_err(|_| "IRC: malformed frame".to_owned())?;
    if let Command::Raw(command, _) = &mut message.command {
        *command = name.to_ascii_uppercase();
    }
    Ok(message)
}

pub(crate) fn parts(message: &Message) -> (String, Vec<String>) {
    match &message.command {
        Command::Raw(name, args) => (name.to_ascii_uppercase(), args.clone()),
        _ => parts(&parse(&message.to_string()).expect("irc-proto serializes a valid message")),
    }
}

pub(crate) fn tag<'a>(message: &'a Message, name: &str) -> Option<&'a str> {
    message.tags.as_ref()?.iter().find(|t| t.0 == name)?.1.as_deref()
}

pub(crate) fn has_tag(message: &Message, name: &str) -> bool {
    message
        .tags
        .as_ref()
        .is_some_and(|tags| tags.iter().any(|t| t.0 == name))
}

/// Strip IRC formatting, including colour parameters, before display/mentions.
/// Ordinary digits and punctuation must survive unchanged.
pub(crate) fn plain(text: &str) -> String {
    let mut chars = text.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\x03' || c == '\x04' {
            let limit = if c == '\x03' { 2 } else { 6 };
            let colour = |c: char| {
                if limit == 2 {
                    c.is_ascii_digit()
                } else {
                    c.is_ascii_hexdigit()
                }
            };
            for _ in 0..limit {
                if chars.peek().is_some_and(|c| colour(*c)) {
                    chars.next();
                } else {
                    break;
                }
            }
            if chars.peek() == Some(&',') {
                let mut next = chars.clone();
                next.next();
                if next.peek().is_some_and(|c| colour(*c)) {
                    chars.next();
                    for _ in 0..limit {
                        if chars.peek().is_some_and(|c| colour(*c)) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
            }
        } else if !c.is_control() || matches!(c, '\n' | '\t') {
            out.push(c);
        }
    }
    out
}

pub(crate) fn command(name: &str, args: &[&str]) -> Message {
    // Raw retains the intended argument list (in particular custom ISUPPORT modes).
    Command::Raw(name.into(), args.iter().map(|s| (*s).into()).collect()).into()
}

/// A bounded IRC frame. An embedded newline is rejected even for /raw.
pub(crate) fn encode(message: &Message) -> Result<String, String> {
    let line = message.to_string();
    let body = line.trim_end_matches(['\r', '\n']);
    if body.contains(['\r', '\n', '\0']) {
        return Err("IRC: a command cannot contain CR, LF or NUL".into());
    }
    let command = if body.starts_with('@') {
        body.split_once(' ').map_or(body, |(_, s)| s)
    } else {
        body
    };
    if command.len() > 510 || body.len() > 8190 {
        return Err("IRC: command exceeds the server line limit".into());
    }
    Ok(format!("{body}\r\n"))
}

/// Split every line on UTF-8 boundaries, counting bytes including command and ACTION
/// framing. Reserve space for the server's sender prefix when it relays the message.
pub fn split_message(target: &str, text: &str, kind: MessageKind, max: usize) -> Result<Vec<String>, String> {
    if !atom(target) {
        return Err("IRC: invalid message target".into());
    }
    let framing = if kind == MessageKind::Action { 9 } else { 0 };
    let limit = max.min(510).saturating_sub(target.len() + 12 + framing + 100);
    if limit < 4 {
        return Err("IRC: target leaves no room for a message".into());
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut result = Vec::new();
    for line in text.split('\n') {
        let clean: String = line.chars().filter(|c| !c.is_control() || *c == '\t').collect();
        let mut rest = clean.as_str();
        if rest.is_empty() {
            result.push(" ".into());
        }
        while !rest.is_empty() {
            let mut end = rest.len().min(limit);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            result.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
    }
    if result.len() > 256 {
        return Err("IRC: paste is too large (over 256 messages); upload it as a file".into());
    }
    Ok(result)
}

/// Service conversations are ephemeral: command grammars are extensible and may
/// contain passwords anywhere. Also exclude explicit authentication/raw commands.
pub fn is_sensitive(target: &str, text: &str) -> bool {
    target.eq_ignore_ascii_case("BouncerServ")
        || text.split_whitespace().next().is_some_and(|s| {
            matches!(
                s.trim_start_matches('/').to_ascii_lowercase().as_str(),
                "ns" | "cs" | "ms" | "hs" | "os"
            )
        })
        || target.to_ascii_lowercase().ends_with("serv")
        || text.split_whitespace().any(|w| {
            let word = w
                .trim_start_matches(['/', ':'])
                .split('=')
                .next()
                .unwrap_or(w)
                .to_ascii_lowercase();
            if word.ends_with("serv") {
                return true;
            }
            matches!(
                word.as_str(),
                "pass"
                    | "authenticate"
                    | "identify"
                    | "password"
                    | "-pass"
                    | "-password"
                    | "-sasl-password"
                    | "oper"
            )
        })
        || text.split_whitespace().enumerate().any(|(i, w)| {
            w.trim_start_matches('/').eq_ignore_ascii_case("JOIN")
                && text.split_whitespace().nth(i + 2).is_some()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_paste_cannot_inject_and_keeps_all_characters() {
        let text = "Привет 🌍".repeat(200);
        let pieces = split_message("#om", &text, MessageKind::Action, 510).unwrap();
        assert_eq!(pieces.concat(), text);
        for piece in pieces {
            assert!(
                encode(&command("PRIVMSG", &["#om", &format!("\x01ACTION {piece}\x01")]))
                    .unwrap()
                    .len()
                    <= 512
            );
        }
        assert!(encode(&command("PRIVMSG", &["#om", "hi\r\nQUIT"])).is_err());
        assert_eq!(
            split_message("#om", "hello\r\nQUIT", MessageKind::Text, 510).unwrap(),
            ["hello", "QUIT"]
        );
    }

    #[test]
    fn library_preserves_tag_escapes_and_trailing_spaces() {
        let m: Message = "@label=a\\sb;msgid=Aa :nick!u@h PRIVMSG #om :hello  world\r\n"
            .parse()
            .unwrap();
        assert_eq!(tag(&m, "label"), Some("a b"));
        assert_eq!(tag(&m, "msgid"), Some("Aa"));
        assert_eq!(
            parts(&m),
            ("PRIVMSG".into(), vec!["#om".into(), "hello  world".into()])
        );
    }
}
