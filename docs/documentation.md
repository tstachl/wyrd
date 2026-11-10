# Documentation guide

How Wyrd's documentation is organized, what each part may claim, and how
to change it without creating a second source of truth.

## Where things belong

| Question | Canonical document |
|---|---|
| What is Wyrd and where do I look next | `docs/architecture.md` (overview, not a specification) |
| Format, identities, canonical encoding, snapshot DAG | `docs/contracts/object-model.md` |
| Identity boundary, key hierarchy, capabilities, disclosure | `docs/contracts/trust.md`, `docs/contracts/capability-bindings.md` |
| Membership, epochs, snapshot authorization | `docs/contracts/epochs.md` |
| Sync protocol, peer roles, materialization, forget contract | `docs/contracts/sync-and-peers.md` |
| Mounted write path, commit pipeline, durability ordering | `docs/contracts/write-path.md` |
| Drive-owner interface (cross-process boundary) | `docs/contracts/owner-interface.md` |
| Crash windows per subsystem | `docs/contracts/crash-consistency.md` |
| Error taxonomy, fail-closed classification | `docs/contracts/error-conventions.md` |
| Live-operation bounds | `docs/contracts/resource-limits.md` |
| Retention constraints v0.x preserves | `docs/contracts/retention-constraints.md` |
| Upgrade and migration guarantees | `docs/contracts/upgrade-contract.md` |
| Update transaction | `docs/contracts/update.md` |
| Public-read subset | `docs/contracts/release-read.md` |
| How a subsystem works today (fetch, repair, retry, growth, CLI) | `docs/design/` |
| Platform composition, constraints, open decisions | `docs/platforms/` |
| How to cut a release | `docs/releases/release-process.md` |
| How to work (issues, PRs, review, merge) | `docs/collaboration-workflow.md` |
| What a past review claimed | ngit `review:*` issues (evidence, not policy) |
| What is planned and in what order | `ROADMAP.md` |

## Authority

A folder does not confer authority. Each substantive document states its
own status near the top:

- **Normative** — owns externally observable behavior, invariants, wire
  formats, security guarantees, failure semantics. When two documents
  disagree, the normative one wins; the other is wrong and must be fixed.
- **Descriptive** — explains how the current code works. Subordinate to
  the contract it describes; if code and contract disagree, one of them
  changes, and this document follows.
- **Operational** — procedures to run (releases, collaboration). Serves
  its readers; changes when the procedure changes.
- **Proposed** — an open decision. Explicitly not settled; must not be
  cited as a guarantee.

The one rule that keeps the system cheap to maintain:

> Every invariant has one canonical home. Other documents may summarize
> it, but must link to that home rather than maintain independent copies.

The two exceptions prove the rule: the Windows-scope sentence and the
vault-disclosure sentence live in `docs/architecture.md` ("Fixed scope")
because automated contract tests pin them there
(`credential_files_are_unix_only`,
`the_disclosure_contract_is_stated_in_exactly_one_place`). That is their
canonical home by test enforcement; moving them requires updating the
tests first.

## How contracts change

1. Update the normative contract before or alongside the implementation
   change — never after as a separate cleanup.
2. Keep `docs/architecture.md` a router, not a copy: it links to
   contracts instead of restating them.
3. Record important decisions where they are made (the contract's own
   decision log), not in a separate decision system.
4. Say which "recovery" you mean, every time: crash recovery, content
   recovery, or root recovery (`ROADMAP.md` defines the three senses).

## Implementation status vocabulary

Across `ROADMAP.md`, `architecture.md`, and design docs, use exactly
these states — never collapse them into a single "done":

| Status | Meaning |
|---|---|
| Specified | A contract or design has been written down. |
| Implemented | The code path exists. |
| Tested | The behavior has automated evidence. |
| Integrated | The relevant components are wired together. |
| End-to-end verified | The composed workflow has passed its required test. |
| Proposed / Open | The design or decision is not settled. |

Detailed evidence lives in tests and release records. Docs communicate
the status accurately and link to that evidence.

## How to validate changes

- All local Markdown links must resolve. The `doc-links` pre-commit hook
  enforces this on every commit touching prose; run
  `bash .ngit/scripts/tests/test-doc-links.sh` by hand to check on demand.
- Terminology follows the defining document (e.g. the three senses of
  "recovery"); do not redefine terms locally.
- Status claims use the vocabulary above.
- Review reports live verbatim on ngit as `review:*` issues. Findings
  become issues; contracts change through issues, not through edits to
  old reports. The finding workflow: verify each finding against the
  current commit, classify it, track accepted work as issues, and leave
  the original issue body unchanged.
