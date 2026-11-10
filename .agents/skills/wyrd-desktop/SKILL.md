---
name: wyrd-desktop
description: >
  Use when working on the Wyrd desktop Tauri shell: crates/wyrd-desktop
  (Rust backend) plus apps/desktop (bun frontend). Covers the split
  layout, IPC discipline, layer gate, Nix GUI closure, and Tauri v2
  mechanics as used here.
---

# Wyrd desktop (Tauri v2, split layout)

Upstream docs (v2): `https://v2.tauri.app/develop/` (dev server, commands,
state, tests/mocking), `https://v2.tauri.app/start/prerequisites/` (Linux
system deps), `https://v2.tauri.app/develop/configuration-files/`.

## How to read this skill

- **Normative invariants** must hold unless an explicit architecture decision
  changes them (OD/contract update). Do not negotiate them in a PR.
- **Current implementation snapshot** describes what happens to be true now
  (paths, package lists, scripts, env wiring). Verify against the repo before
  changing; never treat a snapshot as a prohibition on legitimate work.

## Layout (non-default, deliberate)

- `crates/wyrd-desktop/`: Tauri backend. Workspace member, owns app
  identity (`tauri.conf.json`, bundle metadata, capabilities, icons)
  and exactly one IPC command surface over the shared `OwnerClient`.
- `apps/desktop/`: web frontend. Bun only (`bun run dev` / `bun run build`);
  no Rust code, no Cargo membership, invisible to the `layer_contracts`
  DAG checker by construction. Never deno (workspace JS toolchain is bun).
- `tauri.conf.json` lives with the backend (snapshot). `frontendDist` points
  at the built `apps/desktop/dist` output; `beforeDevCommand` /
  `beforeBuildCommand` run in `apps/desktop/`. No `devUrl` until a view
  defines a dev server — Tauri serves `frontendDist` itself; do not add Vite
  just to satisfy Tauri.
- `cargo-tauri` accepts a config path, so the non-default location is one
  flag, not a fork. Default Tauri nesting (`src-tauri/` + web dir in one
  folder) is not used here.

## Backend IPC discipline

Current surface (provisional, not a closed enumeration): `lookup`, `stat`,
`readdir`, `read` (reads); `observe`, `liveness`, `freshness` (observation);
`capture_parent`, `submit` (mutation path); plus backend-owned
`reconcile_lookup` (recovery, not a plain command). New operations require an
explicit interface decision and must preserve single-owner and
mutation-authority invariants. Never add a command merely to expose
convenient frontend functionality. `pause`/`resume` stay out until a UI
requirement names them.

- Forward, never interpret. Refusals travel through as data with codes
  intact; two refusals are never collapsed. Preserved in
  `src/backend.rs` as `BackendError::{Unavailable, Unconfirmed, Refused,
  Incompatible}` with `#[serde(tag = "kind", rename_all = "lowercase")]`.
- IPC serialization is an externally observable contract: preserve tagged
  variants and refusal codes exactly, never leak Rust internals as accidental
  API. Shape changes need a contract update plus compatibility tests.
- `capture_parent(path)` captures the parent-directory incarnation token a
  create sends against. `path` is the parent (`""` is the drive root), not
  the file itself. It mutates nothing on its own.
- Poison on `Unconfirmed`: the shared client poisons the broken connection;
  the backend drops it and `reconnect()` mints a new instance. Recovery is a
  new connection, never a resumed stream.
- `submit` is the only mutation path. Single mutation queue, never a second
  route.
- `reconcile_lookup` is backend-owned recovery. Absence is `Ok(None)`; any
  other refusal propagates; transport break stays `Unconfirmed`. Frontend
  never reconciles itself; backend never retries a mutation on its own.
- No store opens. Backend never opens a drive dir, never links the durable
  side, never serves presentation.

## Interface evolution

- `OwnerClient` is the desktop boundary, not a transport convenience.
  Changes to its operations, request semantics, response taxonomy, or
  serialization are interface changes: update the normative interface
  contract (`docs/contracts/owner-interface.md`) before implementation.
- Preserve the four-way distinction (transport failure / uncertain execution
  / owner refusal / protocol incompatibility). No desktop-specific
  reinterpretation of owner semantics; Tauri must not become a second policy
  layer.

## Blocking execution and ownership

