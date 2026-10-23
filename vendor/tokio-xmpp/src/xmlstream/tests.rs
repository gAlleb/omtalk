// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::time::Duration;

use futures::{SinkExt, StreamExt};

use xmpp_parsers::{
    ns,
    stream_error::{DefinedCondition, StreamError},
    stream_features::StreamFeatures,
};

use super::*;

#[derive(FromXml, AsXml, Debug)]
#[xml(namespace = "urn:example", name = "data")]
struct Data {
    #[xml(text)]
    contents: String,
}

#[tokio::test]
async fn test_initiate_accept_stream() {
    let (lhs, rhs) = tokio::io::duplex(65536);
    let initiator = tokio::spawn(async move {
        let mut stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader {
                from: Some("client".into()),
                to: Some("server".into()),
                id: Some("client-id".into()),
            },
            Timeouts::tight(),
        )
        .await?;
        Ok::<_, io::Error>(stream.take_header())
    });
    let responder = tokio::spawn(async move {
        let stream = accept_stream(
            tokio::io::BufStream::new(rhs),
            ns::JABBER_CLIENT,
            Timeouts::tight(),
        )
        .await?;
        assert_eq!(stream.header().from.unwrap(), "client");
        assert_eq!(stream.header().to.unwrap(), "server");
        assert_eq!(stream.header().id.unwrap(), "client-id");
        stream
            .send_header(StreamHeader {
                from: Some("server".into()),
                to: Some("client".into()),
                id: Some("server-id".into()),
            })
            .await
    });
    responder.await.unwrap().expect("responder");
    let server_header = initiator.await.unwrap().expect("initiator");
    assert_eq!(server_header.from.unwrap(), "server");
    assert_eq!(server_header.to.unwrap(), "client");
    assert_eq!(server_header.id.unwrap(), "server-id");
}

#[tokio::test]
async fn test_exchange_stream_features() {
    let (lhs, rhs) = tokio::io::duplex(65536);
    let initiator = tokio::spawn(async move {
        let stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader::default(),
            Timeouts::tight(),
        )
        .await?;
        let (features, _) = stream.recv_features::<Data>().await?;
        Ok::<_, RecvFeaturesError>(features)
    });
    let responder = tokio::spawn(async move {
        let stream = accept_stream(
            tokio::io::BufStream::new(rhs),
            ns::JABBER_CLIENT,
            Timeouts::tight(),
        )
        .await?;
        let stream = stream.send_header(StreamHeader::default()).await?;
        stream
            .send_features::<Data>(&StreamFeatures::default())
            .await?;
        Ok::<_, io::Error>(())
    });
    responder.await.unwrap().expect("responder failed");
    let features = initiator.await.unwrap().expect("initiator failed");
    assert_eq!(features, StreamFeatures::default());
}

#[tokio::test]
async fn test_handle_early_stream_error() {
    let (lhs, rhs) = tokio::io::duplex(65536);
    let err = StreamError::new(DefinedCondition::InternalServerError, "en", "Test error");
    let initiator = tokio::spawn(async move {
        let stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader::default(),
            Timeouts::tight(),
        )
        .await?;
        match stream.recv_features::<Data>().await {
            Ok((v, ..)) => panic!("test expected stream error, got features {v:?}"),
            Err(RecvFeaturesError::Io(e)) => Err(e),
            Err(RecvFeaturesError::StreamError(e)) => Ok(e),
        }
    });
    let responder = {
        let err = err.clone();
        tokio::spawn(async move {
            let stream = accept_stream(
                tokio::io::BufStream::new(rhs),
                ns::JABBER_CLIENT,
                Timeouts::tight(),
            )
            .await?;
            let stream = stream.send_header(StreamHeader::default()).await?;
            stream.send_error(&err).await?;
            Ok::<_, io::Error>(())
        })
    };
    responder.await.unwrap().expect("responder failed");
    let received = initiator.await.unwrap().expect("initiator failed");
    assert_eq!(received.0, err);
}

#[tokio::test]
async fn test_exchange_data() {
    let (lhs, rhs) = tokio::io::duplex(65536);

    let initiator = tokio::spawn(async move {
        let stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader::default(),
            Timeouts::tight(),
        )
        .await?;
        let (_, mut stream) = stream.recv_features::<Data>().await?;
        stream
            .send(&Data {
                contents: "hello".to_owned(),
            })
            .await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "world!"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, RecvFeaturesError>(())
    });

    let responder = tokio::spawn(async move {
        let stream = accept_stream(
            tokio::io::BufStream::new(rhs),
            ns::JABBER_CLIENT,
            Timeouts::tight(),
        )
        .await?;
        let stream = stream.send_header(StreamHeader::default()).await?;
        let mut stream = stream
            .send_features::<Data>(&StreamFeatures::default())
            .await?;
        stream
            .send(&Data {
                contents: "world!".to_owned(),
            })
            .await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "hello"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, io::Error>(())
    });

    responder.await.unwrap().expect("responder failed");
    initiator.await.unwrap().expect("initiator failed");
}

