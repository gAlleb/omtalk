// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::fmt;
use std::io;

use rxml::NcNameStr;

use xso::{error::Error, fromxml::FallibleBuilder, AsXml, FromEventsBuilder, FromXml};

use xmpp_parsers::{component, sasl, sm, starttls, stream_error::ReceivedStreamError};

use crate::Stanza;

use super::ReadError;

/// Any valid XMPP stream-level element.
#[derive(FromXml, AsXml, Debug)]
#[xml()]
pub enum XmppStreamElement {
    /// Stanza
    #[xml(transparent)]
    Stanza(Stanza),

    /// SASL-related nonza
    #[xml(transparent)]
    Sasl(sasl::Nonza),

    /// STARTTLS-related nonza
    #[xml(transparent)]
    Starttls(starttls::Nonza),

    /// Component protocol nonzas
    #[xml(transparent)]
    ComponentHandshake(component::Handshake),

    /// Stream error received
    #[xml(transparent)]
    StreamError(ReceivedStreamError),

    /// XEP-0198 nonzas
    #[xml(transparent)]
    SM(sm::Nonza),
}

#[derive(Debug)]
pub enum PartialStanza {
    Presence,
    Iq,
    Message,
}

impl fmt::Display for PartialStanza {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Presence => f.write_str("presence"),
            Self::Message => f.write_str("message"),
            Self::Iq => f.write_str("iq"),
        }
    }
}

impl PartialStanza {
    pub fn to_ncname(&self) -> &'static NcNameStr {
        match self {
            Self::Presence => rxml::xml_ncname!("presence"),
            Self::Message => rxml::xml_ncname!("message"),
            Self::Iq => rxml::xml_ncname!("iq"),
        }
    }
}

enum CapturedMetadata {
    Nonza {
        qname: rxml::QName,
    },
    Stanza {
        ns: rxml::Namespace<'static>,
        name: PartialStanza,
        from: Option<String>,
        to: Option<String>,
        type_: Option<String>,
        id: Option<String>,
    },
}

impl CapturedMetadata {
    pub fn new(qname: &rxml::QName, attrs: &rxml::AttrMap) -> Self {
        let kind = match qname.1.as_str() {
            "presence" => Some(PartialStanza::Presence),
            "iq" => Some(PartialStanza::Iq),
            "message" => Some(PartialStanza::Message),
            _ => None,
        };
        if let Some(kind) = kind {
            CapturedMetadata::Stanza {
                ns: qname.0.clone(),
                name: kind,
                from: attrs.get(&rxml::Namespace::NONE, "from").cloned(),
                to: attrs.get(&rxml::Namespace::NONE, "to").cloned(),
                id: attrs.get(&rxml::Namespace::NONE, "id").cloned(),
                type_: attrs.get(&rxml::Namespace::NONE, "type").cloned(),
            }
        } else {
            CapturedMetadata::Nonza {
                qname: qname.clone(),
            }
        }
    }
}

pub struct FallibleStreamElementBuilder {
    metadata: Option<CapturedMetadata>,
    builder: FallibleBuilder<<XmppStreamElement as FromXml>::Builder, Error>,
}

impl FromEventsBuilder for FallibleStreamElementBuilder {
    type Output = FallibleStreamElement;

