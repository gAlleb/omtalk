//! The services window: what a server offers and its commands, as Gajim's "Discover
//! Services" and "Execute Command". A model only; the view draws it, the runtime asks the
//! server.

use crossterm::event::{KeyCode, KeyEvent};
use omtalk_xmpp::services::{CommandAction, CommandStatus, CommandStep, Note, ServiceInfo, ServiceItem};
use omtalk_xmpp::{BareJid, Jid};
use xmpp_parsers::data_forms::{DataForm, DataFormType, FieldType};

/// What the window asks the runtime to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Browse {
        account: BareJid,
        jid: Jid,
        node: Option<String>,
    },
    Execute {
        account: BareJid,
        jid: Jid,
        node: String,
        session: Option<String>,
        action: CommandAction,
        form: Option<DataForm>,
    },
}

/// One address (or node) as the server described it.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub info: Option<ServiceInfo>,
    pub items: Vec<ServiceItem>,
    pub commands: Vec<ServiceItem>,
}

#[derive(Debug, Clone)]
pub struct FieldState {
    pub var: Option<String>,
    pub label: String,
    pub kind: FieldType,
    pub required: bool,
    pub desc: Option<String>,
    /// Text being edited, or the chosen option's value.
    pub value: String,
    /// For list-multi: the chosen values.
    pub chosen: Vec<String>,
    /// (label, value)
    pub options: Vec<(String, String)>,
    /// Option under the cursor, for lists.
    pub option_cursor: usize,
}

impl FieldState {
    pub fn editable(&self) -> bool {
        !matches!(self.kind, FieldType::Fixed | FieldType::Hidden)
    }

    pub fn shown(&self) -> bool {
        self.kind != FieldType::Hidden
    }
}

#[derive(Debug, Clone)]
pub struct FormState {
    pub jid: Jid,
    pub step: CommandStep,
    /// The command's title as listed.
    pub title: String,
    pub fields: Vec<FieldState>,
    /// Which field has the cursor, among the editable ones' indexes.
    pub focus: usize,
    /// A button has the cursor instead (Tab past the last field): its place among the
    /// step's actions, then Cancel.
    pub button: Option<usize>,
    /// How far a result (a long table, say) is scrolled down, in rows.
    pub scroll: usize,
}

impl FormState {
    /// The buttons of a running step, in their order: its actions, then Cancel.
    pub fn buttons(&self) -> Vec<CommandAction> {
        let mut buttons = self.step.actions.clone();
        buttons.push(CommandAction::Cancel);
        buttons
    }

    pub fn finished(&self) -> bool {
        self.step.status != CommandStatus::Executing
    }

    pub fn notes(&self) -> &[Note] {
        &self.step.notes
    }
}

pub struct Services {
    pub account: BareJid,
    pub jid: Jid,
    pub node: Option<String>,
    pub listing: Option<Listing>,
    /// Row under the cursor: items first, then commands.
    pub cursor: usize,
    /// Where Back goes.
    back: Vec<(Jid, Option<String>)>,
    pub form: Option<FormState>,
    /// A failure to show, in words.
    pub problem: Option<String>,
    pub loading: bool,
}

impl Services {
    pub fn open(account: BareJid, jid: Jid) -> (Self, Request) {
        let request = Request::Browse {
            account: account.clone(),
            jid: jid.clone(),
            node: None,
        };
        let services = Self {
            account,
            jid,
            node: None,
            listing: None,
            cursor: 0,
            back: Vec::new(),
            form: None,
            problem: None,
            loading: true,
        };
        (services, request)
    }

    /// Where "back" leads from the first place: for a biboumi channel, its IRC server,
    /// whose commands set the connection (port, TLS, password, nick).
    pub fn with_parent(mut self, parent: Jid) -> Self {
        self.back.push((parent, None));
        self
    }

    pub fn rows(&self) -> usize {
        self.listing
            .as_ref()
            .map(|l| l.items.len() + l.commands.len())
            .unwrap_or(0)
    }

    pub fn listed(&mut self, jid: &Jid, node: &Option<String>, listing: Result<Listing, String>) {
        if &self.jid != jid || &self.node != node {
            return; // an answer for a place we have left
        }
        self.loading = false;
        match listing {
            Ok(listing) => {
                self.listing = Some(listing);
                self.problem = None;
            }
            Err(problem) => self.problem = Some(problem),
        }
        self.cursor = self.cursor.min(self.rows().saturating_sub(1));
    }

