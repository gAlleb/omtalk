#!/bin/bash
# Test accounts, (re)registered on every start. Password: "test".
set -e
# A fresh volume belongs to root; Prosody runs as its own user.
chown prosody:prosody /var/lib/prosody
for user in admin alice bob carol dave erin frank $(seq -f "u%02g" 1 40); do
	prosodyctl register "$user" localhost test
done
exec /entrypoint.sh prosody
