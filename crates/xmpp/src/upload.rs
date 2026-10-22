//! Sending files: the server's upload service (XEP-0363) hands out a place for a file, an
//! address to PUT it to and one to share. Prosody's mod_http_file_share, ejabberd's
//! mod_http_upload and the like.

use xmpp_parsers::http_upload::{SlotRequest, SlotResult};
use xmpp_parsers::jid::Jid;

use crate::{Account, Error};

pub const NS: &str = "urn:xmpp:http:upload:0";

/// The account's upload service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub jid: Jid,
    /// The largest file it takes, if it says.
    pub max_size: Option<u64>,
}

/// Where one file goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub put: String,
    /// To send with the PUT (Authorization, Cookie, Expires: the only ones allowed).
    pub headers: Vec<(String, String)>,
    pub get: String,
}

impl Account {
    /// The upload service of the account's server: the server itself or one of its
    /// services. `None` if it has none.
    pub async fn upload_service(&self) -> Result<Option<Service>, Error> {
        let server: Jid = self.jid().domain().as_str().parse().expect("a domain is a JID");
        let mut candidates = vec![server.clone()];
        candidates.extend(
            self.disco_items(&server, None)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|i| i.jid),
        );
        for jid in candidates {
            let Ok(info) = self.disco_info(&jid, None).await else {
                continue;
            };
            if !info.features.contains(NS) {
                continue;
            }
            return Ok(Some(Service {
                max_size: max_size(&info.extensions),
                jid,
            }));
        }
        Ok(None)
    }

    /// A place for a file of `size` bytes named `name`.
    pub async fn upload_slot(
        &self,
        service: &Jid,
        name: &str,
        size: u64,
        content_type: Option<&str>,
    ) -> Result<Slot, Error> {
        let request = SlotRequest {
            filename: name.to_owned(),
            size,
            content_type: content_type.map(Into::into),
        };
        let answer = self
            .get(Some(service.clone()), request)
            .await?
            .ok_or_else(|| Error::Unexpected("empty answer to an upload slot request".into()))?;
        let slot = SlotResult::try_from(answer).map_err(|e| Error::Unexpected(e.to_string()))?;
        Ok(Slot {
            put: slot.put.url,
            headers: slot
                .put
                .headers
                .into_iter()
                .map(|h| (h.name.as_str().to_owned(), h.value))
                .collect(),
            get: slot.get.url,
        })
    }
}

/// The service's size limit, from its disco#info form.
fn max_size(forms: &[xmpp_parsers::data_forms::DataForm]) -> Option<u64> {
    forms
        .iter()
        .filter(|f| f.form_type() == Some(NS))
        .flat_map(|f| f.fields.iter())
        .find(|f| f.var.as_deref() == Some("max-file-size"))
        .and_then(|f| f.values.first())
        .and_then(|v| v.parse().ok())
}