    fn feed(&mut self, ev: rxml::Event, ctx: &xso::Context) -> Result<Option<Self::Output>, Error> {
        match self.builder.feed(ev, ctx) {
            Ok(Some(output)) => Ok(Some(match output {
                Ok(v) => FallibleStreamElement::Ok(v),

                // The FallibleBuilder should never ever emit an rxml::Error,
                // because it cannot *receive* rxml::Error via `feed`.
                Err(Error::XmlError(e)) => unreachable!("feed somehow saw an rxml error: {e}"),

                // This error condition can not be emitted from feed.
                Err(Error::TypeMismatch) => unreachable!("feed somehow saw a TypeMismatch"),

                Err(error) => FallibleStreamElement::Err(
                    match self.metadata.take().expect("feed called after completion") {
                        CapturedMetadata::Nonza { qname } => {
                            StreamElementError::InvalidNonza { qname, error }
                        }
                        CapturedMetadata::Stanza {
                            ns,
                            name,
                            from,
                            to,
                            type_,
                            id,
                        } => StreamElementError::InvalidStanza {
                            ns,
                            name,
                            header: RawStanzaHeader {
                                from,
                                to,
                                type_,
                                id,
                            },
                            error,
                        },
                    },
                ),
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Container for unparsed stanza attributes.
#[derive(Debug)]
pub struct RawStanzaHeader {
    /// The unaltered `from` attribute, if present.
    pub from: Option<String>,

    /// The unaltered `to` attribute, if present.
    pub to: Option<String>,

    /// The unaltered `type` attribute, if present.
    pub type_: Option<String>,

    /// The unaltered `id` attribute, if present.
    pub id: Option<String>,
}

/// Error condition arising from failing to convert a stream-level element
/// into a [`xmpp_parsers`] struct.
#[derive(Debug)]
pub enum StreamElementError {
    /// Failed to convert an expected `<iq/>`, `<presence/>` or `<message/>`
    /// element into a struct.
    InvalidStanza {
        /// Namespace of the element.
        ns: rxml::Namespace<'static>,

        /// Name of the element.
        name: PartialStanza,

        /// Header attributes of the invalid stanza.
        header: RawStanzaHeader,

        /// The error which caused the stanza to fail to parse.
        ///
        /// Note that this is never `xso::error::Error::TypeMismatch`, because
        /// type mismatches do not even start to parse with `FromXml`.
        error: xso::error::Error,
    },

    /// Invalid top-level stream element.
    ///
    /// This is reported if the element header matched the
    /// [`XmppStreamElement`] type, but the payload failed to parse and it was
    /// not a stanza.
    InvalidNonza {
        /// Qualified name of the element which failed to parse.
        qname: rxml::QName,

        /// The error which caused the stanza to fail to parse.
        ///
        /// Note that this is never `xso::error::Error::TypeMismatch`, because
        /// type mismatches do not even start to parse with `FromXml`.
        error: xso::error::Error,
    },
}

impl fmt::Display for StreamElementError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidNonza { qname, error, .. } => write!(
                f,
                "invalid nonza received: <{{{}}}{}/> ({error})",
                qname.0, qname.1
            ),
            Self::InvalidStanza {
                ns, name, error, ..
            } => write!(
                f,
                "invalid stanza received: <{{{}}}{}/> ({error})",
                ns, name
            ),
        }
    }
}

impl core::error::Error for StreamElementError {}

/// Wrapper type to catch parse errors of [`XmppStreamElement`] items.
#[derive(Debug)]
pub enum FallibleStreamElement {
    /// Parsing succeeded.
    Ok(XmppStreamElement),

    /// Parsing failed.
    Err(StreamElementError),
}

impl FallibleStreamElement {
    /// Convert the contained error condition (if any) to a [`ReadError`].
    ///
    /// This can be used in places where you do not care to handle the various
    /// error conditions separately.
    pub fn into_read_error(self) -> Result<XmppStreamElement, ReadError> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Err(e) => Err(ReadError::HardError(io::Error::new(
                io::ErrorKind::InvalidData,
                e,
            ))),
        }
    }
}

impl FromXml for FallibleStreamElement {
    type Builder = FallibleStreamElementBuilder;

    fn from_events(
        qname: rxml::QName,
        attrs: rxml::AttrMap,
        ctx: &xso::Context,
    ) -> Result<Self::Builder, xso::error::FromEventsError> {
        let metadata = Some(CapturedMetadata::new(&qname, &attrs));
        let builder =
            <Result<XmppStreamElement, Error> as FromXml>::from_events(qname, attrs, ctx)?;
        Ok(FallibleStreamElementBuilder { metadata, builder })
    }
}
