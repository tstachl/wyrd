# Drive-owner interface (v1)

Status: Normative. Scope: the cross-process operations a non-owner consumer may submit, and the durability and exactly-once behavior the owner owes in return. Authority: this interface owns the boundary; engine, mutation, durability, synchronization, and trust logic live in their own contracts and are not duplicated here.

A second process uses a drive it does not own through this
interface. One process owns the durable store. Every other local
consumer submits operations to the owner and never opens the store.
The interface forwards each request to the owner's single mutation
queue or its published read state. It interprets nothing.

## Transport

Serve the interface on a Unix socket at `<drive_dir>/owner.sock`.
Derive the path from the drive directory. Run no discovery service.
Delete a stale socket entry at bind while the store lock is held.
Remove the socket file on clean shutdown.

Frame each message with a u32 big-endian length prefix followed by
that many JSON bytes. Refuse frames past 96 MiB by closing the
connection. Treat every broken connection as unconfirmed. Never
treat it as success. Never treat it as failure.

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
projections, registries, queues, and published slots only.
