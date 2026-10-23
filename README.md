# omtalk

**English** · [Русский](README.ru.md)

A terminal XMPP client with OMEMO that works in direct and group chats as reliably as
Gajim.

## Development setup

Every tool version is pinned in the repository (`mise.toml`, `rust-toolchain.toml`,
`tests/oracle/uv.lock`):

```sh
git clone ssh://git@git.3s0.ru:2233/stefan/omtalk.git && cd omtalk
mise trust && mise install
mise run test
```

`mise run test` also starts a test Prosody in Docker (`tests/prosody`). It listens on
`127.0.0.1:15222` only and starts with empty data every time. The accounts `alice`…`frank`
and `u01`…`u60` `@localhost` all have the password `test`. `admin@localhost` is a server
administrator, for testing ad-hoc commands. The upload service is `upload.localhost`, with
HTTP on `127.0.0.1:15280`.

## Running

Binaries to hand out: `mise run dist` puts static `omtalk-x86_64` and `omtalk-aarch64`
into `dist/`. They run on any Linux, with no libraries needed. ARM only (Raspberry Pi 4/5
and the like): `mise run build-arm`. You get one static file,
`target/aarch64-unknown-linux-musl/release/omtalk`, which runs on any aarch64 Linux with
no libraries. The C compiler for this build is the zig from `mise.toml`; nothing is
installed system-wide.

```sh
cargo build --release -p omtalk
target/release/omtalk you@example.org      # one account
target/release/omtalk                      # accounts from ~/.config/omtalk/config.toml
```

```toml
# ~/.config/omtalk/config.toml
[[account]]                               # as many accounts as you like
jid = "you@example.org"
password_command = "pass show xmpp/you"   # otherwise the password is asked for on start
# host = "xmpp.example.org"               # connect to this host, without SRV (STARTTLS)
# port = 5222
# direct_tls = "xmpp.example.org:5223"    # or TLS from the start

[ui]
leader = "ctrl+x"      # prefix key, as in tmux: press, release, then a letter
modifier = "ctrl"      # ctrl, alt, super or none: the same shortcuts while held
icons = "nerd"         # nerd (Nerd Font glyphs) or text
left = 26              # width of the chat list
right = 30             # width of the right pane
viewer = "xdg-open"    # what opens a file you click
auto_download = true   # false: received files only on a click ("скачать")
mouse = true           # false: the mouse belongs to the terminal (tmux-style selection); prefix+m toggles
read_markers = true    # false: the other side does not see that you read their messages
typing = true          # false: the other side does not see that you are typing
avatars = false        # true: the contact's or room's avatar in the right pane
language = "en"        # the interface's language: en or ru

[notify]               # each one: all, private, mention, private+mention or none
sound = "canberra-gtk-play -i message-new-instant"   # any command; "" for no sound
sound_on = "private+mention"   # direct messages and your nick named in a room
desktop_on = "private+mention" # notify-send
bell_on = "all"                # terminal bell (\a): tmux and window managers mark the window by it

[theme]                # optional: your own colour for any role; the rest come from the terminal palette
accent = "#7aa2f7"     # a name (blue, bright-red, gray), #rrggbb or a number 0–255; default is the terminal's
# text, background, dim, border, ok, warn, outgoing work the same way
# selection_text = "black"  selection_background = "yellow"   # the selected row
# nicks = ["red", "green", "#ff8800"]                         # nick colours, used in turn
```

The interface speaks English; `language = "ru"` in `[ui]` (or «Language / Язык» in the
settings) switches it to Russian, at once.

## Settings

`[ui]` and `[notify]` can also be changed inside omtalk: `/settings` or prefix+`p`. The
arrow keys select, `Enter` or a click changes, `←`/`→` step through the values. A change
takes effect at once and is written to `config.toml`; the comments in it are kept. Colours
(`[theme]`) and accounts are edited in the file only.

## Layout and keys

Three columns: chats, the conversation, and the room's participants or the contact's
devices. With several accounts, the chats on the left are grouped by account. A click on
an account heading (or `Space` in the chat list) folds its chats; a folded account shows
its unread count. `z` keeps only the current account open; `z` again opens them all.
Commands go from the account on the contact's server, otherwise from the last one picked
(it is underlined). A click on a nick in a room, or on a participant, opens a direct chat
with them.

