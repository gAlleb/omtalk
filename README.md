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

## Устройство

| Каталог | Что там |
|---|---|
| `crates/omemo` | OMEMO 0.3: X3DH, Double Ratchet, формат libsignal, полезная нагрузка AES-GCM. Без XML и сети |
| `crates/omemo-xml` | OMEMO на проводе: devicelist, bundle, `<encrypted>` поверх типов xmpp-parsers; шифрование одного сообщения для многих устройств |
| `crates/xmpp` | соединения для нескольких аккаунтов, проверка пароля, keepalive, обязательные ответы клиента (disco, ping, caps), PEP, MUC (вход, закрытая комната, участники), архив сообщений (MAM) |
| `crates/omemo-client` | OMEMO для аккаунта: своё устройство в списке, carbons, шифрование для всех устройств собеседника и своих, группы (все участники, включая отсутствующих), доверие BTBV, догрузка пропущенного из архива |
| `crates/store` | SQLite-файл с данными клиента: пока состояние OMEMO каждого аккаунта (ключи, сессии, доверие) |
| `crates/testkit` | только для тестов: «устройство» — клиент со своим соединением и ключами против тестового Prosody |
| `crates/omemo-oracle` | только для тестов: запуск эталонного устройства и разговор с ним |
| `tests/prosody` | одноразовый Prosody 13 для интеграционных тестов |
| `tests/oracle` | эталонное устройство на python-omemo; `crates/omemo/tests/oracle.rs` сверяется с ним байт в байт |
