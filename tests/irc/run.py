#!/usr/bin/env python3
"""Disposable real-soju fixture. Never reads a user's omtalk/soju configuration."""
import asyncio
import datetime
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
REV = "e63e8d6b8c794114f4122375393589760326307f"


def build():
    configured = os.environ.get("OMTALK_TEST_SOJU_BIN")
    if configured:
        return Path(configured)
    cache = ROOT / "target" / "irc-fixture" / REV
    binary = cache / "bin"
    if (binary / "soju").exists() and (binary / "sojuctl").exists():
        return binary
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / "source.tar.gz"
    urllib.request.urlretrieve(f"https://codeberg.org/emersion/soju/archive/{REV}.tar.gz", archive)
    with tarfile.open(archive) as tar:
        tar.extractall(cache, filter="data")
    source = cache / "soju"
    binary.mkdir(exist_ok=True)
    # /tmp may be a small tmpfs; the linker's temporary binary belongs in the
    # build cache, where Cargo's artifacts already live.
    with tempfile.TemporaryDirectory(prefix="build-", dir=cache) as scratch:
        subprocess.run(["go", "build", "-o", str(binary) + "/", "./cmd/soju", "./cmd/sojuctl"],
                       cwd=source, env={**os.environ, "GOTMPDIR": scratch}, check=True)
    return binary


class IRC:
    """Small independent IRC peer/server fixture; soju itself is unmodified."""
    def __init__(self, name, message_tags=True):
        self.name = name
        self.message_tags = message_tags
        self.clients = {}
        self.channels = {}
        self.counter = 0
        self.joins = 0
        self.parts = 0

    async def client(self, reader, writer):
        nick = "*"
        user = False
        cap = False
        registered = False
        caps = set()

        def send(line, target=writer):
            target.write((line + "\r\n").encode())

        def names(channel):
            members = " ".join("@" + n for n in self.channels.get(channel, []))
            send(f":fixture 353 {nick} = {channel} :{members}")
            send(f":fixture 366 {nick} {channel} :end")

        try:
            while data := await reader.readline():
                line = data.decode().rstrip("\r\n")
                if line.startswith("@"):
                    line = line.split(" ", 1)[1]
                head, sep, tail = line.partition(" :")
                words = head.split()
                if sep:
                    words.append(tail)
                if not words:
                    continue
                cmd, *args = words
                cmd = cmd.upper()
                if cmd == "CAP":
                    sub = args[0].upper()
                    if sub == "LS":
                        cap = True
                        tags = "message-tags " if self.message_tags else ""
                        send(f":fixture CAP {nick} LS :{tags}server-time multi-prefix away-notify")
                    elif sub == "REQ":
                        caps.update(args[-1].split())
                        send(f":fixture CAP {nick} ACK :{args[-1]}")
                    elif sub == "END":
                        cap = False
                elif cmd == "NICK":
                    old = nick
                    nick = args[0]
                    if registered:
                        for target in self.clients.values():
                            send(f":{old}!u@localhost NICK :{nick}", target)
                        self.clients.pop(old, None)
                    self.clients[nick] = writer
                elif cmd == "USER":
                    user = True
                elif cmd == "PING":
                    send(f":fixture PONG fixture :{args[-1]}")
                elif cmd == "JOIN":
                    channel = args[0]
                    self.channels.setdefault(channel, set()).add(nick)
                    self.joins += 1
                    for member in self.channels[channel]:
                        send(f":{nick}!u@localhost JOIN {channel}", self.clients[member])
                    names(channel)
                    send(f":fixture 332 {nick} {channel} :fixture topic")
                elif cmd == "PART":
                    channel = args[0]
                    self.parts += 1
                    for member in self.channels.get(channel, set()).copy():
                        send(f":{nick}!u@localhost PART {channel} :left", self.clients[member])
                    self.channels.get(channel, set()).discard(nick)
                elif cmd == "NAMES":
                    names(args[0])
                elif cmd == "FIXTURE_HISTORY":
                    # Seed more than one soju page, with distinct timestamps.
                    # Only the disposable upstream implements this command.
                    channel, count = args[0], int(args[1])
                    start = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=1)
                    for i in range(count):
                        stamp = (start + datetime.timedelta(seconds=i)).isoformat(timespec="milliseconds").replace("+00:00", "Z")
                        tags = f"time={stamp}"
                        if self.message_tags:
                            tags += f";msgid={self.name}-{channel}-{i}"
                        for member in self.channels.get(channel, set()):
                            send(f"@{tags} :archive!u@localhost PRIVMSG {channel} :archive {i:04}", self.clients[member])
                    send(f":fixture NOTICE {nick} :fixture history seeded")
                elif cmd in ("PRIVMSG", "NOTICE"):
                    target, body = args[0], args[1]
                    self.counter += 1
                    timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
                    for member in self.channels.get(target, {target}).copy():
                        if member != nick and member in self.clients:
                            send(f"@time={timestamp};msgid={self.name}-{self.counter} :{nick}!u@localhost {cmd} {target} :{body}", self.clients[member])
                elif cmd == "WHO":
                    for member in self.channels.get(args[0], set()):
                        send(f":fixture 352 {nick} {args[0]} u localhost fixture {member} H@ :0 fixture")
                    send(f":fixture 315 {nick} {args[0]} :end")
                elif cmd == "MODE" and args:
                    send(f":fixture 324 {nick} {args[0]} +nt")
                elif cmd == "AWAY":
                    send(f":fixture {'306' if args else '305'} {nick} :away state updated")
                elif cmd == "QUIT":
                    break
                if user and nick != "*" and not cap and not registered:
                    registered = True
                    send(f":fixture 001 {nick} :welcome")
                    send(f":fixture 005 {nick} NETWORK={self.name} CASEMAPPING=rfc1459 CHANTYPES=#& PREFIX=(ov)@+ CHANMODES=b,k,l,imnt :supported")
                    send(f":fixture 376 {nick} :end of MOTD")
                await writer.drain()
        except (ConnectionError, asyncio.CancelledError):
            pass
        finally:
            if self.clients.get(nick) is writer:
                self.clients.pop(nick, None)
                for members in self.channels.values():
                    members.discard(nick)
            writer.close()


