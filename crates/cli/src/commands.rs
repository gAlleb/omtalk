//! The commands typed at the prompt.

use omtalk_omemo::Omemo;
use omtalk_xmpp::{Account, BareJid};

use crate::show;

pub const HELP: &str = "\
/msg JID TEXT        send an OMEMO message
/join ROOM [NICK]    enter a group chat and read what it said meanwhile
/say ROOM TEXT       send an OMEMO message to a group chat
/devices JID         devices of an account, with fingerprints (yours too)
/help                this list
/quit                leave";

#[derive(Debug, PartialEq)]
pub enum Command {
    Msg { to: BareJid, text: String },
    Join { room: BareJid, nick: Option<String> },
    Say { room: BareJid, text: String },
    Devices { jid: BareJid },
    Help,
    Quit,
}

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (name, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    let jid = |text: &str| {
        text.parse::<BareJid>()
            .map_err(|e| format!("not a JID: {text} ({e})"))
    };
    let jid_and_text = |rest: &str| {
        let (target, text) = rest.split_once(char::is_whitespace).ok_or("missing text")?;
        Ok::<_, String>((jid(target)?, text.trim().to_owned()))
    };
    match name {
        "/msg" => jid_and_text(rest).map(|(to, text)| Command::Msg { to, text }),
        "/say" => jid_and_text(rest).map(|(room, text)| Command::Say { room, text }),
        "/join" => {
            let mut words = rest.split_whitespace();
            let room = jid(words.next().ok_or("missing room")?)?;
            Ok(Command::Join {
                room,
                nick: words.next().map(str::to_owned),
            })
        }
        "/devices" => Ok(Command::Devices { jid: jid(rest)? }),
        "/help" => Ok(Command::Help),
        "/quit" => Ok(Command::Quit),
        "" => Err("type /help for commands".into()),
        other => Err(format!("unknown command {other}; type /help")),
    }
}

pub async fn run(omemo: &Omemo, account: &Account, command: Command) {
    match command {
        Command::Msg { to, text } => show::sent(&to, omemo.send(&to, &text).await),
        Command::Say { room, text } => show::sent(&room, omemo.send_group(&room, &text).await),
        Command::Join { room, nick } => {
            let nick =
                nick.unwrap_or_else(|| account.jid().node().map(|n| n.to_string()).unwrap_or_default());
            match account.join(&room, &nick).await {
                Ok(()) => show::caught_up(room.as_str(), omemo.catch_up_room(&room).await),
                Err(e) => show::error(&format!("joining {room} failed: {e}")),
            }
        }
        Command::Devices { jid } => match omemo.devices(&jid).await {
            Ok(devices) => show::devices(&jid, &devices),
            Err(e) => show::error(&format!("reading the devices of {jid} failed: {e}")),
        },
        Command::Help => println!("{HELP}"),
        Command::Quit => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse() {
        assert_eq!(
            parse("/msg bob@example.org привет, Боб").unwrap(),
            Command::Msg {
                to: "bob@example.org".parse().unwrap(),
                text: "привет, Боб".into()
            }
        );
        assert_eq!(
            parse("  /join room@muc.example.org alice ").unwrap(),
            Command::Join {
                room: "room@muc.example.org".parse().unwrap(),
                nick: Some("alice".into())
            }
        );
        assert_eq!(
            parse("/join room@muc.example.org").unwrap(),
            Command::Join {
                room: "room@muc.example.org".parse().unwrap(),
                nick: None
            }
        );
        assert!(parse("/msg bob@example.org").is_err(), "a message needs text");
        assert!(parse("/devices not a jid").is_err());
        assert!(
            parse("hello").is_err(),
            "plain text is not sent anywhere by accident"
        );
    }
}
