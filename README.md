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

## Устройство

| Каталог | Что там |
|---|---|
| `crates/omemo` | OMEMO 0.3: X3DH, Double Ratchet, формат libsignal, полезная нагрузка AES-GCM. Без XML и сети |
| `tests/oracle` | эталонное устройство на python-omemo; `crates/omemo/tests/oracle.rs` сверяется с ним байт в байт |
