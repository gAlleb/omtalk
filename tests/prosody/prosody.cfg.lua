-- Disposable server for omtalk's integration tests. Plaintext on purpose: it listens
-- on 127.0.0.1 only; `mise run prosody` starts it on empty data.

admins = { "admin@localhost" }
pidfile = "/var/run/prosody/prosody.pid"
log = { info = "*console" }

modules_enabled = {
	"disco", "roster", "saslauth", "presence", "message", "iq", "ping",
	"carbons", "pep", "mam", "offline", "smacks", "adhoc", "admin_adhoc", "uptime", "invites", "invites_adhoc", "blocklist", "bookmarks", "vcard4", "vcard_legacy",
}
modules_disabled = { "tls" }

c2s_require_encryption = false
allow_unencrypted_plain_auth = true
authentication = "internal_plain"
archive_expires_after = "never"

-- File sharing (XEP-0363) over plain HTTP, reachable from the host at 127.0.0.1:15280.
http_ports = { 5280 }
http_interfaces = { "*" }
https_ports = {}
http_external_url = "http://127.0.0.1:15280/"
http_default_host = "upload.localhost"
default_archive_policy = true

VirtualHost "localhost"

Component "muc.localhost" "muc"
	modules_enabled = { "muc_mam" }
	restrict_room_creation = false

Component "upload.localhost" "http_file_share"
	http_file_share_size_limit = 10 * 1024 * 1024
