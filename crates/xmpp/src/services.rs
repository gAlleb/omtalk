//! What a server offers, and its commands: service discovery (XEP-0030) and ad-hoc
//! commands (XEP-0050), as Gajim's "Discover Services" and "Execute Command". Commands
//! are how an own server is run from the client: users, announcements, statistics
//! (XEP-0133).

use std::collections::BTreeSet;

use xmpp_parsers::data_forms::DataForm;
use xmpp_parsers::disco::{DiscoInfoQuery, DiscoInfoResult, DiscoItemsQuery, DiscoItemsResult};
use xmpp_parsers::jid::Jid;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::minidom::rxml::xml_ncname;

use crate::{Account, Error};

pub const COMMANDS: &str = "http://jabber.org/protocol/commands";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub category: String,
    pub kind: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceInfo {
    pub identities: Vec<Identity>,
    pub features: BTreeSet<String>,
    /// Forms describing it further (XEP-0128): limits, contact addresses.
    pub extensions: Vec<DataForm>,
}

/// Something listed under an address: a service, a node, or a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceItem {
    pub jid: Jid,
    pub node: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    Execute,
    Next,
    Prev,
    Complete,
    Cancel,
}

impl CommandAction {
    fn name(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "execute" => Self::Execute,
            "next" => Self::Next,
            "prev" => Self::Prev,
            "complete" => Self::Complete,
            "cancel" => Self::Cancel,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    /// Waiting for the next step, usually a filled form.
    Executing,
    Completed,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub kind: NoteKind,
    pub text: String,
}

/// A table in a result: its columns (labelled, or by their var), a row an item, the
/// values of a field with several joined by ", ".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// The table of a form, if it has one. xmpp-parsers knows no `<reported>`, and refuses
/// a form with one; the rest of the form is read without them.
fn table_of(form: &Element) -> Option<Table> {
    let ns = xmpp_parsers::ns::DATA_FORMS;
    let reported = form.get_child("reported", ns)?;
    let vars: Vec<(String, String)> = reported
        .children()
        .filter(|f| f.is("field", ns))
        .filter_map(|f| {
            let var = f.attr(xml_ncname!("var"))?.to_owned();
            let label = f.attr(xml_ncname!("label")).unwrap_or(&var).to_owned();
            Some((var, label))
        })
        .collect();
    let rows = form
        .children()
        .filter(|c| c.is("item", ns))
        .map(|item| {
            vars.iter()
                .map(|(var, _)| {
                    item.children()
                        .filter(|f| f.is("field", ns) && f.attr(xml_ncname!("var")) == Some(var.as_str()))
                        .flat_map(|f| f.children().filter(|v| v.is("value", ns)).map(|v| v.text()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .collect()
        })
        .collect();
    Some(Table {
        columns: vars.into_iter().map(|(_, label)| label).collect(),
        rows,
    })
}

/// The form without its table, which xmpp-parsers would refuse.
fn without_table(form: &Element) -> Element {
    let ns = xmpp_parsers::ns::DATA_FORMS;
    let mut bare = form.clone();
    while bare.remove_child("reported", ns).is_some() {}
    while bare.remove_child("item", ns).is_some() {}
    bare
}

/// Where a command stands after a step.
#[derive(Debug, Clone)]
pub struct CommandStep {
    pub node: String,
    /// Carries the command's state between steps; give it back with the next one.
    pub session: Option<String>,
    pub status: CommandStatus,
    /// The form to fill (while executing) or the result to show (once completed).
    pub form: Option<DataForm>,
    pub notes: Vec<Note>,
    /// A result with several items (XEP-0004 `<reported>` and `<item>`): a table.
    pub table: Option<Table>,
    /// The steps offered now, besides cancelling.
    pub actions: Vec<CommandAction>,
    /// The step Enter means.
    pub default_action: Option<CommandAction>,
}

impl Account {
    pub async fn disco_info(&self, jid: &Jid, node: Option<&str>) -> Result<ServiceInfo, Error> {
        let query = DiscoInfoQuery {
            node: node.map(Into::into),
        };
        let answer = self
            .get(Some(jid.clone()), query)
            .await?
            .ok_or_else(|| empty("disco#info"))?;
        let info = DiscoInfoResult::try_from(answer).map_err(|e| Error::Unexpected(e.to_string()))?;
        Ok(ServiceInfo {
            identities: info
                .identities
                .into_iter()
                .map(|i| Identity {
                    category: i.category,
                    kind: i.type_,
                    name: i.name,
                })
                .collect(),
            features: info.features,
            extensions: info.extensions,
        })
    }

    pub async fn disco_items(&self, jid: &Jid, node: Option<&str>) -> Result<Vec<ServiceItem>, Error> {
        let query = DiscoItemsQuery {
            node: node.map(Into::into),
            rsm: None,
        };
        let answer = self
            .get(Some(jid.clone()), query)
            .await?
            .ok_or_else(|| empty("disco#items"))?;
        let items = DiscoItemsResult::try_from(answer).map_err(|e| Error::Unexpected(e.to_string()))?;
        Ok(items
            .items
            .into_iter()
            .map(|i| ServiceItem {
                jid: i.jid,
                node: i.node,
                name: i.name,
            })
            .collect())
    }

    /// The commands `jid` offers this account; a server lists only those it may run.
    pub async fn commands(&self, jid: &Jid) -> Result<Vec<ServiceItem>, Error> {
        self.disco_items(jid, Some(COMMANDS)).await
    }

    /// One step of a command: start it (`Execute`, no session), go on with a filled
    /// form, or cancel.
    pub async fn execute(
        &self,
        jid: &Jid,
        node: &str,
        session: Option<&str>,
        action: CommandAction,
        form: Option<DataForm>,
    ) -> Result<CommandStep, Error> {
        let mut command = Element::builder("command", COMMANDS)
            .attr(xml_ncname!("node").into(), node)
            .attr(xml_ncname!("action").into(), action.name());
        if let Some(session) = session {
            command = command.attr(xml_ncname!("sessionid").into(), session);
        }
        if let Some(form) = form {
            command = command.append(Element::from(form));
        }
        let answer = self
            .set(Some(jid.clone()), command.build())
            .await?
            .ok_or_else(|| empty("command"))?;
        step(node, &answer)
    }
}

fn empty(what: &str) -> Error {
    Error::Unexpected(format!("empty answer to {what}"))
}

/// Reads the answer to a command step.
pub(crate) fn step(node: &str, answer: &Element) -> Result<CommandStep, Error> {
    if !answer.is("command", COMMANDS) {
        return Err(Error::Unexpected(format!(
            "not a command answer: <{}>",
            answer.name()
        )));
    }
    let status = match answer.attr(xml_ncname!("status")) {
        Some("executing") => CommandStatus::Executing,
        Some("canceled") => CommandStatus::Canceled,
        // Some servers leave it out on the last step.
        _ => CommandStatus::Completed,
    };
    let mut step = CommandStep {
        node: answer.attr(xml_ncname!("node")).unwrap_or(node).to_owned(),
        session: answer.attr(xml_ncname!("sessionid")).map(Into::into),
        status,
        form: None,
        notes: Vec::new(),
        table: None,
        actions: Vec::new(),
        default_action: None,
    };
    for child in answer.children() {
        if child.is("actions", COMMANDS) {
            step.actions = child
                .children()
                .filter_map(|a| CommandAction::parse(a.name()))
                .collect();
            step.default_action = child.attr(xml_ncname!("execute")).and_then(CommandAction::parse);
        } else if child.is("note", COMMANDS) {
            let kind = match child.attr(xml_ncname!("type")) {
                Some("warn") => NoteKind::Warn,
                Some("error") => NoteKind::Error,
                _ => NoteKind::Info,
            };
            step.notes.push(Note {
                kind,
                text: child.text(),
            });
        } else if child.is("x", xmpp_parsers::ns::DATA_FORMS) {
            step.table = table_of(child);
            step.form = DataForm::try_from(without_table(child)).ok();
        }
    }
    // A form to fill with no actions listed still has the one step: completing it.
    if step.status == CommandStatus::Executing && step.actions.is_empty() {
        step.actions.push(CommandAction::Complete);
        step.default_action.get_or_insert(CommandAction::Complete);
    }
    if step.default_action.is_none() {
        step.default_action = step.actions.last().copied();
    }
    Ok(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_with_items_is_a_table_and_the_rest_of_its_form_stays() {
        let answer: Element = r#"<command xmlns="http://jabber.org/protocol/commands" node="list" status="completed">
            <x xmlns="jabber:x:data" type="result"><title>Online</title>
              <field var="total" type="text-single" label="Total"><value>2</value></field>
              <reported><field var="jid" label="Address"/><field var="res" label="Resources"/></reported>
              <item><field var="jid"><value>a@x</value></field><field var="res"><value>pc</value><value>phone</value></field></item>
              <item><field var="res"><value>web</value></field><field var="jid"><value>b@x</value></field></item>
            </x></command>"#
            .parse()
            .unwrap();
        let step = step("list", &answer).unwrap();
        assert_eq!(
            step.table,
            Some(Table {
                columns: vec!["Address".into(), "Resources".into()],
                rows: vec![
                    vec!["a@x".into(), "pc, phone".into()],
                    vec!["b@x".into(), "web".into()]
                ],
            }),
            "columns as reported, whatever order an item gives them in"
        );
        let form = step.form.expect("the form without its table is read still");
        assert_eq!(form.title.as_deref(), Some("Online"));
        assert_eq!(form.fields[0].values, ["2"]);
    }

    #[test]
    fn a_step_with_a_form_actions_and_a_note_is_read() {
        let answer: Element = r#"<command xmlns="http://jabber.org/protocol/commands" node="config" sessionid="s1" status="executing">
            <actions execute="next"><prev/><next/><complete/></actions>
            <note type="warn">careful</note>
            <x xmlns="jabber:x:data" type="form"><field var="name" type="text-single"><value>v</value></field></x>
        </command>"#
            .parse()
            .unwrap();
        let step = step("config", &answer).unwrap();
        assert_eq!(step.session.as_deref(), Some("s1"));
        assert_eq!(step.status, CommandStatus::Executing);
        assert_eq!(
            step.actions,
            [CommandAction::Prev, CommandAction::Next, CommandAction::Complete]
        );
        assert_eq!(step.default_action, Some(CommandAction::Next));
        assert_eq!(
            step.notes,
            [Note {
                kind: NoteKind::Warn,
                text: "careful".into()
            }]
        );
        assert_eq!(step.form.unwrap().fields[0].values, ["v"]);
    }

    #[test]
    fn a_bare_form_can_still_be_completed() {
        let answer: Element = r#"<command xmlns="http://jabber.org/protocol/commands" node="n" sessionid="s" status="executing">
            <x xmlns="jabber:x:data" type="form"/></command>"#
            .parse()
            .unwrap();
        let step = step("n", &answer).unwrap();
        assert_eq!(step.actions, [CommandAction::Complete]);
        assert_eq!(step.default_action, Some(CommandAction::Complete));
    }
}
