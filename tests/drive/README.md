# tests/drive/ — system-level evidence for the declarative drive service

Proves the `wyrd-drive` NixOS/nix-darwin module on real supervised
systems, not just in evaluation (`tests/harness/test-wyrd-module.sh`
pins the evaluated contract; this directory boots it).

## Layout

- `drive-service-entry.sh` — entryPoint for the
  `[tasks.wyrd-drive-e2e]` gate task. Reads the provision datum,
  holds the provision lock, publishes the run env, execs the bats
  suite. Run by the task unit, never by hand.
- `drive-service.bats` — four assertions over the provisioned guest:
  units active after boot (with a controlled reboot, so boot
  activation itself is proven, not a manual start), vault back
  after SIGKILL of the supervised child, held drive fails once
  with exit 78 and no retry (holder verified first), recovery once
  the holder releases. Odin-only: the drive box is a real
  provisioned microVM.
- `drive-service-launchd.sh` — macOS mirror leg. Evaluates the
  darwin fixture, copies each evaluated `serviceConfig` verbatim
  with only per-run fields replaced, lints both plists, bootstraps
  the vault in the user domain, and runs the same supervisor
  contract as the guest leg. The mount daemon is generated and
  linted only (its run needs macFUSE). Run from the checkout:
  `./tests/drive/drive-service-launchd.sh [--keep]`. No sudo: the
  user agent domain carries identical KeepAlive semantics for what
  this leg proves.

## Design notes both legs share

- The holder is verified *before* the refusal is asserted, with the
  vault already stopped, so the holder is the only possible
  contender — a lock nobody holds would make the 78 assertions
  vacuous.
- systemd's `StartLimitBurst` budget does not survive the leg's own
  start sequence (boot without secrets, setup, SIGKILL restart), so
  the held-drive start is preceded by an explicit `reset-failed`;
  without it the start is refused with start-limit-hit and the
  stale `ExecMainStatus` masquerades as the refusal under test.
- The guest mount lives in the service's mount namespace
  (`PrivateTmp`), invisible from the host: probe serving from
  inside the supervised process's namespace via `nsenter`, never
  from the host mount table.
- When ssh itself is unreachable, no guest-side check can run. The
  `wyrd-guest-debug` oneshot in `nix/microvm/common.nix` (drive
  guest only) dumps unit states, the key-install journal, share
  visibility, and mount options to `/mnt/wyrd-state/run/logs/` on
  every boot, so the host can diagnose a dark guest from the
  shared state dir.
