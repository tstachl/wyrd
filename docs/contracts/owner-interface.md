# Drive-owner interface (v2)

Status: Normative. Scope: the cross-process operations a non-owner consumer may submit, and the durability and exactly-once behavior the owner owes in return. Authority: this interface owns the boundary; engine, mutation, durability, synchronization, and trust logic live in their own contracts and are not duplicated here.

A second process uses a drive it does not own through this
interface. One process owns the durable store. Every other local
consumer submits operations to the owner and never opens the store.
The interface forwards mutation requests to the owner's single
mutation queue, policy requests to the owner's loop-thread policy
executor (the same engine, the same thread — four engine operations,
not mutations), local verbosity configuration to the owner's
process-local diagnostics level, and reads to its published read
state. It interprets nothing.

## Transport

Serve the interface on a Unix socket at `<drive_dir>/owner.sock`.
Derive the path from the drive directory. Run no discovery service.
Delete a stale socket entry at bind while the store lock is held.
Remove the socket file on clean shutdown.

The owner additionally serves the same interface inside the App
Group container for the File Provider extension, which cannot
reach the drive directory from its sandbox: one socket per drive
at
`<container>/Sockets/<fingerprint>.sock` plus a
`Sockets/by-drive/<drive-hex>.sock` symlink alias and a
`Sockets/by-drive/<drive-hex>.label` file beside it. The label holds
only the validated registry name (a `[a-z0-9-]` token of at most 64
characters) for the Finder domain display name. It is control
metadata, never drive content: no bytes, manifests, keys or secrets
go into the group container. Only a live owner writes the label, so
a locked or stopped drive has no Finder domain; the domain
identifier stays the full `DriveId`. This
second listener is the daemon half of the registry-owned
production association on
macOS: with no dev env vars set, the owner derives the container
from the platform convention
(`~/Library/Group Containers/<group>`, where `<group>` is the
effective `WYRD_APP_GROUP`: a runtime override wins, otherwise the
value baked at build time from `WYRD_APP_GROUP` or a derived
`APPLE_TEAM_ID`, otherwise the team-prefixed default
`JU2RFHWQPH.group.md.enesis.wyrd`; verified for development signing
only — Developer ID and notarized paths are unverified, see the
packaging note) and the drive
from the served drive itself, then publishes the alias the domain
resolves through; it is compiled on every
platform but binds the group socket only where the container
exists, so the "macOS" scoping is the consumer plus the container,
not a platform gate in the framing code. Absent container means a
primary-only bind with a bounded late retry; the containing app
initializes the container at launch, and if the owner started first
the retry publishes the alias from the same server without a restart.
A present-but-unbindable container aborts a startup bind
all-or-nothing rather than running deaf to the extension (see the
bind docs); the late retry is bounded and ends with a visible error.
The daemon creates only `Sockets/` inside the container, never the
container itself. The
filename derives from the served `DriveId`
(`DriveId::fileprovider_socket_fingerprint`, 64-bit domain
separated derivation, hex-encoded); the daemon is the only
computer of it, and the extension resolves the alias by
readlink, never by hashing. A dev-only env pair
(`WYRD_FILEPROVIDER_GROUP_CONTAINER` plus
`WYRD_FILEPROVIDER_DRIVE_ID`) overrides for harness and manual
development: the configured drive is checked
against the drive actually served before anything binds: a
mismatch refuses the run. The alias target must stay inside
`Sockets/`; anything else fails closed. A live endpoint at the
group path is never unlinked — the bind fails closed instead —
while a dead entry reclaims. Both sockets serve from one server:
one owner-instance identity, one uid check, and one shared
connection pool of at most 16 concurrent connections across both
accept paths. The daemon half is production: with no dev env vars,
the owner publishes the alias for the served drive whenever the
container exists, so registration plus the managed owner service
(at login) leaves the socket where a registry-registered domain
resolves it. The domain half is bundled with the desktop app and run
by a per-user launch agent, not a resident app process: one signed Swift
helper process reconciles `NSFileProviderDomain` registrations every 5
seconds from its own loop, so macOS asks for permission once rather
than on each launch
against the published `by-drive` aliases, keyed by the same 64-hex
drive id the appex parses from the domain identifier. The contract
here still owns only the socket half; the helper's path-to-domain
association is proven by the aliases, not by a new protocol field.
The container literal is verified for development signing only;
distribution signing and notarization stay with the signing owner.

