// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::{pin::Pin, task::Context};
use futures::{ready, task::Poll, Stream};

use crate::{client::Client, Event};

/// Incoming XMPP events
///
/// In an `async fn` you may want to use this with `use
/// futures::stream::StreamExt;`
impl Stream for Client {
    type Item = Event;

    /// Low-level read on the XMPP stream, allowing the underlying
    /// machinery to:
    ///
    /// * connect,
    /// * starttls,
    /// * authenticate,
    /// * bind a session, and finally
    /// * receive stanzas
    ///
    /// ...for your client
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        Poll::Ready(match ready!(self.stanza_rx.poll_recv(cx)) {
            None => None,
            Some(event) => {
                if let Event::Online {
                    ref bound_jid,
                    ref features,
                    ..
                } = event
                {
                    // This unwrap() will never fail because the server MUST send us a full JID.
                    self.bound_jid = Some(bound_jid.try_as_full().unwrap().clone());
                    self.features = Some(features.clone());
                }

                Some(event)
            }
        })
    }
}
