# Collaboration Workflow

Issues and PRs go over ngit, driven through `gate` inside the repo's `devenv` shell. Never pipe or redirect its output. `gate help` lists the verbs, and the `gate-workflow` skill covers the PR lifecycle, authority limits and evidence rules. This document adds Wyrd's project policy on top.

## 1. Issues

Describe the problem or change, its context, and how to verify it. If the issue maps to a single implementation change, use a Conventional Commit-style subject. Implementation issues also state their persistent-format impact in one line: which durable formats or protocol bytes change and what that means for `docs/contracts/upgrade-contract.md`, or `none`.

Apply one label per category:

- **Type:** `bug`, `enhancement`, `chore`, or `docs` (for documentation-focused work)
- **Priority:** `P0` to `P4`
- **Milestone:** one `release:*` label, chosen against the milestone thesis in `ROADMAP.md` (including the v0.2 entry test), not by recency or the current release line. Uncommitted future work takes `release:backlog`. No triaged issue goes without one.
- **Review (only for review reports):** one `review:*` label (`review:architecture`, `review:security`, `review:code-quality`, `review:release`), swapping within its family. Review issues are otherwise triaged as `chore`/`P3`/`release:backlog`.
- **Audit (only for audit findings):** one `audit:*` label (`audit:blocker`, `audit:should-fix`, `audit:nice-to-have`), swapping within its family. Audit issues otherwise carry a normal type, priority and milestone.

## 2. PRs

- Use Conventional Commit subjects and focused commits. Hooks check the staged tree, not the worktree, so every commit in a stack must compile on its own. Otherwise keep the PR to one commit.
- The first push of a `pr/` branch creates a draft (`auto_draft` in `gate.toml`). `gate pr push` refuses a dirty tree or a non-`pr/` branch.
- **Description.** An empty description is a review finding. After pushing, set it with `gate pr cover --body-file cover.md`. Summarize the whole PR and link the issue with a `nostr:nevent1...` URI, never a bare number or raw event ID. Progress goes in comments.
- **CI** runs on `master` pushes and PR `ready_for_review`, only for changes touching paths listed in `.ngit/nix/workflows/*.yaml`. Drafts and docs-only changes run no jobs; docs are covered by the `doc-links` pre-commit hook, which link-checks every local Markdown reference.

## 3. Review and CI

Read the PR thread as plain text, oldest first (`gate review show`). Address concrete findings with new commits, run relevant checks locally, and push. Stay in draft while iterating, and stay on the PR's branch until it merges; switching mid-review orphans the review context and attached CI.

## 4. Merge

Confirm the PR is `open` (not `draft`), the final revision is correct, and CI is green on that exact revision. A green run on an older revision doesn't count. Docs-only PRs merge on a green `doc-links` hook, local checks, and review.
- **E2E legs must go green at the PR tip before merge.** Review and CI never execute them, so neither counts as evidence for them. Record the revision, command, and logs on the PR.

Merge from the `master` worktree, which needs the user or an explicit exception. The merge commit is created locally; publishing it and the `applied` status takes a human-signed push, since gate never pushes the target. Confirm with `gate pr status`.

## 5. Resolve the issue

A commit auto-resolves an issue only if its message has `fixes` or `resolves` followed by the issue ID or `nostr:nevent1...` reference. Otherwise resolve it after the merge with `gate issue resolve <id> --reason "Implemented and merged in nostr:nevent1..."`. Use `gate issue close` for rejected, duplicate, or otherwise non-completed work.

## 6. Releases

Follow `docs/releases/release-process.md`. The `release:*` milestone on the release issue is the tracking spine, and the v0.3 umbrella carries the next cut.
