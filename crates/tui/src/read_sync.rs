//! Coalesced read-state publication. Wait for login, serialize writes, and retain a
//! failed latest update for retry instead of losing it after the first timeout.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use omtalk_xmpp::{BareJid, Error};
use tokio::sync::mpsc;
use tokio::time::Instant;

const RETRY: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadMark {
    pub chat: BareJid,
    pub by: BareJid,
    pub id: String,
}

enum Event {
    Online(bool),
    Mark(ReadMark),
}

struct Pending {
    mark: ReadMark,
    retry_at: Instant,
    reported: bool,
}

fn receive(event: Event, online: &mut bool, pending: &mut BTreeMap<BareJid, Pending>) {
    match event {
        Event::Online(ready) => {
            *online = ready;
            if ready {
                for p in pending.values_mut() {
                    p.retry_at = Instant::now();
                }
            }
        }
        Event::Mark(mark) => {
            if pending.get(&mark.chat).is_none_or(|p| p.mark != mark) {
                pending.insert(
                    mark.chat.clone(),
                    Pending {
                        mark,
                        retry_at: Instant::now(),
                        reported: false,
                    },
                );
            }
        }
    }
}

#[derive(Clone)]
pub struct ReadSync(mpsc::UnboundedSender<Event>);

impl ReadSync {
    pub fn spawn<P, F, R>(publish: P, report: R) -> Self
    where
        P: Fn(ReadMark) -> F + Send + 'static,
        F: Future<Output = Result<(), Error>> + Send + 'static,
        R: Fn(&ReadMark, &Error, bool) + Send + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut online = false;
            let mut pending: BTreeMap<BareJid, Pending> = BTreeMap::new();
            type Sending = Pin<Box<dyn Future<Output = (ReadMark, Result<(), Error>)> + Send>>;
            let mut sending: Option<Sending> = None;
            loop {
                // An ACK and several newer positions may become ready together.
                // Consume queued changes before selecting the next mark to publish.
                loop {
                    match rx.try_recv() {
                        Ok(event) => receive(event, &mut online, &mut pending),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => return,
                    }
                }
                if sending.is_none()
                    && online
                    && let Some(next) = pending.values().find(|p| p.retry_at <= Instant::now())
                {
                    let mark = next.mark.clone();
                    let result = publish(mark.clone());
                    sending = Some(Box::pin(async move { (mark, result.await) }));
                }
                let next_retry = pending
                    .values()
                    .map(|p| p.retry_at)
                    .min()
                    .unwrap_or_else(Instant::now);
                tokio::select! {
                    event = rx.recv() => match event {
                        None => break,
                        Some(event) => receive(event, &mut online, &mut pending),
                    },
                    (mark, result) = async { sending.as_mut().expect("guarded").await }, if sending.is_some() => {
                        sending = None;
                        if let Some(p) = pending.get_mut(&mark.chat)
                            && p.mark == mark
                        {
                            match result {
                                Ok(()) => { pending.remove(&mark.chat); }
                                Err(error) => {
                                    let retry = matches!(error, Error::Timeout | Error::Send(_) | Error::Queued | Error::NotConfirmed);
                                    if !p.reported { report(&mark, &error, retry); p.reported = true; }
                                    if retry { p.retry_at = Instant::now() + RETRY; }
                                    else { pending.remove(&mark.chat); }
                                }
                            }
                        }
                    }
                    _ = tokio::time::sleep_until(next_retry), if online && sending.is_none() && !pending.is_empty() => {}
                }
            }
        });
        Self(tx)
    }

    pub fn online(&self, online: bool) {
        let _ = self.0.send(Event::Online(online));
    }
    pub fn queue(&self, mark: ReadMark) {
        let _ = self.0.send(Event::Mark(mark));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    type Call = (ReadMark, oneshot::Sender<Result<(), Error>>);
    fn fixture() -> (
        ReadSync,
        mpsc::UnboundedReceiver<Call>,
        mpsc::UnboundedReceiver<bool>,
    ) {
        let (calls, call_rx) = mpsc::unbounded_channel();
        let (errors, error_rx) = mpsc::unbounded_channel();
        let sync = ReadSync::spawn(
            move |mark| {
                let (reply, result) = oneshot::channel();
                calls.send((mark, reply)).unwrap();
                async move { result.await.unwrap() }
            },
            move |_, _, retry| {
                let _ = errors.send(retry);
            },
        );
        (sync, call_rx, error_rx)
    }
    fn mark(chat: &str, id: &str) -> ReadMark {
        ReadMark {
            chat: chat.parse().unwrap(),
            by: "me@example.org".parse().unwrap(),
            id: id.into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn startup_waits_for_login_and_sends_only_the_latest_queued_position() {
        let (sync, mut calls, _errors) = fixture();
        sync.queue(mark("room@example.org", "old"));
        sync.queue(mark("room@example.org", "new"));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(
            calls.try_recv().is_err(),
            "no IQ may time out waiting for initial login"
        );
        sync.online(true);
        let (sent, reply) = calls.recv().await.unwrap();
        assert_eq!(sent.id, "new");
        reply.send(Ok(())).unwrap();
        tokio::task::yield_now().await;
        assert!(calls.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_retries_without_blocking_other_chats_and_pauses_offline() {
        let (sync, mut calls, mut errors) = fixture();
        sync.online(true);
        sync.queue(mark("a@example.org", "a1"));
        let (_, first) = calls.recv().await.unwrap();
        sync.queue(mark("a@example.org", "a2"));
        sync.queue(mark("a@example.org", "a3"));
        sync.queue(mark("b@example.org", "b1"));
        first.send(Ok(())).unwrap();
        let (sent, reply) = calls.recv().await.unwrap();
        assert_eq!(sent.id, "a3", "intermediate positions are coalesced");
        reply.send(Err(Error::Timeout)).unwrap();
        assert!(errors.recv().await.unwrap());
        let (sent, reply) = calls.recv().await.unwrap();
        assert_eq!(sent.id, "b1", "one room's failure must not block another chat");
        reply.send(Ok(())).unwrap();
        tokio::time::advance(RETRY).await;
        let (sent, reply) = calls.recv().await.unwrap();
        assert_eq!(sent.id, "a3");
        reply.send(Err(Error::Timeout)).unwrap();
        tokio::task::yield_now().await;
        assert!(
            errors.try_recv().is_err(),
            "repeated timeout is not reported every retry"
        );
        sync.online(false);
        tokio::time::advance(RETRY * 6).await;
        assert!(calls.try_recv().is_err());
        sync.online(true);
        let (sent, reply) = calls.recv().await.unwrap();
        assert_eq!(sent.id, "a3");
        reply.send(Ok(())).unwrap();
        tokio::time::advance(RETRY * 2).await;
        assert!(calls.try_recv().is_err(), "an acknowledged mark leaves the queue");
    }

    #[tokio::test(start_paused = true)]
    async fn a_superseded_failure_cannot_replace_or_retry_over_a_newer_mark() {
        let (sync, mut calls, mut errors) = fixture();
        sync.online(true);
        sync.queue(mark("room@example.org", "old"));
        let (_, reply) = calls.recv().await.unwrap();
        sync.queue(mark("room@example.org", "new"));
        tokio::task::yield_now().await;
        reply.send(Err(Error::Timeout)).unwrap();
        let (sent, reply) = calls.recv().await.unwrap();
        assert_eq!(sent.id, "new");
        reply.send(Ok(())).unwrap();
        tokio::time::advance(RETRY * 2).await;
        assert!(errors.try_recv().is_err());
        assert!(calls.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_permanent_protocol_error_is_reported_without_endless_retries() {
        let (sync, mut calls, mut errors) = fixture();
        sync.online(true);
        sync.queue(mark("room@example.org", "one"));
        let (_, reply) = calls.recv().await.unwrap();
        reply
            .send(Err(Error::Unexpected("unsupported response".into())))
            .unwrap();
        assert!(!errors.recv().await.unwrap());
        tokio::time::advance(RETRY * 6).await;
        assert!(calls.try_recv().is_err());
    }
}