#[tokio::test]
async fn test_clean_shutdown() {
    let (lhs, rhs) = tokio::io::duplex(65536);

    let initiator = tokio::spawn(async move {
        let stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader::default(),
            Timeouts::tight(),
        )
        .await?;
        let (_, mut stream) = stream.recv_features::<Data>().await?;
        SinkExt::<&Data>::close(&mut stream).await?;
        match stream.next().await {
            Some(Err(ReadError::StreamFooterReceived)) => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, RecvFeaturesError>(())
    });

    let responder = tokio::spawn(async move {
        let stream = accept_stream(
            tokio::io::BufStream::new(rhs),
            ns::JABBER_CLIENT,
            Timeouts::tight(),
        )
        .await?;
        let stream = stream.send_header(StreamHeader::default()).await?;
        let mut stream = stream
            .send_features::<Data>(&StreamFeatures::default())
            .await?;
        match stream.next().await {
            Some(Err(ReadError::StreamFooterReceived)) => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        SinkExt::<&Data>::close(&mut stream).await?;
        Ok::<_, io::Error>(())
    });

    responder.await.unwrap().expect("responder failed");
    initiator.await.unwrap().expect("initiator failed");
}

#[tokio::test]
async fn test_exchange_data_stream_reset_and_shutdown() {
    let (lhs, rhs) = tokio::io::duplex(65536);

    let initiator = tokio::spawn(async move {
        let stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader::default(),
            Timeouts::tight(),
        )
        .await?;
        let (_, mut stream) = stream.recv_features::<Data>().await?;
        stream
            .send(&Data {
                contents: "hello".to_owned(),
            })
            .await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "world!"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        let stream = stream
            .initiate_reset()
            .send_header(StreamHeader {
                from: Some("client".into()),
                to: Some("server".into()),
                id: Some("client-id".into()),
            })
            .await?;
        assert_eq!(stream.header().from.unwrap(), "server");
        assert_eq!(stream.header().to.unwrap(), "client");
        assert_eq!(stream.header().id.unwrap(), "server-id");

        let (_, mut stream) = stream.recv_features::<Data>().await?;
        stream
            .send(&Data {
                contents: "once more".to_owned(),
            })
            .await?;
        SinkExt::<&Data>::close(&mut stream).await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "hello world!"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        match stream.next().await {
            Some(Err(ReadError::StreamFooterReceived)) => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, RecvFeaturesError>(())
    });

    let responder = tokio::spawn(async move {
        let stream = accept_stream(
            tokio::io::BufStream::new(rhs),
            ns::JABBER_CLIENT,
            Timeouts::tight(),
        )
        .await?;
        let stream = stream.send_header(StreamHeader::default()).await?;
        let mut stream = stream
            .send_features::<Data>(&StreamFeatures::default())
            .await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "hello"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        let stream = stream
            .accept_reset(&Data {
                contents: "world!".to_owned(),
            })
            .await?;
        assert_eq!(stream.header().from.unwrap(), "client");
        assert_eq!(stream.header().to.unwrap(), "server");
        assert_eq!(stream.header().id.unwrap(), "client-id");
        let stream = stream
            .send_header(StreamHeader {
                from: Some("server".into()),
                to: Some("client".into()),
                id: Some("server-id".into()),
            })
            .await?;
        let mut stream = stream
            .send_features::<Data>(&StreamFeatures::default())
            .await?;
        stream
            .send(&Data {
                contents: "hello world!".to_owned(),
            })
            .await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "once more"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        SinkExt::<&Data>::close(&mut stream).await?;
        match stream.next().await {
            Some(Err(ReadError::StreamFooterReceived)) => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, io::Error>(())
    });

    responder.await.unwrap().expect("responder failed");
    initiator.await.unwrap().expect("initiator failed");
}

#[tokio::test(start_paused = true)]
async fn test_emits_soft_timeout_after_silence() {
    let (lhs, rhs) = tokio::io::duplex(65536);

    let client_timeouts = Timeouts {
        read_timeout: Duration::new(300, 0),
        response_timeout: Duration::new(15, 0),
    };

    // We do want to trigger only one set of timeouts, so we set the server
    // timeouts much longer than the client timeouts
    let server_timeouts = Timeouts {
        read_timeout: Duration::new(900, 0),
        response_timeout: Duration::new(15, 0),
    };

    let initiator = tokio::spawn(async move {
        let stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader::default(),
            client_timeouts,
        )
        .await?;
        let (_, mut stream) = stream.recv_features::<Data>().await?;
        stream
            .send(&Data {
                contents: "hello".to_owned(),
            })
            .await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "world!"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        // Here we prove that the stream doesn't see any data and also does
        // not see the SoftTimeout too early.
        // (Well, not exactly a proof: We only check until half of the read
        // timeout, because that was easy to write and I deem it good enough.)
        match tokio::time::timeout(client_timeouts.read_timeout / 2, stream.next()).await {
            Err(_) => (),
            Ok(ev) => panic!("early stream message (before soft timeout): {:?}", ev),
        };
        // Now the next thing that happens is the soft timeout ...
        match stream.next().await {
            Some(Err(ReadError::SoftTimeout)) => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        // Another check that the there is some time between soft and hard
        // timeout.
        match tokio::time::timeout(client_timeouts.response_timeout / 3, stream.next()).await {
            Err(_) => (),
            Ok(ev) => {
                panic!("early stream message (before hard timeout): {:?}", ev);
            }
        };
        // ... and thereafter the hard timeout in form of an I/O error.
        match stream.next().await {
            Some(Err(ReadError::HardError(e))) if e.kind() == io::ErrorKind::TimedOut => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, RecvFeaturesError>(())
    });

    let responder = tokio::spawn(async move {
        let stream = accept_stream(
            tokio::io::BufStream::new(rhs),
            ns::JABBER_CLIENT,
            server_timeouts,
        )
        .await?;
        let stream = stream.send_header(StreamHeader::default()).await?;
        let mut stream = stream
            .send_features::<Data>(&StreamFeatures::default())
            .await?;
        stream
            .send(&Data {
                contents: "world!".to_owned(),
            })
            .await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "hello"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        match stream.next().await {
            Some(Err(ReadError::HardError(e))) if e.kind() == io::ErrorKind::InvalidData => {
                match e.downcast::<rxml::Error>() {
                    // the initiator closes the stream by dropping it once the
                    // timeout trips, so we get a hard eof here.
                    Ok(rxml::Error::InvalidEof(_)) => (),
                    other => panic!("unexpected error: {:?}", other),
                }
            }
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, io::Error>(())
    });

    responder.await.unwrap().expect("responder failed");
    initiator.await.unwrap().expect("initiator failed");
}

#[tokio::test]
async fn test_can_receive_after_shutdown() {
    let (lhs, rhs) = tokio::io::duplex(65536);

    let initiator = tokio::spawn(async move {
        let stream = initiate_stream(
            tokio::io::BufStream::new(lhs),
            ns::JABBER_CLIENT,
            StreamHeader::default(),
            Timeouts::tight(),
        )
        .await?;
        let (_, mut stream) = stream.recv_features::<Data>().await?;
        match stream.next().await {
            Some(Err(ReadError::StreamFooterReceived)) => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        match stream.next().await {
            None => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        stream
            .send(&Data {
                contents: "hello".to_owned(),
            })
            .await?;
        stream
            .send(&Data {
                contents: "world!".to_owned(),
            })
            .await?;
        <XmlStream<_, _> as SinkExt<&Data>>::close(&mut stream).await?;
        Ok::<_, RecvFeaturesError>(())
    });

    let responder = tokio::spawn(async move {
        let stream = accept_stream(
            tokio::io::BufStream::new(rhs),
            ns::JABBER_CLIENT,
            Timeouts::tight(),
        )
        .await?;
        let stream = stream.send_header(StreamHeader::default()).await?;
        let mut stream = stream
            .send_features::<Data>(&StreamFeatures::default())
            .await?;
        stream.shutdown().await?;
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "hello"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        match stream.next().await {
            Some(Ok(Data { contents })) => assert_eq!(contents, "world!"),
            other => panic!("unexpected stream message: {:?}", other),
        }
        match stream.next().await {
            Some(Err(ReadError::StreamFooterReceived)) => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        match stream.next().await {
            None => (),
            other => panic!("unexpected stream message: {:?}", other),
        }
        Ok::<_, io::Error>(())
    });

    responder.await.unwrap().expect("responder failed");
    initiator.await.unwrap().expect("initiator failed");
}