- `OwnerClient` is synchronous (`&mut self` per exchange). Never hold an
  exchange on a Tauri async executor thread; run it off-runtime so a 120s
  `DEFAULT_RESPONSE_TIMEOUT` never stalls the event loop.
- `spawn_blocking` requires a `Send + 'static` closure: acquire the managed
  state through an owned handle inside the blocking task, or use a dedicated
  worker that owns `DesktopBackend`. Do not move borrowed `State` references
  or `MutexGuard`s across the boundary.
- Serialize exchanges through the single `DesktopBackend` owner. Never hold
  the state mutex across an unrelated async await. Dedicated worker vs
  Tokio blocking pool stays implementation detail until demonstrated.

## Unavailable vs Unconfirmed, reconnection, dirty quit

- `Unavailable` = could not dispatch to an owner. Retry after availability
  returns, subject to the operation's semantics.
- `Unconfirmed` = may have dispatched but the outcome was not observed.
  Establish a new connection and reconcile mutations before offering another
  submission. Reconnection never transfers outstanding requests or replays
  them.
- Recovery proves nothing about the prior op: never infer success from owner
  recovery or a later `liveness` answer.
- Dirty quit: graceful shutdown drains outstanding requests to terminal
  answers or defined timeouts. Forced termination leaves every dispatched
  request without an observed terminal response `Unconfirmed`; requests
  known never to have dispatched remain known absent. `Unconfirmed`
  mutations require reconcile-after-reconnect before retry; nothing is
  automatically replayed.
- Shutdown lifecycle: closing a window and terminating the owner are
  separate events. Desktop shutdown must not kill the independently managed
  owner (no UI-supervised ownership without an OD-16-E scope change). No UI
  shutdown hook may claim a mutation succeeded because the window closed
  cleanly.

## State and commands (Tauri-macro constraints, not general Rust rules)

- `Builder::manage(Mutex<DesktopBackend>)`, access via
  `State<'_, Mutex<DesktopBackend>>` (no extra `Arc`; Tauri holds it).
  Wrong `State` type fails at runtime (`state not managed`), not compile
  time — pin with a type alias (`type AppState = Mutex<Inner>`).
- Borrowed args (`&str`, `State<'_, T>`) in `async fn` commands require a
  `Result<_, _>` return (command-macro constraint). Otherwise take owned args
  (`String`). Declare per-module `pub fn` with `#[tauri::command]`, register
  once via `generate_handler![...]` — never multiple `invoke_handler` calls.

## Layer gate (two tiers)

Normative: the production dependency closure must not include the
drive-authoring implementation or any crate exposing its capabilities — no
`wyrd-core` / `wyrd-sync` / `wyrd-view` / `wyrd-format` reach, even
transitively. Check member-declared production edges only (dev-deps
excluded); the closure test must fail when an intermediary introduces a
forbidden dep even if the desktop manifest is unchanged. `cargo tree` in CI
is the backstop over the production graph (dev-deps excluded), not the full
graph including tests.

Snapshot: direct policy (`policy_for("wyrd-desktop")`) is `wyrd-owner-client`
only plus reserved `tauri` + `serde`/`serde_json` + `thiserror`, with a
negative test per forbidden edge; the transitive test currently asserts the
desktop closure reaches no workspace member but `wyrd-owner-client`. The
`wyrd-owner-client` extraction (`desktop -> daemon -> core` was the witness)
is why the policy points at the consumer surface. `dev-dependencies` may
reach core/daemon fixtures for the real-owner proofs; they must not leak
into production.

## Nix GUI closure

Normative invariant: desktop GUI dependencies must not leak into headless
Wyrd distribution or execution closures (`wyrd`, `wyrd-host`, contracts stay
GUI-free; flake desktop package opt-in beside them; `-p wyrd-cli` stays the
distribution build).

Snapshot (verify `devenv.nix`, `.ngit/nix/builds/default.nix` before
changing): `cargo-tauri` unconditional; Linux-only set beside `optionals
isLinux` (`glib`, `glib-networking`, `gsettings-desktop-schemas`, `gtk3`,
`webkitgtk_4_1`, `libsoup_3`, `gdk-pixbuf`, `cairo`, `pango`, `harfbuzz`,
`atk`, `dbus`, `librsvg`, `libayatana-appindicator`); `LD_LIBRARY_PATH` ->
dlopen'd appindicator, `GIO_MODULE_DIR` -> glib-networking modules,
`GSETTINGS_SCHEMAS_PATH` (devenv-local intermediate) -> schema dirs consumed
only by the `enterShell` prepend to `XDG_DATA_DIRS` (preserve the data flow,
not the name). macOS: no Nix WebKit; system WKWebView via Xcode.

