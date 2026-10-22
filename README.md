# omtalk

Терминальный XMPP-клиент с OMEMO, который работает в личных и групповых чатах так же
надёжно, как Gajim.

## Окружение

Все версии инструментов зафиксированы в репозитории (`mise.toml`, `rust-toolchain.toml`,
`tests/oracle/uv.lock`):

```sh
git clone ssh://git@git.3s0.ru:2233/stefan/omtalk.git && cd omtalk
mise trust && mise install
mise run test
```

`mise run test` поднимает и тестовый Prosody в Docker (`tests/prosody`, слушает только
`127.0.0.1:15222`, каждый запуск с пустыми данными, аккаунты `alice`…`frank` и `u01`…`u40` `@localhost` с паролем `test`).

## Запуск

```sh
cargo build --release -p omtalk
target/release/omtalk you@example.org      # один аккаунт
target/release/omtalk                      # аккаунты из ~/.config/omtalk/config.toml
```

```toml
# ~/.config/omtalk/config.toml
[[account]]
jid = "you@example.org"
password_command = "pass show xmpp/you"   # иначе пароль спросят при запуске

[ui]
modifier = "ctrl"   # ctrl, alt или super — клавиша всех сочетаний
icons = "nerd"      # nerd (значки Nerd Font) или text
left = 26           # ширина списка чатов
right = 30          # ширина правой панели
viewer = "xdg-open" # чем открывать файл по клику
```

Три колонки: чаты, переписка, участники или устройства собеседника. Цвета берутся из
палитры терминала. Мышь: клик по чату, перетаскивание границ панелей, `‹`/`›` прячут
панели, клик по замку у поля ввода включает и выключает OMEMO для чата, колесо листает.
Клавиши (с `modifier`): `O` — OMEMO, `B`/`R` — левая и правая панели, `↑`/`↓` — чаты,
`←`/`→` — ширина списка, `Q` — выход; `Tab` — фокус, `PgUp`/`PgDn` — листать. Команды в
поле ввода: `/msg`, `/join`, `/leave`, `/add`, `/accept`, `/refuse`, `/devices`, `/help`.

## Проверка с другими клиентами: omtalk-cli

Строчный OMEMO-клиент для живой проверки с Gajim, Monal, Conversations и Dino, пока
интерфейса нет:

```sh
cargo build --release -p omtalk-cli
target/release/omtalk-cli you@example.org
```

Пароль берётся из `OMTALK_PASSWORD` или спрашивается. Данные лежат в
`~/.local/share/omtalk/omtalk.db` (`--db` меняет путь), поэтому каждый запуск — то же
устройство. Сервер находится по SRV-записям домена; `--host HOST [--port PORT]` задаёт
STARTTLS вручную, `--direct-tls HOST:PORT` — прямой TLS. Команды: `/msg`, `/join`, `/say`,
`/devices` (отпечатки в виде, как у Gajim), `/help`, `/quit`.

## Устройство

| Каталог | Что там |
|---|---|
| `crates/omemo` | OMEMO 0.3: X3DH, Double Ratchet, формат libsignal, полезная нагрузка AES-GCM. Без XML и сети |
| `crates/omemo-xml` | OMEMO на проводе: devicelist, bundle, `<encrypted>` поверх типов xmpp-parsers; шифрование одного сообщения для многих устройств |
| `crates/xmpp` | соединения для нескольких аккаунтов, проверка пароля, keepalive, обязательные ответы клиента (disco, ping, caps), PEP, MUC (вход, закрытая комната, участники), архив сообщений (MAM) |
| `crates/omemo-client` | OMEMO для аккаунта: своё устройство в списке, carbons, шифрование для всех устройств собеседника и своих, группы (все участники, включая отсутствующих), доверие BTBV, догрузка пропущенного из архива |
| `crates/store` | SQLite-файл с данными клиента: пока состояние OMEMO каждого аккаунта (ключи, сессии, доверие) |
| `crates/testkit` | только для тестов: «устройство» — клиент со своим соединением и ключами против тестового Prosody |
| `crates/tui` | `omtalk`: интерфейс в терминале (ratatui) |
| `crates/media` | общие файлы: `aesgcm://` (XEP-0454) и `https://`, загрузка, расшифровка, кэш |
| `crates/cli` | `omtalk-cli`: строчный клиент для живой проверки |
| `crates/omemo-oracle` | только для тестов: запуск эталонного устройства и разговор с ним |
| `tests/prosody` | одноразовый Prosody 13 для интеграционных тестов |
| `tests/oracle` | эталонное устройство на python-omemo; `crates/omemo/tests/oracle.rs` сверяется с ним байт в байт |
