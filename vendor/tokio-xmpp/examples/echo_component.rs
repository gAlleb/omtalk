use futures::stream::StreamExt;
use std::env::args;
use std::process::exit;
use std::str::FromStr;
use xmpp_parsers::jid::Jid;
use xmpp_parsers::message::{Lang, Message, MessageType};
use xmpp_parsers::presence::{Presence, Show as PresenceShow, Type as PresenceType};

use tokio_xmpp::{connect::DnsConfig, xmlstream::Timeouts, Component};

#[tokio::main]
async fn main() {
    env_logger::init();

    let args: Vec<String> = args().collect();
    if args.len() < 3 || args.len() > 4 {
        println!("Usage: {} <jid> <password> [server:port]", args[0]);
        exit(1);
    }
    let jid = &args[1];
    let password = &args[2];

    let server = if let Some(server) = args.get(3) {
        DnsConfig::addr(server)
    } else {
        DnsConfig::no_srv("127.0.0.1", 5347)
    };

    // Component instance
    println!("{} {} {}", jid, password, server);

    // If you don't need a custom server but default localhost:5347, you can use
    // Component::new() directly
    let mut component = Component::new_plaintext(jid, password, server, Timeouts::tight())
        .await
        .unwrap();

    // Make the two interfaces for sending and receiving independent
    // of each other so we can move one into a closure.
    println!("Online: {}", component.jid);

    // TODO: replace these hardcoded JIDs
    let presence = make_presence(
        Jid::from_str("test@component.linkmauve.fr/coucou").unwrap(),
        Jid::from_str("linkmauve@linkmauve.fr").unwrap(),
    );
    component.send_stanza(presence.into()).await.unwrap();

    // Main loop, processes events
    loop {
        if let Some(stanza) = component.next().await {
            if let Some(message) = Message::try_from(stanza).ok() {
                // This is a message we'll echo
                match (message.from, message.bodies.get("")) {
                    (Some(from), Some(body)) => {
                        if message.type_ != MessageType::Error {
                            let reply = make_reply(from, &body);
                            component.send_stanza(reply.into()).await.unwrap();
                        }
                    }
                    _ => (),
                }
            }
        } else {
            break;
        }
    }
}

// Construct a <presence/>
fn make_presence(from: Jid, to: Jid) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(from);
    presence.to = Some(to);
    presence.show = Some(PresenceShow::Chat);
    presence
        .statuses
        .insert(Lang::from("en"), String::from("Echoing messages."));
    presence
}

// Construct a chat <message/>
fn make_reply(to: Jid, body: &str) -> Message {
    let mut message = Message::new(Some(to));
    message.bodies.insert(Lang::default(), body.to_owned());
    message
}
