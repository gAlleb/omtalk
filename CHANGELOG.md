# Changelog

## Unreleased

- A room's avatar set in `/room` is prepared as your own: its middle square (avatars are drawn round), at most 256×256, made smaller when it would be too large for the server. It was shrunk to 512 on its longer side, uncropped and of any size.
- `/nickname` and `/avatar` choose their account as the other commands do (the active one) and take `--account` for another.
- **`/avatar path`** sets your avatar from a picture: its middle square, at most 256×256, as PNG, made smaller still when it would be too large for a server; Gajim, Monal and Conversations show it at once (XEP-0084). `/avatar` alone takes it away. Tab completes the path.

## 0.1.10 — 2026-10-27

Voice messages in the terminal: play them with a waveform and record your own. Also copy and paste on X11, and omtalk quits even when the server is gone.

- Built-in streamed audio playback with play/pause, stop, elapsed/total time and a clickable waveform for seeking. Supports common audio attachments, including Ogg Opus voice messages; outgoing audio is playable before upload completes.
- Voice-message recording from the microphone button or `/record`, with preview, send/discard and a fixed recipient. Records mono Ogg Opus at 32 kbit/s through the existing upload/OMEMO path. `/play`, `/stop` and `/seek` provide keyboard control.
- Audio uses the PulseAudio/PipeWire socket directly, with bundled codecs and no external player/recorder or audio client libraries. CMake is a build-only dependency.
- Audio attachments retain a working `open` action and a separate `save` dialog with path completion, clipboard paste and explicit overwrite confirmation. Nerd Font mode uses playback glyphs; text mode retains ASCII controls.
- Temporary attachment download failures retry up to three times. Failed files have a clickable retry row; duplicate requests are coalesced, and incomplete responses are kept out of the cache.
- **Copy and paste on X11**: without Wayland, omtalk reads and writes the clipboard and the primary selection through `xclip` (pictures too) or `xsel` (text only); before, only copying worked there (OSC 52) and pasting had to go through the terminal. Programs are looked for on PATH, not run to probe.
- Bound account shutdown and room-entry sends when the server is unreachable. Quitting stops accounts concurrently, restores terminal modes before runtime teardown, and tolerates cancelled client receivers. UI event batches are limited so incoming events cannot indefinitely postpone keyboard input.
- The configured quit shortcut works inside dialogs, including settings and the emoji picker.
- IRC bridge history no longer duplicates live/MAM copies sharing a room archive ID when their sender JIDs or message IDs differ. Existing copies are hidden consistently in history, search and unread counts without deleting original rows or read positions.
- An idle omtalk no longer wakes hundreds of times a second: the audio worker sleeps while nothing plays or records, and a finished voice message lets go of its sound-server stream.
- Saving a file offers the downloads folder from the system's `~/.config/user-dirs.dirs` (it looked in omtalk's own config folder, so a renamed folder such as «Загрузки» was missed).
- The licence text (GPL-3.0, as published by the FSF) is in `LICENSE`; the licence itself is unchanged, GPL-3.0-or-later.

## 0.1.9 — 2026-10-27

- Rooms join independently during startup; archive catch-up preserves read positions instead of counting old history as new messages.
- `/room` opens room management: the server's full configuration form, owners/administrators/members/bans, affiliations, moderator and voice roles, kicks, topic and room avatar. Available actions depend on server permissions.
- Visiting unread chats with modifier+N returns to the original chat after the unread round trip.
- `:` opens a searchable contact and room switcher. `::text` sends `:text`; `//text` sends `/text`.
- `/np` shares the playing MPRIS track through `playerctl`. The hidden `/om` command sends `ॐ असतो मा सद्गमय`.
- An offline emoji picker beside the attachment and mute buttons, or `/emoji`: 375 common emoji, Russian/English search, categories, mouse and arrow navigation. Selecting inserts into the draft and keeps the picker open for more selections; Esc closes it.
- Wide and compound terminal cells are drawn with explicit cursor positioning to prevent emoji from corrupting the interface.
- HTTP/HTTPS links open in the default browser with an ordinary left click, including wrapped links. Uses `xdg-open`, independently of the file viewer setting.
- Touchpad history scrolling keeps focus on messages, preventing accompanying arrow events from switching chats. Optional diagnostics: `OMTALK_SCROLL_LOG`.
- GitHub Actions builds downloadable Linux artifacts after a push; a newer push cancels an unfinished build for the same branch.

