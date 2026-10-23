// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Keep a connection alive
//!
//! This example demonstrates that tokio_xmpp will keep a connection alive
//! as good as it can, transparently reconnecting on interruptions of the TCP
//! stream.

use core::str::FromStr;
use core::time::Duration;
use std::env::args;
use std::process::exit;

use futures::StreamExt;

#[cfg(feature = "rustls-any-backend")]
use tokio_xmpp::rustls;
use tokio_xmpp::{
    connect::{DnsConfig, StartTlsServerConnector},
    parsers::{
        iq::Iq,
        jid::{BareJid, Jid},
        ping,
    },
    stanzastream::StanzaStream,
    xmlstream::Timeouts,
};

#[cfg(all(
    feature = "rustls-any-backend",
    not(any(feature = "aws_lc_rs", feature = "ring"))
))]
compile_error!("using rustls (e.g. via the ktls feature) needs an enabled rustls backend feature (either aws_lc_rs or ring).");

#[tokio::main]
async fn main() {
    env_logger::init();

    #[cfg(all(feature = "aws_lc_rs", not(feature = "ring")))]
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    #[cfg(all(feature = "ring"))]
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args: Vec<String> = args().collect();
    if args.len() != 3 {
        println!("Usage: {} <jid> <password>", args[0]);
        exit(1);
    }
    let jid = BareJid::from_str(&args[1]).expect(&format!("Invalid JID: {}", &args[1]));
    let password = &args[2];

    let mut timeouts = Timeouts::tight();
    timeouts.read_timeout = Duration::new(5, 0);

    let mut stream = StanzaStream::new_c2s(
        StartTlsServerConnector::from(DnsConfig::UseSrv {
            host: jid.domain().as_str().to_owned(),
            srv: "_xmpp-client._tcp".to_owned(),
            fallback_port: 5222,
            resolver: None,
        }),
        jid.clone().into(),
        password.clone(),
        timeouts,
        16,
    );
    let domain: Jid = jid.domain().to_owned().into();
    let mut ping_timer = tokio::time::interval(Duration::new(5, 0));
    let mut ping_ctr: u64 = rand::random();
    let signal = tokio::signal::ctrl_c();
    tokio::pin!(signal);
    loop {
        tokio::select! {
            _ = &mut signal => {
                log::info!("Ctrl+C pressed, shutting down cleanly.");
                break;
            }
            _ = ping_timer.tick() => {
                log::info!("sending ping for fun & profit");
                ping_ctr = ping_ctr.wrapping_add(1);
                let iq = Iq::from_get(format!("ping-{}", ping_ctr), ping::Ping).with_to(domain.clone());
                stream.send(Box::new(iq.into())).await;
            }
            ev = stream.next() => match ev {
                Some(ev) => {
                    log::info!("{:?}", ev);
                }
                None => {
                    panic!("stream terminated unexpectedly!");
                }
            }
        }
    }
    stream.close().await;
}
