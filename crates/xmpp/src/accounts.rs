use std::collections::BTreeMap;

use tokio::sync::mpsc;
use xmpp_parsers::jid::BareJid;

use crate::{Account, AccountConfig, AccountEvent};

/// All configured accounts, each connected on its own. Their events share one channel.
pub struct Accounts {
    accounts: BTreeMap<BareJid, Account>,
    events: mpsc::UnboundedSender<AccountEvent>,
}

impl Accounts {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<AccountEvent>) {
        let (events, receiver) = mpsc::unbounded_channel();
        (
            Self {
                accounts: BTreeMap::new(),
                events,
            },
            receiver,
        )
    }

    /// Starts the account. An account with the same JID is stopped first: one address,
    /// one connection.
    pub async fn add(&mut self, config: AccountConfig) -> Account {
        self.remove(&config.jid).await;
        let account = Account::spawn(config, self.events.clone());
        self.accounts.insert(account.jid().clone(), account.clone());
        account
    }

    pub async fn remove(&mut self, jid: &BareJid) {
        if let Some(account) = self.accounts.remove(jid) {
            account.stop().await;
        }
    }

    pub fn get(&self, jid: &BareJid) -> Option<&Account> {
        self.accounts.get(jid)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }
}
