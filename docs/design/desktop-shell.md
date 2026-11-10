# Desktop shell: Tauri management UI as a non-owning client

Status: Proposed / Open. This note owns the desktop shell shape: the
split between the Tauri backend crate and the web frontend, the IPC
discipline between them, the Nix GUI-dependency boundary, and the
staging into implementable pieces. Normative invariants live in the
contracts linked below. Nothing here authorizes a contract change; the
drive-owner interface (`../contracts/owner-interface.md`) wins every
conflict with this note.

## Goal

A person installs one artifact, launches it, and creates, sees, and
leaves a drive — settings, drive status, lifecycle controls — without
a terminal and without learning the `wyrd` CLI exists. That sentence
is the v0.4 acceptance test (`ROADMAP.md`, v0.4: "without learning
what the `wyrd` CLI is. The UI talks to the drive-owner interface,
not FUSE").

The UI never owns a drive. It is a non-authoritative client of the
drive-owner interface against the independently installed owner
(OD-06-E shape 3): the owner process holds the store and serves
`owner.sock`; the shell forwards operations and interprets nothing —
the same rule as the interface itself.

## Non-goals

- File Provider extension (issue 17 owns it), native per-platform UI
  reimplementations (rejected by OD-06-C absent demonstrated
  inadequacy), auto-update (v0.8), Windows (out of v0.4 per
  `docs/architecture.md`, "Fixed scope").
- Any second mutation path. DG-1's single mutation queue stays
  enforceable: no mutation route through the UI except `submit`
  against the owner's queue, ever.
- Functional views before the interface's consumer rehearsal lands.
  Shell + IPC discipline + contract gate scaffold in parallel; the
  views go live against the interface, not against mocks that invent
  semantics.
- Views beyond the first three. Drive lifecycle (create/join/leave),
  status/posture, and settings ship here; notifications, storage
  usage, and conflicts render through this shell later (issues 18–26).

## Inherited decisions (not re-argued)

- OD-06-C: Tauri for the shared desktop management UI; scope is
  settings, status, lifecycle.
- OD-06-D: desktop distribution ships the host, no discoverable CLI
  entry; the CLI stays first-class and permanent.
- OD-06-E: independently installed owner + Tauri-as-client is the
  production target; a supervised owner needs an explicit scope
  change.
- 16's contract: thin forwarder over `MutationQueue::submit` plus
  status structs, member-hosted disclosure bound, freshness facts. The
  golden vectors (`crates/wyrd-owner-client/vectors.json`) pin the
  bytes both sides agree on; the shared Rust client
  (`crates/wyrd-owner-client/src/client.rs`, re-exported for historical
  importers at `crates/wyrd-daemon/src/owner/client.rs`) is the only
  transport any
  in-language consumer uses.
- OD-16-F: fail closed on absence; recovery is reconnect-only.
  Transport failure never authorizes retry; an unconfirmed outcome is
  reconciled by re-reading through the owner, never recorded as
  success or failure.
- Rejected shape, enforced not remembered: a Tauri UI that owns the
  drive directly (couples UI lifecycle to store ownership, violates
  owner-independence). The `layer_contracts` gate below fails closed
  on it.

## Prerequisite: which binary serves the socket (P2, decided option A)

The P2 owner-socket issue was load-bearing for every composition this
note describes and has landed: both `wyrd vault` / `wyrd-app` (via
`run_composed_owner`) and `wyrd mount` bind `owner.sock` through the
shared `OwnerService`, so a desktop composition running the drive
under `wyrd mount` leaves the window with its interface — the
"mount breaks the desktop app" failure is gone. The alternative
(marking `wyrd mount` explicitly non-product) was rejected: the
desktop-managed surface includes mounted drives, and `wyrd mount`
stays product. The shell builds against exactly one unambiguous
owner surface; the composition test pinning the decision
(`mounted_owner_serves_socket_alongside_backend`: `lookup`/`read`/`submit`
over `owner.sock` while the backend holds the same shared arcs) is
the shell's precondition, not its work.

## Bootstrap and discovery: registry file plus OS services (proposed)

The first connection is specified here; the scaffold must not begin
until this section is approved, and stage 0 stays blocking until
then. Before any drive exists there is no `<drive_dir>/owner.sock`
to connect to, and the interface runs no discovery service — so the
installation-to-first-drive path needs a bootstrap contract. The
candidate shapes were:

1. A separately installed service owns a stable endpoint and manages
   drive registration; the UI always talks to the stable endpoint
   first.
2. The installer provisions and starts the owner independently, with
   a defined bootstrap endpoint the UI knows without discovery.
3. The UI supervises the owner process itself.

Option 3 changes OD-06-E and requires an explicit scope decision;
it is rejected here — it must not arrive silently as a bootstrap
convenience. The P2 socket-composition decision does not answer this
question either: making `wyrd mount` expose the socket still leaves
a new installation with no owner to discover before anything is
mounted.

The proposed shape is option 2, with the endpoint as a file, not a
socket: no new daemon, no new protocol, no general-purpose control
plane. Three pieces, all precedented in-tree:

- **A per-user drive registry**, created at install time with
  owner-only permissions — and owned, at the filesystem level, by
  the user account, with the desktop application's registration
  flow as its normal writer. It is per-user configuration, not
  privileged system state, so no installer-side registry manager is
  introduced; "owned by the installer" means the installer creates
  and perms the file, nothing more. Each entry holds drive facts
  (`name`, `drive_dir`, relay set, service name, mode) and never
  secrets. Updates are crash-safe: write to a temporary file in the
  same directory, sync it, atomically replace, and sync the
  directory, so an interrupted registration can never leave a
  truncated registry that makes every drive undiscoverable. The UI
  treats every entry as untrusted configuration: it validates paths
  before use and still authenticates each owner connection with the
  existing local peer-credential and protocol checks — registry
  content is discovery metadata, never proof of drive identity or
  authorization. Proposed locations, not frozen:
  `XDG_STATE_HOME/wyrd/drives.json` on Linux,
  `~/Library/Application Support/Wyrd/drives.json` on macOS,
  resolved through the platform path APIs the frontend already uses.
  JSON, not TOML, deliberately: the daemon already carries
  `serde_json`, and the registry must add no third-party dependency
  to its closure — the format follows the dependency rule, and the
  registration flow (not hand-editing) is the writer.
- **Per-drive owner services**, installed from a template the
  installer ships: a launchd agent per drive on macOS, a systemd
  user unit per drive on Linux — the same shapes `nix/drive`
  already supervises declaratively, but provisioned imperatively at
  registration time for non-Nix users. The OS service manager owns
  the process lifecycle: enablement is startup-at-login, and a UI
  that exits or crashes never takes the owner down with it, because
  launchd/systemd — not the UI — is the parent. The service runs
  `wyrd vault` with the registration's credential files (never a
  bare env-keyed binary: the supervised owner must load secrets in
  service context, and only the file-credential form works there),
  routed through the `wyrd-supervise` wrapper from `nix/drive`
  (OD-34-C). Managers cannot distinguish the binary's collapsed
  exit codes on their own — every CLI failure is 2 except the
  terminal held-drive 78 — so the wrapper maps the wait status
  onto the exit the supervisor's policy wants, exactly as the Nix
  services already do: launchd `KeepAlive` restarts the wrapper's
  exit 0 and never its 78; systemd `Restart=on-failure` with
  `RestartPreventExitStatus=78`, backoff, and start limits. A bare
  owner binary cannot express this distinction, so a managed
  service without the wrapper beside the `wyrd` binary is refused
  with that pointer instead of installed. A stale socket entry
  from an unclean shutdown is reclaimed at bind while the store
  lock is held, so a restarted owner never wedges on its
  predecessor's socket. The UI distinguishes the lifecycle states
  the policy produces: a restarting owner reads as temporarily
  unavailable (starting / retrying, with the loop's pause switch
  and pass count telling quiet apart from dead), while a disabled
  drive stays registered with its service stopped and an
  unregistered drive simply disappears from discovery — disabling
  stops the service without touching drive data, and only an
  explicit shutdown of the service is a deliberate stop, as
  opposed to a crash the manager will retry. The absence drill
  covers the temporary case; the permanent cases get their own
  first-class states, never an error toast.
