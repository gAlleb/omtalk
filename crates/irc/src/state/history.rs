use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    Latest,
    Before,
    /// Initial backlog preceding messages already received live. Counts as catch-up.
    BeforeCatchUp,
    After,
    Targets,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRequest {
    pub target: String,
    pub kind: HistoryKind,
    pub cursor: Option<String>,
}

pub(super) struct Batch {
    pub history: bool,
    pub source: MessageSource,
    pub label: Option<String>,
    pub parent: Option<String>,
    pub request: Option<HistoryRequest>,
    pub count: usize,
    pub last_time: Option<String>,
    pub last_id: Option<String>,
    pub end: bool,
    pub networks: Option<BTreeSet<String>>,
}

impl State {
    pub fn queue_history(&mut self, request: HistoryRequest, e: &mut Effects) {
        if !self.snapshot().capabilities.history {
            e.events.push(Event::HistoryError {
                network: self.network.clone(),
                request,
                reason: "IRC: remote history is unavailable".into(),
            });
            return;
        }
        if self.history_queue.len() >= 256 {
            e.events.push(Event::HistoryError {
                network: self.network.clone(),
                request,
                reason: "IRC: history queue is full".into(),
            });
            return;
        }
        if !self.history_queue.contains(&request)
            && !self.history_pending.as_ref().is_some_and(|(r, _)| r == &request)
        {
            self.history_queue.push_back(request);
        }
        self.next_history(e);
    }

    pub fn next_history(&mut self, e: &mut Effects) {
        if self.history_pending.is_some() {
            return;
        }
        let Some(request) = self.history_queue.pop_front() else {
            return;
        };
        if !wire::atom(&request.target) && request.kind != HistoryKind::Targets {
            e.events.push(Event::HistoryError {
                network: self.network.clone(),
                request,
                reason: "IRC: invalid history target".into(),
            });
            self.next_history(e);
            return;
        }
        let limit = self.support.history_limit.to_string();
        let cursor = request.cursor.as_deref().unwrap_or("*");
        if cursor != "*"
            && (!wire::atom(cursor) || !(cursor.starts_with("timestamp=") || cursor.starts_with("msgid=")))
        {
            e.events.push(Event::HistoryError {
                network: self.network.clone(),
                request,
                reason: "IRC: invalid history cursor".into(),
            });
            self.next_history(e);
            return;
        }
        let message = match request.kind {
            HistoryKind::Latest => wire::command("CHATHISTORY", &["LATEST", &request.target, cursor, &limit]),
            HistoryKind::Before | HistoryKind::BeforeCatchUp => {
                wire::command("CHATHISTORY", &["BEFORE", &request.target, cursor, &limit])
            }
            HistoryKind::After => wire::command("CHATHISTORY", &["AFTER", &request.target, cursor, &limit]),
            HistoryKind::Targets => wire::command(
                "CHATHISTORY",
                &[
                    "TARGETS",
                    request.cursor.as_deref().unwrap_or(&format!(
                        "timestamp={}",
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    )),
                    "timestamp=1970-01-01T00:00:00.000Z",
                    &limit,
                ],
            ),
        };
        self.history_pending = Some((request, Instant::now()));
        e.send.push(message);
    }

    pub fn fail_history(&mut self, reason: &str, e: &mut Effects) {
        if let Some((request, _)) = self.history_pending.take() {
            e.events.push(Event::HistoryError {
                network: self.network.clone(),
                request,
                reason: reason.into(),
            });
        }
        self.batches.retain(|_, b| !b.history);
        if self.snapshot().capabilities.history {
            self.next_history(e);
        } else {
            for request in self.history_queue.drain(..) {
                e.events.push(Event::HistoryError {
                    network: self.network.clone(),
                    request,
                    reason: reason.into(),
                });
            }
        }
    }

    pub(super) fn count_history(&mut self, id: &str, time: Option<String>, server_id: Option<String>) {
        let mut next = Some(id.to_owned());
        for _ in 0..16 {
            let Some(id) = next else {
                break;
            };
            let Some(batch) = self.batches.get_mut(&id) else {
                break;
            };
            batch.count += 1;
            if time.is_some() {
                batch.last_time = time.clone();
            }
            if server_id.is_some() {
                batch.last_id = server_id.clone();
            }
            next = batch.parent.clone();
        }
    }