Your own folders, inside each account (omtalk only; rooms can go in them too): the list on
the left is a tree of account → folders → chats. `/folder NAME` puts the current chat into
a folder of its account (creating it if needed); `/folder` with no name takes it out. You
can also drag a chat with the mouse onto a folder heading, or back onto the account. `m` in
the chat list starts the same command. A folder folds like an account. An empty folder
disappears. `/rename-folder OLD NEW` (for names with spaces: `old folder → new`).

Colours come from the terminal palette (pywal and similar tools recolour omtalk along with
everything else), and `[theme]` can give any role a colour of your own.

Mouse: click a chat, drag pane borders, `‹`/`›` hide the panes, a click on the lock by the
input line turns OMEMO on and off for the chat, the wheel scrolls.

Keys: the `leader` prefix (Ctrl+X), release, then a letter. A list pops up after the
prefix: `o` OMEMO, `b`/`r` left and right panes, `↑`/`↓` chats, `←`/`→` width of the chat
list, `s` services, `p` settings, `z` current account only, `q` quit. The same letters work
with `modifier` held, except those the input line needs. `Tab` switches focus,
`PgUp`/`PgDn` scroll. In a room, `Tab` after the input line goes to the participants on
the right: `↑`/`↓` (`k`/`j`) pick someone, `Enter` opens a direct chat with them, `Esc`
goes back to the input line.

The input line edits like a shell: Ctrl+W/U/K cut a word, to the start, to the end; Ctrl+Y
pastes what was cut; Ctrl+A/E go to the start and end; Ctrl+←/→ and Alt+B/F move by words.
Alt+Enter (or Shift+Enter, where the terminal tells it apart) starts a new line in the
message. Pasted text with several lines goes as one message, indentation kept; the input
line grows up to 8 lines. Errors are shown in full in the bottom line and fade after 10 s;
they stay in full in the account's server channel.

## Commands

Typing `/` shows the list of commands above the input line, with descriptions (`/help`
does the same). `Tab` completes a command, a nick in a room (at the start of a line, with
`: `), an address after `/msg`, `/add` and so on, a room after `/join`, a folder after
`/folder`. `Tab` again goes through the choices, `Shift+Tab` back. In an empty line `Tab`
switches focus.

Commands: `/msg`, `/join`, `/leave`, `/add`, `/accept`, `/refuse`, `/devices`,
`/services [address]`, `/folder`, `/rename-folder`, `/create`, `/invite`, `/topic`,
`/name`, `/nick`, `/reply`, `/me`, `/keys`, `/upload`, `/commands`, `/settings`, `/close`, `/help`, `/quit`.

A chat is closed with `/close`, prefix+`w` or a middle click on it in the list: it leaves
the list, its history stays, and it comes back by itself when someone writes in it.
Closing a room leaves it.

`/join` without a nick takes the nick from the account's bookmarks (XEP-0402), as Gajim and
other clients do, so all clients of one account count as one participant in the room. A
new room is added to the bookmarks automatically.

## Message styling

Messages are styled as in Conversations, Gajim, Monal and Dino (XEP-0393): `*strong*`,
`_emphasis_`, `~strike~`, `` `code` ``, a block of preformatted text between ```` ``` ````
lines, and `>` quotes anywhere in a message. The directives stay visible, dimmed, so
copied text is what was written; nothing is styled inside code.

`/me waves` goes as written and is shown as `* nick waves`, as in IRC and Gajim
(XEP-0245).

## Files and pictures

Sending: `/upload path` (Tab completes the path), the paperclip by the input line, a file
dropped onto the terminal window (a ready command appears; press Enter), or Ctrl+V with a
picture in the clipboard. The file goes to your server's upload service (XEP-0363). In an
encrypted chat it is encrypted first, and the other side gets an `aesgcm://` link
(XEP-0454), as from Gajim, Conversations and Monal. The line shows the upload percentage,
then the usual ✓ and ✓✓. Your own picture is shown at once, without downloading it again.

Received files and pictures are downloaded automatically; with `auto_download = false` in
`[ui]` only on a click (the file says «скачать», "download"). Files already downloaded are
shown at once.

## Replies and quotes

