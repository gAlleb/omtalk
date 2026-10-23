use std::borrow::Cow;
use std::io;

use futures::StreamExt;

use xmpp_parsers::{
    bind::{BindFeature, BindQuery, BindResponse},
    iq::Iq,
    message::Message,
    presence::{Presence, Show, Type as PresenceType},
    sm,
};

use crate::jid::{BareJid, ResourcePart};
use crate::minidom::Element;
use crate::xmlstream::{
    accept_stream, initiate_stream, FallibleStreamElement, ReadError, RecvFeaturesError,
    StreamHeader, Timeouts, XmppStreamElement,
};

use super::*;

static STREAM_ID: &str = "stream-id";
static STREAM_MANAGEMENT_ID: &str = "sm-id";

fn map_eof<T>(v: Option<Result<T, ReadError>>) -> Result<T, ReadError> {
    match v {
        Some(v) => v,
        None => Err(ReadError::HardError(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "eof while receiving stream element",
        ))),
    }
}

fn map_io<T>(v: Option<Result<T, ReadError>>) -> Result<T, io::Error> {
    match map_eof(v) {
        Ok(v) => Ok(v),
        Err(e) => match e {
            ReadError::SoftTimeout => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "soft timeout tripped",
            )),
            ReadError::HardError(e) => Err(e),
            ReadError::ParseError(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            ReadError::StreamFooterReceived => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected stream footer",
            )),
        },
    }
}

fn map_err(
    opt: Option<Result<FallibleStreamElement, ReadError>>,
) -> Option<Result<XmppStreamElement, ReadError>> {
    opt.map(|res| res.map(|v| v.into_read_error()).flatten())
}

async fn custom_stream_pair(
    identity: BareJid,
    mut features: StreamFeatures,
) -> io::Result<(StanzaStream, XmppStream)> {
    // We must always offer the resource binding feature.
    features
        .bind
        .get_or_insert_with(|| BindFeature { required: false });

    let (lhs, rhs) = tokio::io::duplex(8192);
    let lhs = tokio::io::BufReader::new(lhs);
    let rhs = tokio::io::BufReader::new(rhs);

    let lhs_identity = identity.clone();
    let rhs_identity = identity.clone();

    let lhs_task = tokio::spawn(async move {
        let lhs = initiate_stream(
            lhs,
            "jabber:client",
            StreamHeader {
                from: Some(Cow::Borrowed(lhs_identity.as_str())),
                to: Some(Cow::Borrowed(lhs_identity.domain().as_str())),
                id: Some(Cow::Borrowed(STREAM_ID)),
            },
            Timeouts::default(),
        )
        .await?;
        lhs.recv_features().await.map_err(|e| match e {
            RecvFeaturesError::StreamError(e) => io::Error::new(io::ErrorKind::Other, e),
            RecvFeaturesError::Io(e) => e,
        })
    });
    let rhs_task = tokio::spawn(async move {
        let rhs = accept_stream(rhs, "jabber:client", Timeouts::default()).await?;
        let rhs = rhs
            .send_header(StreamHeader {
                to: Some(Cow::Borrowed(rhs_identity.as_str())),
                from: Some(Cow::Borrowed(rhs_identity.domain().as_str())),
                id: Some(Cow::Borrowed(STREAM_ID)),
            })
            .await?;
        rhs.send_features(&features).await
    });

    let lhs = lhs_task.await.unwrap()?;
    let mut server = rhs_task.await.unwrap()?.box_stream();

    let mut lhs = Some((lhs, identity.clone()));
    let client = StanzaStream::new(
        Box::new(move |_, sink| match lhs.take() {
            Some(((features, stream), identity)) => match sink.send(Connection {
                features,
                stream: stream.box_stream(),
                identity: identity.into(),
            }) {
                Ok(()) => (),
                Err(_) => {
                    panic!("stanza stream crashed while test suite was preparing the stream!")
                }
            },
            None => panic!(
                "reconnection attempt, but reconnection logic is not available in this test suite"
            ),
        }),
        16,
    );

    match map_io(map_err(server.next().await))? {
        XmppStreamElement::Stanza(stanza) => match stanza {
            Stanza::Iq(Iq::Set {
                from,
                to,
                id,
                payload,
            }) => {
                let bind: BindQuery = payload.try_into().expect("failed to parse BindQuery");
                let identity = if let Some(resource) = bind.resource {
                    identity.with_resource(&*ResourcePart::new(&resource).unwrap())
                } else {
                    identity.with_resource(&*ResourcePart::new("foobar").unwrap())
                };
                let response = BindResponse { jid: identity };
                let response = Iq::Result {
                    from: to,
                    to: from,
                    id: id,
                    payload: Some(response.into()),
                };
                server.send(&response).await?;
            }
            v => panic!("unexpected stanza: {v:?}"),
        },
        v => panic!("unexpected stream element: {v:?}"),
    };

    Ok((client, server))
}

