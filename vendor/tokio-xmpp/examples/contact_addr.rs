use futures::stream::StreamExt;
use std::env::args;
use std::process::exit;
use std::str::FromStr;
use tokio_xmpp::rustls;
use tokio_xmpp::{Client, IqRequest, IqResponse};
use xmpp_parsers::{
    disco::{DiscoInfoQuery, DiscoInfoResult},
    jid::{BareJid, Jid},
    ns,
    server_info::ServerInfo,
};

#[tokio::main]
async fn main() {
    env_logger::init();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args: Vec<String> = args().collect();
    if args.len() != 4 {
        println!("Usage: {} <jid> <password> <target>", args[0]);
        exit(1);
    }
    let jid = BareJid::from_str(&args[1]).expect(&format!("Invalid JID: {}", &args[1]));
    let password = args[2].clone();
    let target = Jid::from_str(&args[3]).expect(&format!("Invalid JID: {}", &args[3]));

    // Client instance
    let mut client = Client::new(jid, password);

    let mut token = client
        .send_iq(
            Some(target),
            IqRequest::Get(DiscoInfoQuery { node: None }.into()),
        )
        .await;

    // Main loop, processes events
    loop {
        tokio::select! {
            response = &mut token => match response {
                Ok(IqResponse::Result(Some(payload))) => {
                    if payload.is("query", ns::DISCO_INFO) {
                        if let Ok(disco_info) = DiscoInfoResult::try_from(payload) {
                            for ext in disco_info.extensions {
                                if let Ok(server_info) = ServerInfo::try_from(ext) {
                                    print_server_info(server_info);
                                }
                            }
                        }
                    }
                    break;
                }
                Ok(IqResponse::Result(None)) => {
                    panic!("disco#info response misses payload!");
                }
                Ok(IqResponse::Error(err)) => {
                    panic!("disco#info response is an error: {:?}", err);
                }
                Err(err) => {
                    panic!("disco#info request failed to send: {}", err);
                }
            },
            event = client.next() => {
                let Some(event) = event else {
                    println!("Client terminated");
                    break;
                };
                if event.is_online() {
                    println!("Online!");
                }
            },
        }
    }
    client.send_end().await.expect("Stream shutdown unclean");
}

fn convert_field(field: Vec<String>) -> String {
    field
        .iter()
        .fold((field.len(), String::new()), |(l, mut acc), s| {
            acc.push('<');
            acc.push_str(&s);
            acc.push('>');
            if l > 1 {
                acc.push(',');
                acc.push(' ');
            }
            (0, acc)
        })
        .1
}

fn print_server_info(server_info: ServerInfo) {
    if server_info.abuse.len() != 0 {
        println!("abuse: {}", convert_field(server_info.abuse));
    }
    if server_info.admin.len() != 0 {
        println!("admin: {}", convert_field(server_info.admin));
    }
    if server_info.feedback.len() != 0 {
        println!("feedback: {}", convert_field(server_info.feedback));
    }
    if server_info.sales.len() != 0 {
        println!("sales: {}", convert_field(server_info.sales));
    }
    if server_info.security.len() != 0 {
        println!("security: {}", convert_field(server_info.security));
    }
    if server_info.support.len() != 0 {
        println!("support: {}", convert_field(server_info.support));
    }
}
