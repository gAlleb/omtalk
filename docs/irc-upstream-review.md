# Native IRC: implementation and regression notes

Reviewed on 2026-11-05 against omtalk `ac2959d`, canonical
[soju on Codeberg](https://codeberg.org/emersion/soju) at
`e63e8d6b8c794114f4122375393589760326307f` (2026-10-29), and
[senpai on SourceHut](https://git.sr.ht/~delthas/senpai) at `e65c078` (2026-09-26).
The earlier GitHub soju mirror is not the acceptance fixture. The interface
continues to be omtalk's; senpai supplied protocol failure cases, not a UI design.

## Changes and boundaries

| Module | Responsibility |
|---|---|
| `crates/core` | Protocol/account/network/conversation identity, message intent, capabilities, exact timestamps and opaque message IDs |
| `crates/irc` | irc-proto framing; Tokio/rustls sessions; CAP/SASL/BIND; per-network state; roster/history/read/pin events |
| `crates/store/src/conversations.rs` | Additive schema 18, separate IRC history/state and shared history facade; existing XMPP row IDs/OMEMO blobs remain intact |
| `crates/media/src/filehost.rs` | Authenticated OPTIONS + binary POST, progress, timeout, verified TLS, no redirects, relative Location |
| `crates/media/src/metadata.rs` | Filename/MIME learned during an authorized download, extensionless media and safe cache paths |
| `crates/tui/src/address.rs` | Explicit XMPP/IRC addresses; no fake JID parsing for IRC |
| `crates/tui/src/irc_ui.rs` | Current-network commands, capability checks, typed conversation selection |
| `crates/tui/src/irc_runtime.rs` | Store/event/media adapter, catch-up and own reads, durable upload drafts, original-recipient routing |
| Existing TUI panels/audio | Shared rendering and controls, with XMPP-only encryption/devices/reply/edit/reactions gated at every entry point |

IRC uses a maintained parser instead of splitting complete IRC frames by spaces.
Two irc-proto 1.1.0 representations need special care: typed MODE rejects/loses
custom PREFIX mode arguments, and typed BATCH normalizes vendor batch types.
`wire::parse` preserves those command arguments through the library's raw command
representation while retaining its prefix/tag/parameter parsing.

## Upstream fixes checked

Commit IDs below are in the respective repositories above. Tests are behavioral;
they exercise the failure case rather than asserting that a named helper exists.

| Upstream change | Defense in omtalk / verification |
|---|---|
| senpai `4b426b9c069b` — chathistory end | Recognize the valueless `draft/chathistory-end` tag on BATCH start; a full final page still terminates |
| senpai `ae62bb9dd823` — mixed-case history cursor | Use negotiated casemapping for target lookup; BEFORE/AFTER/TARGETS stop if the cursor does not advance |
| senpai `b209af172659` — late CASEMAPPING | Re-key roster and refresh NAMES; stable conversation identity survives a display/casemapping change; collisions cannot reuse another local key |
| senpai `213257017daf` — extra CR | Accept CRCRLF and lossy UTF-8 while filtering terminal control bytes |
| senpai `7128ad8f57af` — STATUSMSG | Normalize audience-prefixed channel targets into the existing channel rather than opening a phantom query |
| senpai `f2f3671260a8` — colored mentions | Strip IRC formatting including color parameters before display and mention detection |
| senpai `a532aae30446` — rejoin history | Clear in-progress history bounds when leaving; rejoining may catch up again |
| senpai `e4a2fe2` — highlights left after MARKREAD | Only chat messages count as unread; startup JOIN/replay cannot manufacture a newer read frontier |
| senpai `7b9b1fc` — historical TAGMSG | Historical control/roster/typing messages do not mutate live state or notify |
| senpai `fcd6b14` — registration flood | One batched CAP REQ and bounded, paced user outbox; control PONG is not trapped behind the outbox |
| senpai `8b36304` — notification markup | Existing notify-send body escaping (`&`, `<`, `>`) is retained |
| soju `29c082472e4d` — pending command timeout | One in-flight history request; failure/timeout releases it and drains/fails queued requests correctly |
| soju `280a0131223d` — history boundaries | Full precision server timestamps, bounded pages, empty-batch completion, exact server cursor; duplicate content is not a dedup key |
| soju `7e21e79eaba0` — read broadcast | Actual independent downstream observes own MARKREAD; local frontier never regresses |
| soju `3dec1ed` — partial upload | Atomic download cache writes; incomplete fetches do not become ready files; failed upload leaves the local recording |
| soju `ab36141` / `be3c6d7` — status-prefixed history | STATUSMSG target normalization also applies to replay |
| soju `66e0f9a` — upstream echo | Accept own messages from another client; use server IDs/labels, not matching sender/text/time heuristics |

The real fixture exposed another compatibility requirement: generated CHATHISTORY
TARGETS bounds must use UTC `YYYY-MM-DDTHH:MM:SS.mmmZ`, which soju parses strictly.
Original incoming server-time values retain all supplied fractional precision.

Live testing exposed a capabilities mistake on OFTC: `message-tags` is an upstream
pass-through capability in soju, while `batch`, `server-time` and CHATHISTORY remain
available without it. omtalk now permits timestamp-only history, as described in
the [CHATHISTORY fallback rules](https://ircv3.net/specs/extensions/chathistory).
The fixture's second upstream omits `message-tags`; a real TUI/runtime test checks
initial unread, local and remote older pages, unchanged scroll position and recovery
of a local database containing only one live message. The latter needs a backward
catch-up in addition to the ordinary forward cursor, without overlapping ID-less
messages. Backlog initialization is stored separately from receipt of live messages.

Large IRC replays use ordered insertion instead of repeatedly parsing/sorting the
entire transcript. The idle transcript follows the existing 300-message display
window; all messages remain in SQLite for paging, and an actively scrolled window
is retained. Manual pages do not request more pages by themselves.

The separate final review added regressions for late frames from aborted history
batches, an empty TARGETS failure creating a bogus chat, server NOTICE routing,
late binding mismatch, serialized read frontiers in UTC, queued stale MARKREAD
after another device read further, and retaining local media/outgoing direction
when replay enriches the same stored server ID. Restored explicit read frontiers
are reconciled after reconnect; that does not mark new replay read. Own uploaded
files use their existing local path rather than an unnecessary HTTP download.

Pin uses `draft/metadata-2` and `soju.im/pinned`, with SUB after successful binding,
live/761 responses and GET of cached pins after reconnect. It never copies local
stale pins over soju's initial state. Independent clients verify SET in both
directions. Local pins and folded groups include protocol/profile/network identity.

## Reproducible verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
mise run test
cargo build -p omtalk
python3 tests/irc/run.py
# Full recording pipeline, when PipeWire/PulseAudio and pactl are available:
OMTALK_TEST_AUDIO=1 python3 tests/irc/run.py
```

`mise run test` prepares the frozen Python OMEMO oracle and disposable Prosody,
then tests the Rust workspace. It does not silently substitute unit tests for
server integration. The separately ignored real-soju tests are run by the Python
runner above. Go, Python 3.12+, OpenSSL and network access to Codeberg/Go modules
are needed on the first run. Sources/binaries are cached below `target/irc-fixture/`.
`OMTALK_TEST_SOJU_BIN` can explicitly select a directory containing `soju` and
`sojuctl`; it changes the tested version and should be recorded with the result.

The runner starts real soju with a private CA, fresh SQLite database, FILEHOST,
two independent local IRC upstreams and a transparent connection-drop proxy.
All credentials and messages are fixtures; it does not read user soju/omtalk
configuration. It saves server output to `target/irc-fixture-last.log` and stops
its processes in `finally`. The optional audio run creates/removes a dedicated
null sink and records its monitor, never the hardware microphone.

Coverage includes discovery/binding, preserved upstream nick, independent-client
echo/read/pin, FILEHOST URL consumption, both connection breaks, offline new
private chat via TARGETS, network rename/removal, TLS/SASL rejection, direct IRC
without history/echo, original-recipient `/np`, cancellation, and durable media.
The audio run drives the real recorder and preview, uploads the recording,
forces the original-chat-close failure, retries the ready URL with the source
temporarily unavailable, checks one echo, restores local history and plays it.
HTTP unit fixtures cover relative Location, authentication, status 401/413/415,
redirect rejection and timeout. Store tests preserve old XMPP row IDs, read state
and OMEMO bytes through migration; UI tests cover both icon modes and common media.

## Known issues at the 2026-11-05 stopping point

The `omtalk-irc` branch is an unfinished development snapshot. Live testing found
a broken `/list` response path: the generic numeric handler treats a channel name
in a server response as a conversation target, and the UI creates a buffer for
that target. A LIST response can therefore open many unwanted channel buffers.
This is **not fixed** in this snapshot. The user closed the unwanted buffers;
do not modify their saved conversation state to clean them up again.

The user also reported transient `1` lines in the central transcript and `1`
indicators beside channels during connection/LIST. Their exact cause has not
been established. The existing tests (403 passing plus five separately exercised
soju/audio checks) missed the complete LIST response-to-UI path. Add a regression
covering real 321/322/323 replies, their rendering, and unchanged conversation
membership before describing LIST as working.

The user requested stopping debugging for today, saving and pushing this state
to `omtalk-irc`, and producing a small release binary. The release build is a
snapshot of these changes, not a claim that the IRC implementation is complete.

## Deliberate limits

Capabilities control history, own read sync and FILEHOST. Direct IRC without them
still supports basic chat. No hidden uploader, automatic plaintext fallback,
offline resend queue, peer-read claims, IRC OMEMO, DCC, IRC edits/reactions/replies,
server SEARCH, or graphical BouncerServ administration is introduced. Local
history is retained. A URL is ordinary IRC text and file confidentiality depends
on the server, not on an OMEMO icon. Upstream TLS and downstream TLS are separate.