    /// The answer to a command step.
    pub fn stepped(&mut self, jid: Jid, title: String, step: Result<CommandStep, String>) {
        self.loading = false;
        match step {
            Ok(step) => {
                let fields = step.form.as_ref().map(fields_of).unwrap_or_default();
                let focus = fields.iter().position(FieldState::editable).unwrap_or(0);
                self.form = Some(FormState {
                    jid,
                    step,
                    title,
                    fields,
                    focus,
                    button: None,
                    scroll: 0,
                });
                self.problem = None;
            }
            Err(problem) => self.problem = Some(problem),
        }
    }

    /// `None` keeps the window open; `Some(request)` asks the runtime.
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if self.form.is_some() {
            return self.form_key(key);
        }
        match key.code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                Outcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.rows().saturating_sub(1));
                Outcome::Stay
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.enter(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.go_back(),
            KeyCode::Char('r') => self.reload(),
            _ => Outcome::Stay,
        }
    }

    /// Opens the row under the cursor: an address or node is browsed, a command run.
    pub fn enter(&mut self) -> Outcome {
        let Some(listing) = &self.listing else {
            return Outcome::Stay;
        };
        if self.cursor < listing.items.len() {
            let item = listing.items[self.cursor].clone();
            self.back.push((self.jid.clone(), self.node.clone()));
            self.jid = item.jid;
            self.node = item.node;
            self.listing = None;
            self.cursor = 0;
            return self.reload();
        }
        let Some(command) = listing.commands.get(self.cursor - listing.items.len()).cloned() else {
            return Outcome::Stay;
        };
        let Some(node) = command.node.clone() else {
            return Outcome::Stay;
        };
        self.loading = true;
        Outcome::Request(
            Request::Execute {
                account: self.account.clone(),
                jid: command.jid.clone(),
                node,
                session: None,
                action: CommandAction::Execute,
                form: None,
            },
            command.name.unwrap_or_default(),
        )
    }

    pub fn go_back(&mut self) -> Outcome {
        match self.back.pop() {
            Some((jid, node)) => {
                self.jid = jid;
                self.node = node;
                self.listing = None;
                self.cursor = 0;
                self.reload()
            }
            None => Outcome::Stay,
        }
    }

    pub fn reload(&mut self) -> Outcome {
        self.loading = true;
        self.problem = None;
        Outcome::Request(
            Request::Browse {
                account: self.account.clone(),
                jid: self.jid.clone(),
                node: self.node.clone(),
            },
            String::new(),
        )
    }

    /// A click on a listed row: the same as moving there and pressing Enter.
    pub fn click_row(&mut self, row: usize) -> Outcome {
        self.cursor = row.min(self.rows().saturating_sub(1));
        self.enter()
    }

    /// A click on a form field: it takes the cursor; a switch or a choice also turns.
    /// A click on a field's row: focus, a switch or a choice changes, and in a
    /// many-choice list the option on that row is ticked or unticked.
    pub fn click_field(&mut self, index: usize, option: Option<usize>) -> Outcome {
        if let Some(form) = self.form.as_mut().filter(|f| !f.finished())
            && let Some(field) = form.fields.get_mut(index).filter(|f| f.editable())
        {
            form.focus = index;
            match (&field.kind, option) {
                (FieldType::Boolean | FieldType::ListSingle, _) => edit(field, KeyCode::Char(' ')),
                (FieldType::ListMulti, Some(option)) => {
                    field.option_cursor = option;
                    edit(field, KeyCode::Char(' '));
                }
                _ => {}
            }
        }
        Outcome::Stay
    }

    fn form_key(&mut self, key: KeyEvent) -> Outcome {
        let form = self.form.as_mut().expect("checked");
        if form.finished() {
            // A result: scrolled through, and any of Esc, Enter, Backspace leaves it for
            // the listing.
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Backspace => self.form = None,
                KeyCode::Up | KeyCode::Char('k') => form.scroll = form.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => form.scroll += 1,
                KeyCode::PageUp => form.scroll = form.scroll.saturating_sub(10),
                KeyCode::PageDown => form.scroll += 10,
                KeyCode::Home => form.scroll = 0,
                _ => {}
            }
            return Outcome::Stay;
        }
        let editable: Vec<usize> = form
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.editable())
            .map(|(i, _)| i)
            .collect();
        let position = editable.iter().position(|&i| i == form.focus).unwrap_or(0);
        let buttons = form.buttons();
        // Tab goes through the fields, then the buttons, then round again.
        match (key.code, form.button) {
            (KeyCode::Esc, _) => return self.act(CommandAction::Cancel),
            (KeyCode::Enter, Some(button)) => {
                let action = buttons.get(button).copied().unwrap_or(CommandAction::Cancel);
                return self.act(action);
            }
            (KeyCode::Enter, None) => {
                let action = form.step.default_action.unwrap_or(CommandAction::Complete);
                return self.act(action);
            }
            (KeyCode::Left, Some(button)) => form.button = Some(button.saturating_sub(1)),
            (KeyCode::Right, Some(button)) => form.button = Some((button + 1).min(buttons.len() - 1)),
            (KeyCode::Tab, Some(button)) if button + 1 < buttons.len() => form.button = Some(button + 1),
            (KeyCode::Tab, Some(_)) => {
                form.button = None;
                if let Some(&first) = editable.first() {
                    form.focus = first;
                } else {
                    form.button = Some(0);
                }
            }
            (KeyCode::BackTab, Some(0)) | (KeyCode::Up, Some(_)) => {
                form.button = None;
                if let Some(&last) = editable.last() {
                    form.focus = last;
                }
            }
            (KeyCode::BackTab, Some(button)) => form.button = Some(button - 1),
            (KeyCode::Tab, None) if editable.is_empty() || position + 1 == editable.len() => {
                // The default one first, as Enter would press it.
                let default = form.step.default_action.unwrap_or(CommandAction::Complete);
                form.button = Some(buttons.iter().position(|&b| b == default).unwrap_or(0));
            }
            (KeyCode::BackTab, None) if editable.is_empty() || position == 0 => {
                form.button = Some(buttons.len() - 1);
            }
            (KeyCode::Tab | KeyCode::Down, None) if !editable.is_empty() => {
                form.focus = editable[(position + 1) % editable.len()];
            }
            (KeyCode::BackTab | KeyCode::Up, None) if !editable.is_empty() => {
                form.focus = editable[(position + editable.len() - 1) % editable.len()];
            }
            (_, Some(_)) => {}
            (code, None) => {
                if let Some(field) = form.fields.get_mut(form.focus) {
                    edit(field, code);
                }
            }
        }
        Outcome::Stay
    }

    /// A step on the open form: its values go with it, except when cancelling.
    pub fn act(&mut self, action: CommandAction) -> Outcome {
        let Some(form) = &self.form else {
            return Outcome::Stay;
        };
        let filled = (action != CommandAction::Cancel).then(|| submission(form));
        let request = Request::Execute {
            account: self.account.clone(),
            jid: form.jid.clone(),
            node: form.step.node.clone(),
            session: form.step.session.clone(),
            action,
            form: filled,
        };
        let title = form.title.clone();
        if action == CommandAction::Cancel {
            self.form = None;
        } else {
            self.loading = true;
        }
        Outcome::Request(request, title)
    }
}

