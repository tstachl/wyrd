# Collaboration Workflow

Issues and PRs go over ngit, driven through `gate`. Run `gate` inside the repo's `devenv` shell and never pipe or redirect its output. `gate help` lists the verbs, and the gate skill documents them.

## 1. Create an issue

Describe the problem or change, its context, and how to verify it. If the issue maps to a single implementation change, use a Conventional Commit-style subject. Implementation issues also state their persistent-format impact in one line: which durable formats or protocol bytes change and what that means for `docs/contracts/upgrade-contract.md`, or `none`.

Apply one label from each category:

- **Type:** `bug`, `enhancement`, or `chore`
- **Priority:** `P0` to `P4`
- **Milestone:** one `release:*` label, chosen against the milestone thesis in `ROADMAP.md` (including the v0.2 entry test), not by recency or the current release line. Future work with no commitment takes `release:backlog`. No triaged issue goes without a `release:*` label.

```bash
gate issue create --subject "feat(format): path-level tree mutation" \
  --type enhancement --priority P2 --milestone release:backlog \
  --body-file issue.md
```

## 2. Create a PR

Branch `pr/<name>` from current `master` (`gate pr checkout` creates and attaches the linked worktree at `../pr/<name>`). Agents never touch the `master` worktree.

```bash
gate pr checkout <name>
# work, commit
gate pr push
gate pr status
```

Use Conventional Commit subjects and keep commits focused. The first push of a `pr/`-prefixed branch creates the PR as a draft (`auto_draft` in `gate.toml`). `gate pr push` refuses a dirty tree or a non-`pr/` branch, and handles first push, fast-forward, and rewrite.

Hooks check the staged tree, not the worktree, so every commit in a stack must compile on its own. Otherwise keep the PR to one commit.

**Description.** An empty description is a review finding. Link the issue with a `nostr:nevent1...` URI (never a bare issue number or raw event ID) and summarize the change. Set it with `gate pr cover` after pushing:

```bash
gate pr cover --body-file cover.md
```

The cover note describes the whole PR. Progress goes in comments.

**CI** runs on `master` pushes and PR `ready_for_review`, and only for changes touching paths listed in `.ngit/nix/workflows/` (`shell.yaml`, `rust.yaml`, `nix.yaml`: `crates/**`, `Cargo.toml`, `Cargo.lock`, `deny.toml`, `rust-toolchain.toml`, `flake.*`, `nix/**`, `tests/**`, and `.ngit/nix/**`; `page.yaml`: `page/**` on master pushes only; `dist.yaml`: every `v*` tag). Drafts and docs-only changes run no CI jobs. Docs are covered by the `doc-links` pre-commit hook, which link-checks every local Markdown reference.

## 3. Review and update

Read the PR thread as plain text, oldest first. Address concrete findings with new commits, run relevant checks locally, and push. Stay in draft while iterating, and stay on the PR's branch until it merges. Switching branches mid-review orphans the review context and the CI attached to the branch.

```bash
gate pr status
gate review show
# fix, commit
gate pr push
```

Fire the reviewer (prompt and model come from `gate.toml`), then block until it covers the tip. Never poll in a shell loop.

```bash
gate review run
gate review wait
```

Request one review per finished tip, never on a revision you plan to amend. Skip re-review for non-blocking-only deltas; report the tip and move on to CI. Blocking findings earn exactly one re-review after the fix.

**Docs-only findings** go to a follow-up issue, never a PR commit. `docs/**` fires no CI jobs in any native workflow, so a docs fix neither needs nor gets a CI round — and bundling one with trigger-path files burns a full round for nothing.

Reply to review comments as yourself from the PR worktree. `gate review post` is reviewer-only and always posts as the review signer.

```bash
ngit pr comment <id> --body "Addressed in <commit>."
```

When the work is ready, run CI and mark the PR ready. `gate pr ready` refuses unless the review covers the tip and CI is green on it, and names the missing leg. Skip `gate ci wait` when `gate ci status` reports `none` (docs-only PRs).

```bash
gate ci run
gate ci wait
gate pr ready
```

Never push while CI is still running on the tip: the push orphans the in-flight run, wastes its compute, and leaves no usable signal on either revision — wait for it to settle, then push the batch.

**History.** Don't rewrite published history unless the workflow requires it. Merging `master` in for review tooling is fine; gate tolerates it at merge time. Merging in another branch's commits is not, so rebase those out. Keep unrelated worktree changes out of the PR.

## 4. Merge

Confirm the PR is `open` (not `draft`), the final revision is correct, and CI is green on that exact revision. A green run on an older revision doesn't count.

```bash
gate pr status
gate ci status
```

Docs-only PRs merge on a green `doc-links` hook, local checks, and review.

Merge from the `master` worktree, which needs the user or an explicit exception. Then publish with a human-signed push, since gate never pushes the target.

```bash
gate pr merge pr/<name>
git push origin master
```

The merge creates the merge commit locally, and the push publishes it and the `applied` status. Confirm with `gate pr status`.

## 5. Resolve the issue

A commit auto-resolves an issue only if its message has `fixes` or `resolves` followed by the issue ID or `nostr:nevent1...` reference. Otherwise resolve it after the merge:

```bash
gate issue resolve <id> --reason "Implemented and merged in nostr:nevent1..."
```

Use `gate issue close` for rejected, duplicate, or otherwise non-completed work.

## 6. CI triggers and identifiers

**CI.** Use `gate ci run`, never a hand-rolled `ngit ci trigger`. Check `gate ci status` first. Signed per-job failure logs arrive as PR comments and Blossom `.asc` links.

**Identifiers.** Never retype a bech32 identifier (npub, nevent, naddr) from memory, because transcription silently corrupts them. `gate pr id` resolves the current branch, and every verb takes it from there. Don't plumb IDs between commands with `python3 -c`, `head`, `grep`, or files. Gate's exit code and refusal block are the signal. Run `--help` on any unfamiliar subcommand before running a command that writes.

## 7. Releases

Follow the checklist in `docs/releases/release-process.md`: version bump, changelog, fixture, gates, tag, multi-platform build, publish. The `release:*` milestone on the release issue is the tracking spine, and the v0.3 umbrella carries the next cut.
