# Product terminology (desktop and CLI)

Status: **Normative — frozen (2026-11-19).** This document is the canonical
home for these product-language rules. Tracking issue:
`nostr:nevent1qqsyhtumnprpxcqcphnj5jqz9uw5xlnaygn67nr98mfc237pg8y7megpz9mhxue69uhkwunpwdczuap49eehggkvcpu`.
Open decisions are explicitly deferred (see "Deferred decisions") and do not
change the rules below.

## Purpose and scope

This document defines the product language of Wyrd across two surfaces: the
desktop application (ordinary users) and the `wyrd` CLI (operators). It
governs user-facing names, labels, help, prompts, status lines and errors. It
does not govern protocol identifiers, contract vocabulary, or internal code
names.

Authority: normative product language is subordinate to the invariants in
`docs/contracts/`. Where a wording choice and a contract disagree, the
contract wins and this document is wrong.

## Normative language

MUST / MUST NOT / SHOULD / MAY are normative. "Recommendation" is advisory.
"Decision required" items are collected in the decision register.

One concept has one canonical name per surface. Related-but-distinct concepts
keep distinct names. Do not rename a meaningful Wyrd concept to make it
friendlier; explain it instead. Do not expose an implementation mechanism in
ordinary copy merely because it is technically accurate. Plain language must
not manufacture a guarantee the system does not provide.

### How to read source references

Citations such as `registry.rs:198-200` are **review-time evidence** for the
claims they support, not durable authority. File paths, contract sections and
the behavior they describe are what must hold over time; line numbers drift.
Each glossary row separates the **observed meaning** (descriptive, grounded
in the cited code and contracts) from the **wording rules** (normative).

## Product language principles

- **Desktop.** Assume no knowledge of distributed systems, Merkle DAGs,
  protocol internals or service architecture. Explain what is happening,
  what the user can do, what requires attention and what they may lose.
- **CLI.** Assume familiarity with filesystems, devices, identities, relays,
  synchronization, snapshots, recovery and configuration. Command names are
  concise and predictable; help explains behavior and consequences; errors
  explain what happened, whether anything changed, and what to do next.
- **Shared.** Prefer established vocabulary; do not invent a terminology
  framework. Semantic accuracy wins over friendliness. Ship explanations
  with terms rather than replacing terms. Preserve the distinctions between
  reachability, synchronization success, freshness, local availability and
  durable state.

## Canonical glossary

The **Observed meaning** column is descriptive: it reports what the concept
is in the current implementation and contracts. The Desktop, CLI and Avoid
columns are normative rules.