/// What a key did to the window.
#[derive(Debug, Clone, PartialEq)]
// Lives for one key press; boxing would only add noise.
#[allow(clippy::large_enum_variant)]
pub enum Outcome {
    Stay,
    Close,
    /// Ask the server; the text is the command's title, for the form.
    Request(Request, String),
}

fn edit(field: &mut FieldState, code: KeyCode) {
    match field.kind {
        FieldType::Boolean => {
            if matches!(code, KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right) {
                let on = matches!(field.value.as_str(), "1" | "true");
                field.value = if on { "0" } else { "1" }.into();
            }
        }
        FieldType::ListSingle => {
            let len = field.options.len();
            if len == 0 {
                return;
            }
            let step = match code {
                KeyCode::Left => len - 1,
                KeyCode::Right | KeyCode::Char(' ') => 1,
                _ => return,
            };
            field.option_cursor = (field.option_cursor + step) % len;
            field.value = field.options[field.option_cursor].1.clone();
        }
        FieldType::ListMulti => {
            let len = field.options.len();
            if len == 0 {
                return;
            }
            match code {
                KeyCode::Left => field.option_cursor = (field.option_cursor + len - 1) % len,
                KeyCode::Right => field.option_cursor = (field.option_cursor + 1) % len,
                KeyCode::Char(' ') => {
                    let value = field.options[field.option_cursor].1.clone();
                    match field.chosen.iter().position(|v| v == &value) {
                        Some(at) => drop(field.chosen.remove(at)),
                        None => field.chosen.push(value),
                    }
                }
                _ => {}
            }
        }
        _ => match code {
            KeyCode::Char(c) => field.value.push(c),
            KeyCode::Backspace => drop(field.value.pop()),
            _ => {}
        },
    }
}

