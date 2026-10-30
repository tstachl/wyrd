#!/usr/bin/env python3
"""Supervisor wrapper for one Wyrd drive process (OD-34-C).

The supervisors cannot tell the cases apart on their own: the binary
exits 2 for every CLI failure and a signal death is not an exit at
all. So the module supervises this wrapper instead of the binary,
and the wrapper forks the command and maps the *wait status* — the
only place the cases are distinguishable — onto the exit status the
supervisor's policy wants:

- the binary's drive-already-owned code (78) exits 78: the terminal
  status, never restarted (systemd `RestartPreventExitStatus`, launchd
  `SuccessfulExit` sees non-zero and stands down);
- a signal death or any other exit exits the restart-worthy status
  the module passes per supervisor (`--restart-status=1` for systemd,
  whose `Restart=on-failure` restarts non-zero but not clean exits;
  `--restart-status=0` for launchd, whose `KeepAlive` with
  `SuccessfulExit=true` restarts on zero and not otherwise).

Readiness is a log line, never an exit code: the wrapper watches the
posture sidecar the child publishes and logs when it turns fresh, so
the supervisor's capture records that the drive started serving. If
no fresh sidecar appears within the bound, the wrapper logs a warning
and keeps supervising — it never kills a live child over a missing
readiness signal.

Usage: wyrd-supervise --restart-status {0,1} --posture-path PATH
    [--readiness-timeout SECS] -- COMMAND [ARGS...]
"""

import os
import shutil
import sys
import time

# The drive-already-owned code the binary exits (`CliError::exit_code`
# in `crates/wyrd-cli/src/main.rs`, `EX_CONFIG`). Kept in sync by the
# contract test `the_nix_module_declares_exactly_one_owner_per_drive`
# and the `vault_against_held_drive_exits_drive_locked` process test.
DRIVE_LOCKED_CODE = 78

# A posture older than this is stale (`POSTURE_FRESHNESS` in
# `crates/wyrd-daemon/src/posture.rs`, three posture intervals).
# The wrapper compares file mtime, not the JSON body: the atomic
# rename that publishes a generation sets mtime to publish time, so
# mtime freshness implies a live writer without parsing anything.
POSTURE_FRESH_SECS = 180

# How long to wait for the first fresh sidecar before logging the
# warning and supervising without a readiness line. Test knob only
# in practice (see `--readiness-timeout`); production waits out a
# slow first pass rather than warning early.
DEFAULT_READINESS_TIMEOUT_SECS = 240


def usage(message):
    print(f"wyrd-supervise: {message}", file=sys.stderr)
    print(
        "usage: wyrd-supervise --restart-status {0,1} --posture-path PATH "
        "[--readiness-timeout SECS] -- COMMAND [ARGS...]",
        file=sys.stderr,
    )
    return 2


def fresh_sidecar(path):
    try:
        age = time.time() - os.stat(path).st_mtime
    except OSError:
        return False
    return age <= POSTURE_FRESH_SECS


def parse_argv(argv):
    restart_status = None
    posture_path = None
    readiness_timeout = DEFAULT_READINESS_TIMEOUT_SECS
    rest = list(argv)
    while rest and rest[0].startswith("--"):
        flag = rest.pop(0)
        # Both `--flag value` and `--flag=value`: the module emits
        # the latter, the shell tests the former.
        if "=" in flag:
            flag, inline = flag.split("=", 1)
        else:
            inline = None
        if flag == "--":
            break
        if flag == "--restart-status":
            value = inline if inline is not None else (rest.pop(0) if rest else None)
            if value is None:
                return None, "missing value for --restart-status"
            try:
                restart_status = int(value)
            except ValueError:
                return None, "--restart-status needs 0 or 1"
            if restart_status not in (0, 1):
                return None, "--restart-status needs 0 or 1"
        elif flag == "--posture-path":
            value = inline if inline is not None else (rest.pop(0) if rest else None)
            if value is None:
                return None, "missing value for --posture-path"
            posture_path = value
        elif flag == "--readiness-timeout":
            value = inline if inline is not None else (rest.pop(0) if rest else None)
            if value is None:
                return None, "missing value for --readiness-timeout"
            try:
                readiness_timeout = int(value)
            except ValueError:
                return None, "--readiness-timeout needs seconds"
        else:
            return None, f"unknown flag {flag}"
    if restart_status is None:
        return None, "missing --restart-status"
    if posture_path is None:
        return None, "missing --posture-path"
    if not rest:
        return None, "missing COMMAND"
    return (restart_status, posture_path, readiness_timeout, rest), None


def main(argv):
    parsed, error = parse_argv(argv)
    if error is not None:
        return usage(error)
    restart_status, posture_path, readiness_timeout, command = parsed
    if "/" not in command[0]:
        resolved = shutil.which(command[0])
        if resolved is None:
            print(f"wyrd-supervise: cannot start {command[0]}: not on PATH", file=sys.stderr)
            return restart_status
        command = [resolved] + command[1:]
    try:
        child = os.spawnv(os.P_NOWAIT, command[0], command)
    except OSError as error:
        print(f"wyrd-supervise: cannot start {command[0]}: {error}", file=sys.stderr)
        return restart_status
    started = time.monotonic()
    ready_logged = False
    while True:
        try:
            done_pid, status = os.waitpid(child, os.WNOHANG)
        except ChildProcessError:
            break
        except InterruptedError:
            continue
        if done_pid == 0:
            if not ready_logged:
                if fresh_sidecar(posture_path):
                    print(f"wyrd-supervise: ready (posture fresh at {posture_path})")
                    ready_logged = True
                elif time.monotonic() - started > readiness_timeout:
                    print(
                        f"wyrd-supervise: no fresh posture at {posture_path} after "
                        f"{readiness_timeout}s; supervising without a readiness line",
                        file=sys.stderr,
                    )
                    ready_logged = True
            time.sleep(1)
            continue
        break
    if os.WIFEXITED(status):
        code = os.WEXITSTATUS(status)
        if code == DRIVE_LOCKED_CODE:
            return DRIVE_LOCKED_CODE
        return restart_status
    if os.WIFSIGNALED(status):
        return restart_status
    # Unreachable in practice (waitpid reports one or the other), but
    # fail toward restart: a status the wrapper cannot classify is a
    # runtime unknown, never a lock proof.
    return restart_status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
