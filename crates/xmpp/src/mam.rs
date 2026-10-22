//! The server's message archive (XEP-0313), read page by page.
//!
//! Several clients share an account, and the server hands a message to those online at
//! the time; the others only find it here.

use xmpp_parsers::date::DateTime;
use xmpp_parsers::jid::BareJid;
use xmpp_parsers::mam::{Fin, Query, QueryId};
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::rsm::SetQuery;

use crate::{Account, Error};

/// One archived message, as it was sent, with the archive's id for it.
#[derive(Debug, Clone)]
pub struct Archived {
    pub id: String,
    /// When the server received it.
    pub stamp: Option<DateTime>,
    pub message: Message,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub messages: Vec<Archived>,
    /// No more messages after this page.
    pub complete: bool,
    /// Archive id of the page's last message: ask for what follows it.
    pub last: Option<String>,
}

impl Account {
    /// Up to `max` messages after the one with id `after` (from the start if `None`),
    /// oldest first. `room` reads a group chat's archive instead of the account's own.
    pub async fn archive_after(
        &self,
        room: Option<&BareJid>,
        after: Option<&str>,
        max: usize,
    ) -> Result<Page, Error> {
        let set = SetQuery {
            max: Some(max),
            after: after.map(Into::into),
            before: None,
            index: None,
        };
        self.archive(room, set).await
    }

    /// The id of the newest archived message, if any: where to start reading from now on.
    pub async fn archive_newest(&self, room: Option<&BareJid>) -> Result<Option<String>, Error> {
        // An empty `before` asks for the last page.
        let set = SetQuery {
            max: Some(1),
            after: None,
            before: Some(String::new()),
            index: None,
        };
        Ok(self.archive(room, set).await?.last)
    }

    async fn archive(&self, room: Option<&BareJid>, set: SetQuery) -> Result<Page, Error> {
        let queryid = random_id();
        let query = Query {
            queryid: Some(QueryId(queryid.clone())),
            node: None,
            form: None,
            set: Some(set),
            flip_page: false,
        };
        // Results are accepted only from the archive asked: the own account's, or the room.
        let archive = room.cloned().unwrap_or_else(|| self.jid().clone());
        let to = room.map(|room| room.clone().into());
        self.archive_query(to, archive, queryid, query.into()).await
    }
}

/// The page, once the server's closing answer to the query has arrived.
pub(crate) fn page(messages: Vec<Archived>, answer: Option<Element>) -> Result<Page, Error> {
    let answer = answer.ok_or_else(|| Error::Unexpected("archive query answered without <fin/>".into()))?;
    let fin = Fin::try_from(answer).map_err(|e| Error::Unexpected(e.to_string()))?;
    Ok(Page {
        messages,
        complete: fin.complete,
        last: fin.set.last,
    })
}

/// The query a result message names, whoever sent it.
pub(crate) fn queryid_of(message: &Message) -> Option<String> {
    result_payload(message)?.queryid.map(|id| id.0)
}

/// The archived message `message` carries, if `archive` sent it. Archives answer from
/// their bare address; a room occupant writes from `room/nick`, and must not pass.
pub(crate) fn result_of(message: &Message, archive: &BareJid, own_archive: bool) -> Option<Archived> {
    match &message.from {
        // Without `from`, the stanza comes from the own server: the own archive only.
        None if own_archive => {}
        Some(from) if from.resource().is_none() && from.to_bare() == *archive => {}
        _ => return None,
    }
    let forwarded = result_payload(message)?;
    Some(Archived {
        id: forwarded.id,
        stamp: forwarded.forwarded.delay.map(|d| d.stamp),
        message: forwarded.forwarded.message,
    })
}

fn result_payload(message: &Message) -> Option<xmpp_parsers::mam::Result_> {
    message
        .payloads
        .iter()
        .find(|p| p.is("result", xmpp_parsers::ns::MAM))
        .and_then(|p| xmpp_parsers::mam::Result_::try_from(p.clone()).ok())
}

fn random_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::forwarding::Forwarded;
    use xmpp_parsers::mam::Result_;

    fn result_from(from: Option<&str>) -> Message {
        let archived = Message::chat(Some("me@example.org".parse().unwrap()));
        let result = Result_ {
            id: "A1".into(),
            queryid: Some(QueryId("q1".into())),
            forwarded: Forwarded {
                delay: None,
                message: archived,
            },
        };
        let mut message =
            Message::normal(Some("me@example.org/laptop".parse().unwrap())).with_payload(result);
        message.from = from.map(|f| f.parse().unwrap());
        message
    }

    #[test]
    fn results_are_taken_only_from_the_archive_asked() {
        let own: BareJid = "me@example.org".parse().unwrap();
        let room: BareJid = "room@muc.example.org".parse().unwrap();

        assert_eq!(queryid_of(&result_from(None)).as_deref(), Some("q1"));
        assert!(
            result_of(&result_from(None), &own, true).is_some(),
            "the own server"
        );
        assert!(result_of(&result_from(Some("me@example.org")), &own, true).is_some());
        assert!(result_of(&result_from(Some("room@muc.example.org")), &room, false).is_some());

        // Someone else, answering a query of ours.
        assert!(result_of(&result_from(Some("mallory@evil.example")), &own, true).is_none());
        // A room occupant writes from room/nick; only the room itself is the archive.
        assert!(result_of(&result_from(Some("room@muc.example.org/mallory")), &room, false).is_none());
        // No `from` means the own server, which does not speak for a room's archive.
        assert!(result_of(&result_from(None), &room, false).is_none());
        // The room is not the own archive, nor the other way round.
        assert!(result_of(&result_from(Some("room@muc.example.org")), &own, true).is_none());
    }
}
