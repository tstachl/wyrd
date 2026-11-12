# Drive-owner interface (v1)

Status: Normative. Scope: the cross-process operations a non-owner consumer may submit, and the durability and exactly-once behavior the owner owes in return. Authority: this interface owns the boundary; engine, mutation, durability, synchronization, and trust logic live in their own contracts and are not duplicated here.

A second process uses a drive it does not own through this
interface. One process owns the durable store. Every other local
consumer submits operations to the owner and never opens the store.
The interface forwards mutation requests to the owner's single
mutation queue, policy requests to the owner's loop-thread policy
executor (the same engine, the same thread — four engine operations,
not mutations), and reads to its published read state. It interprets
nothing.

## Transport

Serve the interface on a Unix socket at `<drive_dir>/owner.sock`.
Derive the path from the drive directory. Run no discovery service.
Delete a stale socket entry at bind while the store lock is held.
Remove the socket file on clean shutdown.

Frame each message with a u32 big-endian length prefix followed by
that many JSON bytes. Assemble frames explicitly: an idle
connection re-polls on the read deadline, but a stall past the
deadline with a partially assembled frame — a short length prefix
or a short body — closes the connection, as does a length past 96
MiB. A half-read frame is never dispatched. Serve at most 16
concurrent connections: a connection past the bound is closed
without an answer. Treat every broken
connection as unconfirmed. Never treat it as success. Never treat
it as failure.

## Authentication

Check the peer credential on every accepted connection. Require the
peer uid to equal the owner's uid at bind. Drop other peers without
an answer. Log each refusal.

A connected client holds full member-position authority. Offer no
per-operation access list. Every operation answers a member's
question. The interface exposes nothing vault-visible. Keep the
drive directory owner-only. The path is the sandbox boundary.

## Versioning

Tag every request and response with `"v": 1`. Refuse other versions
with `UnknownVersion`. Name the wanted version in the message. Add
only optional fields in later versions. Never re-mean a field.
Refuse unknown operations with `UnsupportedOperation`. Echo the name
in the message. Never ignore an unsupported request. Never answer it
with success.

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
identity support and for synthetic states (`MergedDir` and
`Conflict` carry no token of their own; each conflict version
carries its head's token). The field is additive under the v1
"only optional fields" rule, and it defaults on parse so a
pre-identity reader degrades to "no token reported" instead of
failing. Consumers answer "is this the same entry I saw before?"
from the token, never from path, content, or matching.

## Durable observation

Answer `observe` from the loop-published slot. Return
`Unobserved` before the first pass completes. Never invent a
healthy-looking default. Repeat the same observation byte for byte
while the state stands still. Construct no mailbox on the read
side. Connect to nothing. Change no durable state.

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
same way at the queue's re-check. Never invent a token on the
consumer side. Never reuse a token across an owner restart.

Refuse nested folds at the wire. Let the queue enforce the rest.
Forward all nine operations verbatim.

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

Wire shape note (pre-freeze): `pause`, `resume`, and the `paused`
liveness field are new in v1's current shape. `paused` defaults to
`false` on parse, so a pre-pause reader degrades to "running"
instead of failing; both sides ship together, so the default only
covers mixed-version skew during rollout.

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
silently. Unpin never refuses. Every write is idempotent: re-pinning
commits nothing, and a repeated write is observably a no-op. Policy
facts are device-local: never published, never authorize anything,
never alter a snapshot. A success means exactly what an in-process
policy call's success means.

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
its own code: `PinnedPresent` for the evict refusal, `TooDeep` for
paths past the format depth bound, `Conflicted` for conflicted paths,
the view codes for view failures, `Engine` for engine failures
(including channel shutdown and lock poison). Never collapse two
refusals into one. A status read builds no mailbox, connects to
nothing, and changes no durable state; a policy write commits a
durable policy fact and nothing else.

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
