//! Nix distribution guards: the flake builds the user-facing
//! binary from `wyrd-cli`, and the Nix module declares exactly
//! one owner per drive.

use crate::contract_source::*;
use std::fs;

/// Contract 34 (packaging half): the distribution build compiles the
/// user-facing binary from `wyrd-cli`. The Phase 4 move broke this
/// once (the flake built `-p wyrd-daemon`, which installs no binary,
/// and `wyrd-dist` failed packing `bin/wyrd`), so it is asserted, not
/// remembered. A structural guard over the flake text — not a
/// substitute for a release `nix build` — so it matches every
/// `cargoExtraArgs` line rather than trusting line layout, and fails
/// closed if the flag ever names another package.
#[test]
fn flake_builds_the_binary_from_wyrd_cli() {
    let text =
        fs::read_to_string(workspace_root().join("flake.nix")).expect("flake.nix is readable");
    let args: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("cargoExtraArgs"))
        .collect();
    assert!(
        !args.is_empty(),
        "flake names no binary source package at all"
    );
    assert!(
        args.iter().any(|line| line.contains("-p wyrd-cli")),
        "distribution must build the binary from wyrd-cli, found: {args:?}"
    );
    assert!(
        !args.iter().any(|line| line.contains("-p wyrd-daemon")),
        "distribution must not build the binary from the library-only daemon: {args:?}"
    );
}

/// Service supervision (34): one declaration names exactly one
/// owner per drive, and the supervisor contract distinguishes the
/// terminal lock code from restart-worthy ends on both platforms.
/// A structural pin over the module sources, the wrapper, and the
/// probe — the evaluation tests in `tests/harness/test-wyrd-module.sh`
/// prove the generated units; this proves the sources keep the
/// load-bearing clauses. Most load-bearing: the wrapper's terminal
/// code and the CLI's drive-locked code are the same number, read
/// from both files, so neither side can drift alone.
#[test]
fn the_nix_module_declares_exactly_one_owner_per_drive() {
    let root = workspace_root();
    let options = fs::read_to_string(root.join("nix/drive/options.nix")).expect("options readable");
    let nixos =
        fs::read_to_string(root.join("nix/drive/nixos.nix")).expect("nixos module readable");
    let darwin =
        fs::read_to_string(root.join("nix/drive/darwin.nix")).expect("darwin module readable");
    let wrapper =
        fs::read_to_string(root.join("nix/drive/supervise.py")).expect("wrapper readable");
    let probe = fs::read_to_string(root.join("nix/drive/probe.py")).expect("probe readable");
    let cli = fs::read_to_string(root.join("crates/wyrd-cli/src/main.rs")).expect("cli readable");

    // One mode per declaration is structural (an enum); two
    // declarations for one drive are refused at eval with the drive
    // path in the message, and secrets never enter the store.
    for marker in [
        "One owner process per drive directory",
        "duplicateDirs",
        "Secrets are paths, never store strings",
        "storeSecrets",
        "mountpoint belongs to mount mode",
        "belongs to vault mode",
    ] {
        assert!(
            options.contains(marker),
            "the options module lost {marker:?}"
        );
    }
    // The terminal code is one number on both sides of the boundary.
    let cli_code = cli
        .lines()
        .find_map(|line| {
            let rest = line
                .trim()
                .strip_prefix("pub const EXIT_DRIVE_LOCKED: i32 = ")?;
            rest.strip_suffix(';')
        })
        .expect("the CLI names its drive-locked code");
    let wrap_code = wrapper
        .lines()
        .find_map(|line| line.trim().strip_prefix("DRIVE_LOCKED_CODE = "))
        .expect("the wrapper names its terminal code");
    assert_eq!(
        cli_code, wrap_code,
        "the wrapper's terminal code must be the CLI's drive-locked code"
    );
    assert!(
        wrapper.contains("WIFEXITED") && wrapper.contains("WIFSIGNALED"),
        "the wrapper maps the wait status, not the exit code alone"
    );
    // systemd restarts failures rate-limited, never the lock code;
    // the probe refuses the switch on a foreign holder.
    for marker in [
        "RestartPreventExitStatus",
        "78",
        "Restart = \"on-failure\"",
        "startLimitBurst",
        "wyrd-drive-probe",
        "--restart-status=1",
        "MainPID",
    ] {
        assert!(nixos.contains(marker), "the NixOS unit lost {marker:?}");
    }
    // launchd restarts on zero, never on the terminal code; the
    // probe compares against the daemon PID; readiness stays wired
    // to the posture path.
    for marker in [
        "SuccessfulExit",
        "ThrottleInterval",
        "--restart-status=0",
        "launchctl list",
        "--posture-path=",
        "wyrd-drive-probe",
    ] {
        assert!(darwin.contains(marker), "the darwin daemon lost {marker:?}");
    }
    // The probe takes the lock non-blocking and refuses with the
    // drive and the holder facts when a foreign process holds it,
    // while its own previous generation rolls the switch.
    assert!(
        probe.contains("LOCK_EX") && probe.contains("LOCK_NB"),
        "the probe takes the lock non-blocking"
    );
    assert!(
        probe.contains("return 1"),
        "a foreign holder refuses the switch"
    );
    assert!(
        probe.contains("previous generation"),
        "the probe rolls the service's own previous generation"
    );

    // The freshness bound is one number in three spellings (finding
    // 1): the two Python mirrors must agree with each other, and the
    // Rust side must keep the three-interval relationship the
    // mirrors document. A Python script cannot name a Rust
    // constant, so the test pins the literals instead of sharing
    // them.
    let wrap_fresh = wrapper
        .lines()
        .find_map(|line| line.trim().strip_prefix("POSTURE_FRESH_SECS = "))
        .expect("the wrapper names its freshness bound");
    let probe_fresh = probe
        .lines()
        .find_map(|line| line.trim().strip_prefix("POSTURE_FRESH_SECS = "))
        .expect("the probe names its freshness bound");
    assert_eq!(
        wrap_fresh, probe_fresh,
        "supervisor and probe must share one freshness bound"
    );
    let daemon = fs::read_to_string(root.join("crates/wyrd-daemon/src/posture.rs"))
        .expect("posture readable");
    assert!(
        daemon.contains("POSTURE_INTERVAL.as_secs() * 3"),
        "the Rust threshold stays three posture intervals"
    );

    // The sidecar filenames are one vocabulary across the boundary
    // (finding 3): the daemon's constants and the module's paths
    // must name the same files.
    for name in ["vault.posture", "mount.posture"] {
        assert!(daemon.contains(name), "the daemon defines {name}");
        assert!(
            nixos.contains(name) && darwin.contains(name),
            "both supervisors reference {name}"
        );
    }
}
