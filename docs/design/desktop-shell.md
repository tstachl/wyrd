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
  golden vectors (`crates/wyrd-daemon/src/owner/vectors.json`) pin the
  bytes both sides agree on; the shared Rust client
  (`crates/wyrd-daemon/src/owner/client.rs`) is the only transport any
  in-language consumer uses.
- OD-16-F: fail closed on absence; recovery is reconnect-only.
  Transport failure never authorizes retry; an unconfirmed outcome is
  reconciled by re-reading through the owner, never recorded as
  success or failure.
- Rejected shape, enforced not remembered: a Tauri UI that owns the
  drive directly (couples UI lifecycle to store ownership, violates
  owner-independence). The `layer_contracts` gate below fails closed
  on it.

## Prerequisite: which binary serves the socket

The P2 owner-socket issue is load-bearing for every composition this
note describes. Today `wyrd vault` / `wyrd-app` (via
`run_composed_owner`) binds `owner.sock` while `wyrd mount` never
does, so a desktop composition running the drive under `wyrd mount`
leaves the window with no interface — operationally identical to
"mount breaks the desktop app." Either the owner server moves into
the shared composition so mount and headless both bind it, or
`wyrd mount` is marked explicitly non-product (non-desktop) with the
docs saying so. This note assumes the decision lands first and the
shell builds against exactly one unambiguous owner; the composition
test pinned by that issue (`lookup`/`read`/`submit` over `owner.sock`
while mounted, or the mount-docs exclusion) is the shell's
precondition, not its work.

## Blocking open question: bootstrap and discovery

This note describes the steady-state connection. It does not solve
the first connection, and the scaffold must not begin until the
answer is specified. Before any drive exists there is no
`<drive_dir>/owner.sock` to connect to, and the interface runs no
discovery service — so how does a fresh installation reach its first
drive? Creating and joining drives is in scope for the UI, which
means the UI must be able to establish an owner conversation before
any drive directory exists. The candidate shapes:

1. A separately installed service owns a stable endpoint and manages
   drive registration; the UI always talks to the stable endpoint
   first.
2. The installer provisions and starts the owner independently, with
   a defined bootstrap endpoint the UI knows without discovery.
3. The UI supervises the owner process itself.

Option 3 changes OD-06-E and requires an explicit scope decision;
it must not be introduced silently as a bootstrap convenience. The
P2 socket-composition decision does not answer this question either:
making `wyrd mount` expose the socket still leaves a new
installation with no owner to discover before anything is mounted.
The stage-0 gate (below) resolves the bootstrap and discovery
contract — endpoint, provisioning, and first-drive path — before any
frontend scaffolding. A direction under consideration, not a
decision: an installer-provisioned owner with a stable local
bootstrap endpoint, followed by drive-specific owner connections —
which would avoid introducing a general-purpose control plane solely
to solve initial discovery. The choice belongs to stage 0 and must
be demonstrable (a first-drive path that runs), not merely
described. Views are built against the real client types
on a real owner, never against mocks that invent owner behavior.

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
- Framework choice is intentionally open (reviewer input wanted):
  plain web against the backend IPC surface, no store of drive state
  outside what the owner reports. No mocks that invent interface
  semantics in tests; test against the real client types. The
  frontend keeps no direct filesystem access to the drive, no direct
  owner-socket access, and no networking of its own to implement
  protocol operations; every interface response keeps its typed
  semantics end to end. No state-management framework unless actual
  complexity warrants it — three views do not.
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
(`policy_for`, alongside the `wyrd-host` entry): direct workspace
edges limited to `wyrd-daemon`, externals limited to Tauri,
`serde`/`serde_json`, and error/`thiserror` plumbing. Every other
direct workspace edge — `wyrd-core`, `wyrd-sync`, `wyrd-view`,
`wyrd-format`, `wyrd-host`, `wyrd-cli` — is a violation, with a
negative test that names each prohibited edge the way the existing
host-composition checks do. Unknown members already fail closed
("add it deliberately, do not work around this test"), so the crate
cannot exist in the workspace without its policy.

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
link-graph test excluding `wyrd-core` (and, through it, the sync
authoring surface) from the desktop closure — so the policy looks
green only when the architecture claim holds.

Decision rule, applied before the desktop crate is wired (stage 1):
resolve the closure against the intended invariant. If consuming
the shared client through `wyrd-daemon` reaches daemon composition
or core authoring capabilities — which the manifests say it does,
and the new link-graph test confirms mechanically — extract the
narrowly scoped `wyrd-owner-client` crate first (shared client and
wire types; `wyrd-daemon` keeps the server and composition,
`wyrd-desktop` depends only on the consumer surface) and point the
desktop policy at that edge instead. The extraction is therefore
conditional-required by a mechanical check, not pre-committed crate
proliferation and not deferred multi-consumer hardening: no green
link-graph test, no desktop wiring. Its backlog issue tracks the
work; stage 1 runs the check that decides it.

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
- Backend `Cargo.toml` (Rust side): `tauri` v2 plus only the plugins
  a shipped view needs, `serde`/`serde_json`, `wyrd-daemon`;
  Linux-only `webkit2gtk` + `glib` pins matching the reference.
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
   socket-composition decision, then the bootstrap/discovery
   contract from the section above — endpoint, provisioning, and
   the installation-to-first-drive path. Nothing scaffolds until
   both are specified.
1. Scaffold: `crates/wyrd-desktop` with the IPC surface over
   `OwnerClient`, the frontend directory with `tauri.conf.json`
   wiring, the `layer_contracts` policy plus negatives, the desktop
   link-graph test, and the Nix stanzas. No views; the shell compiles
   and the gate holds. First act inside this stage is the closure
   decision rule: if the link-graph test reaches daemon composition
   or core authoring, the `wyrd-owner-client` extraction lands here,
   before the desktop crate is wired. The
   IPC behavioral tests above land here, against the real client
   types on a real owner.
2. Three views against the live interface — never against mocked
   owner behavior.
3. Packaging inputs consumed by the installer issue; human-path
   rehearsal.

## Open questions for reviewers

1. P2 owner socket: blocking prerequisite, resolved before the
   scaffold (stage 0).
2. First-drive bootstrap and discovery: blocking open question,
   resolved before the scaffold alongside the P2 (stage 0). Option 3
   (UI supervises the owner) is out without an explicit OD-06-E
   scope decision.
3. Frontend framework: leave to the implementing PR, with the
   constraints now in the note (no drive model, no socket or
   filesystem access, no protocol networking, no framework without
   warranted complexity).
4. App identifier and bundle metadata: decide before packaging and
   capability configuration; confirm the existing app-id marker
   check covers the chosen id.
5. Settings hand-off (issue 25): reference that issue; duplicate no
   field definitions here.
6. `wyrd-owner-client` extraction: conditional-required by the
   link-graph decision rule in the layer-gate section, not by future
   consumer count. The backlog issue tracks the work; stage 1 runs
   the check that decides it.

## Persistent-format impact

None. The shell forwards existing mutation and status shapes and
adds no durable fact, envelope, or protocol byte.
