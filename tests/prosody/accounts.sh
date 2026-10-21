#!/bin/bash
# Test accounts, re-created on every start (data lives on a tmpfs). Password: "test".
set -e
for user in alice bob carol dave erin frank; do
	prosodyctl register "$user" localhost test
done
exec /entrypoint.sh prosody
