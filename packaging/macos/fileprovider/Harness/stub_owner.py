#!/usr/bin/env python3
"""Stage 2+3 stub owner: serves the Harness over a Unix socket with
the framing and shapes of `vectors.json` (one request per connection,
like the real server). The conflict version token is patched to a
distinct value so the Harness can isolate the conflicted
classification from ordinary lookup. The submit side enforces the v2
identity precondition exactly like the contract demands: a covered
kind without one refuses Malformed, a wrong token refuses
StaleEntry. Mutation calls beyond the baseline vectors run against
a small in-memory namespace (`mut-*` paths plus the scripted
`stale-dir` fixture): creates, commits, renames, and deletes apply
there, so the Harness proves the adapter's write path end to end.
`stale-dir` is deliberately hidden from `readdir` — it exists only
to script one `StaleParent` for the recapture test. Temporary,
local-only, never part of the appex."""

import hashlib
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

# The mutation namespace: path -> {kind, identity, size, content}.
# Baseline vector paths stay static; everything the Harness creates
# lives here under `mut-*` names (plus the scripted fixtures
# below), so the Stage 2 assertions never see adapter writes.
STATE = {}
MINT = [0]
STALE_DIR = "stale-dir"
STALE_DIR_TOKEN = "ed" * 32


def mint_token():
    MINT[0] += 1
    return ("%02x" % (MINT[0] % 256)) * 32


# `stale-dir` scripts one StaleParent: the first CreateFile under
# it refuses, the recaptured retry commits. A per-process flag, so
# the Harness proves exactly one recapture.
STALE_ONCE = {STALE_DIR: True}


# The served content base is derived from the content itself
# (truncated SHA-256 over the bytes), exactly like a real owner's
# content addressing: a same-size edit produces a different chunk
# list, so size alone never identifies content. Empty content keeps
# the legacy `aa` marker the vectors pin.
def derive_chunks(content_hex):
    if not content_hex:
        return ["aa"]
    return [hashlib.sha256(bytes.fromhex(content_hex)).hexdigest()[:32]]


def state_node(path, entry):
    if entry["kind"] == "dir":
        return {"Dir": {"subtree": "", "identity": entry["identity"]}}
    return {
        "File": {
            "size": entry["size"],
            "executable": False,
            "chunks": derive_chunks(entry["content"]),
            "identity": entry["identity"],
        }
    }


def parent_of(path):
    return path.rsplit("/", 1)[0] if "/" in path else ""


def parent_exists(parent):
    if parent == "":
        return True
    if parent == STALE_DIR:
        return True
    if parent in STATE and STATE[parent]["kind"] == "dir":
        return True
    return parent in ("notes", "agreed", "disagreed")


def answer(op, params):
    path = (params or {}).get("path", "")
    if op == "lookup":
        if path in STATE:
            entry = STATE[path]
            return result_ok(
                {"Lookup": {"node": state_node(path, entry), "generation": 3, "revision": 9}}
            )
        if path == STALE_DIR:
            return result_ok(
                {
                    "Lookup": {
                        "node": {"Dir": {"subtree": "", "identity": STALE_DIR_TOKEN}},
                        "generation": 3,
                        "revision": 9,
                    }
                }
            )
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
        if path in STATE:
            entry = STATE[path]
            return result_ok(
                {
                    "Stat": {
                        "attr": {
                            "kind": "Dir" if entry["kind"] == "dir" else "File",
                            "size": entry["size"],
                            "executable": False,
                            "identity": entry["identity"],
                        },
                        "generation": 3,
                        "revision": 9,
                    }
                }
            )
        if path == STALE_DIR:
            return result_ok(
                {
                    "Stat": {
                        "attr": {
                            "kind": "Dir",
                            "size": 0,
                            "executable": False,
                            "identity": STALE_DIR_TOKEN,
                        },
                        "generation": 3,
                        "revision": 9,
                    }
                }
            )
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
            entries = list(READDIR_OK["result"]["ok"]["Readdir"]["entries"])
            for name in sorted(STATE):
                if "/" not in name:
                    entries.append({"name": name, "node": state_node(name, STATE[name])})
            return result_ok({"Readdir": {"entries": entries, "generation": 3, "revision": 9}})
        if path in STATE and STATE[path]["kind"] == "dir":
            entries = [
                {"name": name.rsplit("/", 1)[1], "node": state_node(name, STATE[name])}
                for name in sorted(STATE)
                if parent_of(name) == path
            ]
            return result_ok({"Readdir": {"entries": entries, "generation": 3, "revision": 9}})
        if path in ("notes", "agreed", "disagreed"):
            return result_ok({"Readdir": {"entries": [], "generation": 3, "revision": 9}})
        return result_err("NotFound", "no such directory")
    if op == "read":
        if path in STATE:
            entry = STATE[path]
            if entry["kind"] != "file":
                return result_err("NotAFile", "not a file")
            return result_ok(
                {"Read": {"bytes_hex": entry["content"], "generation": 3, "revision": 9}}
            )
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


def committed(size):
    return result_ok(
        {
            "Submitted": {
                "outcome": {
                    "Committed": {
                        "size": size,
                        "executable": False,
                        "chunks": ["aa"],
                    }
                }
            }
        }
    )


class DropConnection(Exception):
    """The stub drops the connection instead of answering: the
    client's unconfirmed path, never a frame."""


# One-shot drops keyed by (kind, path): the first submit with a
# `drop-before/*` path closes before processing (nothing landed —
# reconcile must find absence and retry); the first with a
# `drop-after/*` path processes, then closes without answering
# (the write landed — reconcile must report unknown, never claim
# success or failure). Later submits run normally.
DROP = {}


