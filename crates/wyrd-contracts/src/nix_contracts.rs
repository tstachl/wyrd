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

/// The Swift import surface the adapter may use: platform
/// frameworks and the socket client only. This pins the import
/// surface, not file or API access — a future rule owns shapes like
/// `Process` or drive-path construction. `UniformTypeIdentifiers`
/// joined in Stage 1: item content types require it, and it is the
/// same trust class as Foundation.
fn allowed_swift_import(module: &str) -> bool {
    [
        "Foundation",
        "FileProvider",
        "UniformTypeIdentifiers",
        "Darwin",
        "os",
    ]
    .contains(&module)
}

/// Import violations in an explicit file list: path to offending
/// module. Pure so the negative tests prove the checker bites
/// without touching the filesystem. Import lines fail closed:
/// attribute prefixes (`@testable`, `@_exported`), tab separators,
/// and semicolon-joined imports all resolve; a line that starts an
/// import and resolves to no allowed shape is a violation, never a
/// silent skip.
fn check_swift_imports(files: &[(&str, &str)]) -> Vec<String> {
    let mut violations = Vec::new();
    for (path, text) in files {
        for line in text.lines().map(str::trim_start) {
            for segment in line.split(';') {
                if let Some(module) = import_module(segment.trim_start()) {
                    if module.is_empty() || !allowed_swift_import(module) {
                        violations.push(format!("{path} imports {module}"));
                    }
                }
            }
        }
    }
    violations
}

/// The module an import segment brings in, or `None` when the
/// segment is not an import at all. Access-level modifiers
/// (`public`, `package`, `internal`, `fileprivate`, `private`,
/// `open`) and attribute prefixes (`@testable`, `@_exported`) count:
/// anything starting an import that does not resolve is the
/// caller's violation, never a skip.
fn import_module(segment: &str) -> Option<&str> {
    let words: Vec<&str> = segment.split_whitespace().collect();
    let mut words = words.as_slice();
    if let Some((&first, rest)) = words.split_first() {
        if [
            "public",
            "package",
            "internal",
            "fileprivate",
            "private",
            "open",
        ]
        .contains(&first)
        {
            words = rest;
        }
    }
    let (head, tail) = (words.first()?, &words[1..]);
    let after_import = if *head == "import" || *head == "@import" {
        tail
    } else if head.starts_with('@') && tail.first().is_some_and(|w| *w == "import") {
        &tail[1..]
    } else {
        return None;
    };
    Some(
        after_import
            .first()
            .copied()
            .unwrap_or("")
            .trim_end_matches(['?', '!', '(']),
    )
}

