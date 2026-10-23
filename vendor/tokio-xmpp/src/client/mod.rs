// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::client::{receiver::ClientReceiver, sender::ClientSender};
use crate::connect::ServerConnector;
use crate::error::Error;
use crate::event::{ensure_stanza_id, Event};
use crate::stanzastream::{self, StanzaStage, StanzaState, StanzaStream, StanzaToken};
use crate::xmlstream::Timeouts;
use crate::Stanza;
use std::io;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use xmpp_parsers::{
    jid::{FullJid, Jid},
    stream_features::StreamFeatures,
};

#[cfg(feature = "direct-tls")]
use crate::connect::DirectTlsServerConnector;
#[cfg(any(feature = "direct-tls", feature = "starttls", feature = "insecure-tcp"))]
use crate::connect::DnsConfig;
#[cfg(feature = "starttls")]
use crate::connect::StartTlsServerConnector;
#[cfg(feature = "insecure-tcp")]
use crate::connect::TcpServerConnector;

mod iq;
pub(crate) mod login;
pub(crate) mod receiver;
pub(crate) mod sender;
mod stream;
mod worker;

pub use iq::{IqFailure, IqRequest, IqResponse, IqResponseToken};
pub use login::auth;

/// XMPP client connection and state
///
/// This implements the `futures` crate's [`Stream`](#impl-Stream) to receive
/// stream state changes as well as stanzas received via the stream.
///
/// To send stanzas, the [`send_stanza`][`Client::send_stanza`] method can be
/// used.
#[derive(Debug)]
pub struct Client {
    // Stanza receiver from the client worker
    stanza_rx: mpsc::Receiver<Event>,
    // Stanza sender to the StanzaStream
    stream_tx: stanzastream::StanzaSender,
    // Shutdown handle for the client worker
    shutdown_tx: oneshot::Sender<()>,
    // Client worker task
    worker: JoinHandle<stanzastream::StanzaReceiver>,
    // JID of the logged-in client
    bound_jid: Option<FullJid>,
    // Stream features of the currently connected stream
    features: Option<StreamFeatures>,
    // Response tracker for IQs
    iq_response_tracker: iq::IqResponseTracker,
}

impl Client {
    /// Get the client's bound JID (the one reported by the XMPP
    /// server).
    pub fn bound_jid(&self) -> Option<&FullJid> {
        self.bound_jid.as_ref()
    }

    /// Send a stanza.
    ///
    /// This will automatically allocate an ID if the stanza has no ID set.
    /// The returned `StanzaToken` is awaited up to the [`StanzaStage::Sent`]
    /// stage, which means that this coroutine only returns once the stanza
    /// has actually been written to the XMPP transport.
    ///
    /// Note that this does not imply that it has been *reeceived* by the
    /// peer, nor that it has been successfully processed. To confirm that a
    /// stanza has been received by a peer, the [`StanzaToken::wait_for`]
    /// method can be called with [`StanzaStage::Acked`], but that stage will
    /// only ever be reached if the server supports XEP-0198 and it has been
    /// negotiated successfully (this may change in the future).
    ///
    /// For sending Iq request stanzas, it is recommended to use
    /// [`send_iq`][`Self::send_iq`], which allows awaiting the response.
    pub async fn send_stanza(&mut self, mut stanza: Stanza) -> Result<StanzaToken, io::Error> {
        ensure_stanza_id(&mut stanza);
        let mut token = self.stream_tx.send(Box::new(stanza)).await;

        match token.wait_for(StanzaStage::Sent).await {
            // Queued < Sent, so it cannot be reached.
            Some(StanzaState::Queued) => unreachable!(),

            None | Some(StanzaState::Dropped) => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "stream disconnected fatally before stanza could be sent",
            )),
            Some(StanzaState::Failed { error }) => Err(error.into_io_error()),
            Some(StanzaState::Sent { .. }) | Some(StanzaState::Acked { .. }) => Ok(token),
        }
    }

    /// Send an IQ request and return a token to retrieve the response.
    ///
    /// This coroutine method will complete once the Iq has been sent to the
    /// server. The returned `IqResponseToken` can be used to await the
    /// response. See also the documentation of [`IqResponseToken`] for more
    /// information on the behaviour of these tokens.
    ///
    /// **Note**: If an IQ response arrives after the `token` has been
    /// dropped (e.g. due to a timeout), it will be delivered through the
    /// `Stream` like any other stanza.
    pub async fn send_iq(&mut self, to: Option<Jid>, req: IqRequest) -> IqResponseToken {
        let (iq, mut token) = self.iq_response_tracker.allocate_iq_handle(
            // from is always None for a client
            None, to, req,
        );
        let stanza_token = self.stream_tx.send(Box::new(iq.into())).await;

        token.set_stanza_token(stanza_token);
        token
    }

    /// Get the stream features (`<stream:features/>`) of the underlying
    /// stream.
    ///
    /// If the stream has not completed negotiation yet, this will return
    /// `None`. Note that stream features may change at any point due to a
    /// transparent reconnect.
    pub fn get_stream_features(&self) -> Option<&StreamFeatures> {
        self.features.as_ref()
    }

    /// Close the client cleanly.
    ///
    /// This performs an orderly stream shutdown, ensuring that all resources
    /// are correctly cleaned up.
    pub async fn send_end(self) -> Result<(), Error> {
        self.shutdown_tx.send(()).expect("ClientWorker crashed.");

        let stream_rx = self.worker.await.unwrap();
        let stream = StanzaStream::reunite(self.stream_tx, stream_rx);
        stream.close().await;

        Ok(())
    }

    /// Split the client into [`ClientSender`] and [`ClientReceiver`].
    pub fn split(self) -> (ClientSender, ClientReceiver) {
        let client = Arc::new(Mutex::new(self));

        let sender = ClientSender(client.clone());
        let receiver = ClientReceiver(client);

        (sender, receiver)
    }

    /// Reunite a [`ClientSender`] and [`ClientReceiver`].
    ///
    /// # Panics
    ///
    /// This functions returns an error if the [`ClientSender`] and
    /// [`ClientReceiver`] don't come from the same [`Client`].
    pub fn reunite(sender: ClientSender, receiver: ClientReceiver) -> Self {
        assert!(
            Arc::ptr_eq(&sender.0, &receiver.0),
            "Unrelated ClientSender and ClientReceiver passed to reunite."
        );

        drop(sender);

        let inner = Arc::try_unwrap(receiver.0).expect("Failed to unwrap ClientReceiver Arc");
        inner.into_inner()
    }
}

