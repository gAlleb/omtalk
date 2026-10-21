use std::time::Duration;

use xmpp_parsers::jid::BareJid;

#[derive(Clone, Debug)]
pub struct AccountConfig {
    pub jid: BareJid,
    pub password: String,
    /// Kept per device, so the server and other clients see the same resource across
    /// restarts. [`AccountConfig::new`] picks a random one.
    pub resource: String,
    pub server: Server,
    /// How long to wait for the answer to an IQ request. A peer that never answers must
    /// not stall whatever is waiting on it.
    pub iq_timeout: Duration,
    /// After this long without a byte from the server, ask it whether it is still there.
    /// A connection can die without either side noticing (sleep, a router dropping it,
    /// a server that parked the session); only asking finds out.
    pub keepalive: Duration,
    /// How long the server has to answer that question before the connection counts
    /// as dead and is replaced.
    pub keepalive_answer: Duration,
}

impl AccountConfig {
    pub fn new(jid: BareJid, password: impl Into<String>) -> Self {
        let suffix: [u8; 4] = random();
        let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            jid,
            password: password.into(),
            resource: format!("omtalk.{suffix}"),
            server: Server::Auto,
            iq_timeout: Duration::from_secs(30),
            keepalive: Duration::from_secs(60),
            keepalive_answer: Duration::from_secs(30),
        }
    }
}

impl AccountConfig {
    pub(crate) fn timeouts(&self) -> tokio_xmpp::xmlstream::Timeouts {
        tokio_xmpp::xmlstream::Timeouts {
            read_timeout: self.keepalive,
            response_timeout: self.keepalive_answer,
        }
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("system randomness");
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Server {
    /// The domain's SRV records, STARTTLS.
    Auto,
    /// STARTTLS to a fixed host.
    StartTls { host: String, port: u16 },
    /// TLS from the first byte (XEP-0368) to a fixed host.
    DirectTls { host: String, port: u16 },
    /// Unencrypted TCP to `addr` (`host:port`). Only for the local test server.
    #[cfg(feature = "insecure-tcp")]
    Plaintext { addr: String },
}