/// The macOS File Provider adapter stays a dumb owner-client: its
/// Swift sources import platform frameworks and the socket client
/// only — never store, engine, or sync internals. The adapter is not
/// a Cargo member, so this scan (not a `MemberPolicy` allow-list) is
/// the executable form of the OD-8 boundary.
#[test]
fn fileprovider_swift_stays_within_the_owner_boundary() {
    let root = workspace_root();
    let (files, errors) = collect_files(
        &root.join("packaging/macos/fileprovider/Sources"),
        &root,
        "swift",
    );
    assert!(
        errors.is_empty(),
        "source walk errors:\n- {}",
        errors.join("\n- ")
    );
    // Never pass vacuously: an empty file set would check nothing.
    assert!(!files.is_empty(), "the adapter holds Swift sources");
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let violations = check_swift_imports(&borrowed);
    assert!(
        violations.is_empty(),
        "owner-boundary violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Stage 3 sub-PR 1 widens the Swift adapter's vocabulary to the
/// owner mutation calls it must speak: the two mutation-side
/// operations and the identity precondition every entry-addressed
/// kind carries (the adapter never sends a mutation in this
/// sub-PR — no UI path constructs one — but the wire proof needs
/// the shapes). The boundary still forbids store, engine, sync and
/// credential vocabulary. The Harness proves the framing against
/// a stub owner; this pins the surface against drift.
#[test]
fn fileprovider_mutation_vocabulary_stays_owner_shaped() {
    let root = workspace_root();
    let (files, errors) = collect_files(
        &root.join("packaging/macos/fileprovider/Sources"),
        &root,
        "swift",
    );
    assert!(
        errors.is_empty(),
        "source walk errors:\n- {}",
        errors.join("\n- ")
    );
    assert!(!files.is_empty(), "the adapter holds Swift sources");
    let text = files
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for required in [
        "\"lookup\"",
        "\"stat\"",
        "\"readdir\"",
        "\"read\"",
        "\"submit\"",
        "\"capture-parent\"",
        "expected_identity",
        "wyrd:v1:",
        "wyrd:legacy:v1:",
        "wyrd-anchor-v1:",
        "by-drive",
        "destinationOfSymbolicLink",
    ] {
        assert!(text.contains(required), "the adapter names {required}");
    }
    for forbidden in [
        "DurableStore",
        "Mailbox",
        "Nostr",
        "nostr",
        "Engine",
        "Vault",
        "CryptoKit",
        "BLAKE3",
        "blake3",
        "Tauri",
        "tauri",
        "Keychain",
        "NSXPC",
    ] {
        assert!(
            !text.contains(forbidden),
            "the adapter must not name {forbidden}"
        );
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn nested_source_is_discovered_and_checked() {
        struct ScratchDir(PathBuf);
        impl Drop for ScratchDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = ScratchDir(std::env::temp_dir().join(format!(
            "wyrd-swift-walk-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let nested = root.0.join("Sources").join("Nested");
        fs::create_dir_all(&nested).expect("scratch nested dir is creatable");
        fs::write(nested.join("Bad.swift"), "import WyrdSync\n").expect("scratch file is writable");
        let (files, errors) = collect_files(&root.0.join("Sources"), &root.0, "swift");
        assert!(
            errors.is_empty(),
            "scratch walk errors: {}",
            errors.join("; ")
        );
        assert_eq!(files.len(), 1, "the nested source is discovered");
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        assert_eq!(check_swift_imports(&borrowed).len(), 1);
    }

    #[test]
    fn disallowed_import_is_rejected() {
        let files = [("Sources/Bad.swift", "import WyrdSync\n")];
        let violations = check_swift_imports(&files);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("WyrdSync"));
    }

    #[test]
    fn objc_import_fails_closed() {
        let files = [("Sources/Bad.swift", "@import WyrdSync;\n")];
        assert_eq!(check_swift_imports(&files).len(), 1);
    }

    #[test]
    fn attribute_prefixed_import_fails_closed() {
        let files = [("Sources/Bad.swift", "@testable import WyrdSync\n")];
        assert_eq!(check_swift_imports(&files).len(), 1);
        let files = [("Sources/Bad.swift", "@_exported import WyrdSync\n")];
        assert_eq!(check_swift_imports(&files).len(), 1);
    }

    #[test]
    fn access_modifier_import_fails_closed() {
        for modifier in ["public", "package", "internal", "fileprivate", "private"] {
            let source = format!("{modifier} import WyrdSync\n");
            let files = [("Sources/Bad.swift", source.as_str())];
            assert_eq!(check_swift_imports(&files).len(), 1, "{modifier}");
        }
    }

    #[test]
    fn tab_separated_import_fails_closed() {
        let files = [("Sources/Bad.swift", "import\tWyrdSync\n")];
        assert_eq!(check_swift_imports(&files).len(), 1);
    }

    #[test]
    fn second_import_on_one_line_is_checked() {
        let files = [("Sources/Bad.swift", "import Foundation; import WyrdSync\n")];
        assert_eq!(check_swift_imports(&files).len(), 1);
    }

    #[test]
    fn qualified_import_fails_closed() {
        let files = [("Sources/Bad.swift", "import struct Foundation.URL\n")];
        assert_eq!(check_swift_imports(&files).len(), 1);
    }

    #[test]
    fn allowed_imports_pass() {
        let files = [(
            "Sources/Good.swift",
            "import Foundation\nimport FileProvider\nimport os\n",
        )];
        assert!(check_swift_imports(&files).is_empty());
    }
}
