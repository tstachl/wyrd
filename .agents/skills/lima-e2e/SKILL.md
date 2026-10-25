---
name: lima-e2e
description: >
  Use when running, debugging, or interpreting Wyrd's Lima end-to-end
  tests, Odin microVM runs, guest logs, relay behavior, mount lifecycle,
  or revision-bound integration evidence.
---

# Wyrd Lima E2E

## Shared infrastructure

- The Lima instance, relay port, logs, and guest state are shared.
- Only one agent may run the full Lima suite at a time. Local compilation and unit tests may run in parallel.
- Parallel E2E requires separate instance names, ports, and state directories.
- Odin runs consume shared host resources and can overwrite guest state. Treat them as coordinated operations, not exploratory commands.

## Before launching

Every expensive E2E or Odin run must answer three questions:

1. What specific hypothesis does this run test?
2. What existing local evidence cannot answer it?
3. What new log, telemetry, or observation will this run capture?

If these questions have no concrete answers, do not launch the run.

- Inspect the current revision, existing test results, and available logs first.
- After a red run, identify a fix or add instrumentation before rerunning. Never rerun unchanged code merely hoping for a green result.
- Use the repository's established E2E commands and Gate workflow. Do not invent launch commands or bypass the harness.

## Run execution

- Run only one full Lima suite at a time against shared state.
- Keep detached-run polling bounded and use the prescribed wait command rather than ad hoc shell polling loops.
- Store run logs in the tool-provided temporary directory, not `/tmp`. `/tmp` may be inaccessible to review agents or fragile in the current execution environment.
- Capture the tested revision and enough run metadata to associate every result with that revision.

## Evidence collection

After a run:

1. Pull one tarball containing the entire guest-log directory into the tool-provided temporary directory.
2. Verify the archive's file list and confirm the expected logs are present.
3. Stop interacting with Odin for that run.
4. Debug from the local archive.

Further SSH access can race another agent's run, whose setup may wipe the guest state. It also consumes shared host resources.

If the archive lacks the decisive evidence, improve the test leg's instrumentation so the evidence is emitted into its log on failure. Do not compensate for missing instrumentation with repeated remote inspection.

## Revision-bound evidence

A green result proves only what ran against the revision and configuration actually tested.

- Re-run the relevant E2E suite at the PR tip before merging when teardown, transport shutdown, CLI mount paths, or other exercised behavior has changed since the last green run.
- Do not treat unrelated green CI jobs as substitutes for required Lima evidence.
- If a required tip-level run cannot be performed, document the exception and its rationale in the PR, and include it explicitly in the merge check.
- Verify that the suite actually includes the intended tests and that fixtures reach the guest and code path under test. A green suite is not evidence for a test that was never wired into the runner or a fixture that never reached the guest.

## Reporting results

Report:

- The exact revision tested.
- The suite or test legs that actually ran.
- Passes, failures, and missing evidence.
- The location of the captured logs.
- Any difference between the tested revision and the current PR tip.

Never describe evidence as current or merge-ready without checking revision alignment. Distinguish an infrastructure failure from a product failure, and distinguish missing evidence from a passing test.