def claim_drop(name, path):
    if "/" not in path:
        return None
    if path.split("/")[0] not in ("drop-before", "drop-after"):
        return None
    key = (name, path)
    if key in DROP:
        return None
    DROP[key] = True
    return path.split("/")[0]


BASELINE_NAMES = {"hello.txt", "notes", "legacy.bin", "agreed", "disagreed", "split.txt"}


def answer_mkdir(body):
    path = body.get("path", "")
    if path in STATE or path in BASELINE_NAMES:
        return result_err("AlreadyExists", "an entry already names %r" % path)
    if not parent_exists(parent_of(path)):
        return result_err("NotFound", "no such parent")
    STATE[path] = {"kind": "dir", "identity": mint_token(), "size": 0, "content": ""}
    return result_ok({"Submitted": {"outcome": "Done"}})


def answer_create(body):
    path = body.get("path", "")
    parent = body.get("parent", {})
    if not isinstance(parent, dict) or parent.get("instance") != 9:
        return result_err("StaleParent", "previous owner instance")
    if parent_of(path) == STALE_DIR and STALE_ONCE.get(STALE_DIR):
        STALE_ONCE[STALE_DIR] = False
        return result_err("StaleParent", "the directory changed before create")
    if path in STATE or path in BASELINE_NAMES:
        return result_err("AlreadyExists", "an entry already names %r" % path)
    if not parent_exists(parent_of(path)):
        return result_err("NotFound", "no such parent")
    STATE[path] = {"kind": "file", "identity": mint_token(), "size": 0, "content": ""}
    return result_ok(
        {"Submitted": {"outcome": {"Created": {"size": 0, "executable": False, "chunks": []}}}}
    )


def answer_stateful(name, body, path, entry):
    if name == "CommitFile":
        if entry["kind"] != "file":
            return result_err("IsDirectory", "not a file")
        # The full content base is checked, not just the size: a
        # same-size edit with different chunks is still stale. The
        # expected chunks are derived from the currently served
        # content, like a real owner's content addressing.
        base = body.get("base", {})
        if (
            not isinstance(base, dict)
            or base.get("size") != entry["size"]
            or base.get("executable") is not False
            or base.get("chunks") != derive_chunks(entry["content"])
        ):
            return result_err("Stale", "the content changed before commit")
        content = body.get("content_hex", "")
        entry["content"] = content
        entry["size"] = len(bytes.fromhex(content))
        return committed(entry["size"])
    if name == "AppendFile":
        if entry["kind"] != "file":
            return result_err("IsDirectory", "not a file")
        entry["content"] += body.get("content_hex", "")
        entry["size"] = len(bytes.fromhex(entry["content"]))
        return result_ok({"Submitted": {"outcome": "Done"}})
    if name == "Unlink":
        if entry["kind"] != "file":
            return result_err("IsDirectory", "is a directory")
        del STATE[path]
        return result_ok({"Submitted": {"outcome": "Done"}})
    if name == "Rmdir":
        if entry["kind"] != "dir":
            return result_err("NotADirectory", "not a directory")
        if any(parent_of(other) == path for other in STATE):
            return result_err("DirectoryNotEmpty", "the directory is not empty")
        del STATE[path]
        return result_ok({"Submitted": {"outcome": "Done"}})
    if name == "Rename":
        dest = body.get("to", "")
        if dest in STATE or dest in BASELINE_NAMES:
            return result_err("AlreadyExists", "an entry already names %r" % dest)
        if not parent_exists(parent_of(dest)):
            return result_err("NotFound", "no such parent")
        STATE[dest] = entry
        del STATE[path]
        return result_ok({"Submitted": {"outcome": "Done"}})
    return result_ok({"Submitted": {"outcome": "Done"}})


def answer_submit(params):
    kind = (params or {}).get("kind", {})
    if len(kind) != 1:
        return result_err("Malformed", "the submit names exactly one kind")
    name, body = next(iter(kind.items()))
    if name == "Fold":
        return result_err("UnsupportedOperation", "the stub answers no folds")
    path = body.get("from", body.get("path", ""))
    drop = claim_drop(name, path)
    if drop == "drop-before":
        raise DropConnection()
    if name == "Mkdir":
        result = answer_mkdir(body)
    elif name == "CreateFile":
        result = answer_create(body)
    elif name not in COVERED:
        result = result_err("UnsupportedOperation", "unknown submit kind %s" % name)
    else:
        result = answer_covered(name, body, path)
    if drop == "drop-after":
        raise DropConnection()
    return result


def answer_covered(name, body, path):
    # Entry-addressed kinds require the v2 precondition, exactly like
    # the owner: absent is Malformed, never a silent path-only write.
    # The token is compared on every path — the baseline fixtures
    # and the stateful namespace alike.
    expected = body.get("expected_identity")
    if not isinstance(expected, str) or len(expected) != 64:
        return result_err("Malformed", "a covered kind carries expected_identity")
    if path in STATE:
        entry = STATE[path]
        if expected != entry["identity"]:
            return result_err(
                "StaleEntry",
                "stale entry identity for %r: expected %s, found %s"
                % (path, expected, entry["identity"]),
            )
        return answer_stateful(name, body, path, entry)
    if path not in TOKENS:
        return result_err("NotFound", "no such path")
    if expected != TOKENS[path]:
        return result_err(
            "StaleEntry",
            "stale entry identity for %r: expected %s, found %s" % (path, expected, TOKENS[path]),
        )
    if name == "CommitFile":
        size = len(bytes.fromhex(body.get("content_hex", "")))
        return committed(size)
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
        except DropConnection:
            pass
        except ConnectionError:
            pass
        finally:
            conn.close()


main()
