use super::store::DurableStore;
use super::tests_harness::{TestDir, PASSPHRASE};
use super::DurableError;
use crate::membership::test_util::drive;
#[cfg(unix)]
use crate::runtime::test_util::{file_mode as mode_of, UmaskGuard};

/// Open a fresh store under the given umask and return its directory
/// (the store is dropped: mode assertions need no lock held).
#[cfg(unix)]
fn open_fresh_store(name: &str, mask: u32) -> TestDir {
    let _guard = UmaskGuard::set(mask);
    let dir = TestDir::new(name);
    let store = DurableStore::open(dir.path.clone(), drive(), PASSPHRASE).unwrap();
    drop(store);
    dir
}

#[cfg(unix)]
#[test]
fn store_key_wrap_is_not_group_or_world_readable() {
    use super::store::SECRET_FILE_MODE;
    let mut modes = Vec::new();
    for mask in [0o000, 0o077] {
        let dir = open_fresh_store("custody-store-key", mask);
        let mode = mode_of(&dir.path.join("store-key.wrap"));
        assert_eq!(mode & 0o077, 0, "umask {mask:03o}: {mode:03o}");
        modes.push(mode);
    }
    assert_eq!(modes[0], modes[1], "the mode is chosen, not umask-derived");
    assert_eq!(modes[0] & 0o777, SECRET_FILE_MODE);
}

#[cfg(unix)]
#[test]
fn drive_id_file_is_not_group_or_world_writable() {
    // `DRIVE` is public material, so unlike the secrets it has no
    // umask-independent value: the creation mode is a ceiling (`0o644`
    // masked by the umask), which fails safe toward stricter. The
    // invariant is never group- or world-writable under any umask.
    for mask in [0o000, 0o022, 0o077] {
        let dir = open_fresh_store("custody-drive", mask);
        let mode = mode_of(&dir.path.join("DRIVE"));
        assert_eq!(mode & 0o022, 0, "umask {mask:03o}: {mode:03o}");
    }
}

#[cfg(unix)]
#[test]
fn a_fresh_drive_directory_is_owner_only() {
    // End-state pin: `TestDir` pre-creates the directory, so the
    // helper takes its early return here and the `0o700` comes from
    // the fresh-`DRIVE` backstop. The helper's own create path is
    // pinned by `ensure_owner_only_dir_creates_at_owner_only` below;
    // the CLI test pins the whole `init` path.
    for mask in [0o000, 0o077] {
        let dir = open_fresh_store("custody-dir", mask);
        assert_eq!(mode_of(&dir.path) & 0o077, 0, "drive dir, umask {mask:03o}");
        assert_eq!(
            mode_of(&dir.path.join("commits")) & 0o077,
            0,
            "commits dir, umask {mask:03o}"
        );
    }
}

