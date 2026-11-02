# Collaboration Workflow

Issues and PRs go over ngit, driven through `gate` inside the repo's `devenv` shell. Never pipe or redirect its output. `gate help` lists the verbs, and the `gate-workflow` skill covers the PR lifecycle, authority limits and evidence rules. This document adds Wyrd's project policy on top.

## 1. Issues

Describe the problem or change, its context, and how to verify it. If the issue maps to a single implementation change, use a Conventional Commit-style subject. Implementation issues also state their persistent-format impact in one line: which durable formats or protocol bytes change and what that means for `docs/contracts/upgrade-contract.md`, or `none`.

Apply one label per category:

- **Type:** `bug`, `enhancement`, or `chore`
- **Priority:** `P0` to `P4`
- **Milestone:** one `release:*` label, chosen against the milestone thesis in `ROADMAP.md` (including the v0.2 entry test), not by recency or the current release line. Uncommitted future work takes `release:backlog`. No triaged issue goes without one.

```bash
gate issue create --subject "feat(format): path-level tree mutation" \
  --type enhancement --priority P2 --milestone release:backlog \
  --body-file issue.md
```

## 2. PRs

- Branch `pr/<name>` from current `master` with `gate pr checkout`, which creates the linked worktree at `../pr/<name>`. Agents never touch the `master` worktree.
- Use Conventional Commit subjects and focused commits. Hooks check the staged tree, not the worktree, so every commit in a stack must compile on its own. Otherwise keep the PR to one commit.
- The first push of a `pr/` branch creates a draft (`auto_draft` in `gate.toml`). `gate pr push` refuses a dirty tree or a non-`pr/` branch.
- **Description.** An empty description is a review finding. After pushing, set it with `gate pr cover --body-file cover.md`. Summarize the whole PR and link the issue with a `nostr:nevent1...` URI, never a bare number or raw event ID. Progress goes in comments.
- **CI** runs on `master` pushes and PR `ready_for_review`, only for changes touching paths listed in `.ngit/nix/workflows/*.yaml`. Drafts and docs-only changes run no jobs; docs are covered by the `doc-links` pre-commit hook, which link-checks every local Markdown reference.

## 3. Review and CI

Read the PR thread as plain text, oldest first (`gate review show`). Address concrete findings with new commits, run relevant checks locally, and push. Stay in draft while iterating, and stay on the PR's branch until it merges; switching mid-review orphans the review context and attached CI.

- `gate review run` request one review per finished tip, never on a revision you plan to amend. Blocking findings earn exactly one re-review after the fix. Skip re-review for non-blocking-only deltas; report the tip and move on to CI.
- `gate review wait` blocks until the review covers the tip. Never poll in a shell loop.
- **Docs-only findings** go to a follow-up issue, never a PR commit. `docs/**` fires no CI, so a docs fix neither needs nor gets a round, and bundling one with trigger-path files burns a full round for nothing.
- Reply to review comments as yourself from the PR worktree: `gate review reply --comment <COMMENT> --body "Addressed in <commit>."`.
- Start CI with `gate ci run` and check `gate ci status` first. Skip `gate ci wait` when status reports `none` (docs-only PRs). Per-job failure logs arrive as signed PR comments and Blossom `.asc` links.
- Never push while CI is running on the tip. The push orphans the in-flight run and leaves no usable signal on either revision. Let it settle, then push the batch.
- `gate pr ready` refuses unless the review covers the tip and CI is green on it, and names the missing leg.
- **History.** Don't rewrite published history unless the workflow requires it. Merging `master` in for review tooling is fine; gate tolerates it at merge time. Merging in another branch's commits is not, so rebase those out. Keep unrelated worktree changes out of the PR.
- **Identifiers.** Take them from `gate pr id`; never retype a bech32 identifier or plumb IDs between commands with `python3 -c`, `head`, `grep`, or files.

## 4. Merge

Confirm the PR is `open` (not `draft`), the final revision is correct, and CI is green on that exact revision. A green run on an older revision doesn't count. Docs-only PRs merge on a green `doc-links` hook, local checks, and review.

Merge from the `master` worktree, which needs the user or an explicit exception. The merge commit is created locally; publishing it and the `applied` status takes a human-signed push, since gate never pushes the target. Confirm with `gate pr status`.

```bash
gate pr merge pr/<name>
git push origin master
```

## 5. Resolve the issue

A commit auto-resolves an issue only if its message has `fixes` or `resolves` followed by the issue ID or `nostr:nevent1...` reference. Otherwise resolve it after the merge with `gate issue resolve <id> --reason "Implemented and merged in nostr:nevent1..."`. Use `gate issue close` for rejected, duplicate, or otherwise non-completed work.

## 6. Releases

Follow `docs/releases/release-process.md`. The `release:*` milestone on the release issue is the tracking spine, and the v0.3 umbrella carries the next cut.