Carrier rule: the `gateArgs.buildInputs` GUI hunk rides with the `tauri`
crate link (views PR), never blind in a scaffold — the link cannot be
compile-verified from macOS. Required evidence: Linux desktop package
builds, headless package stays GUI-free, existing Linux CLI build stays
green, macOS desktop builds against system frameworks.

## Frontend rules

- Tauri IPC (`invoke`) only. No socket, wire bytes, drive-dir, or protocol
  networking access. No drive-state store; presentation cache only
  (selection, dialogs, inputs, pending indicators, last observed response).
- Four-way taxonomy stays typed end to end; refusal codes exact;
  `Unconfirmed` never becomes success/failure through mapping. Retry follows
  an explicit user decision from the reconciled verdict, never automatically.
- `bun build` transpiles only — it does not typecheck. The views PR must add
  `tsconfig` + `"typecheck": "tsc --noEmit"` and gate on it. At IPC
  boundaries: no `any`, no generic string refusal codes, exhaustive handling
  of the four variants, plus Rust-to-TS serialization compatibility tests
  (types on either side alone do not prove wire agreement).
- Unavailable is a first-class view, not a toast. Owner back resumes without
  restart.
- Production frontend is static, locally bundled assets only: no remote
  frontend, CDN, external app server, or dev server. No framework without
  warranted complexity; no mocks that invent interface semantics.

## Observation semantics (from the interface contract, not invented here)

- Observation is not authority; events are not durable state. A missed event
  must not permanently corrupt presentation — the frontend refreshes from
  authoritative queries. Liveness does not establish freshness; freshness
  does not establish mutation completion. Keep `observe` / `liveness` /
  `freshness` distinct.

## Security

- The registered command handler is the authoritative application IPC
  surface: keep it minimal, validate every command's arguments and
  authorization assumptions in the backend. Capabilities are least-privilege
  grants for Tauri core/plugin APIs only — they do not make an exposed
  custom command safe, and never load remote content into a privileged
  window. Any extra window/webview needs an explicit IPC trust decision.
- `tauri.conf.json` `app.security.csp: null` is a scaffold placeholder
  only; views/packaging sets a restrictive CSP plus capabilities.
- Owner socket (`<drive_dir>/owner.sock`) is a local privileged boundary:
  validate ownership/permissions, no attacker-controlled path substitution,
  peer identity per the interface's same-UID rule where available.
  Establishing transport is not proof of the intended owner.
- Installer (issue 07) provisions two independently running processes that
  find each other via the socket. No discovery service, no UI-supervised
  owner.

## Testing

- Backend IPC proofs (`tests/ipc_behavior.rs`) run against the real client
  types on a real owner (composed view + live loop over a real socket).
  Never mock owner behavior. Minimum matrix: unavailable preserved,
  post-dispatch break stays unconfirmed, refusal codes exact and pairwise
  distinguishable, reconnect mints a new instance with no replay,
  reconcile-absence `Ok(None)` / reconcile-presence / reconcile-break
  stays unconfirmed, `submit`-only mutation, availability returns without
  restart, in-flight forced-termination outcomes stay uncertain. Add:
  concurrent submissions cannot bypass the single queue, stale/delayed
  responses from an old connection cannot overwrite newer state or be
  mistaken for a new submission, frontend rendering preserves refusal codes.
  Not every row needs an expensive real-owner test — place focused cases in
  unit/transport tests, keep composition in E2E.
- Rust mock runtime (upstream `develop/tests/`): execution needs no
  webview/WebDriver/display (plain nextest stays green); Linux compile still
  needs webkitgtk dev libs (the builds-file hunk above).
- Frontend units: `@tauri-apps/api/mocks` (`mockIPC`, `mockWindows`,
  `clearMocks` between runs). `mockIPC` intercepts frontend IPC in the mock
  environment only — it does not exercise the Rust command. Real-owner
  integration stays the backend authority; WebDriver covers the running app.

## What not to add

No frontend framework just for testing; no generic IPC framework (surface
is deliberately small); no second owner connection or mutation queue; no
automatic mutation retries; no async-everywhere (sync `OwnerClient` is
deliberate); no Tauri tutorial duplication (link upstream).
