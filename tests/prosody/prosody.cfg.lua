-- Disposable server for omtalk's integration tests. Plaintext on purpose: it listens
-- on 127.0.0.1 only; `mise run prosody` starts it on empty data.

admins = {}
pidfile = "/var/run/prosody/prosody.pid"
log = { info = "*console" }

modules_enabled = {
	"disco", "roster", "saslauth", "presence", "message", "iq", "ping",
	"carbons", "pep", "mam", "offline", "smacks", "blocklist", "bookmarks", "vcard4", "vcard_legacy",
}
modules_disabled = { "tls" }

c2s_require_encryption = false
allow_unencrypted_plain_auth = true
authentication = "internal_plain"
archive_expires_after = "never"
default_archive_policy = true

VirtualHost "localhost"

Component "muc.localhost" "muc"
	modules_enabled = { "muc_mam" }
	restrict_room_creation = false