#[cfg(feature = "direct-tls")]
impl Client {
    /// Start a new XMPP client using DirectTLS transport and autoreconnect
    ///
    /// It use RFC 7590 _xmpps-client._tcp lookup for connector details.
    pub fn new_direct_tls<J: Into<Jid>, P: Into<String>>(jid: J, password: P) -> Self {
        let jid_ref = jid.into();
        let dns_config = DnsConfig::srv_xmpps(jid_ref.domain().as_ref());
        Self::new_with_connector(
            jid_ref,
            password,
            DirectTlsServerConnector::from(dns_config),
            Timeouts::default(),
        )
    }

    /// Start a new XMPP client with direct TLS transport, useful for testing or
    /// when one does not want to rely on dns lookups
    pub fn new_direct_tls_with_config<J: Into<Jid>, P: Into<String>>(
        jid: J,
        password: P,
        dns_config: DnsConfig,
        timeouts: Timeouts,
    ) -> Self {
        Self::new_with_connector(
            jid,
            password,
            DirectTlsServerConnector::from(dns_config),
            timeouts,
        )
    }
}

#[cfg(feature = "starttls")]
impl Client {
    /// Start a new XMPP client using StartTLS transport and autoreconnect
    ///
    /// Start polling the returned instance so that it will connect
    /// and yield events.
    pub fn new<J: Into<Jid>, P: Into<String>>(jid: J, password: P) -> Self {
        let jid = jid.into();
        let dns_config = DnsConfig::srv_default_client(jid.domain().as_ref());
        Self::new_starttls(jid, password, dns_config, Timeouts::default())
    }

    /// Start a new XMPP client with StartTLS transport and specific DNS config
    pub fn new_starttls<J: Into<Jid>, P: Into<String>>(
        jid: J,
        password: P,
        dns_config: DnsConfig,
        timeouts: Timeouts,
    ) -> Self {
        Self::new_with_connector(
            jid,
            password,
            StartTlsServerConnector::from(dns_config),
            timeouts,
        )
    }
}

#[cfg(feature = "insecure-tcp")]
impl Client {
    /// Start a new XMPP client with plaintext insecure connection and specific DNS config
    pub fn new_plaintext<J: Into<Jid>, P: Into<String>>(
        jid: J,
        password: P,
        dns_config: DnsConfig,
        timeouts: Timeouts,
    ) -> Self {
        Self::new_with_connector(
            jid,
            password,
            TcpServerConnector::from(dns_config),
            timeouts,
        )
    }
}

impl Client {
    /// Start a new client given that the JID is already parsed.
    pub fn new_with_connector<J: Into<Jid>, P: Into<String>, C: ServerConnector>(
        jid: J,
        password: P,
        connector: C,
        timeouts: Timeouts,
    ) -> Self {
        let stream = StanzaStream::new_c2s(connector, jid.into(), password.into(), timeouts, 16);
        let (stream_tx, stream_rx) = stream.split();

        let iq_response_tracker = iq::IqResponseTracker::new();
        let (worker, shutdown_tx, stanza_rx) =
            worker::ClientWorker::new(stream_rx, iq_response_tracker.clone(), 16);

        let worker = tokio::task::spawn(async move { worker.run().await });

        Self {
            stream_tx,
            stanza_rx,
            worker,
            shutdown_tx,
            iq_response_tracker,
            bound_jid: None,
            features: None,
        }
    }
}
