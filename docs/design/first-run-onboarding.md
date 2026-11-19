# First-run identity onboarding (v0.4)

Status: normative for the v0.4 onboarding implementation. Frozen
from `planning/v0.4/first-run-identity-onboarding-DRAFT.md` v5
(local working draft, not tracked); this file is the canonical
home the implementation and its fixtures cite.

## Decisions

- Onboarding persists the device identity + passphrase files on
  this device (0600, owner-only, never overwrite) in both drive
  postures — the device gate and the unlock's `--identity-file`
  need them. The file-backed posture stays the default for managed
  drives: the drive passphrase file exists, so the service manager
  starts and restarts the owner unattended and the device remembers.
  The per-session posture persists no drive passphrase file and
  renders no service definition, so the drive is locked-until-unlock
  from registration — reboot, explicit lock, or stopping the drive
  relocks it (quitting the app leaves a running session drive
  unlocked: quit never locks, per
  `docs/contracts/desktop-lifecycle.md`), and only a typed
  passphrase opens it for the session. The mandate
  below (write-down proof, backup acknowledgement, never overwrite,
  read-back) binds in both postures. OS keychain migration stays the
  tracked follow-up owned by settings — a security feature, not a
  convenience option.
- Terminology is always "this device's identity", never "your
  identity". Entry is named "Restore from backup" (never Import,
  never a Recommended nudge). Stage name is Passphrase; the
  relaunch verb keeps Unlock. Generation is preselected.
- T14 halves, DeviceId display, raw nsec/hex, and QR codes never
  appear in this flow. "Removed" is the word; "retired" never
  appears.

## Security boundary (non-negotiable copy constraint)

A 0600 file stops other unprivileged local users. It does not
stop same-user malware, a compromised session, processes with the
user's file access, or anyone holding an unencrypted backup. With
the passphrase remembered on disk, someone holding both
credential files holds everything. The UI states the
on-this-device / on-another-device split plainly and never
implies the remembered passphrase authenticates anyone or stops
same-user attackers:

- On this device: Wyrd remembers the passphrase; the files are
  accessible to the user account; full-disk encryption and the
  login are the actual protection.
- On another device: the backed-up identity files plus the
  passphrase restore the identity.

## Flow (three stages, not screens)

```
Identity -> Passphrase -> Recovery -> Ready
    \- Restore branch -> Passphrase (verify) -> Recovery (abbreviated) -> Ready
```

Back always allowed; Continue/Finish validate on press with
helper text (never greyed-out silence). Quitting mid-flow raises
a discard prompt: the identity is not saved yet and anything
generated dies with the session. Progress shows the three
conceptual stages only.

### Identity

"Welcome to Wyrd" — "This device needs its own identity to join
drives." Choices: "Create a new identity for this device." /
"Restore from backup". Before it, the mental model: the
identity's two parts (identity key: generated randomly, cannot
be recreated from the passphrase; unlock passphrase: protects
access, does not replace or recover the key), "You need both",
and the on-device / on-another-device split above.

Generation happens in the CLI, never in the UI or the desktop
backend: the passphrase is minted for display at the Passphrase
stage (`identity new-passphrase`) and held in memory; the identity
scalar is minted in-process by `identity commit` at Finish and
never leaves the CLI. Nothing is written before Finish. No public
identifier, no copy button.

### Restore

"Choose the files belonging to the identity you want to restore
on this machine." Plus the cloning rule: "A second device that
stays in use needs its own identity". No paste-a-secret textbox:
paths only. Wrong-passphrase vs damaged-file is one combined
message (OD-24-D) — the UI never promises a distinction the
contract forbids. Clone-vs-replace: two live machines holding the
same secret are one device (same DeviceId, shared fate); a
removed key can never be re-admitted.

### Passphrase

