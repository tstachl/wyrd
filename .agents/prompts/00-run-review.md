# Run the Wyrd review trio

Goal: produce three independent, evidence-only reviews at one commit
from a clean, isolated worktree. Reviews are historical evidence, not
policy (see `docs/reviews/README.md`).

The orchestrator (human or parent agent) owns provenance, cohesion,
and the `README.md` index. Each reviewer owns only its archive file.
This separation buys independence and prevents write races.

## Setup (manual, before spawning)

These steps run in the master/integration checkout, not the PR worktree.

1. From master run `gate pr checkout {model}-review`
   (e.g. `gate pr checkout crouton-alpha-review`). This creates or
   attaches the `pr/{model}-review` worktree per the workspace rules.
2. `session_move` to the PR directory, e.g.
   `session_move("/Users/thomas/workspace/wyrd/pr/crouton-review")`.
   Verify `git rev-parse HEAD`, `git branch --show-current`, and
   `git status --porcelain` from the new directory.
3. Run the three subagents from that PR directory (see below).

Do not run reviewers from master or from a shared worktree.
Do not attach two worktrees to the same branch.

## Preconditions

- Worktree pinned and clean (`git status --porcelain` empty).
- Output dir: `docs/reviews/archive/`; files are untracked and stay
  unstaged/uncommitted until the human reviews and commits them.
- Model slug derived from deployment name (e.g. `crouton-alpha`).

## Fan-out

Spawn exactly three isolated subagents, one per prompt:

- `01-architecture-review.md` → `{date}-{model}-architecture-review.md`
- `02-security-review.md` → `{date}-{model}-security-review.md`
- `03-codebase-review.md` → `{date}-{model}-codebase-review.md`

Each subagent:
- Reads only its prompt file + repo source/contracts. Must NOT read
  `docs/reviews/` (avoids cross-contamination).
- Records SHA/status itself (`git rev-parse HEAD`); never accepts a
  pasted SHA on trust.
- Writes only its own archive file; does NOT edit `README.md`.
- Returns: path, SHA, finding counts by class, skipped checks.
- Leaves everything unstaged/uncommitted.

## Synthesis (orchestrator, after all three complete)

- Verify the three files exist, unstaged/uncommitted (`git status`).
- Spot-check that each records the same SHA.
- Append the three blurbs to `docs/reviews/README.md` Index in one edit.
- Report paths + one-line finding summary per review; offer next step
  (issue tree / fix) only on explicit request.

## Rules

- Never infer independence from count: same model ≠ confirmation.
- Frame reviews as evidence to be re-verified, never as policy.
- Keep the trio symmetric; any divergence in scope must be disclosed.