Frame each message with a u32 big-endian length prefix followed by
that many JSON bytes. Assemble frames explicitly: an idle
connection re-polls on the read deadline, but a stall past the
deadline with a partially assembled frame — a short length prefix
or a short body — closes the connection, as does a length past 96
MiB. A half-read frame is never dispatched. Serve at most 16
concurrent connections in total across every bound socket: a
connection past the bound is closed without an answer. Treat
every broken connection as unconfirmed. Never treat it as
success. Never treat it as failure.

## Authentication

Check the peer credential on every accepted connection. Require the
peer uid to equal the owner's uid at bind. Drop other peers without
an answer. Log each refusal.

A connected client holds full member-position authority. Offer no
per-operation access list. Every operation answers a member's
question. The interface exposes nothing vault-visible. Keep the
drive directory owner-only. The path is the sandbox boundary.

## Versioning

Tag every request and response with `"v": 2`. Refuse other versions
with `UnknownVersion`. Name the wanted version in the message. Add
only optional fields in later versions. Never re-mean a field.
Refuse unknown operations with `UnsupportedOperation`. Echo the name
in the message. Never ignore an unsupported request. Never answer it
with success.

v1 to v2: the entry-addressed mutations (`CommitFile`,
`AppendFile`, `Unlink`, `Rmdir`, `Rename`, `SetAttrs`, and `Fold`
members of those kinds) carry a required `expected_identity`
(lowercase hex EntryIdentity). v1 has no such field, so a v1
payload can never express the precondition — and a v1 server
silently ignores it when present. Both directions therefore
refuse across the boundary with `UnknownVersion`: a v1 client
against a v2 server, and a v2 client against a v1 server, fail
closed with an explicit upgrade directive instead of running
path-only mutations under an identity-conditional client (or
vice versa). Both sides ship together, so the only skew window
is a mixed-release deployment, which refuses loudly rather than
drifting silently. v2 changes nothing else, except one additive
read field described below: absent, reads, parent tokens, folds,
and every refusal code keep their v1 meaning.

## Read operations

Serve `lookup`, `stat`, `readdir`, and `read` from the published
generation. Clone the generation under a short read lock. Serve
lock-free after the clone.

Attach the serving generation and the durable revision to every
read answer. Correlate answers with the freshness judgment. Never
assume recency.

Read whole files only. Refuse files past 32 MiB with `TooLarge`.
Never truncate. Never stream past the bound. Register demand behind
reads through the want registry. Block bounded for remote content.
Map expiry to `Timeout`. Tell the consumer to retry. The hex
encoding doubles the bytes on the wire, so the 32 MiB bound keeps
every accepted response under the 96 MiB frame ceiling.