- **Owner credential loading independent of the UI.** The owner
  must obtain its secrets without the UI alive: at login, at
  restart, at 3 a.m. with nobody watching. Secrets live in the OS
  keychain / credential files (issue 25 owns the mechanism), and
  the service's loading behavior must work in the actual service
  context — a launchd user agent or systemd user unit, not a login
  shell. The sharp edge is macOS, where keychain access can depend
  on login and unlock state: the installer requests the needed
  keychain access at registration time, and the exact ACLs travel
  with the settings contract, but the requirement is fixed here —
  if the secrets are unreachable in service context, the drive
  reads as registered but unavailable, never as broken, and the UI
  says exactly that.
- **A fixed handoff sequence** for create and join, with
  initialization as an explicit owner-side provisioning operation
  that runs before anything is registered. Creating is
  `Engine::create` (new single-device drive; fails if the directory
  already holds one); joining is `Engine::join` (sealed
  invitation file; commits genesis, and the
  admission itself still arrives through normal intake, so the join
  is not complete until the loop confirms it). Both run in the same
  ownership model as everything else — exclusive keystore access,
  no UI-held store — and neither is a mutation through the UI: the
  UI never acquires authoring capability, it only drives the
  provisioning flow whose terminal states the owner defines. The
  order is fixed: initialize → verify the open (`open_keystore`
  succeeds) → write the registry entry → install, enable, and start
  the per-drive service → verify the socket. Registration and
  enablement happen only after initialization succeeds, never
  speculatively. A managed service needs at least one relay: a
  headless `wyrd vault` refuses a relay-less drive, so supervision
  without relays is refused with that pointer instead of installing
  a definition that can never verify. Partial completion has
  explicit recovery, and a drive is reported ready only when its
  socket verifies this session — never because its registry entry
  exists:
  - initialization fails (directory already holds a drive, bad
    invitation) → nothing is registered; the error surfaces, and a
    retry that finds an existing drive reconciles instead of
    re-creating (`create` refuses; the flow detects and re-verifies);
  - initialization succeeds but the registry write fails → the
    drive exists on disk, unregistered; registration retries
    idempotently;
  - registry written but service installation or enablement fails
    → registered but unavailable; enablement retries, readiness
    is withheld;
  - service starts but the socket never verifies → registered but
    unavailable, same handling.
  The UI's drive states mirror exactly this: not registered,
  provisioning incomplete, registered but unavailable, verified
  available. No larger provisioning state machine is invented; the
  owner stays authoritative for actual drive state, and the
  registry entry is a claim to verify, never proof of health.

