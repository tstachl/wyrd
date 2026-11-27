# macOS File Provider acceptance scenario

This scenario runs the packaged-app filesystem acceptance test on a real macOS
guest. It is deliberately separate from the NixOS/FUSE suite in `tests/lima`:
macOS File Provider callbacks, App Group containers and Finder's CloudStorage
mount cannot be exercised by the Linux FUSE path.

## Files

- `macos-27.yaml` — Lima template pinned to macOS 27.2.1 (26B131). Its system
  provisioning is idempotent: it configures automatic login (needed for an
  Aqua session and the App Group container) and stages a PPPC profile granting
  Full Disk Access to the responsible processes.
- `run-acceptance.sh` — host half: stage an RC bundle, register the appex and
  domain registrar, create a fresh drive, start the owner, run the guest
  driver, and copy evidence back to the host. `--create-mode app` (default)
  creates the drive with the same credential-commit and bootstrap commands
  the desktop onboarding and add-drive views use, including a registry entry
  and the adopted-pair record the desktop writes after commit, so the app
  opens on the drive rather than the adoption choice. It uses the desktop's
  `--no-service` posture because the clean guest has no Command Line Tools
  for the Python supervise wrapper, and starts the owner directly.
  `--create-mode cli` keeps the original `identity generate` + `drive init`
  leg, which has no registry entry and exercises alias-based discovery.
- `filesystem-acceptance.sh` — guest half: Finder AppleScript navigation and
  filesystem operations with per-step assertions, owner-view verification via
  the Wyrd CLI, and an app+owner restart subset.

## One-time manual prerequisites

macOS 27 removed `profiles install`: the provisioning script stages
`/etc/wyrd/lima-fda.mobileconfig` but **cannot install it silently**. On a
fresh instance, install it once in System Settings → Profiles, then grant the
two Apple Event permissions by allowing the prompts when the driver first
calls `osascript` (Finder and Terminal), and grant Accessibility / Device
Control and Data Access to `sshd-keygen-wrapper`. A snapshot with these
already in place removes all prompts from later runs.

Pass the App Group explicitly with `WYRD_APP_GROUP` when building the RC and
`--group` when running the scenario. Use a short, team-prefixed group: a long
group plus the default `/Users/<user>.guest` home makes the owner's AF_UNIX
socket path exceed macOS's 104-byte limit. The team identifier is a local
deployment input; it is never checked into this repository.

## Run

```sh
WYRD_APP_GROUP=<team-prefixed-short-group> \
  ./tests/lima/macos/run-acceptance.sh \
    --app /path/to/rc-stage/Wyrd.app \
    --revision <git-sha>
```

The driver exits non-zero on any failed assertion. The runner still copies
per-step logs, the owner's export tree, the domain plist, the desktop
registry and the plugin registration when the driver fails, and records the
appex hash, the create mode and the `Connected` precondition in
`metadata.json`. The `app` leg also saves a screenshot of the desktop after
launch. A run with `Connected=false`
does **not** claim a full pass against the acceptance issue's definition of
done, even when all filesystem checks succeed.

## Caveats

- A usable CloudStorage mount and real Finder writes are verified. The domain
  plist's `Connected` flag has stayed `false` even on successful runs. The
  driver records this as an unmet precondition and the runner refuses to
  claim the issue's full pass; the underlying flag remains a product issue.
- mtime-only `update-item` stale-base refusals can produce `-2005` lines that
  macOS re-drives; file create/rename/move/delete must produce none.
