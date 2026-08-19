#!/usr/bin/env python3
"""Measure what one session costs, and how that scales.

This is the benchmark for the open memory work. Run it before and after any
change to how the map's terrain, overlays or tile cache are held.

    python3 scripts/memory.py

As of the last run (release build, real India basemap mounted):

    idle server                     4 MB
    session on home only          +37 MB
    session that opens the map   +111 MB      <- linear per session

The map's resident cost is what makes 100 concurrent visitors ~11 GB. Almost
all of it is read-only (terrain grid, parsed boundaries, decoded tiles), so
sharing it in-process should collapse it to roughly a constant. If a change
worked, the third number is the one that drops.

Needs the real basemap: with only the embedded sample the figures are
meaningless. Check `map/data/india.pmtiles` resolves first.
"""
import asyncio, subprocess, sys, time, websockets

PORT = 8199
BIN = "./target/release/portfolio"


def rss_mb(pid):
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) // 1024
    return -1


async def session(hold, key):
    """Open a session and leave it open, so its memory stays counted."""
    ws = await websockets.connect(f"ws://127.0.0.1:{PORT}/ws", max_size=None)
    await ws.send("r180x46")

    async def drain(sec):
        end = asyncio.get_event_loop().time() + sec
        while asyncio.get_event_loop().time() < end:
            try: await asyncio.wait_for(ws.recv(), timeout=0.2)
            except asyncio.TimeoutError: pass

    await drain(0.5)
    await ws.send(b"x")          # skip the opening
    await drain(0.5)
    if key:
        await ws.send(key)
        await drain(6.0)         # let the descent pull tiles in
    hold.append(ws)


async def main():
    srv = subprocess.Popen([BIN, "--web", "--web-port", str(PORT)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    if srv.poll() is not None:
        sys.exit(f"{BIN} exited immediately -- built with --release?")
    pid, hold = srv.pid, []
    idle = rss_mb(pid)
    print(f"  idle server                {idle:5d} MB")

    await session(hold, None); await asyncio.sleep(0.5)
    home = rss_mb(pid)
    print(f"  + home only                {home:5d} MB   (+{home - idle})")

    await session(hold, b"1"); await asyncio.sleep(1.0)
    one = rss_mb(pid)
    print(f"  + one on the map           {one:5d} MB   (+{one - home})")

    await session(hold, b"1"); await asyncio.sleep(1.0)
    two = rss_mb(pid)
    print(f"  + another on the map       {two:5d} MB   (+{two - one})")
    print(f"\n  per map session: ~{two - one} MB   ->  100 users ~= {(two - one) * 100 / 1024:.1f} GB")
    srv.kill()

asyncio.run(main())
