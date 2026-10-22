//! What the client prints. Lines from the server and replies to commands interleave on
//! one terminal, so every line says what it is.

use omtalk_omemo::{CatchUp, DeviceInfo, Direction, Error, Incoming, SendReport, fingerprint};
use omtalk_xmpp::{BareJid, Contact};

pub fn info(text: &str) {
    println!("-- {text}");
}

pub fn error(text: &str) {
    println!("!! {text}");
}

pub fn incoming(message: &Incoming) {
    let Some(text) = &message.text else { return };
    let arrow = match message.direction {
        Direction::Incoming => "<-",
        // Sent from another device of this account.
        Direction::Outgoing => "->",
    };
    let who = match &message.nick {
        Some(nick) if message.sender == message.peer => nick.clone(),
        Some(nick) => format!("{nick} ({})", message.sender),
        None => message.sender.to_string(),
    };
    let how = match message.device {
        Some(device) => format!("device {device}"),
        None => "WITHOUT ENCRYPTION".into(),
    };
    println!("[{}] {arrow} {who} ({how}): {text}", message.peer);
}

pub fn caught_up(archive: &str, result: Result<CatchUp, Error>) {
    match result {
        Ok(caught) => {
            if !caught.messages.is_empty() {
                info(&format!("{} missed in {archive}:", caught.messages.len()));
            }
            for message in &caught.messages {
                incoming(message);
            }
            for (id, e) in &caught.failed {
                error(&format!("{archive}: archived message {id} does not decrypt: {e}"));
            }
        }
        Err(e) => error(&format!("reading {archive} failed: {e}")),
    }
}

pub fn sent(to: &BareJid, result: Result<SendReport, Error>) {
    match result {
        Ok(report) => {
            info(&format!("sent to {to}: {} device(s)", report.devices.len()));
            for (jid, device, e) in &report.failed {
                error(&format!("  {jid} device {device} cannot read it: {e}"));
            }
            for jid in &report.without_omemo {
                error(&format!("  {jid} has no OMEMO device"));
            }
        }
        Err(e) => error(&format!("sending to {to} failed: {e}")),
    }
}

pub fn devices(jid: &BareJid, devices: &[DeviceInfo]) {
    if devices.is_empty() {
        info(&format!("{jid} publishes no OMEMO devices"));
    }
    for device in devices {
        let key = match (device.identity_key, device.known_identity_key) {
            (Some(now), Some(known)) if now != known => {
                format!("{}  !! CHANGED, was {}", fingerprint(&now), fingerprint(&known))
            }
            (Some(now), _) => fingerprint(&now),
            (None, _) => "no bundle (inactive device?)".into(),
        };
        let mut notes = Vec::new();
        if device.this_device {
            notes.push("this device");
        }
        if device.has_session {
            notes.push("session");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  [{}]", notes.join(", "))
        };
        println!("{jid} {:>10}  {key}{notes}", device.device);
    }
}

pub fn contacts(contacts: &[Contact]) {
    if contacts.is_empty() {
        info("no contacts");
    }
    for contact in contacts {
        println!("{}", contact_line(contact));
    }
}

pub fn contact_line(contact: &Contact) -> String {
    if contact.removed {
        return format!("{} removed", contact.jid);
    }
    let name = contact
        .name
        .as_deref()
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();
    let status = match (contact.we_see_them, contact.they_see_us) {
        (true, true) => "you see each other's status",
        (true, false) => "you see their status",
        (false, true) => "they see your status",
        (false, false) if contact.asked => "asked to see their status, no answer yet",
        (false, false) => "no status either way",
    };
    format!("{}{name}: {status}", contact.jid)
}

/// Why a room did not let us in, in words.
pub fn refusal(error: &omtalk_xmpp::Error) -> String {
    use omtalk_xmpp::stanza_error::DefinedCondition as C;
    match error {
        omtalk_xmpp::Error::Stanza(e) => match e.defined_condition {
            C::RegistrationRequired => "members only; you need an invitation".into(),
            C::Conflict => "that nick is taken; try another".into(),
            C::Forbidden => "you are banned from it".into(),
            C::ServiceUnavailable => "it is full".into(),
            C::ItemNotFound => "it does not exist and cannot be created".into(),
            C::NotAuthorized => "it needs a password".into(),
            ref other => format!("{other:?}"),
        },
        other => other.to_string(),
    }
}
