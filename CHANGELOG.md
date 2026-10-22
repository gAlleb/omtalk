# Changelog

## Unreleased

- **The interface speaks English by default**; `[ui] language = "ru"` or the settings window switches it to Russian, at once.
- **Message styling (XEP-0393)** as in Conversations: `*strong*`, `_emphasis_`, `~strike~`, `` `code` ``, preformatted blocks and `>` quotes.
- **Avatars in desktop notifications**: the person's, or the room's with the writer's in its corner.
- **When each OMEMO device last wrote**, in the keys window and beside a chat.
- **Delivery ticks survive a restart.**
- **A direct chat new to this device reads its history** from the server's archive; what was encrypted before the device existed is counted.
- **Room participants can be picked with the keyboard** (Tab after the input line).
- **Leaving is clean** on a closed terminal window (SIGHUP) or SIGTERM, and each device keeps one resource: omtalk no longer lingers online for contacts.
- **A password changed elsewhere stops the account and says so**, instead of reconnecting forever.
- **The services window wraps** long rows, values and notes; results show each value on its own row, and a result with several items (XEP-0004 `<reported>`) is a table that scrolls. Such results were not shown at all before.
- Notifications: history loaded from the archive no longer notifies; a mention in a room sounds again; a room is named by its title.

## 0.1.0 — 2026-10-22

The first release of omtalk, a terminal XMPP client with OMEMO for direct and group chats.

Static binaries for Linux, no libraries needed:

- `omtalk-x86_64`
- `omtalk-aarch64` (Raspberry Pi 4/5 and other 64-bit ARM)

Tested live with Gajim, Monal and Conversations. Not yet tested with Dino.

### What it does

- **OMEMO 0.3** in direct chats and closed rooms, with its own Rust implementation checked byte for byte against python-omemo. Messages are encrypted for all of a contact's devices and your own, and carbons keep your other clients in sync.
- **Key management as in Gajim**: fingerprints, verify / distrust / blind trust (BTBV), and removing your own old devices. Messages from unverified devices are marked.
- **Several accounts on several servers** at once, grouped in the chat list, plus your own folders.
- **Rooms**: create closed rooms ready for OMEMO, invite people, set title and topic, change nick, see members who are offline. Nicks come from your bookmarks, so all your clients count as one participant.
- **History** from the server archive (MAM), for chats and rooms, including what came while you were away.
- **Files and pictures**: HTTP upload, encrypted `aesgcm://` links in encrypted chats, pictures shown inline (kitty graphics, block characters elsewhere), with upload and download progress.
- **Replies and quotes** (XEP-0461), shown as a block, readable in clients without reply support.
- **Delivery and read state**: sending, on the server, delivered, read, failed with the reason. Read state is synced with your other clients (XEP-0490).
- **Typing notifications**, presence with the client name (Gajim, Monal, Conversations…), and a server channel per account with logins, reconnects, errors and invitations.
- **Notifications**: sound command, `notify-send` and the terminal bell, each set separately for direct messages, mentions, all or none. Nothing is marked read while the terminal window is not in focus.
- **Mouse and keyboard on equal terms**: clicks, dragging pane borders and chats, selection that copies only message text, a tmux-style prefix key and a configurable modifier, and a shell-like input line.
- **Settings window** (`/settings`): changes take effect at once and are written to `config.toml`, keeping your comments.
- **Avatars** (off by default), round, from PEP or the vCard.
- **Colours** from the terminal palette (works with pywal), or your own in `[theme]`.
- **Server services and ad-hoc commands**, as in Gajim, with forms. Server administrators can manage users and modules from here.

### Known limitations

- The interface is in Russian only for now.
- Delivery ticks are not kept across restarts.
- A direct chat opened for the first time does not load its older history from the server.
- Participants in a room can be picked with the mouse only, not with the keyboard.
- Service forms show no result tables, and multi-value fields are edited on one line.
- A password changed on the server while omtalk runs goes unnoticed until restart.
- Dropping a file onto the window works in kitty and in tmux, not in Alacritty.

### Configuration

`~/.config/omtalk/config.toml`; see the README for a full example. Run `omtalk you@example.org` to start with one account and no config file.

License: GPL-3.0-or-later.