    pub(super) fn history_message(
        &mut self,
        m: &Message,
        cmd: &str,
        args: &[String],
        e: &mut Effects,
    ) -> bool {
        match (cmd, args) {
            ("BATCH", [id, rest @ ..]) if id.starts_with('+') => {
                if self.batches.len() >= 128 {
                    self.fatal = Some("IRC: too many nested batches".into());
                    return true;
                }
                let kind = rest.first().map(String::as_str).unwrap_or("");
                let parent = wire::tag(m, "batch").map(str::to_owned);
                let inherited = parent.as_ref().and_then(|id| self.batches.get(id));
                let history = matches!(kind, "chathistory" | "draft/chathistory-targets")
                    || inherited.is_some_and(|b| b.history);
                let request = if matches!(kind, "chathistory" | "draft/chathistory-targets") {
                    self.history_pending
                        .as_ref()
                        .filter(|(r, _)| {
                            (kind == "draft/chathistory-targets" && r.kind == HistoryKind::Targets)
                                || rest
                                    .get(1)
                                    .is_some_and(|s| self.support.case_mapping.equivalent(s, &r.target))
                        })
                        .map(|(r, _)| r.clone())
                } else {
                    None
                };
                let source = if request.as_ref().is_some_and(|r| r.kind == HistoryKind::Before) {
                    MessageSource::Older
                } else if let Some(batch) = inherited {
                    batch.source
                } else if history {
                    MessageSource::CatchUp
                } else {
                    MessageSource::Live
                };
                let label = wire::tag(m, "label")
                    .map(str::to_owned)
                    .or_else(|| inherited.and_then(|b| b.label.clone()));
                self.batches.insert(
                    id[1..].into(),
                    Batch {
                        history,
                        source,
                        label,
                        parent,
                        request,
                        count: 0,
                        last_time: None,
                        last_id: None,
                        end: wire::has_tag(m, "draft/chathistory-end"),
                        networks: (kind == "soju.im/bouncer-networks").then(BTreeSet::new),
                    },
                );
                true
            }
            ("BATCH", [id, ..]) if id.starts_with('-') => {
                if let Some(batch) = self.batches.remove(&id[1..]) {
                    if let Some(ids) = batch.networks {
                        e.events.push(Event::NetworksListed {
                            network: self.network.clone(),
                            ids,
                        });
                    }
                    if let Some(request) = batch.request {
                        self.history_pending = None;
                        e.events.push(Event::HistoryDone {
                            network: self.network.clone(),
                            request,
                            count: batch.count,
                            exhausted: batch.end || batch.count < self.support.history_limit,
                            last_time: batch.last_time,
                            last_id: batch.last_id,
                        });
                        self.next_history(e);
                    }
                }
                true
            }
            ("CHATHISTORY", [sub, target, timestamp, ..]) if sub == "TARGETS" => {
                if let Some(id) = wire::tag(m, "batch") {
                    self.count_history(id, Some(timestamp.clone()), None);
                }
                e.events.push(Event::HistoryTarget {
                    network: self.network.clone(),
                    target: target.clone(),
                    timestamp: timestamp.clone(),
                });
                true
            }
            ("FAIL", args) if args.first().is_some_and(|s| s == "CHATHISTORY") => {
                self.fail_history("IRC: remote history request failed", e);
                true
            }
            ("421", args) if args.iter().any(|s| s == "CHATHISTORY") => {
                self.caps.remove("draft/chathistory");
                self.fail_history("IRC: server does not support CHATHISTORY", e);
                e.events.push(Event::Snapshot(self.snapshot()));
                true
            }
            _ => {
                if wire::has_tag(m, "draft/chathistory-end")
                    && let Some(id) = wire::tag(m, "batch")
                    && let Some(batch) = self.batches.get_mut(id)
                {
                    batch.end = true;
                }
                false
            }
        }
    }
}
