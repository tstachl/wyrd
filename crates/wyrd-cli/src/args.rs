//! The `wyrd` command line: clap argument definitions. The doc
//! comments here are the `--help` text, so this module moves only
//! with a byte-identical help comparison (see the CLI-split issue,
//! `nostr:nevent1qqsyfxq3flw7nx0slwkvmvpuau47a9xmfsgstk5leulu297af7fvkqgpz9mhxue69uhkwunpwdczuap49eehgfncncm`).
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// The `wyrd` binary: create a drive, mount its live projection,
/// export its namespace to a plain directory tree, administer its
/// membership, pair a new device, or sync headless. Every
/// drive-scoped subcommand needs the credential files (read and
/// hardened by wyrd code, never by clap); `identity` is the
/// exception — it creates them and takes paths to write, not to
/// read.
/// `--relay` is deployment state shared by every networked
/// subcommand — nothing in the keystore names relays, so they arrive
/// as flags, defaulted from the drive's `default.toml` when the flags
/// name none (see `drive_config`).
/// Export is offline by construction: no relays, no serving, no FUSE.
#[derive(Debug, Parser)]
#[command(name = "wyrd", version, about)]
pub(crate) struct Cli {
    /// Drive deployment configuration elsewhere than the drive
    /// directory's `default.toml`: every subcommand reads the same
    /// file, so one override covers them all. A missing override is
    /// a named error, never an empty configuration; an absent
    /// default is simply no configuration.
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Args)]
pub(crate) struct Credentials {
    /// Nostr identity secret: 32 raw bytes or 64 hex characters.
    /// Defaults to the configuration file's `identity_file` when
    /// the flag is absent; the flag always wins.
    #[arg(long, value_name = "PATH")]
    pub(crate) identity_file: Option<PathBuf>,

    /// Keystore passphrase, UTF-8 text. Defaults to the
    /// configuration file's `passphrase_file` when the flag is
    /// absent; the flag always wins. `--passphrase-stdin` reads one
    /// line from stdin instead (per-session unlock: nothing
    /// persisted); it conflicts with this flag.
    #[arg(long, value_name = "PATH", conflicts_with = "passphrase_stdin")]
    pub(crate) passphrase_file: Option<PathBuf>,

    /// Read the keystore passphrase from stdin (one line, bounded
    /// 4096 bytes, single trailing newline stripped): the
    /// per-session unlock path that never persists a passphrase
    /// file. The identity secret still comes from its file.
    /// Conflicts with `--passphrase-file`.
    #[arg(long, conflicts_with = "passphrase_file")]
    pub(crate) passphrase_stdin: bool,
}

