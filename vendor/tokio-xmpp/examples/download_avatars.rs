use futures::stream::StreamExt;
use std::collections::BTreeSet;
use std::env::args;
use std::fs::{create_dir_all, File};
use std::io::{self, Write};
use std::process::exit;
use std::str::FromStr;
use tokio_xmpp::rustls;
use tokio_xmpp::{Client, Stanza};
use xmpp_parsers::{
    avatar::{Data as AvatarData, Metadata as AvatarMetadata},
    caps::{compute_disco, hash_caps, Caps},
    disco::{DiscoInfoQuery, DiscoInfoResult, Identity},
    hashes::Algo,
    iq::Iq,
    jid::{BareJid, Jid},
    ns,
    presence::{Presence, Type as PresenceType},
    pubsub::{
        self,
        pubsub::{Items, PubSub},
        NodeName,
    },
    stanza_error::{DefinedCondition, ErrorType, StanzaError},
};

#[tokio::main]
async fn main() {
    env_logger::init();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args: Vec<String> = args().collect();
    if args.len() != 3 {
        println!("Usage: {} <jid> <password>", args[0]);
        exit(1);
    }
    let jid = BareJid::from_str(&args[1]).expect(&format!("Invalid JID: {}", &args[1]));
    let password = args[2].clone();

    // Client instance
    let mut client = Client::new(jid.clone(), password);

    let disco_info = make_disco();

    // Main loop, processes events
    while let Some(event) = client.next().await {
        if event.is_online() {
            println!("Online!");

            let caps = get_disco_caps(&disco_info, "https://gitlab.com/xmpp-rs/tokio-xmpp");
            let presence = make_presence(caps);
            client.send_stanza(presence.into()).await.unwrap();
        } else if let Some(stanza) = event.into_stanza() {
            match stanza {
                Stanza::Iq(Iq::Get {
                    payload, id, from, ..
                }) => {
                    if payload.is("query", ns::DISCO_INFO) {
                        let query = DiscoInfoQuery::try_from(payload);
                        match query {
                            Ok(query) => {
                                let mut disco = disco_info.clone();
                                disco.node = query.node;
                                let iq = Iq::from_result(id, Some(disco)).with_to(from.unwrap());
                                client.send_stanza(iq.into()).await.unwrap();
                            }
                            Err(err) => {
                                client
                                    .send_stanza(
                                        make_error(
                                            from.unwrap(),
                                            id,
                                            ErrorType::Modify,
                                            DefinedCondition::BadRequest,
                                            &format!("{}", err),
                                        )
                                        .into(),
                                    )
                                    .await
                                    .unwrap();
                            }
                        }
                    } else {
                        // We MUST answer unhandled get iqs with a service-unavailable error.
                        client
                            .send_stanza(
                                make_error(
                                    from.unwrap(),
                                    id,
                                    ErrorType::Cancel,
                                    DefinedCondition::ServiceUnavailable,
                                    "No handler defined for this kind of iq.",
                                )
                                .into(),
                            )
                            .await
                            .unwrap();
                    }
                }
                Stanza::Iq(Iq::Result {
                    payload: Some(payload),
                    from,
                    ..
                }) => {
                    if payload.is("pubsub", ns::PUBSUB) {
                        let pubsub = PubSub::try_from(payload).unwrap();
                        let from = from.unwrap_or(jid.clone().into());
                        handle_iq_result(pubsub, &from);
                    }
                }
                Stanza::Iq(Iq::Set { from, id, .. }) => {
                    // We MUST answer unhandled set iqs with a service-unavailable error.
                    client
                        .send_stanza(
                            make_error(
                                from.unwrap(),
                                id,
                                ErrorType::Cancel,
                                DefinedCondition::ServiceUnavailable,
                                "No handler defined for this kind of iq.",
                            )
                            .into(),
                        )
                        .await
                        .unwrap();
                }
                Stanza::Iq(Iq::Error { .. }) | Stanza::Iq(Iq::Result { payload: None, .. }) => (),
                Stanza::Message(message) => {
                    let from = message.from.clone().unwrap();
                    if let Some(body) = message.get_best_body(vec!["en"]) {
                        if body.0 == "die" {
                            println!("Secret die command triggered by {}", from);
                            break;
                        }
                    }
                    for child in message.payloads {
                        if child.is("event", ns::PUBSUB_EVENT) {
                            let event = pubsub::Event::try_from(child).unwrap();
                            if let pubsub::event::Payload::Items {
                                node,
                                published,
                                retracted: _,
                            } = event.payload
                            {
                                if node.0 == ns::AVATAR_METADATA {
                                    for item in published.into_iter() {
                                        let payload = item.payload.clone().unwrap();
                                        if payload.is("metadata", ns::AVATAR_METADATA) {
                                            // TODO: do something with these metadata.
                                            let _metadata =
                                                AvatarMetadata::try_from(payload).unwrap();
                                            println!(
                                                "[1m{}[0m has published an avatar, downloading...",
                                                from.clone()
                                            );
                                            let iq = download_avatar(from.clone());
                                            client.send_stanza(iq.into()).await.unwrap();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Nothing to do here.
                Stanza::Presence(_) => (),
            }
        }
    }
}

fn make_error(
    to: Jid,
    id: String,
    type_: ErrorType,
    condition: DefinedCondition,
    text: &str,
) -> Iq {
    let error = StanzaError::new(type_, condition, "en", text);
    Iq::from_error(id, error).with_to(to)
}

fn make_disco() -> DiscoInfoResult {
    let identities = vec![Identity::new("client", "bot", "en", "tokio-xmpp")];
    let features = BTreeSet::from([
        String::from(ns::DISCO_INFO),
        format!("{}+notify", ns::AVATAR_METADATA),
    ]);
    DiscoInfoResult {
        node: None,
        identities,
        features,
        extensions: vec![],
    }
}

fn get_disco_caps(disco: &DiscoInfoResult, node: &str) -> Caps {
    let caps_data = compute_disco(disco);
    let hash = hash_caps(&caps_data, Algo::Sha_1).unwrap();
    Caps::new(node, hash)
}

// Construct a <presence/>
fn make_presence(caps: Caps) -> Presence {
    let mut presence = Presence::new(PresenceType::None).with_priority(-1);
    presence.set_status("en", "Downloading avatars.");
    presence.add_payload(caps);
    presence
}

fn download_avatar(from: Jid) -> Iq {
    Iq::from_get(
        "coucou",
        PubSub::Items(Items {
            max_items: None,
            node: NodeName(String::from(ns::AVATAR_DATA)),
            subid: None,
            items: Vec::new(),
        }),
    )
    .with_to(from)
}

fn handle_iq_result(pubsub: PubSub, from: &Jid) {
    if let PubSub::Items(items) = pubsub {
        if items.node.0 == ns::AVATAR_DATA {
            for item in items.items {
                match (item.id.clone(), item.payload.clone()) {
                    (Some(id), Some(payload)) => {
                        let data = AvatarData::try_from(payload).unwrap();
                        save_avatar(from, id.0, &data.data).unwrap();
                    }
                    _ => {}
                }
            }
        }
    }
}

// TODO: may use tokio?
fn save_avatar(from: &Jid, id: String, data: &[u8]) -> io::Result<()> {
    let directory = format!("data/{}", from);
    let filename = format!("data/{}/{}", from, id);
    println!(
        "Saving avatar from [1m{}[0m to [4m{}[0m.",
        from, filename
    );
    create_dir_all(directory)?;
    let mut file = File::create(filename)?;
    file.write_all(data)
}
