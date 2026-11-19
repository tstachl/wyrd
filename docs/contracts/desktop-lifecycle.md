# Desktop application lifecycle (v0.4)

Status: Normative. Scope: the desktop shell process's window-close and
quit behavior, what survives them, and how the application reopens on
each platform. Authority: this contract owns the UI lifecycle; the
cross-process owner boundary stays with
`docs/contracts/owner-interface.md`, and `docs/design/desktop-shell.md`
is the design and implementation document subordinate to this contract.
Where the two disagree, this contract wins.

This contract supersedes the landed tray decision recorded in
`nostr:nevent1qqs2v8cey0fgkyyqfnyeg66qckky5887n7p43k6mahvr3q66typg65spz9mhxue69uhkwunpwdczuap49eehg870au2`:
there is no tray (menu-bar extra) in the v0.4 desktop. Status is
available through the main window only.

## Window close and application quit are distinct events

Closing the last window and quitting the application are different
events with different outcomes. No close path implies a quit, and no
quit path implies a lock (see "Quit never locks" below).

- **Window close** is a per-window event. What happens next is
  platform-defined (see "Process state after the last window closes").
- **Application quit** is process termination: Cmd+Q, a Quit menu
  action, any code-less exit request, or the last-window close on
  platforms where close means quit (Linux). Quit always runs the
  dirty-quit discipline below before the process exits.

## Process state after the last window closes

| Platform | After the last window closes |
|---|---|
| macOS | The process stays alive with the window hidden. The Dock icon and Spotlight re-activation return to the running process (see "Reopening"). |
| Linux | The process quits: last-window close is quit, and quit runs the drain below, then exits. |
| Windows | Deferred: Windows is out of v0.4 entirely (`docs/architecture.md`, "Fixed scope"). No lifecycle semantics are defined for Windows here. |

## Reopening

- **macOS:** the Dock icon and a Spotlight launch while the process
  runs activate the existing process — macOS bundle activation never
  starts a second process while one runs — and the activation arrives
  as a reopen event the shell answers by showing the main window. No
  new single-instance mechanism exists or is required: the reopen
  handler is the whole mechanism, and the hidden window is always
  reachable while the process lives.
- **Linux:** quit-on-close means there is no hidden running process
  to re-activate; the launcher starts a new process, which re-reads
  the registry and re-probes each drive's socket. Fresh process,
  verified state, never assumed continuity.
- **Windows:** deferred with the platform.

## Effects of clean termination on owners

Clean termination of the desktop shell affects no owner, of either
kind:

- **Service-backed owners** (a manager definition exists): the OS
  service manager is the parent. The shell exiting or crashing never
  takes an owner down, because the shell was never its parent.
- **Session-backed owners** (per-session posture, spawned by the
  shell's `drive unlock` with a piped passphrase): the owner keeps
  running. Quitting the shell drops the supervisor handle and the
  shell's in-memory copy of the passphrase; it does not kill the
  child and does not scrub the owner's own loaded secret. Relaunch
  reattaches through the socket probe, which reads `Unlocked` without
  consulting the (empty) session map. Only an explicit lock ends a
  session-backed owner (see "Explicit lock").

**Quit never locks.** Quit is a UI lifecycle action; lock is a drive
security action. Quitting the shell must not implicitly lock a
session drive, kill a session owner, or scrub session state. Any copy
claiming otherwise contradicts this contract and is wrong.

## Explicit lock

Only an explicit lock ends a session-backed owner:

- Lock is the user's explicit action (the header Lock control,
  `drive_lock` underneath). It kills only children this application
  instance spawned (handles held in the session map) and scrubs the
  in-memory secret.
- Lock refuses where the application holds no session for the drive:
  a reattached owner the application did not start is never killed —
  the shell stays non-owning even when asked to lock.
- Locking is synchronous under the session-map lock: the entry
  (child plus secret) leaves the map before the kill, so a monitor
  poll can never respawn a locked drive, including mid-respawn.

Who may lock: the local user through the running shell. There is no
remote, scheduled, or manager-driven lock; stopping a service-backed
owner is a service-manager operation, not a lock.

## Dirty quit and its guarantees

Two cases, specified separately because only one of them can offer
any guarantee:

- **Graceful shutdown.** Track outstanding exchanges, and let each
  reach its terminal answer or the defined 30-second bound. Report
  nothing as successful without the owner's authoritative answer.
  Only then exit. The drain does not gate admission: an exchange
  dispatched during the drain still enters and is waited on like
  any other. Exchanges still outstanding past the bound stay
  `Unconfirmed` — never successes, never failures.
- **Forced termination or crash.** Make no claim about any
  outstanding exchange: the owner may have committed it even though
  the answer never reached the shell. On next launch, reconnect and
  reconcile the affected paths through the owner before permitting a
  retry. This case stays possible no matter how carefully the
  graceful path is implemented.

The drain waits on dispatched IPC exchanges only. The per-session
monitor threads hold no drain guard — supervision is not IPC work —
so the drain never waits on a persistent owner. New work may still
dispatch during the drain; the bound keeps a wedged owner from
wedging quit.

## Threat model: what remains after the UI exits

After a clean quit of the desktop shell, with no lock performed:

- **Still alive:** every running owner, service-backed and
  session-backed. The drives keep serving their sockets exactly as
  before; availability does not depend on the shell running.
- **Still accessible:** each running owner's socket to any same-UID
  local process (the owner-interface's peer-credential check is
  unchanged), and — after relaunch — to the shell itself, which
  reattaches without a passphrase.
- **Gone with the process:** the shell's in-memory copy of each
  session passphrase (never persisted, never logged; process exit
  releases it) and the supervisor handles. The owner's own loaded
  secret persists in owner memory — it must, to keep serving — so a
  quit removes no secret the owner needs and revokes no access the
  OS already grants.
- **No new exposure is created:** a same-UID local process could
  connect to a running session owner's socket before the quit, and
  can still do so after it. Quitting changes neither side of that
  fact.

Plainly: quitting the shell without locking leaves the drives
running and re-openable without a passphrase by anyone holding the
device unlocked. That is the stated behavior, not a vague promise —
lock before quitting if the session must not survive.

## Platform differences

- Startup ordering is a UI behavior, not a manager ordering: owner
  units are dynamic per-drive state the login artifacts never
  hard-code. A drive whose owner is not answering yet reads
  unavailable, and the owner appearing needs no restart — the view
  resumes on explicit refresh/retry.
- Per-session drives have no manager definition to start: there is
  nothing to auto-start or restart, and a down definition-less drive
  reads locked.
- The tray's five-second registry-and-liveness poll is gone with the
  tray. Nothing else depended on its cadence: the main window reads
  state on explicit refresh and retry only. Status is pull-based;
  notifications are deferred and are not a tray replacement.

## What this contract does not own

- The drive-owner interface, the mutation queue, and refusal
  taxonomy (`docs/contracts/owner-interface.md`).
- Crash windows of durable subsystems
  (`docs/contracts/crash-consistency.md`).
- The desktop shell's views, IPC shape, and staging
  (`docs/design/desktop-shell.md`).
- Windows lifecycle semantics: deferred with the platform, not
  decided here.
