#!/usr/bin/env python3
"""Switch-time conflict probe for one Wyrd drive (OD-34-C).

Run by the module's activation script before the supervisor starts
the new generation: if a *foreign* process holds the drive's lock,
print the drive and whatever holder facts the sidecars carry, and
exit 1 so the switch is refused. Exit 0 proceeds.

The probe is diagnostics, not enforcement — the lock is advisory and
released immediately, so another process can take it in the window
before the service's own open. The enforcement stays where it was:
the kernel lock plus the wrapper's terminal status.

A holder that is this service's own previous generation is not a
conflict: the switch stops it and starts the new generation, so the
probe allows the switch when every holder PID is the service's own
main PID or a child of it, and says so. Anything else — a
hand-started mount, a `sync now` still running, an unknown PID — is
refused with the drive path and the holder facts actually obtained.

Usage: wyrd-drive-probe DRIVE_DIR OWN_MAIN_PID
(OWN_MAIN_PID is 0 when the service is not running.)
"""

import fcntl
import json
import os
import shutil
import subprocess
import sys
import time

# A posture older than this is stale. This is the daemon's
# freshness threshold, duplicated here rather than imported: a
# Python script cannot name a Rust constant, so the contract test
# `the_nix_module_declares_exactly_one_owner_per_drive` pins all
# three spellings to the same number instead. Depends on `lsof`
# and `ps` on PATH: the NixOS and nix-darwin modules put them
# there; a hand run needs them installed.
POSTURE_FRESH_SECS = 180


def lock_free(lock_path):
    """True when an exclusive lock can be taken right now."""
    try:
        fd = os.open(lock_path, os.O_RDONLY)
    except OSError:
        return True
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        return False
    finally:
        os.close(fd)
    return True


def holder_pids(lock_path, lsof):
    """PIDs with the lock file open, via lsof. Empty on any doubt."""
    try:
        out = subprocess.run(
            [lsof, "-t", lock_path],
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        return []
    if out.returncode != 0:
        return []
    pids = set()
    for token in out.stdout.split():
        try:
            pids.add(int(token))
        except ValueError:
            continue
    return pids


def child_pids(main_pid):
    """PIDs whose parent is the service's main PID, via ps."""
    try:
        out = subprocess.run(
            ["ps", "-ax", "-o", "pid=,ppid="],
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        return set()
    if out.returncode != 0:
        return set()
    children = set()
    for line in out.stdout.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        try:
            pid, ppid = int(parts[0]), int(parts[1])
        except ValueError:
            continue
        if ppid == main_pid:
            children.add(pid)
    return children


def read_sidecar(path):
    """(posture_unix, instance, mount_mode) or None when unusable."""
    try:
        with open(path, "rb") as handle:
            file = json.load(handle)
    except (OSError, ValueError):
        return None
    try:
        if file.get("schema") != 1:
            return None
        instance = file["instance"]
        hexdigits = "0123456789abcdef"
        if len(instance) != 32 or any(c not in hexdigits for c in instance):
            return None
        no_serving = file.get("serving_id", "") == ""
        return (file["posture_unix"], instance, no_serving)
    except (KeyError, TypeError):
        return None


def rank_claim(posture_unix):
    """Fresh beats stale: recency alone cannot promote a dead
    holder over a live one. Mirrors the daemon's `fresher`."""
    age = int(time.time()) - posture_unix
    if age <= POSTURE_FRESH_SECS:
        return (2, -age)
    return (1, -age)


def holder_sentence(drive_dir):
    """The fresher sidecar claim, or the absence said plainly."""
    claims = []
    for name in ("vault.posture", "mount.posture"):
        claim = read_sidecar(os.path.join(drive_dir, name))
        if claim is not None:
            posture_unix, instance, mount_mode = claim
            rank = rank_claim(posture_unix)
            claims.append((rank, posture_unix, instance, mount_mode))
    if not claims:
        return "no posture recorded (the holder never published)"
    best = max(claims, key=lambda claim: (claim[0], claim[1]))
    _, posture_unix, instance, mount_mode = best
    age = int(time.time()) - posture_unix
    if mount_mode:
        return (
            f"process {instance} (mount mode, no serving endpoint, "
            f"posture {age}s ago; liveness unverified)"
        )
    return (
        f"process {instance} "
        f"(posture {age}s ago; liveness unverified)"
    )


def main(argv):
    if len(argv) != 2:
        print("usage: wyrd-drive-probe DRIVE_DIR OWN_MAIN_PID",
              file=sys.stderr)
        return 2
    drive_dir = argv[0]
    try:
        own_main = int(argv[1])
    except ValueError:
        print("wyrd-drive-probe: OWN_MAIN_PID must be a pid", file=sys.stderr)
        return 2
    lock_path = os.path.join(drive_dir, "LOCK")
    if lock_free(lock_path):
        return 0
    lsof = shutil.which("lsof") or "/usr/sbin/lsof"
    holders = holder_pids(lock_path, lsof)
    own = {own_main} | child_pids(own_main) if own_main else set()
    if holders and holders <= own:
        print(
            f"wyrd-drive-probe: {drive_dir} is held by this service's "
            "previous generation; the switch rolls it",
            file=sys.stderr,
        )
        return 0
    print(f"wyrd-drive-probe: refusing switch: {drive_dir} is held",
          file=sys.stderr)
    print(f"wyrd-drive-probe: {holder_sentence(drive_dir)}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