- Per-chat notification preferences in `/settings chat`: inherit, all messages or mentions only, plus independent sound on/off/inherit. Mute overrides every signal. Choices persist locally per account and chat, including IRC bridges.
- Contacts declaring the standard XMPP `client/bot` identity get a robot icon in the chat list and header. Discovery is limited to viewed contacts and remembered locally; names are not used to guess.
- Jump to the first unread chat with modifier+N or prefix+N, including folded groups. The direct key is configurable through `unread_shortcut` or settings; Ctrl+A and reply on prefix+A are unchanged.
- Ready files, including MP4 videos, can be opened by clicking their name or “open” in compact mode. The sender name and attachment now have separate click areas.
- `End` returns a scrolled conversation to the latest message even with input focus; the “↓ End” label is clickable. At the bottom, input editing keeps its usual End key.
- Addressed commands accept `--account JID` immediately after the command name, with Tab completion and validation of configured accounts. Message text is preserved; chat-bound commands keep their own account.
- Commands use the active account instead of silently selecting one on the recipient’s server, including when several accounts share a server.
- Unencrypted messages and pictures sent to yourself merge with their own echo instead of appearing twice; live delivery now agrees with archive history about their outgoing direction.
- Loaded pictures retain the unencrypted mark, for both sent and received attachments in both layouts.
- An offline message received again from MAM is shown and counted once. This fixes duplicate unencrypted pictures on login; sending the same picture again remains a separate message.
- Optional image-link previews: enable “Preview image links” in `/settings` or `[ui] image_links = true`. Standalone HTTPS image links appear inline, including in local history, and follow `auto_download`. Off by default; uploaded attachments are unaffected.

## 0.1.8 — 2026-10-25

- **The cause of the startup read-mark timeout is fixed in the XMPP library.** An own-account IQ queued before login was tracked under no address; after login the server's successful reply was matched under the account address and discarded. The tracker now knows the account before any IQ can be queued. A regression test publishes before login to real local Prosody and verifies both acknowledgement and stored state.

- **Read-state synchronization waits for login and retries temporary failures.** Pending positions are coalesced per chat and published in order; one chat waiting for retry does not block another. Errors name the chat and say when a retry is scheduled, instead of losing the update after a timeout.

- The mute glyph has extra space before the unread badge, so wide Nerd Font glyphs do not paint over its background.

- **See who reacted**: click the reactions below a message, or `/reactions` for the selected message (else the latest with reactions). A small scrollable window lists the names for each emoji: room nicks in rooms, contact names in direct chats. Esc or a click outside closes it.

- **Read messages stay read after a restart, including IRC bridge messages with no live archive id.** The precise local read position is kept independently of server synchronization; an old server mark no longer makes messages read here unread again. New messages still count, and a newer known mark from another client advances the local position. Empty server mark ids are ignored.

- **The mute bell responds where it is drawn**: its mouse hitbox now includes the spacing between the input border's buttons. The mute and unencrypted marks in the chat list have a separating cell so Nerd Font glyphs can use their normal width.

- **Mute any chat** with `/mute`, restore it with `/unmute`, or click the bell beside the input's paperclip. Direct chats, XMPP rooms and IRC channels through biboumi all work alike: no sound, desktop notification or terminal bell, even for mentions. The chat list marks muted chats; messages and unread counts continue, and the choice survives a restart.
- **Reactions to your own encrypted room messages work too**: their echoed copy has no key for the sending device, so its room-assigned id is now taken before skipping its unreadable body. That id is also kept in the history instead of lost on a duplicate save.

- **Your messages in a room can be reacted to once echoed back**: their room-assigned id is retained, including when an upload finishes after the echo.

- **Reactions show under pictures and files too**, before and after download, in both layouts; they were sent correctly but missing from the attachment rows.

- **Every message shows its own name and time**, in both normal and compact layouts. Consecutive messages from the same person no longer look like paragraphs of one message.
- **A room is entered under the name you gave yourself** (`/nickname`) when it has no nick of its own (in its bookmark, or from last time), else under your address's name part, as Gajim does; `/nick` alone goes back to the same.

## 0.1.7 — 2026-10-24

Names as Gajim has them: yours and your contacts', set and taken away from omtalk; and who is who in a room, as IRC marks it.

- **All three names can be set and taken away**: `/nickname Name` is the name you give yourself for everyone (XEP-0172), `/name Name` in a direct chat the name you give the contact (in your roster, for all your clients), `/nick` in a room your nick there. Each without a name takes it away; `/nick` alone goes back to the default nick.
- **In a direct chat the contact writes under the name the chat list has for them** (the one in your roster, or the one they gave themselves), in the chat and in search results; it was the address's name part ("tanya" in the chat with "Mama").
- **In a direct chat your messages go by the name you gave yourself** (`/nickname`, read from the server), else your address's name part, as Gajim has it; "me" stays only in the chat with yourself.
- **In a room your messages go by your nick there**, from omtalk as from your other clients (Gajim, Monal), which the room names so; they were "me" from here and the nick from elsewhere. Those from your other clients get the ✓ too: the room sent them round, so they are in. As Gajim does.
- **A room's participants show their standing**, as IRC clients mark it: `~` owner, `@` admin (an IRC op through biboumi), `%` moderator (halfop), `+` member (voice); they are listed first, in that order. In the right pane only, not in the chat.
- English: "…before this device existed: they cannot be read here" (it said "them").