Why a file instead of the stable-socket endpoint the review
preferred: a socket endpoint needs a always-on manager process
speaking a new protocol with its own auth, lifecycle, and failure
modes — a control plane built solely to solve initial discovery.
The registry file plus the OS service manager provides the same
properties (stable location, installer-provisioned, UI-independent
lifecycle) with two precedented mechanisms and zero new privileged
code. Per-socket peer authentication stays exactly where the
interface puts it: same-UID checks at each drive socket.

P2 interplay (decided option A): the owner server moved into the
shared composition, so registered desktop drives may run mounted;
the non-desktop marking was rejected. The registry records the mode
per drive; it does not re-decide P2.

Stage-0 exit: the handoff runs end to end through the built
command — `wyrd bootstrap` registers, a separately spawned `wyrd
vault` owns and serves, a non-owner `OwnerClient` gets a terminal
`observe` answer out of the drive socket, and SIGTERM shuts the
owner down clean — pinned as the `bootstrap_then_vault` CLI
integration test, with the supervise wait-status mapping pinned by
the existing `test-supervise.sh` harness leg. What stays manual,
documented on the bootstrap issue: enablement and restart under a
real launchd agent / systemd user unit, which no CI sandbox
provides. The installer mechanics (template, enablement UX,
keychain wiring) stay with the installer issue; what stage 0
freezes is the registry shape, the service template's supervision
contract, and the handoff order above.

