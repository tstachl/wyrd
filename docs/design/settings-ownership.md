# Settings ownership, stage 2: UI editor as restricted writer

Status: Frozen, with the app-wide diagnostics amendment in §9 (T20 in
`docs/contracts/trust.md`). This note resolves OD desktop-settings-ownership
stage 2 (`settings_view.rs`): the ownership, concurrency, and
runtime-application contract for UI-driven settings mutation.
Normative invariants live in the contracts linked below; nothing
here authorizes a contract change. Slice order in §8; each slice
is independently reviewable.

## Goal

A non-technical person configures per-drive bounds from the
Settings page, while all of the following keep holding:

- absent key = compiled default, and defaults stay live across
  releases (the file wins over compiled defaults, so a file that
  materializes today's numbers pins them forever);
- the shell shows configured values only — never defaults dressed
  as configuration, never effective runtime posture;
- everything the editor commits, the daemon's strict reader
  accepts (`deny_unknown_fields`, version pin, key validation).

Non-goals: changing precedence (flags > file > defaults),
changing what the daemon applies or when, editing relays (drive
deployment configuration), editing notification records,
multi-file management. The editor is a restricted configuration
writer, not a general settings-management subsystem.

Starting point: `crates/wyrd-daemon/src/settings.rs` (schema
owner, version 1, five optional bounds + `notified` records),
`crates/wyrd-desktop/src/settings_view.rs` (read-only observer),
`docs/contracts/resource-limits.md` (schema home).

## Frozen decisions

| Item | Decision |
| --- | --- |
| Unknown fields | Refuse the edit, preserve the file |
| Unowned supported data | Preserve semantically (not byte-identical) |
| Shared schema | Extract before writer; new minimal crate |
| Transaction | Shared lock (incl. daemon `notified` writer) + content-hash defense |
| Runtime application | Restart-to-apply, except the app-wide `diagnostics` level (§9 / T20), which is live-applied with its shape defined in `docs/contracts/owner-interface.md` |
| Reset | Per-key clearing only |
| Permissions | 0600 + private parent dir (new and existing files) |
| Stopgap | Ship first (path + example empty state) |
| IPC | One drive, sparse update — never a whole-document replacement |
| Persistent-format impact | None |

## 1. Preservation is not acceptance

Strict reading (`deny_unknown_fields`), committed files the
daemon always accepts, and verbatim preservation of unknown
hand-edited keys cannot all hold: an unknown key inside a
schema-governed object fails the strict reader no matter who
writes the file. Hence:

- Known, supported fields: parse and validate strictly.
- Unknown fields in schema-governed objects: refuse the edit,
  preserving the original file — never silently remove unknown
  data, never downgrade a newer version.
- Fields and records outside the editor's ownership that the
  schema permits (other drives' sections, `notified` records):
  preserve without modification.
- Forward-compatible preservation of arbitrary unknown fields
  needs an explicit daemon-reader contract first; not required
  for v0.

## 2. Semantic preservation

A JSON deserialize-modify-serialize cycle may change whitespace,
key order, number formatting, and escaping: byte-identity is the
wrong contract. Unowned values stay semantically identical;
absent settings stay absent; no effective defaults are
materialized. Contract tests: absent keys remain absent;
unmodified configured keys retain values; clearing a key removes
it (empty section removed only where safe, §6B); unrelated
records and other drives' sections survive; no defaults
materialized.

## 3. Schema extraction (prerequisite for the writer)

Two implementations of one contract will drift; parity tests only
detect it after the fact. The shared crate owns: settings
version, serializable schema types, optional bound fields,
`notified` representation, strict parsing and schema validation,
drive-key validation rules. It must not own: filesystem access,
locking, atomic writes, path resolution, UI concerns, daemon
runtime application. Target: a new minimal crate (not
`wyrd-format`: settings are not content-addressed protocol data;
not `wyrd-owner-client`: serde-only types fit its dependency rule
but not its responsibility row). Slice 1 adds the
architecture-table row and the `wyrd-contracts` mechanical edges.

## 4. Transaction

1. Acquire the exclusive sibling lock. 2. Read the current file.
3. Strictly validate (§1 rule). 4. Apply only the explicitly
requested changes. 5. Re-read the path immediately before
committing (same resolved path identity both reads — no TOCTOU
via symlink/file replacement). 6. Verify unchanged via content
hash of the original bytes (not mtime): the lock covers
cooperating writers; the hash detects non-cooperating external
modification. 7. Write a temp file in the same directory.
8. Apply permissions (§5). 9. `fsync` the temp file. 10. Atomic
rename. 11. `fsync` the parent directory. 12. Re-read through the
normal reader and return the observed configured state.

The daemon's `notified` writer must join the same lock protocol —
without it the desktop lock is theater. Implementation gate: the
writer does not start until that lands. Conflict returns a
distinct error; never auto-merge or retry. Stress test:
concurrent notification landings during saves.

## 5. Apply and permissions

Restart-to-apply for v0 per-drive settings; live reload of those is
rejected (new runtime transition, in-flight-work semantics,
owner-interface expansion). The app-wide `diagnostics` level is the one
exception (§9 / T20): its live delivery is `diagnostics-set` with a
`diagnostics-status` read-back in `docs/contracts/owner-interface.md`,
with no durable fact, mutation or policy entry, and in-flight work
keeping the level it started under.
Saving and applying are separate outcomes: saved /
restart-required / restarting / applied-on-restartup / saved-but-
restart-failed. Never report application from restart acceptance.
Reuse the existing service lifecycle and refusal taxonomy; the
confirm copy states that restart interrupts availability. Owner
already stopped: save only. Permissions 0600 for created files
and corrected under the same lock for existing over-permissive
files, with private parent dirs — the credentials convention, no
second policy.

## 6. Editor boundaries

- Drive identity: exactly the daemon's drive-key construction —
  one shared function after §3, never a second desktop copy.
- File states: no file → bootstrap a sparse document; existing
  file without this drive's section → add only this drive's set
  keys; section left empty → remove it where safe. All three
  preserve other drives.
- Failure semantics: failed write = not committed; committed +
  failed restart = committed but not applied. Never conflated.
- Command shape: one drive identity plus one sparse update
  (set-list with values, clear-list). No whole-document
  replacement.

## 7. UX shape

Bound rows render `Using default` (genuine absence, no number) or
the configured value. `Set value` exposes the input;
`Using default` removes the key from the pending update;
validation errors name the key; cancel discards pending changes;
multi-key edits validate fully before writing (no partial saves).
Empty states gain "Set bounds for this drive" once the editor
exists.

## 8. Slice order

1. OD record (this note) + architecture-table row and gate edges.
2. Shared schema extraction. No behavior change.
3. Sparse writer: adapter, locking incl. daemon participation,
   conflict detection, atomic persistence, bootstrap. No UI.
4. Frontend + IPC: controls, sparse updates, validation feedback,
   empty-state entry.
5. Restart-to-apply; docs sync.
6. Full review + CI + live rehearsal (set → diff shows exactly
   that key → restart → applied; clear → key vanishes), then
   merge.

## 9. Amendment: app-wide diagnostics setting (T20)

`docs/contracts/trust.md` T20 introduces one application-wide local
diagnostics level (`default` / `detailed`). It is an exception to this
note's per-drive, five-key scope, not a second settings authority.

| Item | Amendment |
| --- | --- |
| Scope | `AppSettingsFile` gains an app-wide `diagnostics` section beside, not inside, the per-drive map. The five-key sparse contract for per-drive operational bounds is unchanged. |
| Authority | The CLI/settings backend remains the only mutation writer; unknown fields are still refused; the frontend gains no filesystem authority. |
| Ownership | One authoritative user-visible setting. No `diagnostics.json` and no competing process-local source of truth. A `--verbose` launch flag stays a CLI development convenience, never the setting. |
| Activation | Live for subsequent operations, overriding this note's restart-to-apply rule for this setting only: Rust workers through `diagnostics-set` / `diagnostics-status` in `docs/contracts/owner-interface.md`, the File Provider extension through a read-only app-group mirror read at callback boundaries. Desktop surfaces show the persisted state. |
| Compatibility | The new top-level section is not an unversioned additive change: `deny_unknown_fields` makes older readers refuse it. Introducing it bumps `APP_SETTINGS_VERSION`; the reader migrates a v1 file to v2 with an absent level meaning `default`, and refuses a newer version as today. |
| Reader authority | The extension reads only; it never writes settings. The app-group mirror distributes the authoritative value rather than creating a second configuration source. |
| Privacy | The setting widens verbosity under the field-level allowlist in `docs/contracts/trust.md`; it never changes the default-visible warn surface or operational behavior. |

Implementation order puts this amendment first: the extended schema,
the restricted writer, and the read-only distribution path are the
prerequisite for any live activation.

## Acceptance invariants

1. Absence is meaningful. 2. Sparse ownership (five keys, one
   drive). 3. Strict compatibility with the daemon reader.
4. No silent data loss. 5. Atomic multi-key commits. 6. Save ≠
   applied. 7. Configured display only. 8. No frontend filesystem
   authority. 9. One app-wide diagnostics setting, written only through
   the restricted writer, live-applied to workers and the extension,
   never a second configuration authority.
