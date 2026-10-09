#!/usr/bin/env python3
"""Exercise a real terminal adapter: startup, map navigation, resize and exit.

Pass any command after --, e.g. a direct client, ssh -tt, or mosh invocation.
Uses only the standard library; does not emulate the client event loop.
"""
import argparse
import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sections',action='store_true',help='Exercise Projects, Skills, Taste and Ask before exiting')
parser.add_argument("command", nargs=argparse.REMAINDER)
args = parser.parse_args()
command = args.command[1:] if args.command[:1] == ["--"] else args.command
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 160, 0, 0))
before = termios.tcgetattr(slave)
env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", LANG="C.UTF-8")
started = time.monotonic()
process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True)
transcript = bytearray()


def wait_for(text, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if select.select([master], [], [], .1)[0]:
            try:
                transcript.extend(os.read(master, 65536))
            except OSError:
                break
        if text in transcript:
            return
        if process.poll() is not None:
            break
    raise AssertionError(f"Missing {text!r}; process={process.poll()}; output={bytes(transcript[-2000:])!r}")


try:
    wait_for(b"PRINCE PATEL")
    print(f"startup_ms={(time.monotonic()-started)*1000:.1f}")
    transcript.clear()
    os.write(master, b"2")
    wait_for(b"KNOWLEDGE HIGH SCHOOL")
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    os.kill(process.pid, signal.SIGWINCH)
    time.sleep(.3)
    transcript.clear()
    os.write(master, b"1")
    wait_for(b"PRINCE PATEL")
    if args.sections:
        for key,heading in [(b'3',b'NETJAIL'),(b'4',b'skills'),(b'5',b'SNUFKIN'),(b'6',b'ASK')]:
            transcript.clear();os.write(master,key);wait_for(heading)
        transcript.clear();os.write(master,b'\x1b');wait_for(b'PRINCE PATEL')
    os.write(master, b"q")
    # Continue draining output while the writer finishes its final frame.
    end = time.monotonic()+10
    while process.poll() is None and time.monotonic()<end:
        if select.select([master],[],[],.1)[0]:
            try: os.read(master,65536)
            except OSError: break
    assert process.wait(timeout=2) == 0
    after = termios.tcgetattr(slave)
    assert before == after, "terminal mode was not restored"
    print("PASS: real PTY startup, navigation, resize, return home, exit, terminal restoration")
finally:
    if process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
    os.close(slave)
