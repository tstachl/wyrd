# Desktop application lifecycle (v0.4)

Status: Normative. Scope: the desktop shell process's window-close and
quit behavior, what survives them, and how a launch starts a fresh
process on each platform. Authority: this contract owns the UI
lifecycle; the cross-process owner boundary stays with
`docs/contracts/owner-interface.md`, and `docs/design/desktop-shell.md`
is the design and implementation document subordinate to this contract.
Where the two disagree, this contract wins.

This contract supersedes the landed tray decision recorded in
`nostr:nevent1qqs2v8cey0fgkyyqfnyeg66qckky5887n7p43k6mahvr3q66typg65spz9mhxue69uhkwunpwdczuap49eehg870au2`:
there is no tray (menu-bar extra) in the v0.4 desktop. Status is
available through the main window only.

## Closing the last window quits the application

Closing the last window is an application quit on macOS and Linux.
There is no hidden-window state and no running indicator left behind.
A close request follows the same dirty-quit drain as Cmd+Q or any
other exit request before the process exits, and quitting never
implies a lock (see "Quit never locks" below). Window close is not a
distinct lifecycle event with its own outcome.

| Platform | After the last window closes |
|---|---|
| macOS | The process quits: the drain runs, then the process exits. |
| Linux | The process quits: the drain runs, then the process exits. |
| Windows | Deferred: Windows is out of v0.4 entirely (`docs/architecture.md`, "Fixed scope"). No lifecycle semantics are defined for Windows here. |

## Launching starts a fresh process

Every launch — a Dock icon click or Spotlight launch on macOS, a
launcher start on Linux — starts a fresh process. No launch returns to
an existing hidden process, and no single-instance mechanism or reopen
handler is required. The new process re-reads the registry and
re-probes each drive's socket, showing verified current state without
assuming continuity from the previous process. Only presentation
state — the selected drive, open dialogs and form input — is lost,
and that state is deliberately ephemeral.

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
  session-backed owner while its session continues (see "Explicit
  lock"); session end terminates it per "Logout discipline" below.

**Quit never locks.** Quit is a UI lifecycle action; lock is a drive
security action. Quitting the shell must not implicitly lock a
session drive, kill a session owner, or scrub session state. Any copy
claiming otherwise contradicts this contract and is wrong.

## The login session bounds desktop owners

Quitting the GUI is not the boundary — logging out (or otherwise
ending the user session) is.

### Desktop owner session boundary

A desktop drive owner is intended to be bounded by the
authenticated user-session domain in which it was started.

Before enabling or starting a managed owner, Wyrd MUST determine
the applicable session-boundary configuration. On Linux systemd
hosts, Wyrd MUST refuse enablement or start when lingering is
enabled or its state cannot be determined. Wyrd MUST NOT change
the user's lingering configuration. Non-systemd hosts do not
support this user-unit boundary and are handled by the existing
service-manager error path.

Wyrd checks this condition at manager interactions: enablement,
start, and the login-start enablement toggle. It does not
continuously monitor the setting while an owner runs. An
administrator can therefore enable lingering after an owner has
started, allowing that owner to survive logout. This is an
explicit limitation of the desktop session boundary, not a
condition Wyrd can guarantee against.

Wyrd-managed service definitions are user-scoped desktop
lifecycle infrastructure (launchd user agents, systemd user
units): Wyrd ships no system-scope service. Server deployments
register with `--no-service` and supervise `wyrd owner start`
independently; they never pass through the managed enable/start
paths, so the lingering refusal does not apply to them. There is
no opt-out flag: the boundary follows the supervision path, not
the credential backend — file-backed desktop drives share it.

Subject to that limitation, owner termination at the end of the
user-session domain relies on operating-system session teardown.
Wyrd requests graceful shutdown where the operating system
delivers the termination signal and preserves unresolved
operations as `Unconfirmed` if the operating system forces
termination.

### User-session domain

The desktop user-session domain is per UID. It remains active
while at least one authenticated login session for that UID
exists and ends when the last session ends.

Ending one session while another remains does not terminate the
user manager, its units or agents, or their owners. No owner
termination is attempted solely because one of several sessions
ends. Fast user switching is not session end; full logout is.

A concurrent login must not create a duplicate owner for an
existing drive. The drive ownership and store-lock mechanisms
prevent a second owner from acquiring the same drive.
Provisioning an existing drive reconciles and verifies it rather
than creating another owner.

On Linux, this domain follows the per-UID systemd user manager.
The equivalent macOS behavior is expected from the per-user
launchd domain but remains subject to platform verification in
the logout and multi-session drills.

Logout termination behavior must be tested on each supported
configuration (see "Platform differences" below): the bound is
verified, not assumed.

## Logout discipline

Session end is where all desktop owners in that session domain
terminate — session-backed and service-backed alike — after an
orderly shutdown is requested:

- On orderly session termination, Wyrd requests a bounded
  graceful shutdown. The owner attempts to drain in-flight
  exchanges within the available termination window.
- Termination may be forced by the operating system (including
  SIGKILL without further notice). This is expected, not a
  failure of the drain.
- The durable outcome stays correct either way: any exchange
  without a confirmed terminal outcome remains `Unconfirmed`;
  it must never be recast as successful or failed.

Logout is not lock: lock remains the in-session explicit action
defined below, and logout creates no locked state that survives
into the next session. After logout the drives are stopped, not
locked.

## Explicit lock

Only an explicit lock ends a session-backed owner while its
session continues:

- Lock is the user's explicit action (the header Lock control,
  `drive_lock` underneath). It kills only session children this
  application instance spawned (handles held in the session map)
  and scrubs the in-memory secret. Service-backed owners are
  unchanged by a lock, unless separately specified.