class Proxy:
    """Transparent TLS relay: only the tested client's downstreams are dropped."""
    def __init__(self, upstream):
        self.upstream = upstream
        self.connections = set()

    async def client(self, reader, writer):
        upstream_reader, upstream_writer = await asyncio.open_connection("127.0.0.1", self.upstream)
        self.connections.add(writer)
        async def relay(source, target):
            while data := await source.read(65536):
                target.write(data)
                await target.drain()
        tasks = [asyncio.create_task(relay(reader, upstream_writer)), asyncio.create_task(relay(upstream_reader, writer))]
        try:
            await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        finally:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            self.connections.discard(writer)
            writer.close()
            upstream_writer.close()

    async def control(self, reader, writer):
        if (await reader.readline()).strip() == b"drop":
            for connection in tuple(self.connections):
                connection.close()
            writer.write(b"OK\n")
            await writer.drain()
        writer.close()


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


async def main():
    binary = build()
    with tempfile.TemporaryDirectory(prefix="omtalk-irc-") as temporary:
        folder = Path(temporary)
        first, second = IRC("alpha"), IRC("beta", message_tags=False)
        servers = [await asyncio.start_server(s.client, "127.0.0.1", 0) for s in (first, second)]
        upstreams = [s.sockets[0].getsockname()[1] for s in servers]
        irc_port, http_port = port(), port()
        def openssl(*args):
            subprocess.run(["openssl", *map(str,args)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=omtalk fixture CA", "-keyout", folder / "ca-key.pem", "-out", folder / "ca.pem")
        openssl("req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost", "-keyout", folder / "key.pem", "-out", folder / "server.csr")
        (folder / "extensions").write_text("basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n")
        openssl("x509", "-req", "-in", folder / "server.csr", "-CA", folder / "ca.pem", "-CAkey", folder / "ca-key.pem", "-CAcreateserial", "-days", "1", "-extfile", folder / "extensions", "-out", folder / "cert.pem")
        config = folder / "soju.conf"
        config.write_text(f'''hostname localhost
listen ircs://127.0.0.1:{irc_port}
listen https://127.0.0.1:{http_port}
listen unix+admin://{folder}/admin
tls {folder}/cert.pem {folder}/key.pem
db sqlite3 {folder}/soju.db
message-store db
file-upload fs {folder}/files
http-ingress https://localhost:{http_port}
''')
        log = (folder / "soju.log").open("w+")
        process = await asyncio.create_subprocess_exec(str(binary / "soju"), "-config", str(config), stdout=log, stderr=log)
        proxy = Proxy(irc_port)
        relay = await asyncio.start_server(proxy.client, "127.0.0.1", 0)
        control = await asyncio.start_server(proxy.control, "127.0.0.1", 0)

        async def admin(*args):
            child = await asyncio.create_subprocess_exec(str(binary / "sojuctl"), "-config", str(config), *args, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT)
            output, _ = await child.communicate()
            if child.returncode or b"error:" in output.lower():
                raise RuntimeError(output.decode())

        try:
            for _ in range(100):
                if (folder / "admin").exists():
                    break
                if process.returncode is not None:
                    raise RuntimeError("soju exited during startup")
                await asyncio.sleep(.1)
            await admin("user", "create", "-username", "fixture", "-password", "fixture-password")
            for name, upstream in zip(("alpha", "beta"), upstreams):
                await admin("user", "run", "fixture", "network", "create", "-name", name, "-addr", f"irc+insecure://127.0.0.1:{upstream}", "-nick", f"saved-{name}")
            env = dict(os.environ, OMTALK_TEST_SOJU_PORT=str(relay.sockets[0].getsockname()[1]), OMTALK_TEST_SOJU_DIRECT_PORT=str(irc_port), OMTALK_TEST_CONTROL_PORT=str(control.sockets[0].getsockname()[1]), OMTALK_TEST_IRC_PORT=str(upstreams[0]), OMTALK_TEST_IRC_LEGACY_PORT=str(upstreams[1]), OMTALK_TEST_SOJU_CA=str(folder / "ca.pem"))
            test = await asyncio.create_subprocess_exec("cargo", "test", "-p", "omtalk", "--bin", "omtalk", "real_soju_channel_history_and_unread_with_and_without_message_tags", "--", "--ignored", "--nocapture", cwd=ROOT, env=env)
            if await test.wait():
                raise RuntimeError("channel history integration failed")
            test = await asyncio.create_subprocess_exec("cargo", "test", "-p", "omtalk-irc", "--test", "soju", "--", "--ignored", "--nocapture", "--test-threads=1", cwd=ROOT, env=env)
            result = await test.wait()
            if result:
                raise RuntimeError(f"integration tests failed: {result}")
            if os.environ.get("OMTALK_TEST_AUDIO") == "1":
                # A dedicated null sink and its monitor: no hardware input/output.
                sink = f"omtalk_audio_test_{os.getpid()}"
                module = subprocess.check_output(["pactl", "load-module", "module-null-sink", f"sink_name={sink}"], text=True).strip()
                try:
                    audio_env = dict(env, PULSE_SINK=sink, PULSE_SOURCE=sink + ".monitor")
                    for name in ["audio::device_tests::virtual_device_play_pause_seek_stop_and_record", "runtime::irc::tests::real_soju_voice_record_preview_upload_retry_echo_restart_and_play"]:
                        test = await asyncio.create_subprocess_exec("cargo", "test", "-p", "omtalk", "--bin", "omtalk", name, "--", "--ignored", "--nocapture", cwd=ROOT, env=audio_env)
                        if await test.wait():
                            raise RuntimeError(f"audio integration failed: {name}")
                finally:
                    subprocess.run(["pactl", "unload-module", module], check=True)
        finally:
            process.terminate()
            await process.wait()
            for server in [*servers, relay, control]:
                server.close()
                await server.wait_closed()
            log.seek(0)
            output = log.read()
            log.close()
            (ROOT / "target" / "irc-fixture-last.log").write_text(output)


if __name__ == "__main__":
    asyncio.run(main())