Replies (XEP-0461), as in Conversations, Gajim and Monal: double-click a message,
prefix+`a`, or `r` in the conversation (a reply to the last incoming message), or
`/reply`. The part of a message selected with the mouse is what gets quoted. While you
write the reply, the input line shows what it answers; Esc cancels. The quote goes as
`> ` lines at the start of the text (visible in any client) along with a mark saying which
message it answers. The first line of the quote is the author's nick (`> bob:`), so it is
clear whose words they are even in clients without replies. omtalk shows the quote as a
block with a bar on the left and the name of the person answered.

## OMEMO keys

As in Gajim: `/keys [address]`, prefix+`e`, or a click on the devices to the right of a
direct chat. You see the contact's devices and your own, with fingerprints and status: `●`
blind trust, `✓` verified, `✗` distrusted or undecided. Compare a fingerprint with what the
contact's client shows, then mark the device verified (`v`). After the first verification,
new devices of that contact wait for your decision and get no messages ("blind trust
before verification", as in Gajim and Conversations). `d` distrusts, `b` goes back to blind
trust, `x` removes an old device of your own from the list (press twice). A message from
an unverified or distrusted device is marked `⚠`. Beside each device: when it last wrote
(«писало вчера 14:05», "wrote yesterday 14:05"), so a device silent for months stands out.

## Mouse and clipboard

Drag across messages to select and copy: only the message text, without borders or
neighbouring panes, straight into both the clipboard and primary selection (through
`wl-copy`, or without Wayland through an OSC 52 request to the terminal; in tmux that
needs `allow-passthrough on`). The middle button pastes primary, Ctrl+V in the input line
pastes the clipboard (through `wl-paste`). Prefix+`m` (or `mouse = false` in `[ui]`) hands
the mouse to the terminal: then tmux and the terminal select and paste on their own, as in
WeeChat without the mouse; prefix+`m` takes it back. Shift with the mouse always works:
the terminal takes the mouse for that moment.

## Server channel

Each account has a server channel at the top of the list: logins and reconnects, your own
OMEMO device with its fingerprint, the result of catching up from the archive, joined
rooms, your other clients coming and going, errors, subscription requests, and messages
from the server itself. The right pane shows the account and which of its clients are
online. Only commands can be typed there.

## Notifications

While the window is not in focus, messages are not marked as read (neither for the sender
nor for your other clients) until you come back, and they count as unread even in the open
chat.

Three signals, each set separately in `[notify]`: a sound (any command), a desktop
notification (`notify-send`) and the terminal bell (`\a`). For each one, which messages:
direct ones (`private`), your nick named in a room (`mention`), both, all (`all`) or none
(`none`). No signal while you are looking at that chat. History loaded from the server's
archive gives no signal. In tmux you can turn the bell off (`bell_on = "none"`) and keep the
sound and notifications. The terminal reports focus by itself (foot, kitty, Alacritty); in
tmux this needs `set -g focus-events on`, and without it omtalk treats the window as always
active.

## Typing

"Typing…" (XEP-0085): while the contact types, the bottom of the conversation shows
«bob печатает…» ("bob is typing…"), and the chat list shows `…` by the chat. omtalk reports
its own typing too: started is "composing", 10 s without keys is "paused", sent is the end.
`typing = false` in `[ui]` turns it off.

## Rooms

`/create name [Title]` creates a closed room on your server's room service (members only,
with visible addresses, as OMEMO needs, persistent) and joins it. `/invite address` makes
someone a member and invites them (a direct invitation, XEP-0249, as in Gajim and
Conversations). An invitation sent to you appears in the server channel with a ready
`/join` command.

A room shows its title on the left and in the header, with its topic next to it; a topic
change shows in the chat. `/topic text` changes the topic, `/name Title` the title (owner
only). `/nick nick` changes your nick in the room; other people's nick changes show in the
chat («alice теперь алиса», "alice is now алиса"), and the person stays the same in the
list.

The right pane of a room shows the topic and the room's address at the top, then who is in
the room now, then «не в сети» ("offline"): members (owners, admins, members) who are not
in the room. A click on them opens a direct chat. Open rooms may not show their member
lists; then that section is missing. At the bottom there is a hint with the room commands
(`/topic`, `/name`, `/invite`, `/nick`, `/close`).

## Presence and delivery

Who is online and on what: a direct chat's `@` icon is green while the contact is online.
The chat header and the right pane show which clients they use (Gajim, Monal,
Conversations, omtalk…), their state and status message. The client is recognised by its
caps (XEP-0115).

