//! Room administration uses the existing server-form UI, without ad-hoc commands.
use crate::services::Listing;
use omtalk_xmpp::services::{CommandAction, CommandStatus, CommandStep, Note, NoteKind, ServiceItem};
use omtalk_xmpp::{Account, Affiliation, BareJid, Role};
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType, Option_};

const CONFIG: &str = "config";

pub fn listing(room: &BareJid) -> Listing {
    let commands = [
        (CONFIG, tr!("Room configuration", "Настройки комнаты")),
        ("owner", tr!("Owners", "Владельцы")),
        ("admin", tr!("Administrators", "Администраторы")),
        ("member", tr!("Members", "Участники")),
        ("outcast", tr!("Banned accounts", "Заблокированные")),
        (
            "role",
            tr!("Moderator, voice, kick", "Модератор, право писать, исключение"),
        ),
        ("subject", tr!("Room topic", "Тема комнаты")),
        ("avatar", tr!("Room avatar", "Аватар комнаты")),
    ]
    .into_iter()
    .map(|(node, name)| ServiceItem {
        jid: room.clone().into(),
        node: Some(node.into()),
        name: Some(name.into()),
    })
    .collect();
    Listing {
        commands,
        ..Listing::default()
    }
}

fn field(var: &str, label: &str, kind: FieldType, value: &str) -> Field {
    let mut field = Field::new(var, kind).with_value(value);
    field.label = Some(label.into());
    field
}

fn choices(var: &str, label: &str, values: &[(&str, &str)], selected: &str) -> Field {
    let mut field = field(var, label, FieldType::ListSingle, selected);
    field.options = values
        .iter()
        .map(|(value, label)| Option_ {
            label: Some((*label).into()),
            value: (*value).into(),
        })
        .collect();
    field
}

fn confirm() -> Field {
    field(
        "confirm",
        tr!("Confirm this change", "Подтверждаю изменение"),
        FieldType::Boolean,
        "0",
    )
}

fn value<'a>(form: &'a DataForm, name: &str) -> &'a str {
    form.fields
        .iter()
        .find(|f| f.var.as_deref() == Some(name))
        .and_then(|f| f.values.first())
        .map(String::as_str)
        .unwrap_or_default()
}

fn affiliation(name: &str) -> Result<Affiliation, String> {
    Ok(match name {
        "owner" => Affiliation::Owner,
        "admin" => Affiliation::Admin,
        "member" => Affiliation::Member,
        "outcast" => Affiliation::Outcast,
        "none" => Affiliation::None,
        _ => return Err(tr!("Invalid affiliation", "Неверные постоянные права").into()),
    })
}