## Shape: backend crate plus frontend directory

Tauri's default nests the web UI and the Rust backend in one folder.
This note splits them:

- `crates/wyrd-desktop`: the Tauri backend. A workspace member like
  every other crate, subject to the `layer_contracts` DAG. It owns
  the app identity (identifier, bundle metadata, `tauri.conf.json`,
  capabilities, icons) and exactly one IPC command surface over the
  shared `OwnerClient`. Named desktop-scoped deliberately: a future
  mobile surface gets its own crate and platform story (v0.6), and
  this crate must not imply cross-platform generality.
- `apps/desktop/`: the web frontend. Toolchain-pure (bun, matching
  the `languages.javascript.bun` toolchain already in `devenv.nix`);
  no Rust code, no Cargo membership, invisible to the dependency-DAG
  checker by construction. The three views live here and talk to the
  backend through Tauri IPC only — never to the socket, never to the
  wire bytes.

`tauri.conf.json` lives with the backend (`crates/wyrd-desktop/`),
with `frontendDist` pointing at the built `apps/desktop/` output and
`beforeDevCommand` (`bun run dev`) / `beforeBuildCommand`
(`bun run build`) executing in `apps/desktop/`. No deno anywhere:
the reference editor runs its frontend under deno, but this
workspace's JavaScript toolchain is bun (`languages.javascript.bun`
in `devenv.nix`) and the desktop frontend follows it.
Rationale: the backend owns the app identity and
the IPC surface, so the config that names both sits with it; the
frontend directory stays free of Rust tooling, and `cargo package` /
`cargo check` never see web assets. `cargo-tauri` accepts a config
path, so the non-default location is one flag, not a fork.

## Backend IPC discipline

One Tauri command per interface operation the shipped UI needs,
forwarding through the shared client: `lookup`, `stat`, `readdir`,
`read`, `observe`, `liveness`, `freshness`, `capture_parent`,
`submit`, plus backend-owned reconciliation after unconfirmed
answers. `pause` and `resume` stay out of the initial surface: the
three first views contain no synchronization controls, and there is
no benefit in exposing every owner operation because the interface
supports it. They arrive with the first UI requirement that names
them. The rules, all inherited from the interface contract:

- Forward, never interpret. Refusals travel through as data with
  their codes intact; two refusals are never collapsed into one.
- The four-way answer taxonomy is preserved end to end:
  `Unavailable` (no transport — the owner is down, which says nothing
  about any drive), `Unconfirmed` (no terminal answer — outcome
  unknown), `Refused` (terminal owner refusal), `Incompatible`
  (protocol mismatch). The frontend distinguishes all four because a
  retry decision depends on which one came back.
- Poison on `Unconfirmed`. The shared client already poisons the
  broken connection; the backend drops it and reconnects. Recovery is
  a new connection, never a resumed stream.
- Dirty-quit discipline. A sent request is not a committed write.
  Two cases, specified separately because only one of them can offer
  any guarantee:
  - Graceful shutdown. Stop accepting new commands, track
    outstanding requests, and let each reach its terminal answer or
    its defined timeout. Report nothing as success without the
    owner's authoritative answer. Only then finish orderly shutdown.
  - Forced termination or crash. Make no claim about any outstanding
    mutation: the owner may have committed it even though the answer
    never reached the frontend. On next launch, reconnect and
    reconcile the affected paths through the owner before permitting
    a retry. This case stays possible no matter how carefully the
    graceful path is implemented.
