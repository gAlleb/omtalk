//! Low-level [XMPP](https://xmpp.org/) decentralized instant messaging & social networking implementation with asynchronous I/O using [tokio](https://tokio.rs/).
//!
//! For an easier, batteries-included experience, try the [xmpp crate](https://docs.rs/xmpp).
//!
//! # Getting started
//!
//! In most cases, you want to start with a [`Client`], that will connect to a server over TCP/IP with StartTLS encryption. Then, you can build an event loop by calling the client's `next` method repeatedly. You can find a more complete example in the [examples/echo_bot.rs](https://gitlab.com/xmpp-rs/xmpp-rs/-/blob/main/tokio-xmpp/examples/echo_bot.rs) file in the repository.
//!
//! # Features
//!
//! This library is not feature-complete yet. Here's a quick overview of the feature set.
//!
//! Supported implementations:
//! - [x] Clients
//! - [x] Components
//! - [ ] Servers
//!
//! Supported transports:
//! - [x] Plaintext TCP (IPv4/IPv6)
//! - [x] StartTLS TCP (IPv4/IPv6 with [happy eyeballs](https://en.wikipedia.org/wiki/Happy_Eyeballs) support)
//! - [x] Custom connectors via the [`connect::ServerConnector`] trait
//! - [ ] Websockets
//! - [ ] BOSH
//!
//! # Cargo features
//!
//! ## TLS backends
//!
//! - `aws_lc_rs` (default) enables rustls with the `aws_lc_rs` backend.
//! - `ring` enables rustls with the `ring` backend`.
//! - `rustls-any-backend` enables rustls, but without enabling a backend. It
//!   is the application's responsibility to ensure that a backend is enabled
//!   and installed.
//! - `ktls` enables the use of ktls.
//!   **Important:** Currently, connections will fail if the `tls` kernel
//!   module is not available. There is no fallback to non-ktls connections!
//! - `native-tls` enables the system-native TLS library (commonly
//!   libssl/OpenSSL).
//!
//! **Note:** It is not allowed to mix rustls-based TLS backends with
//! `tls-native`. Attempting to do so will result in a compilation error.
//!
//! **Note:** The `ktls` feature requires at least one `rustls` backend to be
//! enabled (`aws_lc_rs` or `ring`).
//!
//! **Note:** When enabling not exactly one rustls backend, it is the
//! application's responsibility to make sure that a default crypto provider is
//! installed in `rustls`. Otherwise, all TLS connections will fail.
//!
//! ## Certificate validation
//!
//! When using `native-tls`, the system's native certificate store is used.
//! Otherwise, you need to pick one of the following to ensure that TLS
//! connections will succeed:
//!
//! - `rustls-native-certs` (default): Uses [rustls-native-certs](https://crates.io/crates/rustls-native-certs).
//! - `webpki-roots`: Uses [webpki-roots](https://crates.io/crates/webpki-roots).
//!
//! ## Other features
//!
//! - `starttls` (default): Enables support for `<starttls/>`. Required as per
//!   RFC 6120.
//! - `insecure-tcp`: Allow the use of insecure TCP connections to connect to
//!   XMPP servers. Required for XMPP components, but disabled by default to
//!   prevent accidental use.
//! - `serde`: Enable the `serde` feature in `xmpp-parsers`.
//! - `component`: Enable component support (implies `insecure-tcp`).
//!
//! # More information
//!
//! You can find more information on our website [xmpp.rs](https://xmpp.rs/) or by joining our chatroom [chat@xmpp.rs](xmpp:chat@xmpp.rs?join).

#![deny(unsafe_code, missing_docs, bare_trait_objects)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, doc(auto_cfg))]

macro_rules! fail_native_with_any {
    ($($feature:literal),+) => {
        $(
            #[cfg(all(
                not(xmpprs_doc_build),
                not(doc),
                feature = "native-tls",
                feature = $feature,
            ))]
            compile_error!(
                concat!(
                    "native-tls cannot be mixed with the ",
                    $feature,
                    " feature. Pick one or the other."
                )
            );
        )+
    }
}

fail_native_with_any!("ring", "aws_lc_rs", "ktls", "rustls-any-backend");

#[cfg(all(
    feature = "starttls",
    not(feature = "native-tls"),
    not(any(feature = "rustls-any-backend"))
))]
compile_error!(
    "When the starttls feature is enabled, either native-tls or any of the rustls (aws_lc_rs, ring, or rustls-any-backend) features must be enabled."
);

extern crate alloc;

pub use parsers::{jid, minidom};
pub use xmpp_parsers as parsers;

#[cfg(any(feature = "ring", feature = "aws_lc_rs", feature = "ktls"))]
pub use tokio_rustls::rustls;

mod client;
#[cfg(feature = "insecure-tcp")]
mod component;
pub mod connect;
/// Detailed error types
pub mod error;
mod event;
pub mod stanzastream;
pub mod xmlstream;

pub use xso::{asxml::PrintRawXml, error::FromElementError};

#[doc(inline)]
/// Generic tokio_xmpp Error
pub use crate::error::Error;
pub use client::{
    auth as client_login, receiver::ClientReceiver, sender::ClientSender, Client, IqFailure,
    IqRequest, IqResponse, IqResponseToken,
};

#[cfg(feature = "insecure-tcp")]
pub use component::Component;
pub use event::Event;

/// Deprecated reexport of Stanza from xmpp-parsers.
pub use xmpp_parsers::stanza::Stanza;

#[cfg(test)]
mod tests {
    #[test]
    fn reexports() {
        #[allow(unused_imports)]
        use crate::jid;
        #[allow(unused_imports)]
        use crate::minidom;
        #[allow(unused_imports)]
        use crate::parsers;
    }
}
