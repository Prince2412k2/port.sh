#!/usr/bin/env python3
"""Drive a running portfolio session and report what showed up.

A TUI cannot be tested by piping into it: it needs a terminal on the other
end, and over SSH it needs a real pty or the client never negotiates a window
size. This does both transports.

    ./target/release/portfolio --web --web-port 8080 &
    python3 scripts/drive.py web 8080 x 2 4

    ./target/release/portfolio --serve --ssh-port 2222 --host-key /tmp/hk &
    python3 scripts/drive.py ssh 2222 x 1

Arguments after the port are sent one at a time, with a pause between, and the
bytes that came back are searched for a marker per section. `x` skips the
opening. An argument is written to the far end verbatim, so a whole question is
one argument and `$'\r'` is Enter -- which is how the ask section gets driven.
`@20` waits twenty seconds instead of sending anything, for an answer that takes
longer than the pause between keys.

Caution, learned the hard way: do not test by scraping for a phrase and
concluding a key is broken when it is absent. crossterm sends only the cells
that *changed*, so a footer that shares characters with the previous one
arrives fragmented and a working feature reads as broken. Two key-routing bugs
were "reproduced" that way and did not exist. Prefer a Rust test against the
state machine; use this for smoke tests and for things only a real client
exercises (pty sizing, mouse bytes, resize).
"""
import os, sys, time, select, subprocess

MARKERS = {
    "home":       b"PRINCE PATEL",
    "experience": b"KNOWLEDGE HIGH",
    "projects":   b"netjail",
    "skills":     b"raise a tile",
    "taste":      b"SNUFKIN",
    "ask":        b"Ask about the work",
}


def report(buf):
    print(f"  {len(buf)} bytes received")
    for name, needle in MARKERS.items():
        if needle in buf:
            print(f"    saw {name}")


def throwaway_key():
    """A key of our own, rather than whatever the user happens to have.

    The server accepts any key, but the *client* has to offer one -- and if
    ~/.ssh is empty, ssh offers nothing and fails with "Permission denied
    (publickey)" that looks exactly like a server-side rejection. That cost a
    real debugging session once.
    """
    path = "/tmp/portfolio-drive-key"
    if not os.path.exists(path):
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", path],
                       check=True)
    return path


def over_ssh(port, keys):
    import pty, fcntl, termios, struct
    key = throwaway_key()
    # A real pty for the *client*: without one, ssh does not request a window
    # size and the app has nothing to lay out against.
    pid, fd = pty.fork()
    if pid == 0:
        os.execv("/usr/bin/ssh", [
            "ssh", "-p", str(port), "-tt", "-i", key,
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "PreferredAuthentications=publickey",
            "-o", "IdentitiesOnly=yes",
            "-o", "BatchMode=yes",
            "127.0.0.1"])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 46, 180, 0, 0))

    buf = bytearray()
    def pump(sec):
        end = time.time() + sec
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try: buf.extend(os.read(fd, 1 << 20))
                except OSError: return
    pump(3.0)
    for k in keys:
        if k.startswith("@"):
            pump(float(k[1:]))
            continue
        os.write(fd, k.encode())
        pump(5.0)
    report(bytes(buf))
    try: os.kill(pid, 9)
    except ProcessLookupError: pass


def over_web(port, keys):
    import asyncio, websockets

    async def main():
        async with websockets.connect(f"ws://127.0.0.1:{port}/ws", max_size=None) as ws:
            buf = bytearray()
            async def drain(sec):
                end = asyncio.get_event_loop().time() + sec
                while asyncio.get_event_loop().time() < end:
                    try:
                        m = await asyncio.wait_for(ws.recv(), timeout=0.2)
                        if isinstance(m, bytes): buf.extend(m)
                    except asyncio.TimeoutError:
                        pass
            # The size must go first: the session waits for it before drawing.
            await ws.send("r180x46")
            await drain(3.0)
            for k in keys:
                if k.startswith("@"):
                    await drain(float(k[1:]))
                    continue
                await ws.send(k.encode())
                await drain(5.0)
            report(bytes(buf))
    asyncio.run(main())


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    mode, port, keys = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
    (over_ssh if mode == "ssh" else over_web)(port, keys)