- Reconcile before retry. After `Unconfirmed`, re-read the touched
  paths through the owner (`reconcile_lookup`: absence is data, any
  other refusal propagates, a transport break stays `Unconfirmed`)
  and decide from the re-read. Fold needs extra care: re-read every
  path it touches.
- `OwnerClient` is synchronous. Tauri commands are async. The backend
  runs the client off the async runtime (blocking pool), so a
  120-second `DEFAULT_RESPONSE_TIMEOUT` wait never stalls the event
  loop. No new async runtime edge is introduced for this; the exact
  pool wiring is implementation detail.
- No store opens. The backend never opens a drive directory, never
  links the durable side, never serves presentation. If a future
  contributor reaches for `wyrd-core`, `wyrd-sync`, `wyrd-view`, or
  `wyrd-format` from this crate, the gate (next section) refuses.
- One client per drive connection, held under exclusive access. The
  shared client is sequential (`&mut self` per exchange) and poisons
  itself on the first `Unconfirmed`, so concurrent Tauri commands
  serialize on the backend's single instance; when one request
  poisons the connection, every operation after it resolves
  `Unconfirmed` without touching the socket, and reconnection mints
  a new instance. Outstanding operations at reconnect time resolve
  `Unconfirmed` — reconnection never replays a mutation and never
  converts an unconfirmed outcome into success. The implementing PR
  demonstrates these properties; the exact blocking-pool wiring is
  implementation detail, but the concurrency contract is not left
  implicit.

## Responsibility boundary: backend owns recovery, frontend owns display

"Forward, never interpret" governs the backend; the frontend's
four-way taxonomy handling governs display and explicit user
decisions. The split:

| Responsibility    | Backend                  | Frontend                    |
| ----------------- | ------------------------ | --------------------------- |
| Transport         | Owns                     | None                        |
| Protocol taxonomy | Preserves typed results  | Receives typed results      |
| Refusal codes     | Preserves exactly        | Displays appropriately      |
| Reconnection      | Owns                     | Requests/observes state     |
| Reconciliation    | Performs through owner   | Presents outcome            |
| Mutation retry    | Never automatic          | Requires explicit decision  |
| UI presentation   | None                     | Owns                        |

