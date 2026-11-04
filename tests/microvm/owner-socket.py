#!/usr/bin/env python3
# tests/microvm/owner-socket.py — minimal owner-interface client for the
# pause E2E leg. Speaks the real socket framing (u32 big-endian length
# prefix plus JSON, docs/contracts/owner-interface.md) with stdlib only,
# so the leg drives the actual owner operation instead of a seam.
#
# Usage: owner-socket.py <op> <drive-dir> [args...]
#   pause | resume            -> prints "paused=true|false"
#   liveness                  -> prints "paused=<bool> passes=<n>"
#   submit-mkdir <path>       -> prints "submitted" (mkdir needs no token)
#   queued                    -> prints announcements_queued (observe totals)
#   delivered                 -> prints announcements_delivered
# Any refusal or transport break exits non-zero: a broken connection is
# unconfirmed, never a status the leg may read as an answer.
"""Owner-socket client for the pause E2E leg (test-only)."""

import json
import socket
import struct
import sys

FRAME_CEILING = 96 * 1024 * 1024
WIRE_VERSION = 1


def die(msg):
    print("owner-socket: {}".format(msg), file=sys.stderr)
    sys.exit(1)


def recvall(conn, count):
    buf = b""
    while len(buf) < count:
        chunk = conn.recv(count - len(buf))
        if not chunk:
            die("connection broke before a terminal answer (unconfirmed)")
        buf += chunk
    return buf


def transact(sock_path, op, params, req_id):
    req = json.dumps({"v": WIRE_VERSION, "id": req_id, "op": op, "params": params}).encode()
    try:
        conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        conn.settimeout(120)
        conn.connect(sock_path)
    except OSError as exc:
        die("cannot reach owner at {}: {}".format(sock_path, exc))
    with conn:
        conn.sendall(struct.pack(">I", len(req)) + req)
        (length,) = struct.unpack(">I", recvall(conn, 4))
        if length > FRAME_CEILING:
            die("frame past the ceiling (unconfirmed)")
        return json.loads(recvall(conn, length))


def unwrap_ok(response, op):
    result = response.get("result", {})
    if "err" in result:
        err = result["err"]
        die("{} refused: {}: {}".format(op, err.get("code"), err.get("message")))
    if "ok" not in result:
        die("{} answered without a terminal verdict: {}".format(op, response))
    return result["ok"]


def main(argv):
    if len(argv) < 3:
        die("usage: owner-socket.py <op> <drive-dir> [args...]")
    op, drive = argv[1], argv[2]
    sock = drive.rstrip("/") + "/owner.sock"
    req_id = 101

    if op in ("pause", "resume"):
        ok = unwrap_ok(transact(sock, op, None, req_id), op)
        try:
            print("paused={}".format("true" if ok["Paused"]["paused"] else "false"))
        except (KeyError, TypeError):
            die("{} answered without a pause verdict".format(op))
    elif op == "liveness":
        ok = unwrap_ok(transact(sock, "liveness", None, req_id), op)
        try:
            live = ok["Liveness"]["liveness"]
            print("paused={} passes={}".format(
                "true" if live["paused"] else "false", int(live["passes"])))
        except (KeyError, TypeError):
            die("liveness answered without a liveness body")
    elif op == "submit-mkdir":
        if len(argv) != 4:
            die("usage: owner-socket.py submit-mkdir <drive-dir> <path>")
        ok = unwrap_ok(
            transact(sock, "submit", {"kind": {"Mkdir": {"path": argv[3]}}}, req_id), op)
        if "Submitted" not in ok:
            die("submit answered without a submission verdict")
        print("submitted")
    elif op in ("queued", "delivered"):
        ok = unwrap_ok(transact(sock, "observe", None, req_id), op)
        try:
            totals = ok["Observe"]["status"]["totals"]
            key = "announcements_queued" if op == "queued" else "announcements_delivered"
            print(int(totals[key]))
        except (KeyError, TypeError):
            die("observe answered without obligation totals")
    else:
        die("unknown op {!r} (want pause|resume|liveness|submit-mkdir|queued|delivered)".format(op))


main(sys.argv)
