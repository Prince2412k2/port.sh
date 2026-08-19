#!/usr/bin/env python3
"""A minimal ACP server, for testing the client against a real peer.

Not a model and not a mock: a real subprocess speaking real JSON-RPC over real
pipes, which is the only way to exercise the handshake, the restraint
negotiation and the gates end to end. This box has never had a provider key, so
before this the whole of `acp.rs` had only ever run its failure path.

It is deliberately a *badly behaved* agent. It ignores the capabilities the
client advertised and asks for a file, a terminal and a shell anyway, then
reports what it was told -- which is exactly the case the gates exist for, and
the one a well-behaved agent would never produce.

Driven by `acp::tests::the_client_talks_to_a_real_acp_server`. It answers one
prompt and exits, so the client's reader sees EOF and the test terminates.
"""

import json
import sys

# What the client claimed it could do, captured at initialize so the answer can
# report it back and the test can assert on what actually went over the wire.
advertised = {}


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def read():
    line = sys.stdin.readline()
    if not line:
        return None
    line = line.strip()
    if not line:
        return read()
    return json.loads(line)


def call(rid, method, params):
    """Make a request of the client and wait for its reply to that id."""
    send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    while True:
        msg = read()
        if msg is None:
            return None
        if msg.get("id") == rid:
            return msg


def verdict(reply):
    """Summarise a reply as either an error code or its outcome."""
    if reply is None:
        return "no reply"
    if "error" in reply:
        return "error %s" % reply["error"].get("code")
    result = reply.get("result") or {}
    outcome = result.get("outcome") or {}
    return outcome.get("outcome", "result")


def permission(rid, name, title):
    return call(
        rid,
        "session/request_permission",
        {
            "sessionId": "s1",
            "toolCall": {"toolCallId": "t%d" % rid, "name": name, "title": title},
            "options": [
                {"optionId": "no", "kind": "reject_once"},
                {"optionId": "yes", "kind": "allow_once"},
            ],
        },
    )


def chunk(text):
    send(
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": text},
                },
            },
        }
    )


def main():
    global advertised
    while True:
        msg = read()
        if msg is None:
            return
        method = msg.get("method")
        rid = msg.get("id")

        if method == "initialize":
            advertised = msg["params"].get("clientCapabilities", {})
            send(
                {
                    "jsonrpc": "2.0",
                    "id": rid,
                    "result": {
                        "protocolVersion": msg["params"].get("protocolVersion", 1),
                        "agentCapabilities": {"loadSession": False},
                    },
                }
            )

        elif method == "session/new":
            # Advertised the way opencode really does it -- a `configOptions`
            # entry rather than ACP's own `modes` -- so the client's handling of
            # that shape is exercised by something other than a recorded string.
            send(
                {
                    "jsonrpc": "2.0",
                    "id": rid,
                    "result": {
                        "sessionId": "s1",
                        "configOptions": [
                            {
                                "id": "mode",
                                "name": "Session Mode",
                                "currentValue": "build",
                                "options": [{"value": "build"}, {"value": "plan"}],
                            }
                        ],
                    },
                }
            )

        elif method == "session/set_config_option":
            send({"jsonrpc": "2.0", "id": rid, "result": {}})

        elif method == "session/prompt":
            # Reach for everything, having been told we may not.
            got = {
                "fs.read": verdict(
                    call(100, "fs/read_text_file", {"sessionId": "s1", "path": "/etc/passwd"})
                ),
                "terminal": verdict(
                    call(101, "terminal/create", {"sessionId": "s1", "command": "sh"})
                ),
                "bash": verdict(permission(102, "bash", "Run a command")),
                "webfetch": verdict(permission(103, "webfetch", "Fetch https://example.com")),
            }
            chunk("advertised " + json.dumps(advertised, sort_keys=True, separators=(",", ":")) + "\n")
            chunk("answered " + json.dumps(got, sort_keys=True, separators=(",", ":")))
            send({"jsonrpc": "2.0", "id": rid, "result": {"stopReason": "end_turn"}})
            return

        elif rid is not None:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": rid,
                    "error": {"code": -32601, "message": "not this agent"},
                }
            )


if __name__ == "__main__":
    main()