async fn plain_pair() -> io::Result<(StanzaStream, XmppStream)> {
    let (mut client, server) = custom_stream_pair(
        "client@domain.example".parse().unwrap(),
        StreamFeatures::default(),
    )
    .await?;
    match client.next().await {
        Some(Event::Stream(StreamEvent::Reset { .. })) => (),
        other => panic!(
            "unexpected recv result on client side (immediately after stream setup): {other:?}"
        ),
    }
    Ok((client, server))
}

async fn fresh_sm_pair() -> io::Result<(StanzaStream, XmppStream)> {
    let mut features = StreamFeatures::default();
    features.stream_management = Some(sm::StreamManagement { optional: false });
    let (mut client, mut server) =
        custom_stream_pair("client@domain.example".parse().unwrap(), features).await?;
    match map_io(map_err(server.next().await))? {
        XmppStreamElement::SM(sm) => match sm {
            sm::Nonza::Enable(sm::Enable { max, resume }) => {
                server
                    .send(&sm::Enabled {
                        id: Some(sm::StreamId(STREAM_MANAGEMENT_ID.to_owned())),
                        location: None,
                        max,
                        resume,
                    })
                    .await?;
            }
            v => panic!("unexpected SM element: {v:?}"),
        },
        v => panic!("unexpected stream element: {v:?}"),
    };

    match client.next().await {
        Some(Event::Stream(StreamEvent::Reset { .. })) => (),
        other => panic!(
            "unexpected recv result on client side (immediately after stream setup): {other:?}"
        ),
    }

    Ok((client, server))
}

#[tokio::test]
async fn sm_negotiation() {
    let (client, mut server) = fresh_sm_pair().await.expect("stream setup failed");
    client.send(Box::new(Message::new(None).into())).await;
    match map_io(map_err(server.next().await)) {
        Ok(XmppStreamElement::Stanza(Stanza::Message(Message { .. }))) => (),
        other => panic!("unexpected recv result on server side: {other:?}"),
    }
}

#[cfg(not(feature = "component"))]
#[tokio::test]
async fn drop_stanza_with_unparsable_payload_nonsm() {
    let (mut client, mut server) = plain_pair().await.expect("stream setup failed");

    // Here, we construct a deliberately broken <presence/> stanza.
    let mut presence = Presence::new(PresenceType::None);
    presence.show = Some(Show::Away);
    let mut presence: Element = presence.into();
    for child in presence.children_mut() {
        if child.is("show", "jabber:client") {
            child.append_text("-foo");
        }
    }

    server
        .send(&presence)
        .await
        .expect("unexpected send result on server side");
    server
        .send(&Presence::new(PresenceType::Unavailable))
        .await
        .expect("unexpected send result on server side");

    match client.next().await {
        Some(Event::Stanza(Stanza::Presence(Presence { type_, .. }))) => {
            assert_eq!(type_, PresenceType::Unavailable);
        }
        other => panic!("unexpected recv result on client side: {other:?}"),
    }
}

#[cfg(not(feature = "component"))]
#[tokio::test]
async fn drop_stanza_with_unparsable_payload_sm() {
    let (mut client, mut server) = fresh_sm_pair().await.expect("stream setup failed");

    // Here, we construct a deliberately broken <presence/> stanza.
    let mut presence = Presence::new(PresenceType::None);
    presence.show = Some(Show::Away);
    let mut presence: Element = presence.into();
    for child in presence.children_mut() {
        if child.is("show", "jabber:client") {
            child.append_text("-foo");
        }
    }

    server
        .send(&presence)
        .await
        .expect("unexpected send result on server side");
    server
        .send(&sm::R)
        .await
        .expect("unexpected send result on server side");
    match map_io(map_err(server.next().await)) {
        Ok(XmppStreamElement::SM(sm::Nonza::Ack(sm::A { h }))) => {
            assert_eq!(h, 1);
        }
        other => panic!("unexpected recv result on the server side: {other:?}"),
    }
    server
        .send(&Presence::new(PresenceType::Unavailable))
        .await
        .expect("unexpected send result on server side");
    server
        .send(&sm::R)
        .await
        .expect("unexpected send result on server side");
    match map_io(map_err(server.next().await)) {
        Ok(XmppStreamElement::SM(sm::Nonza::Ack(sm::A { h }))) => {
            assert_eq!(h, 2);
        }
        other => panic!("unexpected recv result on the server side: {other:?}"),
    }

    match client.next().await {
        Some(Event::Stanza(Stanza::Presence(Presence { type_, .. }))) => {
            assert_eq!(type_, PresenceType::Unavailable);
        }
        other => panic!("unexpected recv result on client side: {other:?}"),
    }
}
