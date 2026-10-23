# Changelog

## Unreleased

- **Your status is remembered**: after `/away` and a restart you are still away.
- **A room that let you go is entered again** (XEP-0410): omtalk asks each room every five minutes, and after a connection comes back, whether you are still in it; a network that changed or a server restart could leave a room silent with nothing showing it. What was missed comes with entering again.
- **A room's participants show their status**: "bob · away: lunch", the dot in another colour.
- **Contacts with no name in your roster go by the one they gave themselves** (XEP-0172), as Conversations and Dino show it.
- **`/search words`** finds them in the open chat's history, whatever the case, encrypted messages too; Enter or a click shows the chat from that message.
- **An IRC channel through biboumi shows its participants**: biboumi names them by an address without a resource, which omtalk refused, dropping all but yourself; it reads a room's word about its occupants leniently now, as Gajim does.
- A reaction shows its count even when one person sent it ("👍 1").
- **Your own messages in an IRC channel through biboumi no longer show twice.** biboumi names you by an address of its own and gives you the nick set in it (status 210); omtalk now knows itself by the room's "this is you" and takes the nick the room gave, so your messages coming back are yours, and the room check asks after the right nick.
- **OMEMO turned off in a chat stays off** after a restart (and on, if turned on).
- **The buttons of a service's form are reached with Tab** after the last field; ←/→ go between them, Enter presses the one picked; the keys are told under the field's description, not instead of it.

## 0.1.4 — 2026-10-23

- **Corrections (XEP-0308)**: `/edit` corrects your last message; a corrected message, yours or another's, changes in place, marked "edited", also in encrypted chats and after a restart.
- **Reactions (XEP-0444)**: `/react 👍` on the selected or the last message, again to take it back; others' show under their message with counts, kept after a restart.
- **A right click on a message offers reactions** (👍 ❤️ 😂 😮 😢 🙏); `/react` also takes names (`:+1:`, `heart`, `joy`…).
- **Your status**: `/away`, `/dnd`, `/online`, with words of your own, for all accounts; rooms you are in see it too, and it survives a reconnect.
- **Older history as you scroll up** (or click "↑ earlier"): 50 at a time, from what this device keeps, then from the server's archive.
- **Contacts go by their names from the roster** in the chat list and the chat's title; a rename in another client shows at once.
- **A long absence does not flood a room**: all that was missed is read and kept, but the room shows its newest few hundred, the rest comes on scrolling up; the bottom line counts the reading. Messages from an archive stand in their place by time.
- **What was missed in a room shows as it is read**, page by page, not all at once at the end: a long absence (hundreds of encrypted messages) no longer leaves the room looking empty for a while.
- **Every letter typed away from the input line goes into it**: the panes no longer keep `j`/`k`, `g`, `r`, `z`, `m` or Space; they move with the arrows, Home/End and the mouse (a reply: prefix+`a`, `/reply` or a double click; folding: `←`; only the current account: prefix+`z`).
- **Ctrl+C quits only when pressed twice** within three seconds; the first says so.
- The mouse wheel over a room's participants goes through them, as the arrow keys do.
- **A broken OMEMO session puts itself right.** Two devices that started a session with each other at once (each before the other's first message arrived) ended up in different sessions, and every later message showed as "could not be decrypted". omtalk now keeps the sessions a newer one replaced (up to five per device) and tries them, as libsignal does. A device whose message no session here can read gets a new session and an empty message, so only that one message is lost (at most once an hour per device).
- **Nothing read from an archive is lost when a later page fails**: before, all that had been read was thrown away while the place to go on from had already moved past it, so those messages never showed.
- A room whose archive was empty on the first visit is read from its start the next time, not just its last 50 messages.
- A chat found in the archive by a new device stays in the list after a restart, even with nothing readable in it.
- Bookmarks are read once at login, not once per room; one that could not be read is never written over.
- A request whose answer never comes no longer keeps what was waiting for it.

## 0.1.3 — 2026-10-23

- **A request no longer goes unanswered at the start of a session.** tokio-xmpp 6.0.0 could leave a server's answer unread until the request timed out (its receiving half lost a wakeup while a send was under way). omtalk carries the library with that fixed (`vendor/tokio-xmpp`). This was behind keys loading slowly, a device announced late, and some "could not be decrypted" on a first message.
- **A new device finds your rooms and chats**: rooms come from the account's bookmarks (autojoin), as Gajim and Conversations keep them, and the chats from the last 100 messages of the archive. Before, the rooms list stayed empty until `/join`.
- **Bookmarks are no longer lost** when omtalk was the first client to bookmark a room: the server kept only the last one, for all your clients. A node made that way is put right.
- **Replies, the ⚠ of an untrusted device and room message ids survive a restart.**
- **A file is kept under its own name**, so a viewer and "save as" show it, not a hash. Files kept before are moved, not fetched again.
- **A click opens any file** with the viewer again (the limit to known kinds is gone).
- An avatar no longer disappears when the server is slow to answer for it.
- The OMEMO device is announced again when the server does not answer at login.
- An account whose connection task fails connects again instead of staying offline.
- A picture pasted from the clipboard is named `picture-<time>.png` in any language.
- A contact's new device is taken at once, as Gajim and Conversations take it.
- **A link is a link**: a page such as `https://…/notes.md` is shown, never fetched. Files are `aesgcm://` links and links their sender marked as files (XEP-0066), as Conversations, Gajim and Monal send uploads.
- **Nobody can pass for someone else in a room**: only rooms you are in may say who their occupants are; a message in a room's archive carrying a writer of its own has no known writer.
- **Only the recipient marks your message delivered, read or refused**, and carbons count only from your own server.
- Files: avatar ids are only hashes (never paths), nothing is fetched on start with `auto_download = false`, the cache is yours alone (0700), downloads time out.
- **One omtalk per account**: a second one with the same account and database used to knock the first offline (both are the same device); it now says so and how to start it with a database of its own (`--db`). Different accounts still run side by side.
- The keys of a chat show at once from what is known, then refresh from the server.
- Message styling no longer slows down on long lines.
- Typing anywhere types into the input line.
- A temporary login failure is retried; the file viewer leaves no zombies; notification text is escaped; a panic quits instead of leaving a broken terminal.
- **Your own device list is never published over when it could not be read**: a server error at login made omtalk publish a list of itself alone, dropping your other clients from OMEMO.
- **A new session is kept before its one-time key is spent**: a network error in between lost the first message.
- **Sending without a connection no longer freezes the account** (and quitting); the message waits, shown as "waiting for the connection", and goes when the connection is back.
- **✓ means the server has it** (XEP-0198 acknowledgement), not that the bytes left; a message dropped with a refused session resumption is shown as lost.
- **The chat list scrolls**, keeping the chat picked in sight.
- A chat closed and opened again shows its history, and messages are no longer stored twice.
- A refused command leaves the line in the input, to be put right.
- `/me` from you shows your nick (in a room) or your account's name, not "me".
- **↑/↓ in the input line go through what was sent** since omtalk started, as in a shell.
- **`/me`**, as in IRC (XEP-0245): sent as written, shown as `* nick text`, in notifications too.

## 0.1.2 — 2026-10-23

- **A chat's own commands**, as Gajim's "Execute command": `/commands` or prefix+`c`; in a biboumi channel `←` goes on to the IRC server's commands.

## 0.1.1 — 2026-10-23

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