Your own messages show how far they got: `⋯` sending, `✓` on the server (in a room: the
room sent it back), `✓✓` delivered to the contact's client (XEP-0184), highlighted `✓✓`
read (XEP-0333), `✗` not sent, with the reason. omtalk sends receipts itself. omtalk marks
the open chat as read: for the contact (`read_markers = false` in `[ui]` turns this off) and
for your other clients (XEP-0490). Gajim, Monal and Conversations then clear their unread
counts, and what you read in them clears it in omtalk.

## Avatars

As in Gajim: `avatars = true`, or «Аватарки» in the settings; off by default. The contact's
or room's avatar is shown at the top of the right pane. It comes from where other clients
get it: PEP (XEP-0084, changes arrive by themselves), otherwise the vCard
(XEP-0054/0153); a room's comes from its vCard. Downloaded avatars are kept in
`~/.cache/omtalk/avatars` under their SHA-1. Avatars are round: in kitty a real picture,
in Alacritty a circle of half blocks. Desktop notifications carry them too: the person's
from a direct chat, the room's with the writer's small in its corner from a room (when the
room tells who wrote).

## Services

Like "Discover Services" and "Execute Command" in Gajim. The window shows what the server
(or any address) offers: services, their nodes and ad-hoc commands (XEP-0050). A command
opens a form (XEP-0004): text, password (hidden), checkbox, choice from a list, several
values. Keys: `Enter` opens or goes to the next step, `←`/`Backspace` goes back, `Tab`
goes to the next field, `Space` toggles a checkbox or choice, `Esc` cancels the command on
the server and closes. Rows, fields and buttons work with the mouse. An administrator of
their own server can manage it from here: users, passwords, modules, statistics.

A chat's own commands, as Gajim's "Execute command": `/commands` or prefix+`c`. A room's
(a bridge's channel too), a contact's client's while it is online, the own server's from
the server channel. In a biboumi channel (`#chan%irc.server@biboumi…`) `←` goes on to the
IRC server's commands: port, TLS, password, nick, keeping the channel joined. A result
with several items (XEP-0004 `<reported>`) is a table; the arrows scroll a long one.

## Testing against other clients: omtalk-cli

A line-based OMEMO client for live testing with Gajim, Monal, Conversations and Dino:

```sh
cargo build --release -p omtalk-cli
target/release/omtalk-cli you@example.org
```

The password comes from `OMTALK_PASSWORD` or is asked for. Data lives in
`~/.local/share/omtalk/omtalk.db` (`--db` changes the path), so every run is the same
device. The server is found through the domain's SRV records; `--host HOST [--port PORT]`
sets STARTTLS by hand, `--direct-tls HOST:PORT` direct TLS. Commands: `/msg`, `/join`,
`/say`, `/devices` (fingerprints shown as in Gajim), `/help`, `/quit`.

## Structure

| Directory | What is there |
|---|---|
| `crates/omemo` | OMEMO 0.3: X3DH, Double Ratchet, the libsignal format, AES-GCM payload. No XML, no network |
| `crates/omemo-xml` | OMEMO on the wire: devicelist, bundle, `<encrypted>` on top of xmpp-parsers types; encrypting one message for many devices |
| `crates/xmpp` | connections for several accounts, password check, keepalive, the replies a client owes (disco, ping, caps), PEP, MUC (joining, closed rooms, participants), message archive (MAM), avatars |
| `crates/omemo-client` | OMEMO for an account: own device in the list, carbons, encryption for all of the contact's devices and your own, groups (all members, absent ones included), BTBV trust, catching up from the archive |
| `crates/store` | the client's SQLite file: OMEMO state of each account (keys, sessions, trust), history, rooms, folders |
| `crates/testkit` | tests only: a "device", a client with its own connection and keys, against the test Prosody |
| `crates/tui` | `omtalk`: the terminal interface (ratatui) |
| `crates/media` | shared files: `aesgcm://` (XEP-0454) and `https://`, download, decryption, cache |
| `crates/cli` | `omtalk-cli`: the line-based client for live testing |
| `crates/omemo-oracle` | tests only: starting the reference device and talking to it |
| `tests/prosody` | a disposable Prosody 13 for integration tests |
| `tests/oracle` | the reference device on python-omemo; `crates/omemo/tests/oracle.rs` checks against it byte for byte |

## License

GPL-3.0-or-later.