fn fields_of(form: &DataForm) -> Vec<FieldState> {
    form.fields
        .iter()
        .map(|f| {
            let options: Vec<(String, String)> = f
                .options
                .iter()
                .map(|o| {
                    (
                        o.label.clone().unwrap_or_else(|| o.value.clone()),
                        o.value.clone(),
                    )
                })
                .collect();
            let value = match f.type_ {
                FieldType::ListMulti | FieldType::JidMulti | FieldType::TextMulti => {
                    f.values.join(if f.type_ == FieldType::TextMulti {
                        "\n"
                    } else {
                        ", "
                    })
                }
                _ => f.values.first().cloned().unwrap_or_default(),
            };
            let option_cursor = options.iter().position(|(_, v)| v == &value).unwrap_or(0);
            FieldState {
                var: f.var.clone(),
                label: f.label.clone().or_else(|| f.var.clone()).unwrap_or_default(),
                kind: f.type_.clone(),
                required: f.required,
                desc: f.desc.clone(),
                value,
                chosen: if f.type_ == FieldType::ListMulti {
                    f.values.clone()
                } else {
                    Vec::new()
                },
                options,
                option_cursor,
            }
        })
        .collect()
}

/// The server's own form with our values put in: hidden fields (FORM_TYPE and the
/// like) go back untouched, as XEP-0004 asks.
fn submission(form: &FormState) -> DataForm {
    let mut out = form.step.form.clone().unwrap_or(DataForm {
        type_: DataFormType::Submit,
        title: None,
        instructions: None,
        fields: Vec::new(),
    });
    out.type_ = DataFormType::Submit;
    out.title = None;
    out.instructions = None;
    for field in &mut out.fields {
        let Some(state) = form.fields.iter().find(|s| s.var == field.var && s.var.is_some()) else {
            continue;
        };
        field.values = match state.kind {
            FieldType::Hidden | FieldType::Fixed => continue,
            FieldType::ListMulti => state.chosen.clone(),
            FieldType::JidMulti => state
                .value
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(Into::into)
                .collect(),
            FieldType::TextMulti => state.value.lines().map(Into::into).collect(),
            _ if state.value.is_empty() => Vec::new(),
            _ => vec![state.value.clone()],
        };
        field.options.clear();
        field.label = None;
        field.desc = None;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use omtalk_xmpp::Element;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn form() -> DataForm {
        let element: Element = r#"<x xmlns="jabber:x:data" type="form">
            <title>Add user</title>
            <field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/admin</value></field>
            <field var="accountjid" type="jid-single" label="Account"/>
            <field var="password" type="text-private" label="Password"/>
            <field var="notify" type="boolean" label="Notify"><value>0</value></field>
            <field var="role" type="list-single" label="Role"><value>user</value>
              <option label="User"><value>user</value></option><option label="Admin"><value>admin</value></option></field>
            <field type="fixed"><value>Read this first</value></field>
        </x>"#
            .parse()
            .unwrap();
        DataForm::try_from(element).unwrap()
    }

    fn step(form: Option<DataForm>, status: CommandStatus) -> CommandStep {
        CommandStep {
            node: "add-user".into(),
            session: Some("s1".into()),
            status,
            form,
            notes: Vec::new(),
            table: None,
            actions: vec![CommandAction::Complete],
            default_action: Some(CommandAction::Complete),
        }
    }

    fn services() -> Services {
        let (mut services, _) = Services::open(
            "admin@example.org".parse().unwrap(),
            "example.org".parse().unwrap(),
        );
        services.listed(
            &"example.org".parse().unwrap(),
            &None,
            Ok(Listing {
                info: None,
                items: vec![ServiceItem {
                    jid: "muc.example.org".parse().unwrap(),
                    node: None,
                    name: Some("Rooms".into()),
                }],
                commands: vec![ServiceItem {
                    jid: "example.org".parse().unwrap(),
                    node: Some("add-user".into()),
                    name: Some("Add user".into()),
                }],
            }),
        );
        services
    }

    #[test]
    fn tab_goes_past_the_fields_to_the_buttons_and_enter_presses_the_one_reached() {
        use omtalk_xmpp::services::{CommandAction, CommandStatus, CommandStep};
        let mut services = Services::open(
            "me@example.org".parse().unwrap(),
            "irc.example.org".parse().unwrap(),
        )
        .0;
        let step = CommandStep {
            node: "configure".into(),
            session: Some("s1".into()),
            status: CommandStatus::Executing,
            actions: vec![CommandAction::Complete],
            default_action: Some(CommandAction::Complete),
            form: None,
            notes: Vec::new(),
            table: None,
        };
        services.form = Some(FormState {
            jid: "irc.example.org".parse().unwrap(),
            step,
            title: "Configure".into(),
            fields: Vec::new(),
            focus: 0,
            button: None,
            scroll: 0,
        });
        // No field: Tab goes to the buttons, Done first; → Cancel; Enter presses it.
        services.on_key(key(KeyCode::Tab));
        assert_eq!(services.form.as_ref().unwrap().button, Some(0));
        services.on_key(key(KeyCode::Right));
        assert_eq!(services.form.as_ref().unwrap().button, Some(1));
        match services.on_key(key(KeyCode::Enter)) {
            Outcome::Request(Request::Execute { action, .. }, _) => assert_eq!(action, CommandAction::Cancel),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn browsing_goes_into_items_and_back() {
        let mut services = services();
        let outcome = services.on_key(key(KeyCode::Enter));
        assert!(
            matches!(outcome, Outcome::Request(Request::Browse { ref jid, .. }, _) if jid.as_str() == "muc.example.org")
        );
        assert_eq!(services.jid.as_str(), "muc.example.org");
        let back = services.on_key(key(KeyCode::Backspace));
        assert!(
            matches!(back, Outcome::Request(Request::Browse { ref jid, .. }, _) if jid.as_str() == "example.org")
        );
        assert_eq!(services.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn a_command_runs_and_its_form_goes_back_filled() {
        let mut services = services();
        services.on_key(key(KeyCode::Down));
        let run = services.on_key(key(KeyCode::Enter));
        assert!(
            matches!(run, Outcome::Request(Request::Execute { action: CommandAction::Execute, session: None, .. }, ref title) if title == "Add user")
        );

        services.stepped(
            "example.org".parse().unwrap(),
            "Add user".into(),
            Ok(step(Some(form()), CommandStatus::Executing)),
        );
        // The cursor starts on the first field one can edit, skipping hidden ones.
        assert_eq!(
            services.form.as_ref().unwrap().fields[services.form.as_ref().unwrap().focus].label,
            "Account"
        );
        for c in "new@example.org".chars() {
            services.on_key(key(KeyCode::Char(c)));
        }
        services.on_key(key(KeyCode::Tab));
        for c in "секрет".chars() {
            services.on_key(key(KeyCode::Char(c)));
        }
        services.on_key(key(KeyCode::Tab));
        services.on_key(key(KeyCode::Char(' '))); // notify on
        services.on_key(key(KeyCode::Tab));
        services.on_key(key(KeyCode::Right)); // role: admin

        let Outcome::Request(
            Request::Execute {
                action,
                session,
                form: Some(sent),
                ..
            },
            _,
        ) = services.on_key(key(KeyCode::Enter))
        else {
            panic!("Enter submits")
        };
        assert_eq!(
            (action, session.as_deref()),
            (CommandAction::Complete, Some("s1"))
        );
        assert_eq!(sent.type_, DataFormType::Submit);
        let value = |var: &str| {
            sent.fields
                .iter()
                .find(|f| f.var.as_deref() == Some(var))
                .unwrap()
                .values
                .clone()
        };
        assert_eq!(
            value("FORM_TYPE"),
            ["http://jabber.org/protocol/admin"],
            "hidden fields go back untouched"
        );
        assert_eq!(value("accountjid"), ["new@example.org"]);
        assert_eq!(value("password"), ["секрет"]);
        assert_eq!(value("notify"), ["1"]);
        assert_eq!(value("role"), ["admin"]);
    }

    #[test]
    fn escape_cancels_a_running_command_and_a_result_closes_with_enter() {
        let mut services = services();
        services.stepped(
            "example.org".parse().unwrap(),
            "Add user".into(),
            Ok(step(Some(form()), CommandStatus::Executing)),
        );
        let cancel = services.on_key(key(KeyCode::Esc));
        assert!(matches!(
            cancel,
            Outcome::Request(
                Request::Execute {
                    action: CommandAction::Cancel,
                    form: None,
                    ..
                },
                _
            )
        ));
        assert!(services.form.is_none());

        services.stepped(
            "example.org".parse().unwrap(),
            "Stats".into(),
            Ok(step(None, CommandStatus::Completed)),
        );
        assert_eq!(services.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert!(services.form.is_none(), "back to the listing");
    }
}