- Lock refuses where the application holds no session for the drive:
  a reattached owner the application did not start is never killed —
  the shell stays non-owning even when asked to lock.
- Locking is synchronous under the session-map lock: the entry
  (child plus secret) leaves the map before the kill, so a monitor
  poll can never respawn a locked drive, including mid-respawn.

Who may lock: the local user through the running shell. There is no
remote, scheduled, or manager-driven lock; stopping a service-backed
owner is a service-manager operation, not a lock.

## Lock, quit, logout, and next login by owner kind

| Situation | Session-backed owner | Service-backed owner |
|---|---|---|
| Explicit lock | Terminated | Unchanged, unless separately specified |
| GUI quit | Unchanged | Unchanged |
| Logout | Already stopped if locked; otherwise termination attempt | Termination attempt |
| Next login | Starts locked, requiring explicit unlock | Eligible to start if enabled and credentials available |

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

## Service enablement at login (per-drive auto-start)

Whether the service manager starts a drive's owner at login is a
persistent per-drive preference, independent of whether the owner
is running now. "At login" means after the user authenticates and
the session exists: the manager loads the agent within the
session, and the drive starts when its credentials are
available. Pre-login startup is explicitly out of scope for
desktop — unattended operation before any user authenticates
belongs to server deployments, never to a desktop login
preference. Enablement and lifecycle are two separate
observations (lifecycle × enablement: Running/Stopped/Unknown ×
Enabled/Disabled/Unknown) — never one enum, never inferred from
each other, and never new `Stopped*` variants. A manually started
service may run while disabled at login; the UI shows both facts.

Semantics (both platforms):

| Operation | Starts now | Starts at login |
|---|---|---|
| Enable at login | No | Yes |
| Disable at login | Stops the service | No |
| Start manually | Yes | Unchanged |
| Stop manually | No | Unchanged |
| Initial provisioning | Yes | Yes |

Starting never changes enablement: a disabled drive started
manually stays disabled at the next login — the toggle is never
silently reversed by pressing Start. Provisioning explicitly
performs install, enable, start. Toggling enablement never touches
drive data, never opens the keystore, and never implies the owner
answered: a disabled drive reads `Stopped` with the same explicit
Start recovery it always had, and no auto-start path is added
anywhere.

Enablement probes (observation only, never supervision):

- **Linux:** `systemctl --user is-enabled <unit>`, word parsed
  from stdout (`enabled`/`disabled`/`masked` decide; any other
  word, or a bus failure printing no word, reads Unknown).
  A completed answer printing `enabled` is Enabled; a completed
  answer printing `disabled`/`masked` is Disabled; a missing
  tool, a probe timeout, or an unparseable answer is Unknown,
  never Disabled — the exit status is ignored throughout.
  Passing `--quiet` would suppress the very word the parser reads.
- **macOS:** `launchctl print-disabled user/<uid>`, parsed for the
  `"md.enesis.wyrd.<service>" => enabled|disabled` record. A
  present record maps to its value; an absent record is Unknown
  (managed drives provisioned by `load -w` always carry one, so
  absence means the record was never written or was externally
  removed — never a license to claim Disabled); a missing tool,
  timeout, or non-zero answer is Unknown. Verified against the
  live manager: `enable`/`disable` persist without loading or
  stopping (a running job keeps running through a disable, a
  stopped job stays stopped through an enable), plain `load`
  starts without touching the record but refuses a disabled job,
  and `unload -w` stops and persists disabled while keeping the
  definition file. Exit codes lie here (`load` on a disabled job
  exits 0 without loading; `unload -w` on a stopped job exits 0
  with an error on stderr) — every mutation verifies the record
  and the loaded state after, never the exit code alone. One window
  stays best-effort by construction: a manual Start on a disabled
  drive lifts the record to load and re-persists disabled after, so
  a kill landing exactly between the two leaves Enabled — the next
  toggle read reports it honestly, and no completed command ever
  leaves the toggle flipped.

Managed drives only: session-posture (`--no-service`) drives have
no unit to enable or disable and carry no toggle. The toggle reads
"Start drive service at login" (frozen-glossary "Drive service,"
R-D1).

## Platform differences

- Startup ordering is a UI behavior, not a manager ordering: owner
  units are dynamic per-drive state the login artifacts never
  hard-code. A drive whose owner is not answering yet reads
  unavailable, and the owner appearing needs no restart — the view
  resumes on explicit refresh/retry.
- Per-session drives have no manager definition to start: there is
  nothing to auto-start or restart, and a down definition-less drive
  reads locked.
- The session bound is enforced per platform, not assumed from
  the unit type: Linux desktop installation must not enable
  lingering, and pre-existing lingering is detected and handled
  explicitly; concurrent logins for one UID share the
  per-user manager domain (no duplicate owners — the existing
  owner serves). Logout termination is tested on each supported
  configuration before it is claimed.
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