Every read answer that names an entry carries its stable identity
(`object-model.md`, "Entry identity"): `lookup`, `stat`, and
`readdir` expose the token as lowercase hex in an optional
`identity` field. The token is `None` for entries that predate
identity support; a conflicted path reports `None` (each conflict
version carries its head's token), while a merged directory
reports the agreed token when every merged head carries the
identical `Some` token, else `None`. The field stays optional
with `#[serde(default)]` across v1 and v2, so a new reader parses
pre-identity payloads (which omit the key) as "no token reported"
instead of failing. A pre-identity reader likewise tolerates new
payloads, because the wire types do not deny unknown fields. Consumers answer "is this the same entry I
saw before?" from the token, never from path, content, or matching.

A `read` may bind to the verified entry: the request carries an
optional `identity` (lowercase hex EntryIdentity). Absent stays
path-only with the v1 meaning. The owner resolves the path first:
a missing path is `NotFound` and a conflicted path refuses as
conflicted — both before any token validation, so a malformed
token on either answers with the path verdict, not `Malformed`.
Then a present token must parse (`Malformed` otherwise, never
silently dropped) and must equal the served node's token exactly:
a delete-plus-recreate behind the path — or a presented token
against an identity-less entry — refuses as `StaleEntry` instead
of serving foreign bytes. Four conditions stay distinct,
mirroring the mutation rule below: a changed content base under
the same identity serves (the binding is entry-scoped, not
version-scoped); a different identity is a stale entry reference;
a conflicted path carries no token of its own and refuses as
conflicted, exactly like the path-only read; a missing path is
`NotFound`. Never collapse two into one. The answer stays
path-addressed — generation plus revision only, never a token
echo — so the binding lives in the request. The field binds
`read` only: `lookup`, `stat`, `readdir`, capture-parent, and the
policy operations refuse a present token as `Malformed` instead
of serving an unbound answer under a bound request. A pre-binding
v2 owner ignores the unknown field and serves path-only, so a
bound read against one degrades to the old verify-then-read
window; both sides ship together, so the skew covers mixed
releases only. Path-only reads (legacy references included) keep
that window by contract: absent is the long-standing behavior,
not a bound one.

Each conflict version carries its head snapshot's id and
authoring timestamp (milliseconds, display and tiebreak only —
never authorization, never peer identity). The timestamp defaults
to `0` on parse ("unknown"), so a pre-timestamp reader degrades
instead of failing; both sides ship together, so the default only
covers mixed-version skew during rollout. Presentation surfaces
label versions by `(snapshot, timestamp)` in snapshot-id byte
order, which implies no authoritative head.

## Durable observation

Answer `observe` from the loop-published slot. Return
`Unobserved` before the first pass completes. Never invent a
healthy-looking default. Repeat the same observation byte for byte
while the state stands still. Construct no mailbox on the read
side. Connect to nothing. Change no durable state.

The observation also carries an advisory sole-owner fact with three
states: unknown, this device is the sole owner, or another owner is
present. Read it from the derived owner set at the membership tip,
never from the member count — a reader is a member but not an owner.
An absent field means unknown, so a mixed-version reader must not
invent a claim. An empty owner set is terminal and also means unknown:
it must never render as "other owners present". Consumers phrase a
known state "as last reported" and never use the fact to gate
retention, eviction, removal or any other decision.

The observation also carries local retention figures: the bytes of
drive data kept on this device, and the effective ceiling if one is
set. Read the count from the byte accountant the store maintains —
the same figure the commit and receive gates compare against — never
from the disk's free space, a residency census, or a peer's claim.
The ceiling is a refusal boundary over retained content, not a cap
on the device. A `None` ceiling means no limit was set, not an
unknown value; an absent field means this observer held no
accountant (an unheld read or an older writer), so a mixed-version
reader must render neither a zero-byte claim nor a fabricated limit.
These figures are advisory facts about local retention, never a sync
time, a liveness signal, or a gate on any decision. The shell label
is "Drive data kept on this device".

The observation may carry per-device peer-state observations. The owner
derives each comparative state from the verified DAG, determines whether
a device is a current member, and determines whether an observation is
current or historical. Each row carries the reporting device's identity,
its truncated display form, the report sequence, the supplied comparative
state (including unclassifiable and no report), and the supplied
current-or-historical status. The shell renders those supplied values;
it does not independently compare DAGs, validate report sequences or
infer membership. The shell presents rows as advisory facts, phrased
"as last reported", and must not interpret service mode, infer
availability from head agreement, or turn absence into a failure claim.
Rows use separate per-device states; no aggregate verdict, "In sync"
badge or global-convergence claim. Historical rows from removed members
are labelled historical and never presented as current member state. The
shell does not use these rows to gate retention, eviction, removal or any
other decision. The claim and its limits are normative in
[sync-and-peers.md](sync-and-peers.md), "Peer-state reports (advisory)".

## Liveness

Answer `liveness` from the same slot with a separate result
object. Report transport health and the last pass report together.
Pin both to the pass count. Report unknown health with no report
and zero passes before the first pass. Never merge liveness into
the durable observation. A live socket is not a durable fact.

Distinguish three states. Owner unreachable means the connection
refused. Owner reachable with no pass yet means no recent sync
activity. Owner reachable with a report means the given pass count.
Each answer carries the loop's pause switch as it reads back: a
paused owner is reachable and quiet by choice, distinct from an
unreachable owner and from a reachable owner with no pass yet.
`sent: 0` during a paused pass is not an empty outbox.

## Freshness

Expose the projection freshness judgment as its own operation.
Serve four consumer states. Fresh empty and fresh non-empty come
from the slot. Retained last-good carries the refresh failure that
left it in place. No trustworthy projection has no slot value. A
failed composition never publishes, so the consumer observes it as
an absent owner.

The judgment carries `published_unix_secs`, the local time of the last
projection publication. It is not a peer delivery time, a
synchronization-completion time, or a liveness heartbeat, and a consumer
MUST NOT label it as any of those. A retaining failure carries the
previous value forward, since it is not a publication. Absent or zero
means unknown, never a wall-clock date; a mixed-version reader that
ignores the field degrades to the state/revision/generation judgment
above. The shell label is "Drive data last updated on this device" and
the value belongs in the Drive data card, not in the status summary or
beside the run state. A last completed pass is a liveness measurement and
is not carried by this operation. Zero renders as "not yet updated on
this device" until the first projection publication.

## Mutation submission

Forward `submit` to the single mutation queue. Decode the wire
kind first. Refuse bad encodings at the wire with `Malformed`.
Block for the queue's terminal answer. Carry refusals through as
distinct outcomes. Map each queue error to its own code. Never
collapse two refusals into one. A success means exactly what an
in-process submit means.

Capture parent tokens with `capture-parent` before creates. The
token binds the queue counter to the owner instance that minted
it. Send the token back with the create. Let the owner re-check
it at authoring time. Refuse tokens from a dead instance with
`StaleParent` before any queue check. Refuse forged counters the
same way at the queue's re-check. A capture refused because the
table is blocked or saturated also reports `StaleParent` — retry
the capture — and a dead table reports `Poisoned`, never a
missing parent. Never invent a token on the
consumer side. Never reuse a token across an owner restart.

Refuse nested folds at the wire. Let the queue enforce the rest.
Forward all nine operations verbatim.

Every entry-addressed mutation carries the caller's expected
`EntryIdentity` for the path it mutates (`CommitFile`,
`AppendFile`, `Unlink`, `Rmdir`, `Rename` on its `from` path,
`SetAttrs`; creations address absent paths and are covered by
the parent token instead). The owner validates the precondition
inside the serialized queue, at execution: the path must still
resolve to exactly that entry when the mutation runs. Four
conditions stay distinct. Same identity with a changed content
base is a stale content base. A different identity — including
a legacy entry that never had one — is a stale entry reference.
A conflicted path is refused as conflicted. A missing path is
`NotFound`. Never collapse two into one, even where a
presentation surface renders two with the same error. A refused
precondition commits nothing and authors no snapshot: a rename,
delete-plus-recreate, or path reuse between the caller's lookup
and the queue's execution fails closed instead of landing on
the wrong entry. `Fold` members carry their own preconditions;
the fold caution in Crash answers stands.

Bound one submit's carried content: a submit whose decoded file
content (commit and append bodies, summed across fold members)
exceeds 32 MiB is refused with `TooLarge` before any decode or
queue admission, with retry guidance in the message. Hex encoding
doubles bytes on the wire, so the 32 MiB bound keeps every accepted
submit under the 96 MiB frame ceiling. A window saves a larger file
through the mount, or by chunked submission — several appends or
commits, each within the bound — never by a single over-bound
submit. A frame past 96 MiB still closes the connection; the stated
submit bound is what keeps a conforming client from ever meeting
that silence. A `SetAttrs` size is not carried content — it names a
target the loop materializes — so it is not counted here and may
materialize up to the committable ceiling without aggregate
accounting; the owner client is same-UID trusted, and bulk resizes
belong on the mount, which leases the target at submit.

## Pause and resume

Set the loop's pause switch with `pause`, clear it with `resume`.
Both take no parameters. Pausing defers network work and obligation
discharge from the next pass; resuming returns to an ordinary pass.
Local authoring and serving continue either way. The switch is
process-local: a restarted drive runs unpaused. Each operation is
idempotent — pausing twice is one pause, resuming a running drive
changes nothing — and answers with the switch as it reads back, so
the consumer never infers state from silence. Setting the switch
pokes the loop's pacing signal, so the loop re-checks promptly
rather than waiting out its idle deadline.

Wire shape note: `pause`, `resume`, and the `paused` liveness
field arrived in v1 and are unchanged in v2. `paused` defaults to
`false` on parse, so a pre-pause reader degrades to "running"
instead of failing; both sides ship together, so the default only
covers mixed-version skew during rollout.

## Diagnostics level

Set the process-local verbosity level with `diagnostics-set`, read it
with `diagnostics-status`. `diagnostics-set` takes one closed field,
`level`, either `default` or `detailed`; an unknown value is a distinct
refusal, never coerced. `diagnostics-status` takes no parameters. Both
answer with the level as the owner reads it back, so a consumer never
infers the applied state from silence. Both are idempotent: setting the
current level changes nothing.

This is configuration, not a mutation or a policy operation: it enters
neither the mutation queue nor the policy executor, commits no durable
fact, changes no snapshot, and touches no store state. It governs how
much the owner records locally for subsequent operations; work already
in flight keeps the level it started under. The setting is persisted in
`settings.json` by the settings backend; the owner applies the
persisted level in memory on start, and this operation delivers changes
to a running owner. A restarted owner therefore runs at the persisted
level, never at a remembered live override. Disabling takes effect on
subsequent operations.

Only the level crosses this interface. Diagnostics records, file
contents, file names, identities, content ids and credentials never do
— the records stay local to the process that produced them (T20 in
`trust.md`). Each operation uses the crash answers below: an
unconfirmed answer is re-read with `diagnostics-status` before any
retry, never recorded as confirmed or failed.

On the wire, `diagnostics-set` takes exactly one field, `level`, with
the closed string value `default` or `detailed`. `diagnostics-status`
takes no parameters. Both answer with `{"level": "..."}` naming the
level as the owner reads it back, never an inference from silence.
The owner writes the setting through the restricted settings writer,
publishes the read-only app-group mirror, then applies the value to
subsequent operations and reloads its local log filter. A restarted
owner starts at the persisted level, never at a remembered live
override.

Four refusals stay distinct. `InvalidDiagnosticLevel` names an
unknown level value without coercing it; `Malformed` covers a bad
params shape, including an extra field. `DiagnosticsSettings` means
the settings transaction refused and the level is unchanged.
`DiagnosticsMirror` means the setting committed but the mirror could
not be published: the level is not reported as applied.
`DiagnosticsFilter` means the setting committed but the local log
filter could not be reloaded, so the applied level is unchanged. A
server without a configured diagnostics controller answers
`Unavailable` rather than pretending a memory-only change was saved.

## Materialization policy

Forward `policy-pin`, `policy-unpin`, `policy-evict`, and
`policy-status` to the loop-thread policy executor through a
dedicated typed channel — the same engine, the same loop thread as
the mutation queue, but never the mutation queue itself. Policy
operations are four engine operations, not mutations: the queue's
parent-token, fold, and projection machinery is not theirs to touch,
and the mutation family's "forward all nine operations verbatim"
reservation keeps its meaning per op family. There is still exactly
one writer (the loop owns `&mut Engine`), no second mutation path,
no new authority, and no read-side mailbox. Budgets and role stay
CLI-offline: the budgets screen reads durable state directly, the
role file stays local, and setting the role needs no owner.

Pin promises retention for every chunk under the path, unpin
releases the promise back to cacheable policy, and evict returns
unpinned content to `REMOTE_ONLY` policy. Evict changes intent only:
no bytes are deleted, no store is compacted, heads are untouched. A
pinned subtree refuses evict with `PinnedPresent` before committing
anything — narrowing an explicit promise piecemeal never happens
silently. An effective `vault` role refuses evict with
`RoleForbidsEviction` before any walk or commit; a present but unreadable
role file fails role-dependent policy closed with `RoleUnreadable`
rather than assuming mirror. Unpin never refuses. Every write is
idempotent: re-pinning commits nothing, and a repeated write is
observably a no-op. Policy facts are device-local: never published,
never authorize anything, never alter a snapshot. A success means
exactly what an in-process policy call's success means.

Answer `policy-status` on demand only. The loop runs no periodic
census; every status is a bounded walk at request time against the
loop's engine and its published generation. The walk covers at most
the implementer traversal budget (4096 namespace entries: files plus
dirs plus symlinks plus conflict rows) before stopping with
`complete: false` — a prefix, not the subtree, and the consumer
narrows the path or accepts the aggregate as partial. The answer
carries the serving generation and durable revision the loop
actually walked — captured from the same published generation as the
rows, never read separately beforehand — so the consumer correlates
it with the freshness judgment instead of assuming recency.
Reachable content only: the object store as a whole is never
scanned. Conflicted subtrees cost a row, not the report; mutating
commands still refuse conflicted paths outright. Label the counts
"reachable content", never "total storage" or "all files".
Distinguish last-observed from in-progress from unavailable, and
never render a live byte figure the storage contract does not own.

Pin, unpin, and evict resolve against the last published
generation, not the engine's in-flight state: a mutation the same
pass authored is not yet published when the policy drain runs, so a
policy operation addressing that same path agrees with the drain
only on the next pass. Serialisation is still total and safe — the
loop executes the mutation batch and the policy drain in one order
on one thread — but the namespace can lag the engine by one pass.
In-process calls observe durable state directly and have no such
lag; over the interface, re-read (status) after a same-pass author
before assuming the pin landed.

Writes traverse the whole subtree with no entry, chunk, or byte
budget: one pin over a large subtree commits one batch, but the walk
itself runs synchronously on the loop thread, ahead of publication
and of every queued status behind it. Only the status walk is
budgeted; stage a huge tree by subpaths rather than pinning the root
in one operation. The per-commit record ceiling still fences absurd
batches, but a large-but-legal subtree stretches its pass past the
pacing deadline in ordinary-sized trees this never matters — the
operator-size caveat belongs here rather than in a refusal, because
the walk's cost is time on the loop thread, not a correctness bound.

Carry refusals through as distinct codes. Map each policy error to
its own code: `PinnedPresent` for a pinned-subtree evict refusal,
`RoleForbidsEviction` for an evict refused by the effective vault role,
`RoleUnreadable` for a present but unreadable role file, `TooDeep` for
paths past the format depth bound, `Conflicted` for conflicted paths,
the view codes for view failures, `Engine` for engine failures
(including channel shutdown and lock poison; a role-file fault is not
one of these). Never collapse two refusals into one. A status read
builds no mailbox, connects to nothing, and changes no durable state;
a policy write commits a durable policy fact and nothing else.

## Crash answers

Return one of three verdicts for every request. Confirmed means
the owner answered success. Rejected means the owner answered a
defined refusal. Unconfirmed means the connection broke before a
terminal answer. Make sure that consumers never record an
unconfirmed request as confirmed. Make sure that consumers never
record it as failed.

Reconcile before retrying after an unconfirmed answer. Re-read the
touched paths. Compare namespace facts for existence and kind.
Compare file identities for content operations. Retry only after
the re-read reaches a definite verdict.

Fold needs extra care. A repeated fold can commit another durable
event while the logical state stays equivalent. Treat an
unconfirmed fold as unconfirmed, not as absent. Re-read first.
Decide from the re-read, not from the retry.

## Wire types

Own every wire type in the interface module. Map core and
namespace values field for field. Fail the build when a source
type gains a field the mapping ignores. Carry identifiers as
lowercase hex. Render peer handles as `peer-N`. Never expose a
peer identity. Carry drain peer counts only, never identities.

Keep `wyrd_sync` implementation types out of the operation
surface. The mapping may name source types. Operations never
accept or return store internals. The server holds shared
projections, registries, queues, channels, and published slots only.