#[cfg(unix)]
#[test]
fn preexisting_loose_dir_ensures_without_chmod() {
    use super::store::ensure_owner_only_dir;
    use std::os::unix::fs::PermissionsExt;
    let dir = TestDir::new("custody-loose-helper");
    std::fs::set_permissions(&dir.path, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A v0.2-era umask-mode leaf: the open proceeds (upgrade contract
    // Level 1) with an operator warning, and never chmods.
    ensure_owner_only_dir(&dir.path).unwrap();
    assert_eq!(
        mode_of(&dir.path) & 0o777,
        0o755,
        "verification warns, never chmods"
    );
}

#[cfg(unix)]
#[test]
fn substituted_symlink_does_not_satisfy_the_check() {
    use super::store::ensure_owner_only_dir;
    let dir = TestDir::new("custody-symlink");
    let target = dir.path.join("real");
    std::fs::create_dir(&target).unwrap();
    let link = dir.path.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    // The open carries O_NOFOLLOW, so the substituted symlink fails
    // the open itself instead of verifying the target's modes.
    // ELOOP is asserted by raw number: this toolchain predates
    // `ErrorKind::FilesystemLoop`.
    let error = ensure_owner_only_dir(&link).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(libc::ELOOP),
        "unexpected error: {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn leaf_verdict_pins_all_arms() {
    use super::store::{current_euid, leaf_verdict, LeafVerdict};
    let me = current_euid();
    // A verified owner-only leaf is silent.
    assert_eq!(
        leaf_verdict(true, me, 0o40700, me).unwrap(),
        LeafVerdict::Clean
    );
    // Same owner, any group/other bit: opens with a warning.
    for mode in [0o40755, 0o40770, 0o40707, 0o40777] {
        assert_eq!(
            leaf_verdict(true, me, mode, me).unwrap(),
            LeafVerdict::LooseMode,
            "mode {mode:o}"
        );
    }
    // A foreign-owned leaf refuses before any custody state moves.
    let foreign = leaf_verdict(true, me.wrapping_add(1), 0o40700, me).unwrap_err();
    assert_eq!(foreign.kind(), std::io::ErrorKind::PermissionDenied);
    // A non-directory never verifies, whatever owns it.
    let not_dir = leaf_verdict(false, me, 0o40700, me).unwrap_err();
    assert_eq!(not_dir.kind(), std::io::ErrorKind::NotADirectory);
}

#[cfg(unix)]
#[test]
fn ensure_owner_only_dir_creates_at_owner_only() {
    use super::store::ensure_owner_only_dir;
    let _guard = UmaskGuard::set(0o000);
    let dir = TestDir::new("custody-helper");
    // A path `TestDir` has not created: the helper takes its create
    // path (`DirBuilder::mode`), not the early return.
    let fresh = dir.path.join("fresh");
    ensure_owner_only_dir(&fresh).unwrap();
    drop(_guard);
    assert_eq!(mode_of(&fresh) & 0o077, 0);
}

#[cfg(unix)]
#[test]
fn the_store_lock_is_owner_only() {
    for mask in [0o000, 0o077] {
        let dir = open_fresh_store("custody-lock", mask);
        assert_eq!(
            mode_of(&dir.path.join("LOCK")) & 0o077,
            0,
            "umask {mask:03o}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_crash_during_store_key_write_leaves_no_readable_temp() {
    use super::store::{atomic_write_mode, create_mode_temp, SECRET_FILE_MODE};
    let _guard = UmaskGuard::set(0o000);
    let dir = TestDir::new("custody-temp");
    // The temp itself carries the restrictive mode from creation, so a
    // crash between creation and rename strands an owner-only file.
    {
        let _tmp = create_mode_temp(&dir.path, "store-key.wrap", SECRET_FILE_MODE).unwrap();
        assert_eq!(
            mode_of(&dir.path.join("store-key.wrap.tmp")) & 0o077,
            0,
            "a stranded temp is never readable"
        );
    }
    // And the completed write leaves no temp behind at all.
    atomic_write_mode(&dir.path, "probe", b"bytes", SECRET_FILE_MODE).unwrap();
    assert!(!dir.path.join("probe.tmp").exists());
}

#[cfg(unix)]
#[test]
fn a_stale_temp_is_reclaimed_at_the_hardened_mode() {
    use super::store::{atomic_write_mode, SECRET_FILE_MODE};
    use std::os::unix::fs::PermissionsExt;
    let _guard = UmaskGuard::set(0o000);
    let dir = TestDir::new("custody-stale-temp");
    // A loose temp stranded by a pre-fix crash: the next custody
    // write must remove and re-claim it, not publish it.
    std::fs::write(dir.path.join("store-key.wrap.tmp"), b"stale").unwrap();
    std::fs::set_permissions(
        dir.path.join("store-key.wrap.tmp"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    atomic_write_mode(&dir.path, "store-key.wrap", b"fresh", SECRET_FILE_MODE).unwrap();
    drop(_guard);
    assert_eq!(mode_of(&dir.path.join("store-key.wrap")) & 0o077, 0);
    assert!(!dir.path.join("store-key.wrap.tmp").exists());
}

#[cfg(unix)]
#[test]
fn a_non_directory_at_commits_fails_open_early() {
    // The `AlreadyExists` arm of directory creation must not report
    // success for a regular file: the open refuses at the call that
    // names the path, not at the first commit through it.
    let dir = TestDir::new("custody-commits-file");
    std::fs::write(dir.path.join("commits"), b"not a directory").unwrap();
    assert!(
        matches!(
            DurableStore::open(dir.path.clone(), drive(), PASSPHRASE),
            Err(DurableError::Io(_))
        ),
        "a file at commits/ refuses the open at the naming call"
    );
}

#[cfg(unix)]
#[test]
fn a_loose_mode_existing_drive_still_opens() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = UmaskGuard::set(0o077);
    let dir = TestDir::new("custody-loose");
    let own_drive = drive();
    drop(DurableStore::open(dir.path.clone(), own_drive, PASSPHRASE).unwrap());
    // A drive created before the fix keeps its loose files: opening
    // must not refuse them and must not chmod them either.
    for name in ["DRIVE", "store-key.wrap", "LOCK"] {
        std::fs::set_permissions(dir.path.join(name), std::fs::Permissions::from_mode(0o644))
            .unwrap();
    }
    std::fs::set_permissions(&dir.path, std::fs::Permissions::from_mode(0o755)).unwrap();
    drop(_guard);
    let before: Vec<u32> = ["DRIVE", "store-key.wrap", "LOCK"]
        .iter()
        .map(|n| mode_of(&dir.path.join(n)))
        .collect();
    drop(DurableStore::open(dir.path.clone(), own_drive, PASSPHRASE).unwrap());
    let after: Vec<u32> = ["DRIVE", "store-key.wrap", "LOCK"]
        .iter()
        .map(|n| mode_of(&dir.path.join(n)))
        .collect();
    assert_eq!(before, after, "opening reports nothing and changes nothing");
}