/// Control-plane relays: repeatable deployment state shared by every
/// networked subcommand. Nothing in the keystore names relays, so
/// they arrive as flags with identical parsing and identical
/// empty-means-idle semantics everywhere — mount and sync must never
/// drift into subtly different relay handling. When the flags name
/// none, the drive's configuration file supplies the list (see
/// `drive_config`); explicit flags replace the configured list
/// rather than appending to it, so the effective set is knowable.
#[derive(Debug, Args)]
pub(crate) struct RelayArgs {
    /// Control-plane relay; repeatable. With none given and none
    /// configured, intake stays idle and the command runs offline
    /// (`sync now` additionally requires `--offline` to say so
    /// explicitly).
    #[arg(long, value_name = "URL")]
    pub(crate) relay: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Local drive lifecycle: create, provision, delete, reconfigure.
    /// One drive per invocation; `--config` selects the drive
    /// configuration, never all drives at once.
    Drive {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(subcommand)]
        action: DriveAction,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Mount a live projection at `mountpoint` (read-write).
    // Behind the `mount` feature (plain comment: `///` docs would leak
    // into the pinned `--help` surface): a mount-free build has no
    // FUSE userspace library to mount with.
    #[cfg(feature = "mount")]
    Mount {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        /// Where to serve the projection.
        mountpoint: PathBuf,
        #[command(flatten)]
        relays: RelayArgs,
        /// Verbose mount diagnostics: debug-level FUSE request logs
        /// (opcode + latency + reply errno) in stderr and `mount.log`.
        /// Without it the mount logs at info level, and each request
        /// costs one enabled-check.
        #[arg(long)]
        verbose: bool,
        /// Live-loop idle pacing in whole seconds: bounds how long
        /// total silence may delay a sync pass (default 5). Producers
        /// wake the loop immediately, so this only paces quiet
        /// stretches — it is the e2e wall-time knob, not a semantic
        /// bound. Range 1-86400 (a day): zero would busy-spin the loop
        /// and past a day the deadline stops being pacing, so both are
        /// usage errors.
        #[arg(long, value_name = "SECS", default_value_t = 5)]
        interval: u64,
        /// Per-device retained-bytes ceiling in bytes: the same number
        /// bounds local authoring (refused before the commit's first
        /// write, `ENOSPC`) and receive-path admission (refused before
        /// durable storage, counted separately, never stored). Unset
        /// (the default) enforces no ceiling: existing deployments
        /// behave exactly as before until an operator opts in. A
        /// startup below current retention refuses to start with both
        /// numbers named instead of failing every write.
        #[arg(long, value_name = "BYTES")]
        retained_quota: Option<u64>,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Export the drive's namespace to a plain directory tree: files,
    /// directories (empty ones included), symlinks, and the executable
    /// bit, with multi-head conflicts as `name@N` siblings. The output
    /// needs no wyrd software to read — this is the offline egress
    /// path guaranteed before any format break.
    Export {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        /// Destination directory: must not exist or must be empty.
        out_dir: PathBuf,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Administer drive membership: list members, inspect the log,
    /// invite a device, or author remove/rotate/set-owner transitions.
    /// Reads are offline projections of the keystore; writes commit
    /// one transition plus catch-up obligations, sent on the
    /// next mounted sync.
    Member {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(subcommand)]
        action: MemberAction,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Inspect snapshots and merge conflicted heads: classify every
    /// DAG head, preview a merge, or author one merge snapshot over
    /// explicit sources plus a path spec. Reads are offline
    /// projections of the keystore; a merge authors one snapshot
    /// with ordinary member authority and queues the usual
    /// announcements for the next mounted sync.
    Snapshot {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(subcommand)]
        action: SnapshotAction,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Pair this device with a drive: identify it, stage pairing
    /// material for the owner, or join from a sealed invitation. The
    /// owner admits the staged key via `member invite`; the invitation
    /// file travels out-of-band.
    Device {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(subcommand)]
        action: DeviceAction,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// First-run identity onboarding: generate this device's
    /// credential files, or verify them through the hardened reader.
    /// `generate` mints a fresh identity secret plus a generated
    /// passphrase, writes both files (owner-only, never overwrite),
    /// and prints the passphrase on stdout for write-down. `verify`
    /// checks both files (permissions, format) and exits 0/2.
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },
    /// Headless sync: report sync state or run a bounded sync without
    /// mounting. Status never connects and never mutates durable
    /// state; `now` connects when given a relay (and refuses a
    /// relay-less run without `--offline`), may mutate durable
    /// state, and reports liveness.
    Sync {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(flatten)]
        relays: RelayArgs,
        #[command(subcommand)]
        action: SyncAction,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Run the local drive-owner runtime: the mount's live loop and
    /// serving surface with no presentation session. The persistent
    /// process — `sync now` stays the bounded one-shot, this never
    /// stops syncing until SIGINT/SIGTERM. Needs at least one
    /// `--relay`; states what the device is doing, never what it
    /// guarantees. Bare `wyrd owner <drive>` starts the process;
    /// `wyrd owner <drive> status` queries it.
    Owner {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(flatten)]
        relays: RelayArgs,
        /// Optional log file, appended (never truncated): stderr
        /// always carries the ready line and failures for the
        /// supervisor to capture. Start-only: status never writes
        /// diagnostics.
        #[arg(long, value_name = "PATH")]
        log_file: Option<PathBuf>,
        /// Start (default) or query the owner. Relay and log-file
        /// flags are start-only; status takes neither, because it
        /// never constructs a mailbox.
        #[command(subcommand)]
        action: Option<OwnerAction>,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Device-local materialization, one namespace: what this
    /// device intends to retain (`pin` / `unpin` / `evict`), what it
    /// reports (`status` / `budgets`), and the posture baseline both
    /// report against (`role`). Materialization only — never fetches,
    /// never authors, never touches heads, never replicates. Offline
    /// by construction.
    Materialization {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(subcommand)]
        action: MaterializationAction,
    },
    /// Edit this drive's section of the per-user application
    /// settings file: one drive identity plus one sparse update,
    /// never a whole-document replacement. No keystore opens and no
    /// credentials load — the file is per-user state beside every
    /// drive directory, and the transaction (lock, strict read,
    /// commit guard) runs in this process whether the drive's owner
    /// runs or not. Restart-to-apply: a saved edit takes effect at
    /// the next owner start, never live.
    Settings {
        /// Drive directory whose settings section is edited. The
        /// section key is this exact spelling — absolute only, never
        /// resolved against the current directory, so the spelling
        /// the owner will look up is the spelling written. A
        /// relative path refuses before anything is read.
        drive_dir: PathBuf,
        #[command(subcommand)]
        action: SettingsAction,
    },
    /// Upgrade this installation from a local release archive:
    /// quiesce, flush, stop, swap the binary atomically, verify the
    /// new build against the actual store, and hand back to the
    /// supervisor (U1=B: the operator supplies the archive; U3=B:
    /// this command never supervises). Credential paths are
    /// forwarded to the verification child, never read here. Every
    /// terminal outcome records `<exe>.update-verdict` (done,
    /// rolled-back, or refused) beside the binary, so automation
    /// that cannot observe stdout still reads the result.
    Update {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        /// Local release archive under its canonical name
        /// (`wyrd-<version>-<os>-<arch>.tar.gz`): resolved and
        /// validated before anything is stopped.
        #[arg(value_name = "ARCHIVE")]
        archive: PathBuf,
        /// Control-plane relays, forwarded to the owner child under
        /// `--restart` only; refused otherwise.
        #[command(flatten)]
        relays: RelayArgs,
        /// After the update resolves, transfer into long-running
        /// supervision of the owner instead of handing back:
        /// this process runs the new owner as its child (or the
        /// restored owner after a rollback) and does not return.
        /// Exit status then reports the supervised run, not the
        /// update — the update's own outcome is the `update done:`
        /// / `update rollback:` / `update refused:` line plus the
        /// durable `<exe>.update-verdict` record. Needs at least
        /// one `--relay`.
        #[arg(long)]
        restart: bool,
        /// Pin the archive's blake3 digest out-of-band (the release
        /// record's hash, 64 hex characters): a mismatch refuses in
        /// resolve, before anything is stopped or staged, so a
        /// swapped archive fetched over a channel the operator does
        /// not fully control never reaches the transaction.
        /// Omitted for operator-built archives that never left the
        /// machine — the contract's local-archive trust (§4) covers
        /// those, and the pin covers everything else.
        #[arg(long, value_name = "HEX")]
        expect_blake3: Option<String>,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Verify a drive with this binary and report compatibility:
    /// the update transaction's authoritative check, run by the
    /// staged new binary against the actual store. Internal and
    /// unstable (double underscore): operators use `wyrd update`,
    /// never this directly. Opens the store, replays every commit
    /// through this build's readers, authorizes the live heads,
    /// restores epoch keys, and proves byte-identical durable
    /// state afterwards — then prints the U4 report and exits 0
    /// if and only if it is verified.
    #[command(name = "__update_verify", hide = true)]
    UpdateVerify {
        /// Directory holding the drive's keystore and object store.
        drive_dir: PathBuf,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Report this binary's compiled format versions, lock-free: app
    /// version, envelope version, control version. Internal and
    /// unstable (double underscore): the update preflight runs it
    /// on the staged binary and compares against its own. Three
    /// `update <key>: <value>` lines on stdout, exit 0, no store
    /// touched.
    #[command(name = "__build_info", hide = true)]
    BuildInfo,
}

/// One membership administration action. Only an owner authors
/// transitions; the engine enforces that, never the CLI.
#[derive(Debug, Subcommand)]
pub(crate) enum MemberAction {
    /// List members and owners at the canonical tip.
    List,
    /// Show the membership log: every observed transition with its
    /// canonical status.
    Log,
    /// Show membership status: known tip, frozen conflicts, and held
    /// epoch secrets (knowledge is not possession).
    Status,
    /// Remove a device (owner-only). Removing the sole owner is valid
    /// but terminal and requires `--yes`.
    Remove {
        /// Device to remove, 64 hex characters.
        device: String,
        /// Confirm a last-owner removal.
        #[arg(long)]
        yes: bool,
    },
    /// Leave the drive: end your own participation where the
    /// trust contract admits it, and be told precisely what to ask
    /// the owner for where it does not. A non-owner authors nothing
    /// and is told the owner must remove it; the sole owner authors
    /// `Remove(self)` with the terminal warning and needs `--yes`.
    Leave {
        /// Confirm a sole-owner leave (the drive freezes permanently).
        #[arg(long)]
        yes: bool,
    },
    /// Force a fresh epoch secret (owner-only). Membership unchanged.
    Rotate,
    /// Hand ownership to a member (owner-only). v0 ownership is a
    /// singleton.
    SetOwner {
        /// The new owner, 64 hex characters.
        device: String,
    },
    /// Resolve a frozen membership conflict (owner-only): name the
    /// winning tip and exactly the voided siblings. The engine
    /// proves the closed resolution (all ids live contenders at the
    /// frozen epoch, void set exactly the winner's rivals,
    /// pre-transition owner authority) before authoring; anything
    /// less fails closed with no transition. The carry obligations
    /// staged ahead of authoring still commit on a refusal — benign:
    /// the next drain discharges them without authoring while the
    /// heads stay eligible.
    Resolve {
        /// Winning tip, 64 hex characters.
        winner: String,
        /// Voided sibling, repeatable, 64 hex characters each.
        #[arg(long = "void")]
        voided: Vec<String>,
    },
    /// Admit a device (owner-only) and write its sealed invitation to
    /// a file for out-of-band delivery. The transition commits with
    /// the usual catch-up obligations; the newcomer joins from the
    /// invitation file. With `--reader` the device joins read-only:
    /// it holds every epoch secret but authors nothing.
    Invite {
        /// Device to admit, 64 hex characters.
        device: String,
        /// Its encryption key, 64 hex characters (from its
        /// pairing-request output).
        encryption_key: String,
        /// Where to write the sealed invitation.
        out: PathBuf,
        /// Admit as a reader instead of a member.
        #[arg(long)]
        reader: bool,
    },
    /// Reissue a device's sealed invitation from durable state, for
    /// an admission whose invitation never reached a file. Authors
    /// nothing; the reseal opens identically, with fresh randomness.
    Reissue {
        /// Device whose invitation to reissue, 64 hex characters.
        device: String,
        /// Where to write the sealed invitation.
        out: PathBuf,
    },
}

/// One snapshot inspection or merge action. Head references are
/// `@N` over the selected heads in ascending SnapshotId order —
/// head-wise, not the mount's per-path `name@N` numbering (which
/// skips heads that lack the path), and relative to the selection
/// when `--head` narrows it.
#[derive(Debug, Subcommand)]
pub(crate) enum SnapshotAction {
    /// Classify every DAG head: eligible, superseded, stranded,
    /// voided, pending, or rejected. Eligible heads carry their
    /// `@N` merge numbers with author, tree, and parent count, so
    /// this is also the live-head listing; every other class is
    /// retained history with its reason attached and no number.
    Heads,
    /// Report the divergence screen from live engine state: a
    /// diverged drive holds competing histories (more than one
    /// eligible head). The standing indicator over the eligible
    /// heads, the freeze half
    /// with its remedy, and — only on a diverged drive — the bounded
    /// what-differs comparison over the real merge plan. Read-only:
    /// opens the keystore and the object store, authors nothing, and
    /// needs the drive un-mounted like every other snapshot reader.
    Diverged {
        /// Root-path budget for the on-demand comparison: the
        /// differing set past it renders as explicitly incomplete,
        /// never as the complete set.
        #[arg(long, default_value_t = wyrd_core::conflict_host::DEFAULT_INSPECTION_BUDGET)]
        budget: usize,
    },
    /// Merge source heads into one snapshot. Sources default to all
    /// live heads; `--head` narrows to an explicit subset (at least
    /// two). Conflicted root paths take `--take path=@N`, drop with
    /// `--drop path`, or fall back to `--default @N`; paths every
    /// source agrees on take themselves. The merged snapshot parents
    /// onto exactly the selected heads at the current epoch. A
    /// short selection with no eligible heads on a
    /// membership-frozen drive is refused with a pointer to `member
    /// resolve`; a pre-conflict fork still merges.
    Merge {
        /// Source head, 64 hex characters. Repeatable; omitted means
        /// all live heads.
        #[arg(long = "head")]
        heads: Vec<String>,
        /// Default source for conflicted paths without a `--take`
        /// line, `@N` over the selected heads.
        #[arg(long)]
        default: Option<String>,
        /// Take a conflicted root path from one source,
        /// `path=@N`. Repeatable, one line per path.
        #[arg(long = "take")]
        takes: Vec<String>,
        /// Drop a conflicted root path from the merge.
        /// Repeatable.
        #[arg(long = "drop")]
        drops: Vec<String>,
        /// Preview the merge without authoring: the plan rows over
        /// the same `@N` basis plus the spec verdict. Runs every
        /// pre-commit check the merge runs, minus the authoring.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Recover stranded bytes into a recovery-flagged snapshot:
    /// republish selected content from a dead fork under the
    /// current epoch without adopting its lineage. `--dry-run`
    /// shows the plan rows plus the would-graft set before
    /// anything commits; without it the selection grafts.
    Recover {
        /// Source snapshot, 64 hex characters: the stranded fork
        /// whose bytes are grafted.
        #[arg(long = "from")]
        from: String,
        /// Source root path to graft. Repeatable, one path per
        /// line; root-entry granularity like merge.
        #[arg(long = "take")]
        takes: Vec<String>,
        /// Graft the whole source tree instead of `--take` lines.
        #[arg(long = "all")]
        all: bool,
        /// Explicit content id to graft under its hex name, for
        /// content whose path the operator no longer knows.
        /// Repeatable.
        #[arg(long = "content")]
        contents: Vec<String>,
        /// Preview the graft without authoring. Reads need no
        /// ownership; the commit still refuses non-owners.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
}

/// One local-device pairing action. Pairing and join are offline file
/// exchanges; the drive directory holds the staged secret and (after
/// join) the member custody record.
#[derive(Debug, Subcommand)]
pub(crate) enum DeviceAction {
    /// Identify this device: its id plus the encryption key the
    /// membership state registers for it (`unregistered` until the
    /// device's admission arrives — a fresh join only holds genesis).
    /// Also the cheapest reopen probe: it opens the keystore.
    Id,
    /// Show what keys this device holds: its id, the registered
    /// encryption key, and the held epoch secrets — plus where the
    /// secrets live, that the device key cannot be rotated, and the
    /// owner-mediated replacement sequence. Prints an unlocking
    /// notice first: opening the keystore derives keys and shows no
    /// progress.
    Keys,
    /// Stage this device's pairing secret and write the public
    /// pairing material (device plus encryption key, no secrets) for
    /// the owner. Re-running returns the same key.
    PairingRequest {
        /// Where to write the pairing material.
        out: PathBuf,
    },
    /// Join a drive from the owner's sealed invitation. Writes member
    /// custody before accepting, so the device reopens afterwards.
    Join {
        /// The sealed invitation file.
        invitation: PathBuf,
    },
}

/// One first-run identity action: generate or verify the device's
/// credential files. Generation is the onboarding commit source;
/// verification is the Repair-state and read-back probe.
#[derive(Debug, Subcommand)]
pub(crate) enum IdentityAction {
    /// Generate this device's credential files: a fresh identity
    /// secret plus a generated passphrase. Writes both files
    /// (owner-only, never overwrite) and prints the passphrase on
    /// stdout — the single line the desktop displays for write-down.
    /// Single-shot (manual/CLI use); the desktop flow uses
    /// `new-passphrase` + `commit` to preserve its commit boundary.
    Generate {
        /// Where to write the identity secret (32 raw bytes).
        #[arg(long, value_name = "PATH")]
        identity_file: PathBuf,
        /// Where to write the generated passphrase (UTF-8 text).
        #[arg(long, value_name = "PATH")]
        passphrase_file: PathBuf,
    },
    /// Mint a fresh generated passphrase and print it on stdout.
    /// Writes nothing: the desktop displays it for write-down and
    /// holds it in memory until `commit`. Single line, no files.
    NewPassphrase,
    /// Commit both credential files: generate the identity secret and
    /// write it plus the passphrase (read from stdin, one line) to
    /// their paths (owner-only, never overwrite). The identity secret
    /// never leaves this process; only the passphrase transits, via
    /// the stdin pipe (never argv). Stdin is the onboarding-only
    /// exception to paths-only: display requires the text, commit
    /// requires the bytes, and argv must never carry either.
    Commit {
        /// Where to write the identity secret (32 raw bytes).
        #[arg(long, value_name = "PATH")]
        identity_file: PathBuf,
        /// Where to write the passphrase (UTF-8 text).
        #[arg(long, value_name = "PATH")]
        passphrase_file: PathBuf,
    },
    /// Verify both credential files through the hardened reader
    /// (owner-only, no symlinks, identity format, UTF-8 passphrase).
    Verify {
        /// Identity file to check.
        #[arg(long, value_name = "PATH")]
        identity_file: PathBuf,
        /// Passphrase file to check.
        #[arg(long, value_name = "PATH")]
        passphrase_file: PathBuf,
    },
}

/// One local-drive lifecycle action: create, provision, delete,
/// reconfigure. `init` creates identity, root custody, genesis
/// membership; `bootstrap` provisions an existing or new drive and
/// hands it to OS supervision; `delete` authors your own removal then
/// removes the local directory; `reconfigure` changes supervision in
/// place. Only an owner deletes.
#[derive(Debug, Subcommand)]
pub(crate) enum DriveAction {
    /// Create a new drive: identity, root custody, genesis membership.
    /// Idempotent only in the sense that re-running refuses — init
    /// never merges into an existing drive.
    Init,
    /// Provision a drive from nothing and hand it to OS supervision:
    /// initialize, register in the per-user registry, install and
    /// enable the per-drive service, and verify the owner socket.
    /// Ready is reported only when the socket answers this session.
    Bootstrap {
        /// Registration name: 1-58 chars of [a-z0-9-]. Names the
        /// registry entry and derives the `drive-<name>` service name,
        /// so 59-64 chars would overflow the 64-char service token.
        #[arg(long)]
        name: String,
        /// Target service mode: `vault` serves headless, `mount`
        /// serves the FUSE projection at `--mountpoint`. `vault` here
        /// names the supervised mode, not the membership role — the
        /// mode value is stable for existing registrations.
        #[arg(long, value_name = "MODE", default_value = "vault")]
        mode: String,
        /// FUSE mountpoint. Required in `mount` mode, refused in
        /// `vault` mode.
        #[arg(long, value_name = "PATH")]
        mountpoint: Option<PathBuf>,
        #[command(flatten)]
        relays: RelayArgs,
        /// Join as an invited device instead of creating: the sealed
        /// invitation file the inviter's `member invite` wrote.
        #[arg(long, value_name = "PATH")]
        invitation_file: Option<PathBuf>,
        /// Registry file elsewhere than the platform default.
        #[arg(long, value_name = "PATH")]
        registry: Option<PathBuf>,
        /// Owner binary for the service definition elsewhere than
        /// the `wyrd` beside this command.
        #[arg(long, value_name = "PATH")]
        owner_bin: Option<PathBuf>,
        /// `wyrd-supervise` wrapper for the service definition
        /// elsewhere than beside the `wyrd` binary.
        #[arg(long, value_name = "PATH")]
        supervise_bin: Option<PathBuf>,
        /// Register without installing or enabling any OS service:
        /// the owner stays supervised by whoever starts it.
        #[arg(long)]
        no_service: bool,
    },
    /// Delete this drive locally: author your own removal
    /// (owner-only, terminal — the owner set empties) and remove the
    /// local directory. Needs `--yes`. A non-owner is refused and a
    /// frozen drive resolves first, both before the directory is
    /// touched. No other member's copy changes; the removed files'
    /// blocks stay recoverable from this device until overwritten.
    Delete {
        /// Confirm the terminal removal (the drive freezes permanently).
        #[arg(long)]
        yes: bool,
    },
    /// Reconfigure this registered drive's supervision in place:
    /// stop and clear the old service definition, rewrite the
    /// registry entry, reinstall and restart when managed, and verify
    /// the owner socket. The entry is resolved by this drive's
    /// directory (exact match, never a guess across duplicates).
    /// `vault` serves headless, `mount` serves the FUSE projection at
    /// `--mountpoint`; any other mode refuses with the unknown-mode
    /// pointer before anything stops, like provisioning. With
    /// `--relay` flags the relay set is replaced; without them the
    /// registered set is kept, so a credential-path-only reconfigure
    /// never clears relays by accident. A live owner holding the drive
    /// refuses with the store lock: stop the service first, the same
    /// rule every other offline writer states.
    Reconfigure {
        /// Target service mode: `vault` or `mount`. Any other value
        /// refuses with the unknown-mode pointer.
        #[arg(long, value_name = "MODE")]
        mode: String,
        /// FUSE mountpoint. Required in `mount` mode, refused in
        /// `vault` mode.
        #[arg(long, value_name = "PATH")]
        mountpoint: Option<PathBuf>,
        /// Control-plane relay; repeatable. When at least one is
        /// given the registered set is replaced; with none given the
        /// registered set is kept.
        #[arg(long, value_name = "URL")]
        relay: Vec<String>,
        /// Registry file elsewhere than the platform default.
        #[arg(long, value_name = "PATH")]
        registry: Option<PathBuf>,
        /// Owner binary for the service definition elsewhere than
        /// the `wyrd` beside this command. The service runs
        /// `wyrd owner start` or `wyrd mount` with the same credential files.
        #[arg(long, value_name = "PATH")]
        owner_bin: Option<PathBuf>,
        /// `wyrd-supervise` wrapper for the service definition
        /// elsewhere than beside the `wyrd` binary. The restart
        /// policy rests on the wrapper's wait-status mapping, so a
        /// managed service without it is refused.
        #[arg(long, value_name = "PATH")]
        supervise_bin: Option<PathBuf>,
        /// Rewrite the entry without installing or enabling any OS
        /// service: the old definition is disabled and cleared, and
        /// the owner stays supervised by whoever starts it.
        #[arg(long)]
        no_service: bool,
    },
    /// Start this registered drive's installed service without
    /// rewriting anything: enable and start the existing definition
    /// and verify the socket. The recovery for "installed but
    /// stopped" — explicit, never automatic. A missing definition
    /// refuses with the reinstall pointer. Against a live drive this
    /// restarts the owner by design (the enable/start primitives
    /// restart), so offer it only for stopped and wedged drives.
    ServiceStart {
        /// Registry file elsewhere than the platform default.
        #[arg(long, value_name = "PATH")]
        registry: Option<PathBuf>,
    },
    /// Unlock this drive for the session: start the persistent
    /// owner in the foreground with a passphrase from the
    /// terminal prompt (no echo) or `--passphrase-stdin`, never
    /// from a file. The identity secret still comes from its file;
    /// only the passphrase rides the prompt or the pipe. Nothing
    /// is installed, enabled, or rewritten — the manager learns
    /// nothing, so a reboot or an explicit stop locks the drive
    /// again. Against a live drive this refuses with the store
    /// lock like every other offline opener.
    Unlock {
        #[command(flatten)]
        relays: RelayArgs,
        /// Per-device retained-bytes ceiling in bytes: the same
        /// number `owner start` enforces. Unset (the default)
        /// enforces no ceiling.
        #[arg(long, value_name = "BYTES")]
        retained_quota: Option<u64>,
    },
}

/// One owner-runtime action. `Start` runs the persistent owner
/// process; `Status` is the operator query: process posture from the
/// sidecar (never the store) beside durable state when the drive is
/// stopped. The two sections stay separate — a fresh posture is not
/// convergence, and durable state is unavailable (not guessed) while
/// the owner holds the lock. The owner runtime is not a membership
/// role: starting or inspecting it requires no Owner membership, and
/// grants none.
#[derive(Debug, Subcommand)]
pub(crate) enum OwnerAction {
    /// Start the persistent owner process. The default when no
    /// subcommand is given, so the shipped `wyrd owner <drive>`
    /// keeps starting.
    Start {
        /// Per-device retained-bytes ceiling in bytes: the same
        /// number bounds local authoring and receive-path admission
        /// (see `mount --retained-quota`). Unset (the default)
        /// enforces no ceiling.
        #[arg(long, value_name = "BYTES")]
        retained_quota: Option<u64>,
    },
    /// Report the running process's posture and, when the drive is
    /// stopped, its durable state. Never opens the store for the
    /// process section; never claims liveness.
    Status,
}

/// One headless sync action. Status observes durable state only;
/// `now` runs the same sync machinery the mount runs, headless.
#[derive(Debug, Subcommand)]
pub(crate) enum SyncAction {
    /// Show pending outbox obligations (queued, relay-accepted, pending
    /// per snapshot, transition, and epoch), the membership tip
    /// against held epoch secrets, live heads with classification,
    /// and mailbox posture. Never connects, sends, or touches the
    /// seen log or outbox; needs the drive un-mounted.
    Status,
    /// Run bounded sync passes headless: drain, deliver, announce,
    /// fetch — no FUSE session, same live budgets as mount. Stops on
    /// the first quiet pass; the pass limit reports incomplete. A
    /// relay-less run exits 0 having contacted nothing, which reads
    /// as converged to automation — so without `--relay` the command
    /// refuses unless `--offline` explicitly opts into the local run.
    /// Reports per-class fetch diagnostics (client-position counts,
    /// never identities) beside the unfulfilled total.
    Now {
        /// Run without relays: intake stays idle, only local
        /// obligations discharge, nothing new is fetched. Cannot be
        /// combined with --relay.
        #[arg(long)]
        offline: bool,
        /// Bind a serving endpoint for the run (OD-23-V option A):
        /// the headless composition opens the mount's serving
        /// surface, flushes it before announcing, and gates
        /// announcement discharge on its readiness — so peers can
        /// fetch what this run announces. A bridge/compatibility
        /// composition, not a server: the bounded drain keeps its
        /// verdict and exit code, then the endpoint serves the
        /// converged snapshot until SIGINT/SIGTERM and shuts down in
        /// order. Cannot be combined with --offline (no relay means
        /// no route publication, so nothing could discover it).
        #[arg(long)]
        serve: bool,
        /// Per-device retained-bytes ceiling in bytes: the same
        /// number bounds local authoring and receive-path admission
        /// (see `mount --retained-quota`). Unset (the default)
        /// enforces no ceiling.
        #[arg(long, value_name = "BYTES")]
        retained_quota: Option<u64>,
    },
}

/// One materialization-namespace action. Credentials flatten per action, not
/// on the parent, so `materialization role set` alone keeps taking neither flag: a
/// role is settable on a machine whose keystore is unavailable
/// (OD-24-R), and a flag the command never reads is a flag that can
/// silently rot.
#[derive(Debug, Subcommand)]
pub(crate) enum MaterializationAction {
    /// Show every reachable file under `path` with its retention
    /// policy (PINNED / REMOTE_ONLY — what this device intends to
    /// retain) against physical presence (PRESENT / ABSENT — what
    /// bytes happen to exist locally), under the role posture it is
    /// measured against: one report answers policy, presence, and
    /// baseline. The two columns are independent: REMOTE_ONLY +
    /// PRESENT is ordinary (eviction releases intent, never deletes
    /// bytes), and only all-chunks PINNED + PRESENT reads with a
    /// retention promise. Reads durable state only: never connects,
    /// never mutates.
    Status {
        /// Drive path to report (drive-relative; empty means the
        /// whole drive).
        #[arg(default_value = "")]
        path: String,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Show device totals over reachable content (pinned files and
    /// chunks, quadrant counts) plus the effective retention and
    /// fetch budgets. Policy facts are per content identity, not per
    /// path, so pinned paths are not listed — only what they amount
    /// to. Reads durable state only: never connects, never mutates.
    Budgets {
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Device-local materialization role: what this device intends to
    /// be (`mirror`: prunable participant; `vault`: replicates every
    /// object, never prunes). A role is retention posture, never
    /// replicated state — setting one writes one local file, no fact,
    /// no snapshot. It is not the `vault` process: a device can hold
    /// the role without running one, and one can run without the
    /// role. Setting needs no credentials, so a role is settable on
    /// a machine whose keystore is unavailable; only status opens
    /// the node.
    Role {
        #[command(subcommand)]
        action: RoleAction,
    },
    /// Promise retention for a subtree: every byte under `path` is
    /// pinned durably on this device. Never fetches, never authors,
    /// never touches heads. Offline by construction.
    Pin {
        /// Drive path to pin (drive-relative; empty means the root).
        path: String,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Release retention promises under `path`: pinned content
    /// returns to cacheable policy. Never deletes bytes, never
    /// touches heads. Promises are per content identity, so
    /// unpinning a subtree also releases shared chunks other paths
    /// relied on. Offline by construction.
    Unpin {
        /// Drive path to unpin (drive-relative; empty means the root).
        path: String,
        #[command(flatten)]
        credentials: Credentials,
    },
    /// Return unpinned content under `path` to REMOTE_ONLY policy.
    /// Refuses the whole subtree while any of it is pinned (unpin
    /// first), and refuses under an effective `vault` role. Changes
    /// intent only: no bytes are deleted, files that stay fully
    /// local keep reading, heads are untouched. Offline by
    /// construction.
    Evict {
        /// Drive path to evict (drive-relative; empty means the root).
        path: String,
        #[command(flatten)]
        credentials: Credentials,
    },
}

/// One role action. `set` names the posture and reads no key;
/// `status` reports the configured role, any in-flight narrowing,
/// and the effective retention posture — the same numbers `policy
/// status` reports against, because there is one effective policy,
/// not two.
#[derive(Debug, Subcommand)]
pub(crate) enum RoleAction {
    /// Set this device's materialization role, durably and offline.
    /// Widening toward `vault` takes effect immediately. Narrowing
    /// away from it requires `--yes` and records an acknowledged
    /// transition: effective posture stays `vault` until withdrawal
    /// completes, and completion is reported, never assumed.
    Set {
        /// The role to hold: `mirror` or `vault`.
        role: String,
        /// Acknowledge a narrowing (leaving `vault`). Intent only:
        /// withdrawal still has to complete before the posture flips.
        #[arg(long)]
        yes: bool,
    },
    /// Report the configured role, transition, and effective
    /// retention posture. Reads durable state only; never connects,
    /// never mutates.
    Status {
        #[command(flatten)]
        credentials: Credentials,
    },
}

/// One settings-file action. `apply` is the only writer: one drive
/// plus one sparse update through the shared transaction, whether
/// the drive's owner runs or not. No credentials anywhere on this
/// family — the file is per-user state, never keystore state — so a
/// save works on a machine whose keystore is unavailable, exactly
/// like `materialization role set`.
#[derive(Debug, Clone, Subcommand)]
pub(crate) enum SettingsAction {
    /// Apply one sparse update to this drive's settings section:
    /// set the listed bounds, clear the listed ones, one atomic
    /// commit. Unknown keys, contradictory set+clear pairs, and
    /// absurd values refuse before anything is read; a file the
    /// build cannot parse refuses with its bytes preserved; a
    /// concurrent external rewrite is a conflict (exit 3), never a
    /// silent overwrite. Saved edits apply at the next owner start.
    Apply {
        /// Bounds to set as `KEY=VALUE` (repeatable): the five
        /// editable keys with whole-number values, e.g.
        /// `--set max_admit_per_pass=64`.
        #[arg(long, value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Bounds to clear back to the compiled default (repeatable),
        /// e.g. `--clear open_timeout_secs`.
        #[arg(long, value_name = "KEY")]
        clear: Vec<String>,
        /// Settings file elsewhere than the per-user default (tests
        /// and non-standard layouts).
        #[arg(long, value_name = "PATH")]
        settings_path: Option<PathBuf>,
    },
}