pub async fn step(
    xmpp: &Account,
    room: &BareJid,
    node: &str,
    form: Option<DataForm>,
) -> Result<CommandStep, String> {
    let submitted = form.is_some();
    let mut fields = Vec::new();
    let mut server_form = None;
    if let Some(form) = form {
        match node {
            CONFIG => xmpp.configure_room(room, form).await.map_err(|e| e.to_string())?,
            "owner" | "admin" | "member" | "outcast" => {
                require_confirmation(&form)?;
                let jid = value(&form, "jid")
                    .trim()
                    .parse::<BareJid>()
                    .map_err(|e| e.to_string())?;
                xmpp.set_affiliation(room, &jid, affiliation(value(&form, "affiliation"))?)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            "role" => {
                require_confirmation(&form)?;
                let nick = value(&form, "nick").trim();
                if nick.is_empty() {
                    return Err(tr!("Enter a room nickname", "Введите ник в комнате").into());
                }
                let role = match value(&form, "role") {
                    "moderator" => Role::Moderator,
                    "participant" => Role::Participant,
                    "visitor" => Role::Visitor,
                    "none" => Role::None,
                    _ => return Err(tr!("Invalid role", "Неверная роль").into()),
                };
                xmpp.set_role(room, nick, role, Some(value(&form, "reason")))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            "subject" => xmpp
                .set_subject(room, value(&form, "subject"))
                .await
                .map_err(|e| e.to_string())?,
            "avatar" => {
                let png = if value(&form, "remove") == "1" {
                    None
                } else {
                    let path = value(&form, "path").trim().to_owned();
                    if path.is_empty() {
                        return Err(tr!(
                            "Enter an image path or select remove",
                            "Укажите путь к картинке или выберите удаление"
                        )
                        .into());
                    }
                    Some(
                        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                            let path = if let Some(rest) = path.strip_prefix("~/") {
                                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                                    .join(rest)
                            } else {
                                path.into()
                            };
                            let image = image::open(path).map_err(|e| e.to_string())?.thumbnail(512, 512);
                            let mut png = std::io::Cursor::new(Vec::new());
                            image
                                .write_to(&mut png, image::ImageFormat::Png)
                                .map_err(|e| e.to_string())?;
                            Ok(png.into_inner())
                        })
                        .await
                        .map_err(|e| e.to_string())??,
                    )
                };
                xmpp.set_room_avatar(room, png.as_deref())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            _ => return Err(tr!("Unknown room action", "Неизвестное действие комнаты").into()),
        }
    } else {
        match node {
            CONFIG => {
                server_form = Some(xmpp.room_config(room).await.map_err(|e| e.to_string())?);
            }
            "owner" | "admin" | "member" | "outcast" => {
                let members = xmpp
                    .affiliated(room, affiliation(node)?)
                    .await
                    .map_err(|e| e.to_string())?;
                fields.push(field(
                    "current",
                    tr!("Current accounts", "Текущий список"),
                    FieldType::Fixed,
                    &members
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
                fields.push(field(
                    "jid",
                    tr!("Account JID", "JID аккаунта"),
                    FieldType::JidSingle,
                    "",
                ));
                fields.push(choices(
                    "affiliation",
                    tr!("New affiliation", "Новые постоянные права"),
                    &[
                        ("owner", tr!("Owner", "Владелец")),
                        ("admin", tr!("Administrator", "Администратор")),
                        ("member", tr!("Member", "Участник")),
                        ("none", tr!("No membership / unban", "Без членства / снять бан")),
                        ("outcast", tr!("Banned", "Заблокирован")),
                    ],
                    node,
                ));
                fields.push(confirm());
            }
            "role" => {
                fields.push(field(
                    "nick",
                    tr!("Present nickname", "Ник присутствующего"),
                    FieldType::TextSingle,
                    "",
                ));
                fields.push(choices(
                    "role",
                    tr!("New role", "Новая роль"),
                    &[
                        (
                            "participant",
                            tr!("Participant (voice)", "Участник (может писать)"),
                        ),
                        ("moderator", tr!("Moderator", "Модератор")),
                        (
                            "visitor",
                            tr!("Visitor (read only)", "Посетитель (только чтение)"),
                        ),
                        ("none", tr!("Kick from room", "Исключить из комнаты")),
                    ],
                    "participant",
                ));
                fields.push(field(
                    "reason",
                    tr!("Reason (optional)", "Причина (необязательно)"),
                    FieldType::TextSingle,
                    "",
                ));
                fields.push(confirm());
            }
            "subject" => fields.push(field(
                "subject",
                tr!("New topic (empty clears it)", "Новая тема (пусто — очистить)"),
                FieldType::TextMulti,
                "",
            )),
            "avatar" => {
                fields.push(field(
                    "path",
                    tr!(
                        "Image path on this computer",
                        "Путь к картинке на этом компьютере"
                    ),
                    FieldType::TextSingle,
                    "",
                ));
                fields.push(field(
                    "remove",
                    tr!("Remove avatar", "Удалить аватар"),
                    FieldType::Boolean,
                    "0",
                ));
            }
            _ => return Err(tr!("Unknown room action", "Неизвестное действие комнаты").into()),
        }
    }
    let form = if submitted {
        None
    } else {
        Some(server_form.unwrap_or(DataForm {
            type_: DataFormType::Form,
            title: None,
            instructions: None,
            fields,
        }))
    };
    Ok(CommandStep {
        node: node.into(),
        session: None,
        status: if submitted {
            CommandStatus::Completed
        } else {
            CommandStatus::Executing
        },
        form,
        table: None,
        notes: if submitted {
            vec![Note {
                kind: NoteKind::Info,
                text: if node == "subject" {
                    tr!("Topic change sent", "Изменение темы отправлено")
                } else {
                    tr!("Saved", "Сохранено")
                }
                .into(),
            }]
        } else {
            Vec::new()
        },
        actions: if submitted {
            Vec::new()
        } else {
            vec![CommandAction::Complete]
        },
        default_action: Some(CommandAction::Complete),
    })
}

fn require_confirmation(form: &DataForm) -> Result<(), String> {
    if value(form, "confirm") == "1" {
        Ok(())
    } else {
        Err(tr!(
            "Confirm the change before applying it",
            "Перед применением подтвердите изменение"
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "insecure-tcp")]
    #[tokio::test]
    async fn an_offline_members_affiliation_changes_only_after_confirmation() {
        use omtalk_xmpp::{AccountConfig, Accounts, Event, Server};
        let mut config = AccountConfig::new("u63@localhost".parse().unwrap(), "test");
        config.server = Server::Plaintext {
            addr: "127.0.0.1:15222".into(),
        };
        let (mut accounts, mut events) = Accounts::new();
        let owner = accounts.add(config).await;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !matches!(events.recv().await.unwrap().event, Event::Online { .. }) {}
        })
        .await
        .unwrap();
        let room: BareJid = format!("admin-ui-{}@muc.localhost", omtalk_xmpp::receipts::new_id())
            .parse()
            .unwrap();
        let guest: BareJid = "u64@localhost".parse().unwrap();
        owner.join(&room, "owner").await.unwrap();
        owner.make_private(&room).await.unwrap();
        let mut form = step(&owner, &room, "member", None).await.unwrap().form.unwrap();
        form.fields
            .iter_mut()
            .find(|f| f.var.as_deref() == Some("jid"))
            .unwrap()
            .values = vec![guest.to_string()];
        assert!(step(&owner, &room, "member", Some(form.clone())).await.is_err());
        assert!(
            !owner
                .affiliated(&room, Affiliation::Member)
                .await
                .unwrap()
                .contains(&guest)
        );
        form.fields
            .iter_mut()
            .find(|f| f.var.as_deref() == Some("confirm"))
            .unwrap()
            .values = vec!["1".into()];
        let saved = step(&owner, &room, "member", Some(form)).await.unwrap();
        assert_eq!(saved.status, CommandStatus::Completed);
        assert!(
            owner
                .affiliated(&room, Affiliation::Member)
                .await
                .unwrap()
                .contains(&guest)
        );
    }

    #[test]
    fn privilege_changes_require_explicit_confirmation() {
        let mut form = DataForm {
            type_: DataFormType::Submit,
            title: None,
            instructions: None,
            fields: vec![confirm()],
        };
        assert!(require_confirmation(&form).is_err());
        form.fields[0].values = vec!["1".into()];
        assert!(require_confirmation(&form).is_ok());
    }
}
