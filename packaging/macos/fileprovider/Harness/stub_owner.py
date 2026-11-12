#!/usr/bin/env python3
"""Stage 2+3 stub owner: serves the Harness over a Unix socket with
the framing and shapes of `vectors.json` (one request per connection,
like the real server). The conflict version token is patched to a
distinct value so the Harness can isolate the conflicted
classification from ordinary lookup. The submit side enforces the v2
identity precondition exactly like the contract demands: a covered
kind without one refuses Malformed, a wrong token refuses
StaleEntry. Temporary, local-only, never part of the appex."""

import json
import socket
import struct
import sys

SOCK_PATH = sys.argv[1]
VECTORS_PATH = sys.argv[2]
CONFLICT_TOKEN = "99" * 32


def load(name):
    with open(VECTORS_PATH) as handle:
        vectors = json.load(handle)
    for vector in vectors:
        if vector["name"] == name:
            return vector["json"]
    raise KeyError(name)


READDIR_OK = load("readdir/ok")
for entry in READDIR_OK["result"]["ok"]["Readdir"]["entries"]:
    node = entry["node"]
    if "Conflict" in node:
        for version in node["Conflict"]["versions"]:
            inner = version["node"]
            if "File" in inner:
                inner["File"]["identity"] = CONFLICT_TOKEN


def result_ok(op_body):
    return {"ok": op_body}


def result_err(code, message):
    return {"err": {"code": code, "message": message}}


# Entry tokens the stub serves, mirroring the readdir sample.
TOKENS = {}
for _entry in READDIR_OK["result"]["ok"]["Readdir"]["entries"]:
    _node = _entry["node"]
    _kind = next(iter(_node))
    if _kind != "Conflict" and _node[_kind].get("identity") is not None:
        TOKENS[_entry["name"]] = _node[_kind]["identity"]


def answer(op, params):
    path = (params or {}).get("path", "")
    if op == "lookup":
        if path == "hello.txt":
            node = dict(READDIR_OK["result"]["ok"]["Readdir"]["entries"][0]["node"])
            return result_ok({"Lookup": {"node": node, "generation": 3, "revision": 9}})
        if path == "notes":
            node = dict(READDIR_OK["result"]["ok"]["Readdir"]["entries"][1]["node"])
            return result_ok({"Lookup": {"node": node, "generation": 3, "revision": 9}})
        if path == "split.txt":
            for entry in READDIR_OK["result"]["ok"]["Readdir"]["entries"]:
                if entry["name"] == "split.txt":
                    return result_ok(
                        {"Lookup": {"node": entry["node"], "generation": 3, "revision": 9}}
                    )
        return result_err("NotFound", "no such path")
    if op == "stat":
        if path == "hello.txt":
            return result_ok(
                {
                    "Stat": {
                        "attr": {
                            "kind": "File",
                            "size": 26,
                            "executable": False,
                            "identity": "ab12" * 16,
                        },
                        "generation": 3,
                        "revision": 9,
                    }
                }
            )
        if path == "":
            return result_ok(
                {
                    "Stat": {
                        "attr": {"kind": "Dir", "size": 0, "executable": False},
                        "generation": 3,
                        "revision": 9,
                    }
                }
            )
        return result_err("NotFound", "no such path")
    if op == "readdir":
        if path == "":
            return result_ok({"Readdir": READDIR_OK["result"]["ok"]["Readdir"]})
        if path in ("notes", "agreed", "disagreed"):
            return result_ok({"Readdir": {"entries": [], "generation": 3, "revision": 9}})
        return result_err("NotFound", "no such directory")
    if op == "read":
        if path == "hello.txt":
            return result_ok(
                {"Read": {"bytes_hex": "68656c6c6f", "generation": 3, "revision": 9}}
            )
        return result_err("NotFound", "no such path")
    if op == "capture-parent":
        return result_ok({"ParentToken": {"token": {"instance": 9, "counter": 3}}})
    if op == "submit":
        return answer_submit(params or {})
    return result_err("UnsupportedOperation", "unknown op %s" % op)


COVERED = ("CommitFile", "AppendFile", "Unlink", "Rmdir", "Rename", "SetAttrs")


def answer_submit(params):
    kind = (params or {}).get("kind", {})
    if len(kind) != 1:
        return result_err("Malformed", "the submit names exactly one kind")
    name, body = next(iter(kind.items()))
    if name == "Fold":
        return result_err("UnsupportedOperation", "the stub answers no folds")
    if name == "Mkdir":
        return result_ok({"Submitted": {"outcome": "Done"}})
    if name == "CreateFile":
        return result_ok(
            {"Submitted": {"outcome": {"Created": {"size": 0, "executable": False, "chunks": []}}}}
        )
    if name not in COVERED:
        return result_err("UnsupportedOperation", "unknown submit kind %s" % name)
    # Entry-addressed kinds require the v2 precondition, exactly like
    # the owner: absent is Malformed, never a silent path-only write.
    expected = body.get("expected_identity")
    if not isinstance(expected, str) or len(expected) != 64:
        return result_err("Malformed", "a covered kind carries expected_identity")
    path = body.get("from", body.get("path", ""))
    if path not in TOKENS:
        return result_err("NotFound", "no such path")
    if expected != TOKENS[path]:
        return result_err(
            "StaleEntry",
            "stale entry identity for %r: expected %s, found %s" % (path, expected, TOKENS[path]),
        )
    if name == "CommitFile":
        size = len(bytes.fromhex(body.get("content_hex", "")))
        return result_ok(
            {
                "Submitted": {
                    "outcome": {
                        "Committed": {
                            "size": size,
                            "executable": bool(body.get("executable", False)),
                            "chunks": ["aa"],
                        }
                    }
                }
            }
        )
    return result_ok({"Submitted": {"outcome": "Done"}})


def read_exact(conn, count):
    data = b""
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise ConnectionError("peer closed mid-frame")
        data += chunk
    return data


def main():
    try:
        import os

        os.unlink(SOCK_PATH)
    except OSError:
        pass
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(SOCK_PATH)
    listener.listen(16)
    print("stub listening %s" % SOCK_PATH, flush=True)
    while True:
        conn, _ = listener.accept()
        try:
            prefix = read_exact(conn, 4)
            (length,) = struct.unpack(">I", prefix)
            body = read_exact(conn, length)
            request = json.loads(body.decode("utf-8"))
            if request.get("v") != 2:
                response = {
                    "v": 2,
                    "id": request.get("id", 0),
                    "result": {
                        "err": {
                            "code": "UnknownVersion",
                            "message": "unsupported wire version %s, want 2" % (request.get("v"),),
                        }
                    },
                }
            else:
                response = {
                    "v": 2,
                    "id": request["id"],
                    "result": answer(request["op"], request.get("params")),
                }
            payload = json.dumps(response).encode("utf-8")
            conn.sendall(struct.pack(">I", len(payload)) + payload)
        except ConnectionError:
            pass
        finally:
            conn.close()


main()
