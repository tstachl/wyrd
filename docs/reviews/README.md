# Reviews: evidence, not policy

Past external reviews are preserved here as historical evidence. They
record what a reviewer observed at a point in time. They do not override
a contract, prove a finding is still present, or create obligations on
their own.

## Finding workflow

For each actionable finding in a report:

1. Verify it against the current commit.
2. Classify it: confirmed, already fixed, duplicate, rejected, or
   unresolved pending a decision.
3. Track accepted work in the issue tracker — never in a second finding
   list here.
4. Update the authoritative contract if the finding exposes a genuine
   specification gap.
5. Leave the original report unchanged.

A stale finding must never read as a current defect. If a report has no
disposition trail in the issue tracker, assume nothing about whether its
findings still hold — re-verify first.

## Index

Reports live verbatim in `archive/`, named `YYYY-MM-DD-<reviewer>-<type>.md`.
Reviewed commit is listed when the report records one.

| Report | Date | Type | Reviewed commit |
|---|---|---|---|
| [2026-09-10-chatgpt-architecture-review.md](archive/2026-09-10-chatgpt-architecture-review.md) | 2026-09-10 | architecture | not recorded |
| [2026-09-11-claude-architecture-review.md](archive/2026-09-11-claude-architecture-review.md) | 2026-09-11 | architecture | not recorded |
| [2026-09-12-chatgpt-architecture-review.md](archive/2026-09-12-chatgpt-architecture-review.md) | 2026-09-12 | architecture | not recorded |
| [2026-09-13-muse-spark-code-review.md](archive/2026-09-13-muse-spark-code-review.md) | 2026-09-13 | code quality | not recorded |
| [2026-09-17-chatgpt-pre-alpha-review.md](archive/2026-09-17-chatgpt-pre-alpha-review.md) | 2026-09-17 | pre-release | not recorded |
| [2026-09-19-claude-release-strategy-review.md](archive/2026-09-19-claude-release-strategy-review.md) | 2026-09-19 | release strategy | not recorded |
| [2026-09-24-full-security-functionality-review.md](archive/2026-09-24-full-security-functionality-review.md) | 2026-09-24 | security + functionality | not recorded |
| [2026-09-29-master-review.md](archive/2026-09-29-master-review.md) | 2026-09-29 | codebase | not recorded |
| [2026-10-02-luna-v0.2-release-review.md](archive/2026-10-02-luna-v0.2-release-review.md) | 2026-10-02 | release readiness (v0.2.0-alpha) | `4877b1e` cited for CI evidence |
| [2026-10-02-space-bunny-v0.2-release-review.md](archive/2026-10-02-space-bunny-v0.2-release-review.md) | 2026-10-02 | release (v0.2.0-alpha) | not recorded |
| [2026-10-03-space-bunny-v0.2-release-review.md](archive/2026-10-03-space-bunny-v0.2-release-review.md) | 2026-10-03 | release (v0.2.0-alpha) | `7f83d2b`, `962348d` cited |
| [2026-10-18-beam-free-adversarial-security-audit.md](archive/2026-10-18-beam-free-adversarial-security-audit.md) | 2026-10-18 | adversarial security | not recorded |
| [2026-10-18-beam-free-architecture-review.md](archive/2026-10-18-beam-free-architecture-review.md) | 2026-10-18 | adversarial architecture | not recorded |
| [2026-10-18-beam-free-codebase-quality-review.md](archive/2026-10-18-beam-free-codebase-quality-review.md) | 2026-10-18 | adversarial code quality | not recorded |
| [2026-10-20-codex-adversarial-codebase-quality-review.md](archive/2026-10-20-codex-adversarial-codebase-quality-review.md) | 2026-10-20 | adversarial code quality | not recorded |
| [2026-10-20-gpt-6-adversarial-architecture-review.md](archive/2026-10-20-gpt-6-adversarial-architecture-review.md) | 2026-10-20 | adversarial architecture | not recorded |
| [2026-10-20-gpt-6-adversarial-security-audit.md](archive/2026-10-20-gpt-6-adversarial-security-audit.md) | 2026-10-20 | adversarial security | not recorded |
| [2026-10-20-sol-adversarial-prerelease-review.md](archive/2026-10-20-sol-adversarial-prerelease-review.md) | 2026-10-20 | adversarial pre-release | not recorded |