Concretely: `reconcile_lookup` is a backend-owned recovery
operation, not an ordinary frontend-facing command. The frontend
asks the backend to reconcile a path and receives the verdict; it
never performs reconciliation itself. The frontend must never
translate `Unconfirmed` into an ordinary failed mutation, and the
backend never retries a mutation on its own — retry follows an
explicit decision (ultimately the user's) taken from the reconciled
verdict.

## Frontend: three views and the unavailable state

- Drive lifecycle: create, join, leave.
- Status/posture: durable observation, liveness, freshness — the
  three kept distinct (a live socket is not a durable fact; a paused
  owner is reachable and quiet, not down).
- Settings: everything the app persists, handed off to the settings
  surface (issue 25). The owner holds secrets; the UI never sees key
  material beyond the member-hosted disclosure bound, and the
  credential path uses no env vars and no terminal.
- The unavailable state is a first-class view, not an error toast.
  Owner down means an explicit unavailable screen; the shell never
  opens the store itself. Owner back means the shell resumes without
  restart.
- Framework choice, decided: Svelte 5 + shadcn-svelte, no SvelteKit
  (local desktop app: no SSR, routing infrastructure, or server
  endpoints). Svelte components styled with Tailwind, built with
  Vite, against the backend IPC surface; no store
  of drive state outside what the owner reports. No mocks that invent
  interface semantics in tests; test against the real client types. The
  frontend keeps no direct filesystem access to the drive, no direct
  owner-socket access, and no networking of its own to implement
  protocol operations; every interface response keeps its typed
  semantics end to end. Runes-based presentation cache only — never a
  second drive model (see the presentation-cache section below).
- Frontend state is a presentation cache, never a second drive
  model. Transient presentation state is fine and expected: selected
  view, open dialogs, form input, pending-request indicators, last
  observed response. Independently authoritative state is forbidden:
  drive membership, drive lifecycle, materialization policy, durable
  status, mutation outcomes. The frontend displays what the owner
  last reported and marks it as such; when the reports change, the
  display follows. This distinction carries the later views
  (notifications, conflicts, storage usage in issues 18–26), so the
  scaffold sets the precedent now.

## Layer gate (two tiers, honestly described)

`wyrd-desktop` gets a `MemberPolicy` entry in
`crates/wyrd-contracts/src/layer_contracts.rs`
(`policy_for`, alongside the `wyrd-host` entry): the single direct
workspace edge is `wyrd-owner-client` — the extraction the closure
decision rule below required has landed, so the desktop policy
points at the consumer surface instead of `wyrd-daemon` — and
externals are limited to the reserved `tauri` toolkit edge,
`serde`/`serde_json`, and error/`thiserror` plumbing. Every other
direct workspace edge — `wyrd-core`, `wyrd-sync`, `wyrd-view`,
`wyrd-format`, `wyrd-host`, `wyrd-cli`, `wyrd-daemon` — is a
violation, with a negative test that names each prohibited edge the
way the existing host-composition checks do. Unknown members already
fail closed ("add it deliberately, do not work around this test"),
so the crate cannot exist in the workspace without its policy.

The direct tier is not the whole invariant, and the note does not
pretend it is. The intended property is reachability: the desktop
dependency closure must not reach `wyrd-core` or any authoring
capability, even transitively. A direct-edge allow on `wyrd-daemon`
cannot establish that, because the daemon's own manifest reaches
`wyrd-core` — `desktop → daemon → core` is three allowed edges that
link the authoring surface into the desktop crate. The gate has
precedent for exactly this shape:
`view_link_graph_excludes_sync_and_iroh` and
`consumer_link_graph_excludes_presentation` walk the
member-declared production closure, not just direct edges. The
scaffold therefore adds a third check of the same kind — a desktop
transitive Cargo dependency-closure test excluding `wyrd-core`
(and, through it, the sync authoring surface) from the desktop
closure — so the policy looks green only when the architecture
claim holds. Like its precedents it walks member-declared
production edges, not the final linked binary; the name says
exactly that, and `cargo tree` in CI remains the full-graph
backstop.

Decision rule, applied before the desktop crate is wired (stage 1):
resolve the closure against the intended invariant. Consuming the
shared client through `wyrd-daemon` reached daemon composition and
core authoring capabilities (`desktop → daemon → core`), as the
closure test confirmed mechanically — so the narrowly scoped
`wyrd-owner-client` crate was extracted first (shared client and
wire types; `wyrd-daemon` keeps the server and composition,
`wyrd-desktop` depends only on the consumer surface) and the desktop
policy points at that edge instead. No green closure test, no
desktop wiring.

## Nix GUI dependencies (ported from the enesis editor)

Reference implementation: `~/workspace/enesis/editor/devenv.nix`
(package set, env vars, `enterShell` stanza) and
`apps/tauri/src-tauri/Cargo.toml` (Tauri v2, Linux `webkit2gtk`
pin). Port the mechanics, not the extras (no Android SDK, no
GStreamer/media stack — Wyrd v0.4 has no camera or media path).

- `devenv.nix`: add `cargo-tauri` to the unconditional package set
  (dev-only CLI, like `bats`/`nextest`). Add the Linux-only GUI set
  beside the existing `optionals isLinux` block: `glib`,
  `glib-networking`, `gsettings-desktop-schemas`, `gtk3`,
  `webkitgtk_4_1`, `libsoup_3`, `gdk-pixbuf`, `cairo`, `pango`,
  `harfbuzz`, `atk`, `dbus`, `librsvg`, `libayatana-appindicator`.
  Add the three Linux-only env vars verbatim in purpose:
  `LD_LIBRARY_PATH` → `libayatana-appindicator` (dlopen'd at
  runtime, unresolvable at link time); `GIO_MODULE_DIR` →
  `glib-networking` modules (without it WebKitGTK fails every HTTPS
  request with "TLS support is not available"); and
  `GSETTINGS_SCHEMAS_PATH` → `gsettings-desktop-schemas` + `gtk3`
  (FileChooser and desktop schemas), with the `enterShell` prepend
  to `XDG_DATA_DIRS` for binaries the wrapper hooks never see.
  Naming note, verified against the reference: the plural
  `GSETTINGS_SCHEMAS_PATH` is a devenv-local intermediate, not a
  GLib standard — the actual GLib mechanism is `XDG_DATA_DIRS`, and
  the variable is only ever consumed by the `enterShell` prepend. An
  implementation that copies the stanza must preserve the data flow
  (schema dirs into `XDG_DATA_DIRS`), not the name, and must prove
  the resulting desktop package in a clean Nix environment rather
  than relying on the developer shell to conceal missing runtime
  dependencies.
- macOS: no Nix WebKit. System WKWebView via Xcode, same split as
  FUSE today (stubs for build, system facility at runtime). The
  backend crate carries the macOS-only WebKit glue (enesis uses
  `objc2-web-kit`); nothing Nix-side changes for darwin.
- App-package-only rule (OD-06-C Linux policy): the GUI set enters
  the desktop package closure only. `wyrd`, `wyrd-host`, contracts,
  and headless closures stay GUI-system-dependency-free. In
  `flake.nix` the desktop package is opt-in (a new
  `wyrd-desktop`-style package beside `wyrd`/`wyrd-host`), and the
  flake contract keeps `-p wyrd-cli` as the distribution build.
- Backend `Cargo.toml` (Rust side): the shared `wyrd-owner-client`
  (never `wyrd-daemon`: the `desktop → daemon → core` composition the
  closure test rejects is exactly what the extraction removed),
  `serde`/`serde_json`, error/`thiserror` plumbing, and the reserved
  `tauri` toolkit edge; the `tauri` crate itself (plus `tauri-build`
  as a build-dependency, invisible to the layer gate) links with the
  command layer — the Linux GUI closure rides with it and is proven
  by the Linux builder, not from macOS. Only the plugins a shipped
  view needs link with the first view. Linux-only `webkit2gtk` +
  `glib` pins match the reference when the link lands.
  Reviewers: flag any plugin beyond `dialog`/`store`/`opener` scope
  creep at review time.

## Packaging inputs for the installer (issue 07)

"One artifact" means one user-facing installer that provisions two
independently running processes — not one process. The installer
owns installing, upgrading, starting, and stopping both the owner
and the UI, and the two lifecycles stay independent afterwards: the
UI exiting or crashing never affects the owner, and the owner going
down never promotes the UI into an owner. The installer must not
turn the UI into the owner merely to achieve the one-artifact
experience; that would reintroduce the rejected shape through
packaging.

The shell fixes the per-platform start/stop/recovery story for owner
plus UI under shape 3 (independent installs that find each other
through `<drive_dir>/owner.sock`, no discovery service): startup at
login, owner-first ordering, and the File-Provider-before-UI case
documented as behavior (issue 17 owns the extension, not this note).
Distribution mechanics (archives, signing, updates) stay with the
installer issue; auto-update is explicitly v0.8.

## Verification

- `layer_contracts` gate green on the new crate, including the
  negative tests for the rejected `wyrd-core` authoring edge.
- Absence/recovery drill: owner down shows the explicit unavailable
  state and the backend never opens the store; owner back resumes
  without restart (OD-16-F explicitness, UI side).
- Dirty-quit parity: quit with an in-flight operation under graceful
  shutdown. Outstanding operations drain until terminal answers or
  defined timeouts; nothing is reported successful without an
  authoritative owner answer; timed-out operations remain
  `Unconfirmed`, never recast as failures. Forced termination leaves
  every outstanding outcome unknown and requires reconciliation
  after reconnect.
- Human-path rehearsal (shared with the installer issue): install,
  create drive, see it, quit cleanly — no terminal.
- No new wire bytes, so no new golden vectors: the backend speaks
  through the shared client, already pinned by `vectors.json`. Any
  drift in the mapped types fails the existing field-name snapshot,
  not a new test. The snapshot proves the mapping is complete; the
  IPC layer still needs its own behavioral proof, because a complete
  mapping can still collapse two outcomes in transit:
  - every refusal retains its original code through the IPC round
    trip;
  - `Unavailable` and `Unconfirmed` remain distinct at the frontend;
  - reconnection never replays a mutation;
  - an unconfirmed mutation cannot become a success through frontend
    mapping;
  - the UI cannot invoke any mutation path except `submit` against
    the owner's queue.

## Staging

0. Prerequisite gate (blocks everything below): the P2
   socket-composition decision (landed: option A, mount serves the
   socket through the shared `OwnerService`), then the bootstrap/discovery
   contract from the section above — endpoint, provisioning, and
   the installation-to-first-drive path. Nothing scaffolds until
   both are specified.
1. Scaffold: `crates/wyrd-desktop` with the IPC surface over
   `OwnerClient`, the frontend directory with `tauri.conf.json`
   wiring, the `layer_contracts` policy plus negatives, the desktop
   transitive dependency-closure test (member-declared production
   edges, after the `view`/`embed` precedent — not a linked-binary
   audit; `cargo tree` in CI remains the full-graph backstop), and
   the Nix stanzas. No views; the shell compiles
   and the gate holds. The `wyrd-owner-client` extraction the
   closure decision rule required has landed, so the desktop crate
   wires straight to the consumer surface. The
   IPC behavioral tests above land here, against the real client
   types on a real owner. The `tauri` crate link and the command
   wrappers land with the command layer against the already-pinned
   surface, proven by the Linux builder; plugins a shipped view
   needs wait for the first view.
2. Three views against the live interface — never against mocked
   owner behavior.
3. Packaging inputs consumed by the installer issue; human-path
   rehearsal.

## Open questions for reviewers

1. P2 owner socket: decided (option A, landed) — prerequisite met
   before the scaffold (stage 0).
2. First-drive bootstrap and discovery: decided as proposed in
   the bootstrap section (registry file + OS per-drive services,
   option 3 rejected without an OD-06-E scope change). Stays
   blocking until the stage-0 rehearsal runs green and the review
   approves; the installer mechanics stay with the installer issue.
3. Frontend framework: decided with the implementing PR (Svelte 5 +
   shadcn-svelte, no SvelteKit), under the constraints in the note
   (no drive model, no socket or filesystem access, no protocol
   networking, presentation cache only).
4. App identifier and bundle metadata: decide before packaging and
   capability configuration; confirm the existing app-id marker
   check covers the chosen id. The scaffold's `tauri.conf.json`
   reuses the workspace `md.enesis.wyrd` id as a placeholder so the
   existing marker covers it; the distinct desktop bundle id (the
   host Flatpak keeps `md.enesis.wyrd`) is decided with packaging,
   not here.
5. Settings hand-off (issue 25): reference that issue; duplicate no
   field definitions here.
6. `wyrd-owner-client` extraction: landed — the closure decision
   rule in the layer-gate section required it (`desktop → daemon →
   core`), and the desktop policy points at the consumer surface.

## Persistent-format impact

The per-user registry file (`drives.json`, schema version 1, reader
refuses newer versions loudly). Nothing else: the shell and its
provisioning forward existing mutation and status shapes and add no
durable drive fact, envelope, or protocol byte.
