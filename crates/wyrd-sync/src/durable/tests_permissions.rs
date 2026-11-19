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
    // O_DIRECTORY reports the link as ENOTDIR (it is not a
    // directory); ELOOP is the equivalent refusal without it. Either
    // refuses before any custody state moves. Errnos are asserted by
    // raw number: this toolchain predates `ErrorKind::FilesystemLoop`.
    let error = ensure_owner_only_dir(&link).unwrap_err();
    assert!(
        matches!(
            error.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR)
        ),
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
    use std::ffi::OsStr;
    let _guard = UmaskGuard::set(0o000);
    let dir = TestDir::new("custody-temp");
    // The temp itself carries the restrictive mode from creation, so a
    // crash between creation and rename strands an owner-only file.
    // Each writer owns a unique sibling, so the test finds it by
    // extension rather than by a shared name.
    let temps_in = |dir: &TestDir| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&dir.path)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension() == Some(OsStr::new("tmp")))
            .collect()
    };
    {
        let (_tmp, _tmp_path) =
            create_mode_temp(&dir.path, "store-key.wrap", SECRET_FILE_MODE).unwrap();
        let temps = temps_in(&dir);
        assert_eq!(temps.len(), 1, "one unique temp per claim: {temps:?}");
        assert_eq!(
            mode_of(&temps[0]) & 0o077,
            0,
            "a stranded temp is never readable"
        );
    }
    // And the completed write leaves none of its own temps behind.
    atomic_write_mode(&dir.path, "probe", b"bytes", SECRET_FILE_MODE).unwrap();
    assert!(
        temps_in(&dir)
            .iter()
            .all(|path| !path.to_string_lossy().contains("probe.")),
        "no probe temp survives the rename"
    );
}

/// Two claims on the same `(dir, name)` while the first file is
/// still open: the discipline pins distinct paths and the survival
/// of the first temp — deterministically, with no threads or
/// sleeps. Under the old shared-name rule the second claim would
/// have unlinked the first writer's live file; the retry path
/// (`AlreadyExists` → fresh name) is what this exercises.
#[test]
fn concurrent_claims_on_one_name_never_share_a_temp() {
    use super::store::create_mode_temp;
    use super::tests_harness::TestDir;
    let dir = TestDir::new("concurrent-claim");
    let (_first, first_path) = create_mode_temp(&dir.path, "shared", 0o600).unwrap();
    let (_second, second_path) = create_mode_temp(&dir.path, "shared", 0o600).unwrap();
    assert_ne!(
        first_path, second_path,
        "each writer owns a distinct sibling"
    );
    assert!(
        first_path.is_file(),
        "the second claim never unlinks the first writer's temp"
    );
    assert!(second_path.is_file(), "the second claim holds its own temp");
}

#[cfg(unix)]
#[test]
fn a_stale_shared_temp_is_left_alone_while_the_write_hardens() {
    use super::store::{atomic_write_mode, SECRET_FILE_MODE};
    use std::os::unix::fs::PermissionsExt;
    let _guard = UmaskGuard::set(0o000);
    let dir = TestDir::new("custody-stale-temp");
    // A loose temp stranded by a pre-fix crash under the old shared
    // name: the next custody write must not remove it (it could be a
    // live competitor's file under the new discipline) and must not
    // publish it — the destination is claimed at the hardened mode
    // under a unique sibling instead.
    let legacy = dir.path.join("store-key.wrap.tmp");
    std::fs::write(&legacy, b"stale").unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o644)).unwrap();
    atomic_write_mode(&dir.path, "store-key.wrap", b"fresh", SECRET_FILE_MODE).unwrap();
    drop(_guard);
    assert_eq!(mode_of(&dir.path.join("store-key.wrap")) & 0o077, 0);
    assert_eq!(
        std::fs::read(dir.path.join("store-key.wrap")).unwrap(),
        b"fresh"
    );
    assert!(
        legacy.is_file(),
        "the legacy shared temp is never unlinked by a concurrent-safe write"
    );
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
    assert_eq!(before, after, "opening warns but changes nothing");
}

#[cfg(unix)]
#[allow(unsafe_code)]
#[test]
fn a_fifo_at_the_leaf_fails_open_without_blocking() {
    use super::store::ensure_owner_only_dir;
    // A FIFO at the custody leaf must fail the open fast: without
    // `O_DIRECTORY` the verification open would block on the fifo
    // instead of refusing it.
    let parent = TestDir::new("custody-fifo-parent");
    let leaf = parent.path.join("leaf");
    let name = std::ffi::CString::new(leaf.as_os_str().as_encoded_bytes())
        .expect("leaf path is a valid C string");
    // SAFETY: mkfifo on a fresh path in a temp dir; the NUL-free
    // CString carries the path, the return is checked immediately,
    // and no pointer is retained.
    let ret = unsafe { libc::mkfifo(name.as_ptr(), 0o700) };
    assert_eq!(ret, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
    let error = ensure_owner_only_dir(&leaf).unwrap_err();
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::NotADirectory,
        "a fifo leaf fails fast, never blocks: {error:?}"
    );
}

/// Two live writers on one destination through the public helper: no
/// spurious I/O errors, and every observed destination is one
/// complete payload — never mixed or partial. Under the old shared
/// `{name}.tmp` discipline one writer unlinked the other's live temp,
/// surfacing `ENOENT` or publishing a half-written file; unique
/// siblings resolve last-writer-wins at the rename instead. The
/// barrier (not sleeps) makes the overlap deterministic: both threads
/// write together on every round.
#[test]
fn concurrent_atomic_writes_never_clobber_each_other() {
    use super::store::atomic_write_mode;
    use super::tests_harness::TestDir;
    use std::sync::{Arc, Barrier};

    const ROUNDS: usize = 25;
    let dir = TestDir::new("concurrent-atomic-write");
    let dir = Arc::new(dir.path.clone());
    let barrier = Arc::new(Barrier::new(2));

    let writer = |byte: u8, dir: Arc<std::path::PathBuf>, barrier: Arc<Barrier>| {
        std::thread::spawn(move || {
            let payload = vec![byte; 65536];
            barrier.wait();
            for _ in 0..ROUNDS {
                atomic_write_mode(&dir, "shared", &payload, 0o600)?;
            }
            std::io::Result::Ok(())
        })
    };
    let a = writer(0x41, Arc::clone(&dir), Arc::clone(&barrier));
    let b = writer(0x42, Arc::clone(&dir), Arc::clone(&barrier));
    a.join()
        .expect("writer A panicked")
        .expect("writer A saw I/O errors");
    b.join()
        .expect("writer B panicked")
        .expect("writer B saw I/O errors");

    let observed = std::fs::read(dir.join("shared")).unwrap();
    assert_eq!(observed.len(), 65536, "never a partial write");
    assert!(
        observed.iter().all(|&byte| byte == 0x41) || observed.iter().all(|&byte| byte == 0x42),
        "last writer wins whole, never a mixture"
    );
    let debris: Vec<_> = std::fs::read_dir(dir.as_path())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "tmp")
                && path.to_string_lossy().contains("shared.")
        })
        .collect();
    assert!(
        debris.is_empty(),
        "no writer leaves its temp behind: {debris:?}"
    );
}
