# Run the Wyrd review trio

Goal: produce three independent, evidence-only reviews at one commit
from a clean, isolated worktree. Reviews are historical evidence, not
policy). Reviews live on ngit as labeled
issues; find them with `gate issue list --label review:architecture`
(and `review:security`, `review:code-quality`, `review:release`).

The orchestrator (human or parent agent) owns provenance, cohesion,
and posting. Each reviewer owns only its report text.
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
- Model slug derived from deployment name (e.g. `crouton-alpha`).
- Reviews are posted as ngit issues, one per reviewer:
  `gate issue create --subject "review(<type>): <model> <date> @ <sha>" --type chore --priority P3 --milestone release:backlog --label review:<type> --body-file <report>` with `<type>` one of `architecture`, `security`, `code-quality`, `release`. No review files are written to the repo; reports stay in temp files until posted.

## Fan-out

Spawn exactly three isolated subagents, one per prompt:

- `01-architecture-review.md` → `review(architecture):`
- `02-security-review.md` → `review(security):`
- `03-codebase-review.md` → `review(code-quality):`

Each subagent:
- Reads only its prompt file + repo source/contracts. Must NOT search
  existing review issues (avoids cross-contamination).
- Records SHA/status itself (`git rev-parse HEAD`); never accepts a
  pasted SHA on trust.
- Returns its full report text plus SHA, finding counts by class, and
  skipped checks. Writes no repo files; does NOT create the issue.
- Leaves everything unstaged/uncommitted.

## Synthesis (orchestrator, after all three complete)

- Post the three reports as ngit issues with the subjects and
  `review:<type>` labels above; collect the three nevents.
- Spot-check that each report records the same SHA.
- Report nevents + one-line finding summary per review; offer next step
  (issue tree / fix) only on explicit request.

## Rules

- Never infer independence from count: same model ≠ confirmation.
- Frame reviews as evidence to be re-verified, never as policy.
- Keep the trio symmetric; any divergence in scope must be disclosed.