## 0.1.6 — 2026-10-24

What is read and what is delivered is now the same in omtalk, Gajim and Monal, after a restart too.

- **What is read is the same everywhere** (XEP-0490): on logging in omtalk takes from the server how far each chat was read, in Monal, Gajim or here, and shows as unread only what came after; a restart no longer makes everything look read. Opening a chat marks its newest message read for your other clients, messages from before this start too; Monal kept showing those as unread. And omtalk never moves that mark back: read further on another device than omtalk has yet, opening the chat here leaves it where it is.
- **Delivered and read are known after a restart**: a message read (or received) while omtalk was closed showed one grey tick for good; the server keeps the recipient's receipt and read marker in the archive, and omtalk now takes them from there on logging in, as it takes the messages.
- **Read means read up to there**: the recipient's client marks only the newest message it read, and that covers all before it (XEP-0333); omtalk showed only that one as read, the earlier ones kept one tick.
- **A line says where the new messages begin** ("── new ──") on opening a chat with unread ones, and the chat is shown from there when they do not all fit; it goes once the chat is left or one writes in it.
- **`compact = true`** (or "Time and name on the message's line" in the settings) puts the time and the name on the message's own line, names in one column, as IRC clients have it; a short message takes one line instead of two. Off by default.
- **A room's header has its name only**: its topic is in the right pane, whole; with that pane folded, the topic is beside the name as before.
- **The right pane of a room has no list of commands** any more (Tab completes them); members offline show by the name the chat list has for them (the roster's, or the one they gave themselves), without the address.

## 0.1.5 — 2026-10-23

- **Your status is remembered**: after `/away` and a restart you are still away.
- **A room that let you go is entered again** (XEP-0410): omtalk asks each room every five minutes, and after a connection comes back, whether you are still in it; a network that changed or a server restart could leave a room silent with nothing showing it. What was missed comes with entering again.
- **A room's participants show their status**: "bob · away: lunch", the dot in another colour.
- **Contacts with no name in your roster go by the one they gave themselves** (XEP-0172), as Conversations and Dino show it.
- **`/search words`** finds them in the open chat's history, whatever the case, encrypted messages too; Enter or a click shows the chat from that message.
- **An IRC channel through biboumi shows its participants**: biboumi names them by an address without a resource, which omtalk refused, dropping all but yourself; it reads a room's word about its occupants leniently now, as Gajim does.
- A reaction shows its count even when one person sent it ("👍 1").
- **Your own messages in an IRC channel through biboumi no longer show twice.** biboumi names you by an address of its own and gives you the nick set in it (status 210); omtalk now knows itself by the room's "this is you" and takes the nick the room gave, so your messages coming back are yours, and the room check asks after the right nick.
- **OMEMO turned off in a chat stays off** after a restart (and on, if turned on).
- **A room closed stays closed** after a restart: `/close` hides it here only (other clients still enter it), `/leave` leaves for good, turning off "enter by itself" in its bookmark for all your clients. `/join` opens it again.
- **Folded folders and accounts stay folded** after a restart.
- **The buttons of a service's form are reached with Tab** after the last field; ←/→ go between them, Enter presses the one picked; the keys are told under the field's description, not instead of it.
- **`/remove`** takes a contact out of your roster: the open chat's, or the address given. A contact removed (here or in another client) shows by its address again.
- **Rooms and contacts show their names in the list from the start**: kept from the last time, asked again after logging in (a room's on entering it, the roster's and contacts' own).
- **omtalk opens the chat you were in when you left it**; with none to remember, the server channel. It used to open whichever room came first.
- **In a chat with OMEMO off the unencrypted mark is the lock-open icon alone** (an IRC channel had "unencrypted ·" on every line); in a chat with OMEMO on a plain message keeps the icon and the words.
- **The buttons below a service listing too** (Back, Reload, Close): Tab reaches them, ←/→ and Enter; each shows its key.
- **Folders change places**: drag a folder's heading on to another, or `/folder-up`, `/folder-down` for the open chat's folder. The server channel stands right under its account, before the folders.
- **A chat closed once and opened again with `/msg` or a click stays in the list** after a restart; it used to need a message in it first.
- The right-click reactions draw evenly: 💖 instead of ❤️, which is two code points that tmux and terminals draw one or two cells wide (`/react heart` still sends ❤️).

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
