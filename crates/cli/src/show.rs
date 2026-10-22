//! What the client prints. Lines from the server and replies to commands interleave on
//! one terminal, so every line says what it is.

use omtalk_omemo::{CatchUp, DeviceInfo, Direction, Error, Incoming, SendReport, fingerprint};
use omtalk_xmpp::BareJid;

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
    println!(
        "[{}] {arrow} {} (device {}): {text}",
        message.peer, message.sender, message.device
    );
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
