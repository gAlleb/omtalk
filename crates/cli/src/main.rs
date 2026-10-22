//! omtalk-cli: a line-based OMEMO chat client, for checking omtalk against Gajim,
//! Monal, Conversations and Dino on a real server before the TUI exists.
//!
//!     omtalk-cli you@example.org [--db PATH] [--host HOST [--port PORT] | --direct-tls HOST:PORT]
//!
//! The password comes from `OMTALK_PASSWORD`, or is asked for. Type `/help` once running.

mod commands;
mod show;

use std::path::PathBuf;
use std::sync::Arc;

use omemo::OwnKeys;
use omtalk_omemo::{Omemo, State};
use omtalk_store::Store;
use omtalk_xmpp::{AccountConfig, Accounts, BareJid, Event, Server};
use tokio::io::{AsyncBufReadExt, BufReader};

struct Options {
    jid: BareJid,
    db: PathBuf,
    server: Server,
}

fn usage() -> ! {
    eprintln!(
        "usage: omtalk-cli JID [--db PATH] [--host HOST [--port PORT] | --direct-tls HOST:PORT{}]",
        if cfg!(feature = "insecure-tcp") {
            " | --plaintext HOST:PORT"
        } else {
            ""
        }
    );
    std::process::exit(2)
}

fn options() -> Options {
    let mut args = std::env::args().skip(1);
    let Some(jid) = args.next().and_then(|j| j.parse().ok()) else {
        usage()
    };
    let mut db = data_dir().join("omtalk.db");
    let mut server = Server::Auto;
    let mut host = None;
    let mut port = 5222;
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--db" => db = value().into(),
            "--host" => host = Some(value()),
            "--port" => port = value().parse().unwrap_or_else(|_| usage()),
            "--direct-tls" => {
                let (h, p) = split_addr(&value());
                server = Server::DirectTls { host: h, port: p };
            }
            #[cfg(feature = "insecure-tcp")]
            "--plaintext" => server = Server::Plaintext { addr: value() },
            _ => usage(),
        }
    }
    if let Some(host) = host {
        server = Server::StartTls { host, port };
    }
    Options { jid, db, server }
}

fn split_addr(addr: &str) -> (String, u16) {
    let (host, port) = addr.rsplit_once(':').unwrap_or_else(|| usage());
    (host.to_owned(), port.parse().unwrap_or_else(|_| usage()))
}

/// `$XDG_DATA_HOME/omtalk`, or `~/.local/share/omtalk`.
fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("omtalk")
}

fn password() -> String {
    std::env::var("OMTALK_PASSWORD")
        .ok()
        .unwrap_or_else(|| rpassword::prompt_password("password: ").unwrap_or_else(|_| std::process::exit(1)))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let options = options();
    let store = Store::open(&options.db).unwrap_or_else(|e| {
        eprintln!("cannot open {}: {e}", options.db.display());
        std::process::exit(1)
    });
    let state = match store.load_omemo(&options.jid) {
        Ok(Some(state)) => state,
        Ok(None) => State::new(OwnKeys::generate(now()).expect("system randomness")),
        Err(e) => {
            eprintln!("cannot read the OMEMO state: {e}");
            std::process::exit(1)
        }
    };

    let mut config = AccountConfig::new(options.jid.clone(), password());
    config.server = options.server;
    let (mut accounts, mut events) = Accounts::new();
    let account = accounts.add(config).await;
    let omemo = Arc::new(Omemo::persisting(
        account.clone(),
        state,
        store.omemo(&options.jid),
    ));
    show::info(&format!(
        "this device: {} {}",
        omemo.device_id(),
        omtalk_omemo::fingerprint(&omemo.identity_key().await)
    ));

    // Everything from the server: connection state, messages, device lists.
    let driver = omemo.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match &event.event {
                Event::Online { jid, resumed } => {
                    show::info(&format!(
                        "online as {jid}{}",
                        if *resumed { " (resumed)" } else { "" }
                    ));
                    if !resumed {
                        if let Err(e) = driver.announce().await {
                            show::error(&format!("announcing this device failed: {e}"));
                        }
                        show::caught_up("own archive", driver.catch_up().await);
                    }
                }
                Event::Disconnected { reason } => {
                    show::info(&format!("disconnected: {reason}; reconnecting"))
                }
                Event::AuthFailed { reason } => {
                    show::error(&format!("login refused: {reason}"));
                    std::process::exit(1);
                }
                _ => {}
            }
            match driver.handle(&event.event).await {
                Ok(Some(incoming)) => show::incoming(&incoming),
                Ok(None) => {}
                Err(e) => show::error(&format!("{e}")),
            }
        }
    });

    show::info("type /help for commands");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match commands::parse(&line) {
            Ok(commands::Command::Quit) => break,
            Ok(command) => commands::run(&omemo, &account, command).await,
            Err(problem) => show::error(&problem),
        }
    }
    account.stop().await;
    drop(accounts);
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