"Choose a passphrase": Generate one for me / Choose my own.
"This device will remember it so you don't type it each launch.
Short passphrases can be guessed if someone gets a copy of your
files — 12 characters is the floor; longer is better."
Generated branch (inline, visible until Finish commits): "Your
generated passphrase — generated locally on this device", Copy
for the password manager, Generate another. "Write it down or
save it somewhere safe: this copy is for a new machine or a
reinstall." Clipboard clears best-effort where the platform
allows; the stored copy is the backup, not the clipboard.

### Recovery (two layers)

Primary (always shown): "Wyrd cannot recover this device's
identity for you. To restore it, you need a backup of your
identity files and your passphrase. If you lose either one,
Wyrd cannot restore your identity. Keep your identity files and
passphrase in separate places." The path shown pre-commit is the
*intended* location, labeled created-on-finish — never presented
as an existing file.

Expandable detail: restoring the identity alone doesn't bring
back drive contents (need a surviving copy or a fresh
invitation); a removed identity cannot regain access (that drive
needs a new admitted identity); guardian recovery is planned for
a later release.

Either/or nuance: the fatal case is the *backup* pair. An
existing installation keeps working on its remembered credential
even if the user forgets the passphrase; restoration anywhere
else needs both backups.

One acknowledgement checkbox covering both the stored-passphrase
confirmation and the comprehension statement, plus a confirm entry:
the recovery stage asks the person to re-type the passphrase from
their stored copy, and Finish refuses where it does not match the
effective passphrase ("That does not match the passphrase — check
your stored copy and re-type it."). The confirm proves the stored
copy is right before anything commits — a mismatch means the backup
is wrong, never the commit. Finish validates on press.

### Ready

"This device is ready" — "Your device has its own Wyrd
identity." Next: Create a drive / Join with an invitation (the
Join target must read acceptably for a genuinely new invitee).
"Identity files — Stored on this device" with the actual
post-commit path and Show in Finder. Created-vs-backed-up stays
explicit: showing the folder claims no backup. No
recovery/backup status beyond the confirm-entry match — backup is
acknowledged and matched, never independently verified.

## Commit boundary

1. Generate or validate identity in memory.
2. Choose and validate the passphrase.
3. Display the generated passphrase where applicable.
4. Explain recovery, obtain acknowledgement.
5. Explicitly commit credential files atomically (0700 dirs, 0600
   files, O_NOFOLLOW, never overwrite an existing file).
6. Read back through the production hardened reader, both files.
7. Record the pair as adopted (so the gate and later launches
   resolve it), then report success.

No successful onboarding state exists before step 6 passes.
Cancellation before step 5 leaves nothing behind. Failure at 5–6
is a designed state: inline error, retry from retained memory,
passphrase never persisted by a failed commit. Secrets travel as
file paths, never as argv contents; the two documented
passphrase transits (write-down display, stdin-pipe commit) are
the onboarding exception recorded in `desktop-shell.md`.

## Entry and broken-identity states

| Condition | Experience |
|---|---|
| No identity exists | Welcome |
| Valid identity exists | Unlock (automatic where remembered), then Drives |
| Exists but permissions wrong | Repair: human-first copy, Fix-guidance, Verify again, Choose different files, technical detail expandable |
| Exists but format invalid | Repair-or-restore, same pattern; single combined passphrase/damage message |
| Incomplete (one file missing) | Names exactly what is missing |
| Helper missing or mismatched (`versionmismatch` from the gate, including an unbuilt sibling CLI) | Pairing state, never a credential verdict: own headline, reinstall/build fix, Verify again only — no file choosing, no new identity, credential paths not shown |
| Gate unreadable (transport, unusable paths) | Explicit unknown state with Retry, never an open gate |

Never let an inaccessible identity silently become an invitation
to generate. A new identity from a broken state requires explicit
destructive confirmation stating preservation (old file kept
where it is, never overwritten) and consequence (old drives will
not recognize the new device). The adopted record is what makes
the new pair stick: without it the next launch re-enters Repair
against the old defaults.