| Canonical term | Observed meaning (descriptive) | Desktop (normative) | CLI (normative) | Technical-only | Avoid |
|---|---|---|---|---|---|
| Drive | The logical, append-only collection of files and history shared by devices; snapshots bound to a DriveId (`docs/architecture.md:5,11`). | Drive | `drive`, `<DRIVE_DIR>` | DriveId | Collection, workspace, folder |
| Device | A machine participating in a drive. Its identity is represented by an immutable public key; the private proof is separate (workspace AGENTS.md; `docs/architecture.md:50`). | This device | `device`, `<DEVICE>` | DeviceId, Nostr public key | User, account, "member" alone |
| Device identity | A device is recognized by its public identity key; the corresponding private key material proves that the device controls that identity. `DeviceId` is the Nostr public key (`docs/contracts/trust.md:118`; `docs/contracts/object-model.md:35-38`). Not a drive, not a drive's files; distinct from the separate device encryption key (T14, `docs/contracts/trust.md:743`). | "this device's identity" (normative: `docs/design/first-run-onboarding.md:24-30`); progress label "Device identity" | `identity` | Nostr public/private key, identity file, DeviceId | "your identity", "drive identity", "two parts" |
| Drive identity | The protocol-level DriveId binding snapshots and authorization. No product-facing name exists in either surface. | Not exposed; distinguish "device identity" from "joining a drive" | Not exposed | DriveId | "your identity" |
| Identity key / private key material | The corresponding private material that proves control of the public identity key; cannot be recreated from the passphrase (`OnboardingView.svelte:355-358`). | Identity key (keep, with existing explanation — D-4b) | `--identity-file <PATH>` | nsec, 32 raw bytes / 64 hex (`args.rs:36-38`) | Key file as primary label |
| Passphrase | The text protecting access to the local private key material; the shell never stores it (`OnboardingView.svelte:367-370`; `credentials.rs`). | Passphrase | `--passphrase-file`, `--passphrase-stdin` | Keystore passphrase | Password (implies server login) |
| Invitation | A protected file carrying admission material to a device out-of-band; a lost one is reissued, not re-admitted (`invitation_surface.rs:53-59`). | Invitation (sealed only in description) | `member invite`, `member reissue` | Sealed bootstrap | Invite code, token, link |
| Relay | A control-plane transport endpoint; relay acceptance is not recipient receipt (`docs/contracts/sync-and-peers.md:82-99`). | Relay | `--relay <URL>` | Mailbox, control plane | Server (implies data hosting) |
| Drive owner | The single process holding the drive's durable-store lock and serving its socket (`docs/architecture.md:36-38`; `docs/contracts/owner-interface.md:5-19`). | Not introduced by default. Observable-state wording in ordinary copy; explanation only where needed (R-D1). | `owner` | Owner interface, mutation queue | "service", bare "owner" in ordinary app copy |
| Drive service | An OS supervision definition (launchd/systemd) that can start and restart an owner process; distinct from the process itself (`bootstrap/registry.rs` `RegistryEntry`; `bootstrap/service.rs` `render_launchd_plist`/`render_systemd_unit`; `service.rs:28-34`). | Drive service — only where a definition exists (D-1). | Not exposed | Service definition, service token | "Owner" where a definition is meant |
| Session | A per-run owner started with a one-time passphrase, installing no service; survives desktop quit — ends on reboot, explicit lock, or stopping the drive (`docs/contracts/desktop-lifecycle.md`; `registry.rs:187-203`; `UnlockView.svelte:95-96`). | Session (with "for this session" kept) | `drive unlock` | Definition-less owner, IPC round trip | Login session, Nostr signer session |
| Snapshot | A signed, immutable version of the drive DAG. | Snapshot (via history language) | `snapshot` | Merkle snapshot, DAG | Version (alone) |
| Head | A snapshot with no descendants; "eligible" adds authorization and lineage conditions (`docs/contracts/object-model.md:437-438`; `docs/contracts/epochs.md:408-432`). | Technical detail only | `snapshot heads`, `@N` | Head, eligible head | Branch (implies Git semantics) |
| Divergence | More than one eligible head; distinct from a path conflict (`conflict_surface.rs:101-142`; `docs/contracts/object-model.md:453-462`). | Technical detail / status description | `snapshot diverged` | DAG divergence | Conflict (unqualified) |
| Merge | Authoring one snapshot over selected heads with root-level take/drop/default (`docs/contracts/object-model.md:462`; `snapshot.rs:36-125`). | Technical detail | `snapshot merge` | MergedDir content union (`conflict_surface.rs:203-217`) | Combine |
| Recovery | Owner-only authoring that grafts explicitly identified content from a stranded fork without adopting its lineage (`docs/contracts/epochs.md:574-588`; `snapshot.rs:397-406`). | Recovery (in restore-limitations copy) | `snapshot recover` | Recovery snapshot, stranded fork | Restore (of identity) |
| Membership | The signed history of device seats — owner, member, reader — in epochs; not human accounts (`docs/contracts/epochs.md:85-118`; `member.rs:422-445`). | Technical detail / "members" count | `member` | Transition, epoch | Member (meaning a person) |
| Device removal | Authoring a device's exit. Bounds future authorized participation; does not erase copies already obtained (`member_surface.rs:36-51`; `docs/contracts/trust.md:834-847`). | "Remove this device" | `member remove` | Remove(self), removal boundary, content roots | Revoke, wipe, lose access |
| Vault role | A device-local retention/provider posture: content the device holds is not automatically discarded to free space; still subject to the storage limit; not a process and not a service mode. Not a promise that every object ever received is stored, since quota refusal can prevent storage (`docs/contracts/sync-and-peers.md:531-560,576-622`; `role.rs:163-180`). | Vault, with approved explanation. | `materialization role set vault` | `role.rs:38-47` vault role | Backup, archive, "unlimited storage" |
| Vault service mode | A supervised deployment shape: a headless owner service without a FUSE mount (`bootstrap/registry.rs` `ServiceMode`). Same word, different concept. | Not exposed as a mode name | `drive bootstrap --mode vault` | `ServiceMode::Vault` | Bare "vault" in explanatory prose where the distinction matters (`docs/contracts/sync-and-peers.md:555-560`) |
| Local availability | Whether bytes are recorded locally. Residency says nothing by itself about whether other devices hold or can serve the content ("residency is not availability", `docs/contracts/retention-constraints.md:103-105`). | Local files: kept / stored / not stored (R-D2). | `materialization` (D-2). | PINNED/CACHED/REMOTE_ONLY, PRESENT/ABSENT (`docs/contracts/sync-and-peers.md:512-546`) | Bare "available" |
| Pinning | A durable retention promise per ContentId, not per path; never fetches (`policy.rs:546-583`). | "Kept on this device" | `materialization pin` | Pinned fact, retention promise | Favorite, bookmark |
| Caching | The fetch loop's arrival marking; carries no retention promise (`policy.rs:1-18`). | "Stored on this device" | `materialization` status | Cached, fetch admission | Downloaded (implies kept) |
| Synchronization | Bounded passes exchanging snapshots; a clean pass is not convergence (`posture.rs:94-105`). | Sync | `sync` | Pass, convergence, outbox | "Synced" as one green state |
| Pending work | Mixed outstanding work: outbox control work (announcements, transitions, capabilities), locally staged work and unstarted fetches; not uniformly "changes", not files or items (`config.rs:228-242`; `status.rs:196-212`). | Pending work | `sync now` status/errors | Obligation, carry, outbox | Items, pending files |
| Storage limit | Refusal boundary over retained object bytes on authoring and receive paths; not a disk cap (`docs/contracts/resource-limits.md:78`; `docs/contracts/retention-constraints.md:24-37`). | Storage limit + scope description | `--retained-quota` (D-3). | retained_bytes_quota, receive_retention_ceiling | Disk space, free space, disk usage |
| Local persistence | Append-only local objects, no GC in v0; durability ladder Working → Committed → Published (`docs/architecture.md:9`; `docs/contracts/write-path.md:255-285`). | Technical detail | `sync status` | Working/Committed/Published | "Saved" unqualified |
| Peer connectivity | Relay and peer transport health; relay-accepted is the maximum v0 guarantee, not recipient receipt (`docs/contracts/sync-and-peers.md:82-99`). | Connection / relay health | `sync status`, `mailbox` | Mailbox live/degraded, SendReport.accepted | "Delivered" unqualified |

