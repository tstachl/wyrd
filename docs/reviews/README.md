# Reviews: evidence, not policy

Past external reviews live on ngit as issues, not files. They record
what a reviewer observed at a point in time. They do not override
a contract, prove a finding is still present, or create obligations on
their own.

Find them by label:

- `gate issue list --label review:architecture`
- `gate issue list --label review:security`
- `gate issue list --label review:code-quality`
- `gate issue list --label review:release`

Subjects follow `review(<type>): <model> <date> @ <sha>`; the body is
the verbatim report plus a backfill header (source path, backfill date,
reviewed SHA). The 21 historical reports plus the pulse and longcat
trios were backfilled in November 2026, after which the
`docs/reviews/archive/` directory was removed from git.

## Finding workflow

For each actionable finding in a report:

1. Verify it against the current commit.
2. Classify it: confirmed, already fixed, duplicate, rejected, or
   unresolved pending a decision.
3. Track accepted work in the issue tracker — never in a second finding
   list here.
4. Update the authoritative contract if the finding exposes a genuine
   specification gap.
5. Leave the original issue body unchanged; record disposition in
   comments on the review issue.

A stale finding must never read as a current defect. If a report has no
disposition trail in the issue tracker, assume nothing about whether its
findings still hold — re-verify first.