## Desktop terminology rules

**R-D1 — Owner.** The app MUST NOT require ordinary users to learn about the
owner process. Ordinary copy MUST use observable-state wording ("this drive
isn't running", "the drive answered", "last reported drive status"). The
approved explanation MAY appear only where the UI exposes the concept or
needs it to explain an operational state. Bare "owner" MUST NOT appear in
primary status, action or error copy.

- The app MUST NOT globally substitute "Drive service unavailable" for
  "Owner unavailable": a definition-less locked drive has no service
  (`registry.rs:198-200`), and availability, `ServiceState` and
  `SessionState` are separate fields (`registry.rs:152-177`).
- Where a definition exists, "Drive service" SHOULD be used for actions and
  status ("Start drive service", "Restart the service").
- Where no definition exists, copy MUST describe the observable state, not a
  nonexistent service.

**R-D2 — Status labels and card meaning.**

| Current | Proposed | Preserved question |
|---|---|---|
| Durable observation | Sync activity | What has Wyrd recorded or completed? |
| Liveness | Drive activity | What was the drive's last reported loop state? |
| Freshness | Drive data (interim; D-11) | Is the served data current for this device? |
| Queue | Pending work | Is anything waiting to happen? |
| Materialized | Local files | What is the file residency state here? |
| Paused — reachable and quiet, not down. | Synchronization is paused. The drive is still running and can answer requests. | Reachability is not progress. |

These five labels describe different dimensions of drive state; they MUST NOT
be presented as variations of a single status. Their presentation hierarchy,
grouping and layout are implementation concerns outside this document.

- "Drive status" MUST NOT be used as the card label. The screen is already
  titled "Status", and the label implies one state. "Drive activity" MUST be
  defined as: the owner's last terminal answer about its loop — the number
  of completed passes, whether it is paused, whether the relay stream was
  alive and how many relays were connected at that pass, and whether that
  pass published a new projection (`StatusView.svelte:228-266`;
  `wire.rs:586-597`). It MUST NOT claim "recent activity" or "currently
  active": the wire carries no pass timestamp, `stream_alive` is sampled per
  pass, and high pass counts include idle iterations and do not mean writes
  (`pass.rs:1087-1091`; `mailbox.rs:366-390`). The only clock is the shell's
  observation stamp (`format.ts:14-17`).
- The "Last publish" row MUST be relabelled so it cannot read as a time —
  e.g. "Published on last pass" with Yes / No / "No pass reported"
  (`StatusView.svelte:260-266`).
- Freshness (D-11 open). `WireFreshness` exposes only state, revision and
  generation (`wire.rs:619-626`; `results.ts:218-227`). "Fresh" means the
  served projection was regenerated from the engine's latest durable
  revision; it MUST NOT imply convergence with peers and can coexist with
  pending heads (`pass.rs:884-885,933-962,1014-1027`; convergence is a
  separate field at `wire.rs:275-284`). Interim state text: "Current" /
  "Current (empty)" / "Last good copy — stale: {reason}", with the explicit
  note that freshness is not a convergence claim. No synchronization
  timestamp is invented or implied.
- Local files three-way distinction:
  - **Kept on this device** — content selected for retention (pinned): a
    promise to keep it locally; Wyrd will not discard it to free space.
    Pinning does not fetch content (`policy.rs:546-583`); bytes arrive when
    needed or when a sync fetches them.
  - **Stored on this device** — bytes recorded locally (present); may still
    be reclaimed under storage pressure unless kept. Presence is a
    durable-log record, not a live probe (`policy.rs:41-67`).
  - **Not stored on this device** — bytes not recorded locally. This is a
    residency fact only. It MUST NOT be rendered as "available from other
    devices": `REMOTE_ONLY` does not establish that another device currently
    has the content, is reachable, or can serve it
    (`docs/contracts/retention-constraints.md:103-105`).
  - Fetchability MAY be stated in context only where the UI actually has
    peer/content knowledge supporting it; never derived from residency
    alone. The CLI's cached/remote-only collapse
    (`docs/contracts/sync-and-peers.md:523-546`) is a separate CLI question
    (D-2).

**R-D3 — Onboarding.** "This device's identity" MUST remain normative
(`docs/design/first-run-onboarding.md:24-30`). "Restore from backup" MUST NOT
be replaced (D-4a — resolved; pinned by `onboarding-copy.test.ts`). "Sealed invitation" SHOULD become "Invitation"
with the protection explained in the description. The "Wyrd cannot recover
this device's identity" warning MUST remain verbatim
(`OnboardingView.svelte:666-672`; `onboarding-copy.test.ts:83-97`). The
public-identifier/private-proof distinction SHOULD follow the approved
explanation.

**R-D4 — Settings.** The header "This drive's configured bounds, as
configured…" (`SettingsView.svelte:265`) MUST be replaced with the approved
explanation. "About editing bounds" MUST become "About these limits".
`boundLabel(null)` (`format.ts:91-96`) SHOULD read "Using Wyrd's default",
with a disclosure that the shell cannot see the compiled value. No fabricated
default: fields are `number | null` with no default field
(`results.ts:361-375`). The five descriptions MUST use the source-verified
approved text; the current strings at `format.ts:123-127` and
`SettingsView.svelte:481-540` are not sufficiently precise.

**R-D5 — Consequential actions.** Confirmations MUST state consequences
before action. Desktop exposes discard, new identity, leave, local data
removal and service restart; remove device, set owner, resolve and reissue
stay CLI-only. Local data removal runs the installed `drive delete`: it
authors `Remove(self)` before removing the directory, carries the same
terminal acknowledgement and locality copy as the CLI, and does not touch
another member's copy. Core copy is strong — `render_deletion_confirmation`
bans euphemisms and states locality and terminality
(`lifecycle_surface.rs:105-137`) — and MUST NOT be softened. Device removal
MUST follow the approved explanation. "Lock for this session" SHOULD state
that it ends the session until unlocked again, and that reboot or locking
the drive also ends it; it MUST NOT claim quitting the app ends it —
quit leaves a running session drive unlocked
(`UnlockView.svelte:95-96`; `docs/contracts/desktop-lifecycle.md`).

**R-D6 — Technical-detail disclosure.** Restricted terms MUST be moved
behind a disclosure in Status and Settings. A disclosure exists only in
RepairView (`RepairView.svelte:146-161`); onboarding has a limited "Details —
what restore does and doesn't cover" collapsible. Primary copy MUST describe
consequences; disclosures MUST preserve evidence, including content-root
boundaries and protocol identifiers.

**R-D7 — Errors.** Wording MUST follow the actual failure class; unlike
failures MUST NOT be forced into one explanation. The four existing kinds
are Unavailable, Unconfirmed, Refused, Incompatible (`client.rs:42-77`;
`backend.rs:41-75`; `errors.ts:39-92`).

- Local validation/refusal, no request sent (`lib.rs:250-259`, "no socket was
  touched"): MUST read "Wyrd refused this path: {detail}". It MUST NOT name a
  remote component, say "incompatible", or claim a version mismatch.
- Remote unavailable / unconfirmed: keep distinct; do not convert into a
  compatibility claim. Unparseable frames are Unconfirmed and poison the
  client (`client.rs:525-526,548-554`).
- Remote explicit refusal: `Refused(WireError)` with a defined code
  (`wire.rs:842-917`); report the refusal, not a mismatch.
- Wire version skew in a response (`client.rs:527-532`): MAY say the response
  used a different wire version. The server's request-side version refusal
  is `Refused(UnknownVersion)` (`wire.rs:20-29,846-847`), not Incompatible.
- Unexpected/malformed response — wrong echoed id (`client.rs:533-538`),
  unexpected result shape (`client.rs:557-564`), non-hex bytes
  (`client.rs:570-573`): these are `Incompatible(String)` with detail text
  only, not a structured sub-code. They MUST NOT be rendered as "the app and
  the service do not match" or as a version mismatch; prose MUST name the
  observed fact where the detail supports it.
- These malformed cases cannot be machine-distinguished today without
  parsing prose, and unparseable frames land in a different kind — hence D-7
  remains open. This document sets the wording rule, not the enum redesign.
- Other wording: "Incomplete credentials" SHOULD become "Missing
  credentials"; "Skipped registry entries" SHOULD become "Some drives could
  not be loaded"; "Another operation is already in flight…" SHOULD become
  "Another operation is already running on this drive. Wait for it to finish
  and try again."

**R-D8 — Navigation.** Destination "Drives"; add "Add drive"; create
"Create drive"; join "Join drive"; return "Back to drives"; open "Open".
Use "Open" on every drive card (D-8, resolved): "Select & open" opened
the same workspace as "Open", and selection is internal state that the
overview does not present. "Registered on this device" SHOULD become "Drives available on this
device sync independently. Open a drive to see its status and settings."

## CLI terminology rules

Good as-is and MUST NOT be renamed for friendliness: `drive`, `device`,
`identity`, `passphrase`, `relay`, `snapshot`, `merge`, `recover`, `export`,
`sync`, `owner`, `bootstrap`, `reconfigure`, `set-owner`, `pairing-request`,
`passphrase-stdin`, `diverged`.

Good but requiring explanation in help: `diverged` (competing histories
exist); `resolve` (a frozen membership conflict via TransitionId winner/void
sets, `resolve_conflict.rs:17-96`); `reissue` (a lost invitation file, authors
nothing); `owner` (role, process and service-mode naming are separate,
`args.rs:608-611,735-737`); `service-start` (can restart a live owner,
`drive.rs:15-22`); `vault` (two concepts — explain as "Vault role" or "vault
service mode"; help MUST NOT use bare "vault" in explanatory prose where the
distinction matters, `docs/contracts/sync-and-peers.md:555-560`). The literal
command/value `vault` in syntax is always permitted.

Too implementation-oriented or misleading:

- `materialization` is accurate but internal; renaming needs D-2.
  `availability` collides with "residency is not availability" and the
  `unavailable` error class.
- `evict` deletes no bytes and reclaims nothing (`policy.rs:632-693`;
  `materialization_surface.rs:60-63`). Help and receipt MUST say so.
- `delivered` means relay-accepted, not recipient receipt
  (`docs/contracts/sync-and-peers.md:82-99`; `sync.rs:506-531`;
  `owner.rs:31-43`). It SHOULD be relabelled "relay-accepted".
- `obligation` MUST NOT be rendered as "item" (`config.rs:228-242`).
- `admit` is engine vocabulary; the command stays `invite`.

Intentional divergences from the desktop — `owner` vs observable-state
wording, `--retained-quota` vs "Storage limit", `materialization` vs "Local
files", `identity` vs "device identity" — MUST be explained in CLI docs
rather than unified by force.

Error rules (R-C1): state what happened, whether anything changed, and the
next step; technical identifiers MAY follow the plain sentence.
`sync incomplete: pass limit (N) reached with X obligations pending`
(`lib.rs:207-217`) becomes "Sync did not finish: the maximum number of passes
was reached with X pieces of work still pending. Run `wyrd sync now` again to
continue." "No passphrase typed: the owner never starts without it" becomes
"No passphrase provided. This drive's owner cannot start without it."
Source-dependent incompatibility wording MUST follow R-D7; there is no
blanket "Incompatible drive service version". Keep verbatim the receipts in
`member_surface.rs`, `lifecycle_surface.rs`, `invitation_surface.rs`.

## Cross-interface mappings

| Concept | Desktop | CLI | Status |
|---|---|---|---|
| Background process | Observable-state wording; owner only when needed | `owner` | Deliberate divergence; document it |
| OS supervision definition | Drive service | Not exposed | App-only |
| Storage configuration | Storage limit | `--retained-quota` | Same knob (`settings.rs:113-151`) |
| Kept on this device (pinned) | "Kept on this device" | `materialization pin` | Divergence by audience |
| Stored here (present) | "Stored on this device" | Not separately reported today (`docs/contracts/sync-and-peers.md:523-546`) | D-2 sub-question |
| Not stored here | "Not stored on this device" | remote-only in `materialization` status | Divergence; fetchability separate |
| Content obtainable from peers | Stated only with supporting peer/content knowledge | relay/mailbox status | Must not be derived from residency |
| Device identity | this device's identity | `identity` | Same concept |
| Synchronization | Sync | `sync` | Same |
| Relay acceptance | Connection / relay health | "delivered" → relay-accepted | Fix overstatement |
| Pending work | Pending work | obligations / carries (technical) | Divergence by audience |

Same word, different meanings: `vault` (role vs service mode); `owner`
(process / membership role / service mode); `identity` (device / drive / file
/ key); `available` (service running / peer reachable / content local /
storage free); `delivered` (relay-accepted / recipient received — never the
latter in v0); `head` (DAG head vs mount's `name@N`); `merge` (snapshot
authoring vs `MergedDir` union, `conflict_surface.rs:203-217`).

## Restricted terms and approved explanations

### Restricted terms

These terms are prohibited in ordinary desktop copy but MAY appear in a
technical-details disclosure (R-D6), in CLI help/status or in developer
documentation. The prohibition applies to prose, not to literal command
syntax or identifiers.

| Term | Ordinary desktop copy | CLI | Current violation (review evidence) |
|---|---|---|---|
| Epoch, head, tip transition | MUST NOT | MAY in help/status | `StatusView.svelte:161-170` |
| Outbox, carries, fetch | MUST NOT | MAY | `StatusView.svelte:204` |
| Materialization, durable observation, liveness | MUST NOT | MAY | `StatusView.svelte:150,217`; hints `:56-62` |
| Merkle DAG, ContentId, StorageId | MUST NOT | Developer docs only | Not in UI |
| Obligation, transition id, peer id | MUST NOT | MAY where useful | `sync.rs:506-532` |
| Bare "vault" in explanatory prose | MUST NOT | MUST NOT in explanation; literal `vault` in command syntax or enum value is permitted | `args.rs:608-611`; `docs/contracts/sync-and-peers.md:555-560` |
| Quorum | MUST NOT | MUST NOT | No v0 surface occurrence; design/roadmap only (`docs/contracts/retention-constraints.md:5`) |
| Protocol version | MUST NOT (use "app and helper tool match") | MAY | RepairView already uses "app and its helper tool do not match" |

### Approved explanations (normative text)

1. **Drive owner — permitted, not mandatory (R-D1).** "Each drive has a
   background process — its owner — that manages the drive's data and
   coordinates synchronization. When automatic startup is configured, Wyrd
   uses a system service to start the background process." Use only where the
   UI exposes the concept or needs it to explain an operational state. Do not
   imply every drive has an installed service.
2. **Device identity.** "A device is recognized by its public identity key.
   The corresponding private key material, kept in the identity file and
   protected by the passphrase, proves that the device controls that identity.
   Restoring the identity restores the private key material needed to prove
   this device's identity; it does not copy any drive's files here. If this
   device is still a member of a drive, its files can arrive through
   synchronization from another member or vault that still holds them. If this
   identity was removed from a drive, that membership is not restored — the
   owner must admit it again." Grounds: `docs/contracts/trust.md:118,743`;
   `docs/contracts/object-model.md:35-38`; workspace AGENTS.md;
   `OnboardingView.svelte:355-370,707-712`. The public identity key and the
   separate device encryption key (T14) are distinct; do not merge them in
   ordinary copy.
3. **Vault role.** "Vault is a choice for this device only. With the Vault
   role, this device does not automatically discard the content it holds to
   make room for new data. The storage limit still applies: when it is reached,
   Wyrd refuses new data instead of dropping what it keeps. Vault does not
   change who can read the drive, does not start or identify a process, and is
   not a complete backup against device failure." Technical: under Vault the
   baseline policy treats cached and remote-only content as retained without
   writing pins (`role.rs:163-180`); this is not a promise that every object
   ever received is stored, because quota refusal can prevent storage
   (`docs/contracts/sync-and-peers.md:617-622`). Grounds:
   `docs/contracts/sync-and-peers.md:531-560,576-622`.
4. **Vault service mode.** "Vault service mode runs the drive's background
   process without a desktop file mount. It is a different use of the word
   'vault' from the Vault role." Grounds: `bootstrap/registry.rs` `ServiceMode`.
5. **Pinning and local files.** "Wyrd shows three things separately: content
   it promises to keep on this device, content whose bytes are recorded here,
   and content not stored here. Choosing to keep content records a promise;
   it does not download it. Wyrd obtains bytes when they are needed or when a
   sync fetches them. Unpinning removes the promise without deleting bytes,
   and releasing content changes intent only — no file is deleted." Grounds:
   `policy.rs:546-693`; `materialization_surface.rs:60-63`;
   `docs/contracts/retention-constraints.md:103-105`.
6. **Settings defaults.** "These settings let you change the limits for this
   drive. Any setting you leave unchanged uses Wyrd's default. The app cannot
   see that default value, so it does not show a number."
7. **Storage limit.** "The most drive data Wyrd will keep on this device. When
   that limit is reached, Wyrd refuses new data before writing anything. This
   is not a limit on your disk's free space." Grounds:
   `wyrd-settings/src/lib.rs:47-53`; `live/apply.rs:380-404`;
   `runtime/retention.rs:114-143`; `docs/contracts/resource-limits.md:78`.
8. **Sync work limit.** "How many items of sync work Wyrd starts in each sync
   pass. Anything beyond this waits for the next pass; nothing is dropped."
   Technical: counts distinct demanded content identities admitted as durable
   fetch demand, not files or completed fetches (`wyrd-settings/src/lib.rs:54-56`;
   `budgets.rs:43-49`; `live/mod.rs:1153-1182`;
   `docs/contracts/resource-limits.md:57`). This label must not drift into the
   general pending-work vocabulary (D-9).
9. **Download time per sync pass.** "How long Wyrd spends downloading in one
   sync pass. When that time runs out, unfinished downloads wait for the next
   pass." Technical: per-pass wall-clock over the whole fetch phase, not per
   file; partial progress and deferral; clipped by a held mutation's remaining
   deadline (`wyrd-settings/src/lib.rs:57-59`; `live/pass.rs:677-692`;
   `runtime/plan/mod.rs:59-64`).
10. **File change wait limit.** "How long a change you make can wait for the
    content it needs from other devices. If that content does not arrive in
    time, the change fails and you can retry it; nothing is saved." Technical:
    deadline for a held mutation blocked on NeedContent; terminal ETIMEDOUT
    with no later commit, retryable (`wyrd-settings/src/lib.rs:60-62`;
    `mutation/kind.rs:300-317`; `live/apply.rs:171-185`;
    `docs/contracts/write-path.md:434-441`).
11. **File open wait limit.** "How long Wyrd waits for a file's content to
    become available on this device when you open or read it. If it takes
    longer, the open or read fails and you can try again while the download
    continues." Technical: local materialization wait for open and windowed
    reads, not an owner round-trip; retryable timeout / EIO at FUSE
    (`wyrd-settings/src/lib.rs:63-65`; `want.rs:383-411`;
    `read.rs:306-333,365-419`; `docs/design/fetch-on-open.md:199-202`).
12. **Device removal.** "Removing a device stops it from taking part in this
    drive going forward: it cannot authorize future changes and cannot obtain
    new drive content through its former membership. It does not erase copies
    the device already obtained, and old history is not re-encrypted as a side
    effect of removal. If the removed device is the drive's only owner, the
    owner set becomes empty and no future membership or ownership change can
    be authorized — hand over ownership before removing if the drive should
    remain manageable." Technical: authority is pre-state-owner-only
    (`validate.rs:105-114`); owner set empties only when the removed device is
    the singleton owner (`remove_device.rs:63-70`); handover is the avoidance
    path (`set_owners.rs:40-68`; `docs/contracts/trust.md:834-847`;
    `docs/contracts/epochs.md:255-263`). The content-root boundary ("learns no
    new roots", `member_surface.rs:36`) belongs in the technical disclosure,
    not primary copy. Do not strengthen to "no writes of any kind": snapshot
    authoring is member-based (`docs/contracts/epochs.md:417`) and that case is
    not verified.
13. **Invitation.** "An invitation is a protected file that lets this device
    join a drive. Only the intended device can open it."
14. **Pending work.** "Wyrd still has work to do for this drive. This can
    include updates to share with other devices and content waiting to be
    transferred." Do not add reassurance about whether files are missing,
    present or obtainable — pending work alone establishes none of those. The
    associated count or indicator MUST NOT be presented as a queue of idle
    work: it can include work currently in progress.

### Preferred and rejected wording

| Context | Preferred | Rejected | Why |
|---|---|---|---|
| Status card | Sync activity | Durable observation | Mechanism vs outcome |
| Status card | Drive activity (defined as last reported loop state) | Service health / Drive status / "recently active" | No service ambiguity, no unsupported recency claim (`StatusView.svelte:228-266`) |
| Status row | Published on last pass | Last publish | Boolean, not a time (`StatusView.svelte:260-266`) |
| Status card | Drive data (pending D-11) | Freshness / Last synchronization / "Up to date" | No sync timestamp; freshness is not convergence (`wire.rs:619-626`; `pass.rs:884-885,1014-1027`) |
| Status row | Pending work | Queue / obligations / items | Implementation detail, mixed work |
| Status row | Local files | Materialized | Internal term |
| Local files | Kept on this device / Stored on this device / Not stored on this device | Pinned / Cached / Remote-only / "Available from other devices" | Residency is not fetchability (`docs/contracts/retention-constraints.md:103-105`) |
| Paused state | "Synchronization is paused. The drive is still running and can answer requests." | "Paused — reachable and quiet, not down." | Requires liveness knowledge |
| Settings header | "These settings let you change the limits for this drive…" | "configured bounds, as configured" | Developer-facing |
| Setting label | Sync work limit | Items processed per sync | "Processed" implies completion; admits demand only |
| Setting label | Download time per sync pass | Download time limit | Avoid per-file/cumulative reading |
| Tooltip | About these limits | About editing bounds | "Bounds" is not user vocabulary |
| Null bound | Using Wyrd's default | Not configured — compiled default applies (unknown to the shell) | Exposes shell internals |
| Vault | Vault role / vault service mode | Vault (bare in explanatory prose) | Two concepts; literal syntax excepted (`docs/contracts/sync-and-peers.md:555-560`) |
| Error | Missing credentials | Incomplete credentials | Clearer |
| Remote wire version skew | "The response used a different wire version." | "Incompatible owner" / "Incompatible drive service version" | Source-specific (`client.rs:527-532`) |
| Malformed remote response | "The answer did not match the request." / "The answer could not be read." | Any "does not match"/version claim | Wrong id, unexpected shape, non-hex are detail-only (`client.rs:533-538,557-564,570-573`) |
| Local refusal | "Wyrd refused this path: {detail}" | Any remote attribution | "no socket was touched" (`lib.rs:250-259`) |
| Onboarding label | Device identity | Identity (alone) | Avoids device/drive collision |
| Invitation file | Invitation | Sealed invitation | Cryptographic mechanism |

## Decision register

| ID | Question | Evidence / constraint | Status & recommendation |
|---|---|---|---|
| D-1 | "Drive service" vs "Owner" in app status | Only where a definition exists; definition-less locked drives have none (`registry.rs:198-200`); separate availability/service/session fields (`registry.rs:152-177`). | Resolved: conditional wording (R-D1); no mandatory owner introduction; service sentence conditional. |
| D-2 | Rename `wyrd materialization`? | Accurate but internal; `files` collides with `export`; `availability` collides with "residency is not availability" (`docs/contracts/retention-constraints.md:103-105`). CLI collapses cached into remote-only (`docs/contracts/sync-and-peers.md:523-546`). | **Open.** Do not rename merely because the term is internal. Decide a product noun first; separately decide whether local presence needs its own CLI report. |
| D-3 | Rename `--retained-quota`? | Same knob as "Storage limit" (`settings.rs:113-151`); quota is a refusal boundary, not a disk cap. | **Open.** Keep the flag. App label may be friendlier without changing the CLI contract. Alias only with a scripting-impact review. |
| D-4a | "Restore from backup" label | Normative doc mandates "Restore from backup" and forbids "Import"/"Recommended" (`docs/design/first-run-onboarding.md:24-30`); copy tests pin the label (`onboarding-copy.test.ts`). | **Resolved.** Keep "Restore from backup" with a clarifying subtitle if needed. |
| D-4b | "Identity key" label | Copy tests pin "Identity key"; renaming is a public-facing change with no settled replacement. | **Open.** Keep the current label until a product decision is made; the explanation now uses the public/private key wording. |
| D-5 | Vault explanation | Two distinct concepts; quota still applies; role is not service mode (`docs/contracts/sync-and-peers.md:555-622`). | Resolved: approved explanations 3–4; no retention-beyond-limit or complete-backup claim; literal `vault` syntax excepted. |
| D-6 | Status card labels | "Connection" exists in the summary strip (`StatusView.svelte:109`); "Drive status" collides with the screen title; liveness fields do not establish recency. | Partly resolved: Sync activity / Drive activity / Pending work / Local files; freshness deferred to D-11. |
| D-7 | Error taxonomy | Four kinds only (`client.rs:42-77`); version skew, wrong id, unexpected shape and non-hex are one detail-only Incompatible class, while unparseable frames are Unconfirmed (`client.rs:525-526,548-554`). A local refusal is a local decision (`lib.rs:250-259`). | **Open.** R-D7 sets wording rules; a grounded classification of remote error classes must precede enum changes. Do not force unlike failures into one explanation. |
| D-8 | Remove "Select & open"? | It opened the same workspace as "Open"; selection has no distinct semantics on the overview. | **Resolved.** Every drive card uses "Open" with the same styling. Selection stays internal for Status/Settings routing. |
| D-9 | Unit for pending work vs settings label | Pending work is mixed control work (`config.rs:228-242`); `max_admit_per_pass` counts content identities, not files (`live/mod.rs:1153-1182`). | Conceptually resolved: "pieces of work" for pending work; "items of sync work" for the setting. The setting label must not drift into general pending-work vocabulary. |
| D-10 | `policy` → `materialization` naming | Naming drift in versioned prose (`materialization.rs:1-8` vs `args.rs:239` vs `docs/contracts/sync-and-peers.md:590`); planning files are not authoritative. | **Open.** Resolve against normative contracts, not planning prose. |
| D-11 | Freshness timestamp and label | `WireFreshness` has only state/revision/generation (`wire.rs:619-626`); "Fresh" can coexist with pending heads (`pass.rs:884-885,1014-1027`); only shell observation time exists (`format.ts:14-17`). | **Open.** Do not invent a timestamp or label an observation time as a sync time. Interim "Drive data" with non-convergence state text. |
| D-12 | Public identifier product term | Immutable public key (workspace AGENTS.md); no product-facing name exists. | **Open.** No new term unless the UI actually needs one. Public identifier and private proof stay distinct. |
| D-13 | Sole-owner freeze wording | Accurate only when the owner set empties (`remove_device.rs:63-70`); scoped to future transitions (`docs/contracts/epochs.md:255-263`); snapshot authoring is member-based (`:417`), so a stronger claim is unverified. | Resolved: approved explanation 12 states the conditional, transition-scoped guarantee and the handover path; no "no writes of any kind" claim. |

## Deferred decisions

The open decisions below are explicitly deferred to the product owner — the
accountable role for this document — until assigned to a named individual.
Each states what remains undecided. Deferral does not change any rule above
and does not authorize a copy, help or enum change.

| Decision | What remains undecided | Owner |
|---|---|---|
| D-2 | Whether the `materialization` command family should have a different public noun, and whether local presence needs a separate CLI report. | Product owner |
| D-3 | Whether `--retained-quota` should gain an alias; no CLI flag change is proposed now. | Product owner |
| D-4b | Whether the "Identity key" label should change; no replacement is settled. | Product owner |
| D-7 | How to classify local refusal, wire skew, unexpected result shape and non-hex responses into distinct remote classes before any enum redesign. | Product owner |
| D-10 | Which versioned contract settles the `policy` / `materialization` naming. | Product owner |
| D-11 | Whether a real synchronization timestamp will be exposed; the freshness label stays interim until then. | Product owner |
| D-12 | Whether the UI needs a product term for the public identifier. | Product owner |

## Change control

- Changes to normative terminology require updating this document first.
- A wording change that alters an implied guarantee requires contract review.
- Implementation-specific observations must be updated when the underlying
  behavior changes; line numbers are evidence, not authority.
- Removing or renaming a canonical term requires an explicit decision, not
  just a UI cleanup.

## Freeze record

1. The document was reviewed against implementation behavior and the
   normative contracts in `docs/contracts/`. Open decisions are explicitly
   deferred with an owner above rather than guessed.
2. The status line was changed to Normative on 2026-11-19. That change is the
   freeze.
3. Implementation issues are created after the freeze, grouped by surface and
   priority, against this frozen document. Creating them is not a condition
   of the terminology's approval.

Copy changes may begin only after the freeze is recorded. Open decisions do
not themselves authorize changes; the rules above remain in force until
amended through change control.
