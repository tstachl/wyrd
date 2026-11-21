use super::*;
use std::io::Cursor;

/// Stdin strips one trailing newline (LF and CRLF), like the
/// file path, so both surfaces open the same keystore.
#[test]
fn stdin_strips_one_trailing_newline_like_the_file() {
    assert_eq!(
        read_passphrase_stdin(Cursor::new(b"hunter2-hunter2\n"))
            .unwrap()
            .as_str(),
        "hunter2-hunter2"
    );
    assert_eq!(
        read_passphrase_stdin(Cursor::new(b"hunter2-hunter2\r\n"))
            .unwrap()
            .as_str(),
        "hunter2-hunter2"
    );
    assert_eq!(
        read_passphrase_stdin(Cursor::new(b"hunter2-hunter2"))
            .unwrap()
            .as_str(),
        "hunter2-hunter2"
    );
}

/// Empty pipes fail closed before the keystore opens: immediate
/// EOF and a bare newline both refuse as usage, never as an
/// empty passphrase the owner would try.
#[test]
fn empty_stdin_fails_closed() {
    assert!(read_passphrase_stdin(Cursor::new(b"")).is_err());
    assert!(read_passphrase_stdin(Cursor::new(b"\n")).is_err());
}

/// Frozen copy (product-terminology.md R-C1): empty-passphrase
/// refusals carry the approved sentence, so a future edit
/// cannot silently restore the old wording. The prompt path
/// needs a terminal and is pinned by construction through the
/// shared tail; the pipe path below pins its full message.
#[test]
fn empty_passphrase_refusals_use_the_approved_sentence() {
    assert_eq!(
        NO_PASSPHRASE_TAIL, "This drive's owner cannot start without it",
        "the shared tail is the frozen sentence"
    );
    let err = read_passphrase_stdin(Cursor::new(b"")).expect_err("empty stdin must refuse");
    assert_eq!(
        err.to_string(),
        "No passphrase provided on stdin. This drive's owner cannot start without it",
        "stdin names its surface and keeps the sentence: {err}"
    );
}

/// Over-long and non-UTF-8 pipes fail closed at the same 4096
/// bound the file reader enforces.
#[test]
fn overlong_and_non_utf8_stdin_fail_closed() {
    let big = vec![b'x'; 4097];
    assert!(read_passphrase_stdin(Cursor::new(big)).is_err());
    assert!(read_passphrase_stdin(Cursor::new(vec![0xff, 0xfe])).is_err());
    // Exactly at the bound reads fine.
    let at_bound = vec![b'x'; 4096];
    assert!(read_passphrase_stdin(Cursor::new(at_bound)).is_ok());
}

/// The ambiguous pair refuses even where clap never parses:
/// a programmatically built `Credentials` with both surfaces
/// fails instead of silently preferring one.
#[test]
fn stdin_and_file_together_refuse() {
    let creds = Credentials {
        identity_file: Some(PathBuf::from("/k/identity")),
        passphrase_file: Some(PathBuf::from("/k/passphrase")),
        passphrase_stdin: true,
        credential_backend: None,
        credential_account: None,
    };
    assert!(read_credentials(&creds, None).is_err());
}

/// Stdin mode never resolves a passphrase file, even where a
/// config names one: a fixture identity plus a piped passphrase
/// reads end to end, and the provenance names stdin — so a
/// passphrase file is neither required nor consulted.
#[test]
fn stdin_mode_needs_no_passphrase_file() {
    use crate::tests_harness::{write_secret, TempDir};
    let temp = TempDir::new();
    let identity_file = temp.0.join("identity");
    // Raw 32 bytes, the same shape the production reader
    // accepts elsewhere in this package's tests.
    write_secret(&identity_file, [0x11; 32]);
    let config = LoadedConfig {
        path: temp.0.join("default.toml"),
        relays: Vec::new(),
        identity_file: None,
        // A config naming a passphrase file must not matter:
        // stdin mode never resolves it.
        passphrase_file: Some(temp.0.join("passphrase")),
        credential_backend: None,
        credential_account: None,
    };
    let creds = Credentials {
        identity_file: Some(identity_file),
        passphrase_file: None,
        passphrase_stdin: true,
        credential_backend: None,
        credential_account: None,
    };
    let (identity, passphrase, provenance) =
        read_credentials_with(&creds, Some(&config), Cursor::new(b"piped-passphrase\n"))
            .expect("stdin mode reads without any passphrase file");
    let _ = identity;
    assert_eq!(passphrase.as_str(), "piped-passphrase");
    assert!(
        provenance.passphrase_from_stdin,
        "provenance names stdin: {provenance:?}"
    );
    assert!(
        !provenance.passphrase_from_file,
        "stdin is not a file surface: {provenance:?}"
    );
}

fn unlock_creds() -> Credentials {
    Credentials {
        identity_file: None,
        passphrase_file: None,
        passphrase_stdin: false,
        credential_backend: None,
        credential_account: None,
    }
}

/// Unlock never reads a passphrase file: naming one refuses
/// before the keystore opens, even where the file exists.
#[test]
fn unlock_refuses_a_passphrase_file() {
    let mut creds = unlock_creds();
    creds.passphrase_file = Some(PathBuf::from("/k/passphrase"));
    let err = read_unlock_credentials_with(&creds, None, Cursor::new(b""), true)
        .expect_err("a passphrase file must refuse");
    assert!(
        err.to_string().contains("never reads a passphrase file"),
        "names the rule: {err}"
    );
}

/// Provisioning stores fresh files under the commit boundary:
/// the identity lands as the canonical hex the reader accepts,
/// the passphrase verbatim, and the pair reads back through the
/// production loader. A second provision is `Invalid` — never an
/// overwrite — and a reference/material posture mismatch is
/// `Invalid` before anything is written.
#[test]
fn store_provisions_fresh_files_and_refuses_overwrite() {
    use crate::tests_harness::TempDir;
    let backend = FileBackend;
    let temp = TempDir::new();
    let identity = temp.0.join("identity");
    let passphrase = temp.0.join("passphrase");
    let reference = CredentialRef::File {
        identity: identity.clone(),
        passphrase: Some(passphrase.clone()),
    };
    let material = LoadedCredential {
        identity: DeviceIdentitySecret::from_bytes([0x11; 32]).unwrap(),
        passphrase: Some(Zeroizing::new("hunter2-hunter2".to_string())),
    };
    backend
        .store(&reference, &material)
        .expect("fresh provision stores");
    // Hex on disk, verbatim passphrase, both read back.
    let raw = std::fs::read_to_string(&identity).expect("identity file written");
    assert_eq!(raw, "11".repeat(32));
    let loaded = backend.load(&reference).expect("provisioned pair loads");
    assert_eq!(loaded.identity.device_id(), material.identity.device_id());
    assert_eq!(
        loaded.passphrase.expect("named item loads").as_str(),
        "hunter2-hunter2"
    );
    // Provision again: Invalid, and the files are untouched.
    match backend.store(&reference, &material) {
        Err(CredentialError::Invalid { detail, .. }) => assert!(
            detail.contains("already exists"),
            "names the guard: {detail}"
        ),
        other => panic!("re-provisioning is Invalid: {other:?}"),
    }
    // Posture mismatch: nothing is written.
    let bare = CredentialRef::File {
        identity: temp.0.join("bare-identity"),
        passphrase: None,
    };
    match backend.store(&bare, &material) {
        Err(CredentialError::Invalid { detail, .. }) => assert!(
            detail.contains("disagree on the passphrase item"),
            "names the rule: {detail}"
        ),
        other => panic!("a posture mismatch is Invalid: {other:?}"),
    }
    assert!(
        !temp.0.join("bare-identity").exists(),
        "a refused provision writes nothing"
    );
}

/// Session posture provisions the identity alone: no passphrase
/// file, no empty material — and the pair still reads back.
#[test]
fn store_provisions_session_posture_identity_only() {
    use crate::tests_harness::TempDir;
    let backend = FileBackend;
    let temp = TempDir::new();
    let identity = temp.0.join("identity");
    let reference = CredentialRef::File {
        identity: identity.clone(),
        passphrase: None,
    };
    let material = LoadedCredential {
        identity: DeviceIdentitySecret::from_bytes([0x22; 32]).unwrap(),
        passphrase: None,
    };
    backend
        .store(&reference, &material)
        .expect("session provision stores");
    let loaded = backend.load(&reference).expect("identity loads");
    assert!(loaded.passphrase.is_none());
    assert_eq!(loaded.identity.device_id(), material.identity.device_id());
}

/// Replacement overwrites under backup: the new pair reads back,
/// the backups are removed on success, and a stale backup from a
/// killed run refuses before anything is touched.
#[test]
fn replace_overwrites_and_cleans_up_backups() {
    use crate::tests_harness::{write_secret, TempDir};
    let backend = FileBackend;
    let temp = TempDir::new();
    let identity = temp.0.join("identity");
    let passphrase = temp.0.join("passphrase");
    write_secret(&identity, [0x11; 32]);
    write_secret(&passphrase, b"old-pass\n");
    let reference = CredentialRef::File {
        identity: identity.clone(),
        passphrase: Some(passphrase.clone()),
    };
    let material = LoadedCredential {
        identity: DeviceIdentitySecret::from_bytes([0x33; 32]).unwrap(),
        passphrase: Some(Zeroizing::new("new-pass".to_string())),
    };
    backend
        .replace(&reference, &material)
        .expect("replacement overwrites");
    let loaded = backend.load(&reference).expect("replaced pair loads");
    assert_eq!(loaded.identity.device_id(), material.identity.device_id());
    assert_eq!(
        loaded.passphrase.expect("named item loads").as_str(),
        "new-pass"
    );
    assert!(
        !temp.0.join("identity.bak").exists(),
        "backups are removed after a verified replace"
    );
    // A stale backup refuses: the writer never destroys the only
    // copy it cannot account for.
    write_secret(&temp.0.join("identity.bak"), b"stale");
    match backend.replace(&reference, &material) {
        Err(CredentialError::Invalid { detail, .. }) => assert!(
            detail.contains("stale credential backup"),
            "names the guard: {detail}"
        ),
        other => panic!("a stale backup refuses: {other:?}"),
    }
}

/// Deletion removes both files and verifies: a second deletion
/// is `Missing` naming the identity, and a symlink is `Denied`
/// — never deleted through.
#[test]
fn delete_removes_and_verifies() {
    use crate::tests_harness::{write_secret, TempDir};
    let backend = FileBackend;
    let temp = TempDir::new();
    let identity = temp.0.join("identity");
    let passphrase = temp.0.join("passphrase");
    write_secret(&identity, [0x11; 32]);
    write_secret(&passphrase, b"hunter2-hunter2\n");
    let reference = CredentialRef::File {
        identity: identity.clone(),
        passphrase: Some(passphrase.clone()),
    };
    backend.delete(&reference).expect("deletion removes");
    assert!(!identity.exists() && !passphrase.exists());
    match backend.delete(&reference) {
        Err(CredentialError::Missing { subject, .. }) => assert_eq!(
            subject,
            CredentialSubject::File { path: identity },
            "names the identity"
        ),
        other => panic!("deleting nothing is Missing: {other:?}"),
    }
    // A symlink where a credential file belongs is Denied.
    let link = temp.0.join("linked");
    #[cfg(unix)]
    std::os::unix::fs::symlink(temp.0.join("nowhere"), &link).unwrap();
    #[cfg(unix)]
    match backend.delete(&CredentialRef::File {
        identity: link,
        passphrase: None,
    }) {
        Err(CredentialError::Denied { .. }) => {}
        other => panic!("a symlink delete is Denied: {other:?}"),
    }
}

/// Unlock without a terminal and without the pipe refuses
/// before the keystore opens: no silent file fallback, no
/// blocking read on a non-terminal stdin.
#[test]
fn unlock_without_terminal_or_pipe_refuses() {
    use crate::tests_harness::{write_secret, TempDir};
    let temp = TempDir::new();
    let identity_file = temp.0.join("identity");
    write_secret(&identity_file, [0x33; 32]);
    let mut creds = unlock_creds();
    creds.identity_file = Some(identity_file);
    let err = read_unlock_credentials_with(&creds, None, Cursor::new(b""), false)
        .expect_err("non-terminal stdin without --passphrase-stdin must refuse");
    assert!(
        err.to_string().contains("--passphrase-stdin"),
        "points at the pipe: {err}"
    );
    assert_eq!(
        err.to_string(),
        "not a terminal: pipe the passphrase with --passphrase-stdin or run unlock on a terminal. This drive's owner cannot start without it",
        "what happened, and the next step: {err}"
    );
}

/// The piped unlock path reads end to end and names stdin in
/// provenance — never the prompt, never a file.
#[test]
fn unlock_pipe_reads_and_names_stdin() {
    use crate::tests_harness::{write_secret, TempDir};
    let temp = TempDir::new();
    let identity_file = temp.0.join("identity");
    write_secret(&identity_file, [0x22; 32]);
    let mut creds = unlock_creds();
    creds.identity_file = Some(identity_file);
    creds.passphrase_stdin = true;
    let (identity, passphrase, provenance) =
        read_unlock_credentials_with(&creds, None, Cursor::new(b"typed-later\n"), false)
            .expect("piped unlock reads");
    let _ = identity;
    assert_eq!(passphrase.as_str(), "typed-later");
    assert!(provenance.passphrase_from_stdin);
    assert!(!provenance.passphrase_from_prompt);
    assert!(!provenance.passphrase_from_file);
    assert_eq!(provenance.passphrase_word(), "stdin (`--passphrase-stdin`)");
}

fn service_creds(stdin: bool, file: Option<&str>) -> Credentials {
    Credentials {
        identity_file: None,
        passphrase_file: file.map(PathBuf::from),
        passphrase_stdin: stdin,
        credential_backend: None,
        credential_account: None,
    }
}

/// A managed service with a piped passphrase refuses before
/// anything is created: no definition could embed the pipe, so
/// installing one would crash-loop under the manager.
#[test]
fn managed_service_with_a_pipe_refuses() {
    use wyrd_daemon::bootstrap::ServiceCredential;
    let err = resolve_service_credential(&service_creds(true, None), None, true)
        .expect_err("managed plus stdin must refuse");
    assert!(
        err.to_string()
            .contains("managed service needs a passphrase file"),
        "names the managed requirement: {err}"
    );
    // The refusal is backend-independent: a store drive never
    // reaches the pipe check, it resolves.
    let store_creds = Credentials {
        identity_file: None,
        passphrase_file: None,
        passphrase_stdin: true,
        credential_backend: Some("os-store".to_string()),
        credential_account: Some("wyrd-drive-alpha".to_string()),
    };
    assert!(
        matches!(
            resolve_service_credential(&store_creds, None, true),
            Ok(ServiceCredential::OsStore { .. })
        ),
        "a store drive resolves without a pipe: the manager reads the item"
    );
}

/// Session posture skips the file entirely: `--no-service` with
/// a pipe resolves the identity and carries no passphrase path
/// for the daemon to render.
#[test]
fn unmanaged_service_with_a_pipe_carries_no_passphrase_file() {
    use crate::tests_harness::{write_secret, TempDir};
    use wyrd_daemon::bootstrap::ServiceCredential;
    let temp = TempDir::new();
    let identity_file = temp.0.join("identity");
    write_secret(&identity_file, [0x44; 32]);
    let mut creds = service_creds(true, None);
    creds.identity_file = Some(identity_file.clone());
    match resolve_service_credential(&creds, None, false)
        .expect("session posture resolves without a passphrase file")
    {
        ServiceCredential::File {
            identity_file: resolved,
            passphrase_file,
        } => {
            assert_eq!(resolved, identity_file);
            assert!(
                passphrase_file.is_none(),
                "no file for the daemon to render"
            );
        }
        ServiceCredential::OsStore { .. } => {
            panic!("a file posture never resolves the store");
        }
    }
}

/// File posture is unchanged: managed resolves both paths for
/// the definition exactly like before.
#[test]
fn managed_service_with_files_resolves_both_paths() {
    use wyrd_daemon::bootstrap::ServiceCredential;
    let creds = Credentials {
        identity_file: Some(PathBuf::from("/k/identity")),
        passphrase_file: Some(PathBuf::from("/k/passphrase")),
        passphrase_stdin: false,
        credential_backend: None,
        credential_account: None,
    };
    match resolve_service_credential(&creds, None, true).expect("file posture resolves") {
        ServiceCredential::File {
            identity_file,
            passphrase_file,
        } => {
            assert_eq!(identity_file, PathBuf::from("/k/identity"));
            assert_eq!(passphrase_file, Some(PathBuf::from("/k/passphrase")));
        }
        ServiceCredential::OsStore { .. } => {
            panic!("a file posture never resolves the store");
        }
    }
}

/// The flag names the store where no configuration exists: the
/// account travels from the flag, and a missing account
/// refuses before anything loads.
#[test]
fn service_credential_flags_name_the_store_without_a_config() {
    use wyrd_daemon::bootstrap::ServiceCredential;
    let creds = Credentials {
        identity_file: None,
        passphrase_file: None,
        passphrase_stdin: false,
        credential_backend: Some("os-store".to_string()),
        credential_account: Some("wyrd-drive-flagged".to_string()),
    };
    match resolve_service_credential(&creds, None, true).expect("flag store resolves") {
        ServiceCredential::OsStore { account } => {
            assert_eq!(account, "wyrd-drive-flagged");
        }
        ServiceCredential::File { .. } => panic!("a store flag never resolves files"),
    }
    let missing_account = Credentials {
        identity_file: None,
        passphrase_file: None,
        passphrase_stdin: false,
        credential_backend: Some("os-store".to_string()),
        credential_account: None,
    };
    let err = resolve_service_credential(&missing_account, None, true)
        .expect_err("a store backend without an account must refuse");
    assert!(
        err.to_string().contains("no credential account"),
        "names the missing selector: {err}"
    );
}

/// One backend per drive, enforced on the flag surface: an
/// unknown backend, a flag that contradicts the configuration,
/// a mismatched account, and an account beside the file
/// backend all refuse — never a silent fallback.
#[test]
fn effective_backend_refuses_ambiguity() {
    use crate::tests_harness::TempDir;
    let temp = TempDir::new();
    let file_config = LoadedConfig {
        path: temp.0.join("default.toml"),
        relays: Vec::new(),
        identity_file: Some(temp.0.join("identity")),
        passphrase_file: Some(temp.0.join("passphrase")),
        credential_backend: None,
        credential_account: None,
    };
    // Unknown backend flag.
    let mut creds = service_creds(false, None);
    creds.credential_backend = Some("vault".to_string());
    assert!(effective_backend(&creds, Some(&file_config))
        .expect_err("unknown backend must refuse")
        .to_string()
        .contains("unknown --credential-backend"),);
    // Flag contradicts the configuration.
    let mut creds = service_creds(false, None);
    creds.credential_backend = Some("file".to_string());
    let store_config = LoadedConfig {
        path: temp.0.join("default.toml"),
        relays: Vec::new(),
        identity_file: None,
        passphrase_file: None,
        credential_backend: Some("os-store".to_string()),
        credential_account: Some("wyrd-drive-alpha".to_string()),
    };
    assert!(effective_backend(&creds, Some(&store_config))
        .expect_err("a contradicting flag must refuse")
        .to_string()
        .contains("mismatch"),);
    // Account beside the file backend.
    let mut creds = service_creds(false, None);
    creds.credential_account = Some("wyrd-drive-alpha".to_string());
    assert!(effective_backend(&creds, Some(&file_config))
        .expect_err("an account beside files must refuse")
        .to_string()
        .contains("one backend per drive"),);
    // Mismatched accounts.
    let mut creds = service_creds(false, None);
    creds.credential_backend = Some("os-store".to_string());
    creds.credential_account = Some("wyrd-drive-other".to_string());
    assert!(effective_backend(&creds, Some(&store_config))
        .expect_err("mismatched accounts must refuse")
        .to_string()
        .contains("mismatch"),);
}

/// The file backend speaks the loader taxonomy through the
/// production readers in one call: a missing file is
/// `Missing`, bad modes and refused symlinks are `Denied`, a
/// broken shape is `Invalid` — the same outcomes the flag
/// path reports, so the boundary cannot claim what the
/// backend would not do.
#[test]
fn file_backend_maps_failures_onto_the_taxonomy() {
    use crate::tests_harness::{write_secret, TempDir};
    use std::os::unix::fs::PermissionsExt;
    let backend = FileBackend;
    let temp = TempDir::new();
    // Missing identity file: Missing naming the file, never
    // Invalid.
    let outcome = backend.load(&CredentialRef::File {
        identity: temp.0.join("no-identity"),
        passphrase: Some(temp.0.join("no-passphrase")),
    });
    match outcome {
        Err(CredentialError::Missing { subject, detail }) => {
            assert_eq!(
                subject,
                CredentialSubject::File {
                    path: temp.0.join("no-identity")
                },
                "the subject names the file without parsing"
            );
            assert!(
                detail.contains("no-identity"),
                "the evidence names it too: {detail}"
            );
        }
        other => panic!("a missing identity file is Missing: {other:?}"),
    }
    // Valid identity, missing passphrase file: Missing names it.
    let identity_file = temp.0.join("identity");
    write_secret(&identity_file, [0x11; 32]);
    let outcome = backend.load(&CredentialRef::File {
        identity: identity_file.clone(),
        passphrase: Some(temp.0.join("no-passphrase")),
    });
    assert!(
        matches!(outcome, Err(CredentialError::Missing { .. })),
        "a missing passphrase file is Missing: {outcome:?}"
    );
    // No passphrase item at all (session posture): the
    // identity loads with no passphrase — a posture, never a
    // missing item, and never an empty passphrase the owner
    // would try.
    let loaded = backend
        .load(&CredentialRef::File {
            identity: identity_file.clone(),
            passphrase: None,
        })
        .expect("session posture loads without a passphrase item");
    assert!(
        loaded.passphrase.is_none(),
        "no item means no material, not an empty secret"
    );
    // Group-readable identity file: Denied.
    let lax = temp.0.join("lax-identity");
    write_secret(&lax, [0x11; 32]);
    std::fs::set_permissions(&lax, std::fs::Permissions::from_mode(0o644))
        .expect("test fixture chmod");
    let outcome = backend.load(&CredentialRef::File {
        identity: lax,
        passphrase: Some(temp.0.join("no-passphrase")),
    });
    assert!(
        matches!(outcome, Err(CredentialError::Denied { .. })),
        "a group-readable file is Denied: {outcome:?}"
    );
    // Broken identity shape with a valid passphrase file:
    // Invalid.
    let passphrase_file = temp.0.join("passphrase");
    write_secret(&passphrase_file, b"hunter2-hunter2\n");
    let bad = temp.0.join("bad-identity");
    write_secret(&bad, b"too short");
    let outcome = backend.load(&CredentialRef::File {
        identity: bad,
        passphrase: Some(passphrase_file.clone()),
    });
    assert!(
        matches!(outcome, Err(CredentialError::Invalid { .. })),
        "a broken identity shape is Invalid: {outcome:?}"
    );
    // Happy path: material round-trips, newline stripped.
    let loaded = backend
        .load(&CredentialRef::File {
            identity: identity_file,
            passphrase: Some(passphrase_file),
        })
        .expect("valid files load");
    assert_eq!(
        loaded.passphrase.expect("named item loads").as_str(),
        "hunter2-hunter2"
    );
}

/// Every refusal literal lands on its taxonomy arm by rule:
/// the reader and the mapper share `REASON_*` constants, so a
/// rename touches both sides together; the wording-parity
/// test below pins the full render against the `CliError`
/// `Display` impls. A refused symlink (`ELOOP`) is `Denied`:
/// the open was refused, and retrying the same open cannot
/// fix it.
#[test]
fn every_refusal_literal_maps_onto_its_arm() {
    use std::io::ErrorKind;
    let subject = || CredentialSubject::File {
        path: PathBuf::from("/k/id"),
    };
    let arm = |error: CredentialError| match error {
        CredentialError::Missing { .. } => "Missing",
        CredentialError::Denied { .. } => "Denied",
        CredentialError::Unavailable { .. } => "Unavailable",
        CredentialError::Invalid { .. } => "Invalid",
    };
    for (reason, expected) in [
        (REASON_OWNERSHIP, "Denied"),
        (REASON_MODES, "Denied"),
        (REASON_UNIX_ONLY, "Unavailable"),
        (REASON_NOT_REGULAR, "Invalid"),
        (REASON_SIZE_LIMIT, "Invalid"),
    ] {
        let error = cli_to_loader(
            CliError::Credential {
                path: PathBuf::from("/k/id"),
                reason,
            },
            subject(),
        );
        assert_eq!(
            arm(error),
            expected,
            "reason literal maps by rule: {reason}"
        );
    }
    for (kind, expected) in [
        (ErrorKind::NotFound, "Missing"),
        (ErrorKind::PermissionDenied, "Denied"),
        (ErrorKind::Other, "Unavailable"),
    ] {
        let error = cli_to_loader(
            CliError::Io {
                path: PathBuf::from("/k/id"),
                source: std::io::Error::new(kind, "probe"),
            },
            subject(),
        );
        assert_eq!(arm(error), expected, "IO kind maps by rule: {kind:?}");
    }
    // A refused symlink travels the Denied arm end to end:
    // the reader will not follow it, and the taxonomy calls
    // the refusal what it is (matched by errno, since the
    // stable IO kinds cannot name it). Pinned behaviorally in
    // `tests_cli::credential_symlinks_are_rejected_without_following`,
    // and classified here at the unit level.
    let elooped = cli_to_loader(
        CliError::Io {
            path: PathBuf::from("/k/link"),
            source: std::io::Error::from_raw_os_error(libc::ELOOP),
        },
        CredentialSubject::File {
            path: PathBuf::from("/k/link"),
        },
    );
    assert!(
        matches!(elooped, CredentialError::Denied { .. }),
        "a refused symlink (ELOOP) is Denied: {elooped:?}"
    );
    // The taxonomy identity literal stays equal to the
    // `CliError::IdentityFormat` rendering: either side
    // changing its text fails here.
    assert_eq!(
        CliError::IdentityFormat.to_string(),
        IDENTITY_SHAPE_MSG,
        "taxonomy and CLI name the same identity shape"
    );
}

/// Wording parity: the cutover preserves diagnostics byte for
/// byte. Each case compares the full rendered error and exit
/// code from the loader path against the `CliError` rendering
/// it replaced — a future edit to either `Display` impl or
/// the copied literal drifts here, not silently.
#[test]
fn loader_path_reports_byte_identical_diagnostics() {
    use crate::tests_harness::{write_secret, TempDir};
    use std::os::unix::fs::PermissionsExt;
    let temp = TempDir::new();
    // Missing passphrase file: `{path}: {source}` exactly,
    // exit 2.
    let identity_file = temp.0.join("identity");
    write_secret(&identity_file, [0x11; 32]);
    let missing = temp.0.join("no-passphrase");
    let creds = Credentials {
        identity_file: Some(identity_file.clone()),
        passphrase_file: Some(missing.clone()),
        passphrase_stdin: false,
        credential_backend: None,
        credential_account: None,
    };
    let err = read_credentials(&creds, None).expect_err("missing file must refuse");
    let probe = std::fs::File::open(&missing).expect_err("fixture file stays missing");
    let expected_io = CliError::Io {
        path: missing.clone(),
        source: probe,
    };
    assert_eq!(
        err.to_string(),
        expected_io.to_string(),
        "missing file renders exactly"
    );
    assert_eq!(err.exit_code(), 2);
    // Bad modes: `credential file {path}: {reason}` exactly,
    // exit 2.
    let lax = temp.0.join("lax-passphrase");
    write_secret(&lax, b"hunter2-hunter2\n");
    std::fs::set_permissions(&lax, std::fs::Permissions::from_mode(0o644))
        .expect("test fixture chmod");
    let creds = Credentials {
        identity_file: Some(identity_file.clone()),
        passphrase_file: Some(lax.clone()),
        passphrase_stdin: false,
        credential_backend: None,
        credential_account: None,
    };
    let err = read_credentials(&creds, None).expect_err("bad mode must refuse");
    assert_eq!(
        err.to_string(),
        format!("credential file {}: {REASON_MODES}", lax.display()),
        "mode refusal renders exactly"
    );
    assert_eq!(err.exit_code(), 2);
    // Broken identity shape: the shared literal, exit 2.
    let bad = temp.0.join("bad-identity");
    write_secret(&bad, b"too short");
    let passphrase_file = temp.0.join("passphrase");
    write_secret(&passphrase_file, b"hunter2-hunter2\n");
    let creds = Credentials {
        identity_file: Some(bad),
        passphrase_file: Some(passphrase_file),
        passphrase_stdin: false,
        credential_backend: None,
        credential_account: None,
    };
    let err = read_credentials(&creds, None).expect_err("bad shape must refuse");
    assert_eq!(
        err.to_string(),
        IDENTITY_SHAPE_MSG,
        "shape refusal renders exactly"
    );
    assert_eq!(
        err.to_string(),
        CliError::IdentityFormat.to_string(),
        "matches the CLI variant it replaced"
    );
    assert_eq!(err.exit_code(), 2);
}

/// The OS store reference answers `Unavailable` at the file
/// backend, never an implicit file fallback: the dispatcher
/// routes those to the OS credential-store backend instead. The
/// subject names the account, not a file.
#[test]
fn os_store_reference_is_unavailable_not_a_fallback() {
    let backend = FileBackend;
    let outcome = backend.load(&CredentialRef::OsStore {
        account: "wyrd-drive-alpha".to_string(),
    });
    match outcome {
        Err(CredentialError::Unavailable { subject, detail }) => {
            assert_eq!(
                subject,
                CredentialSubject::Account {
                    account: "wyrd-drive-alpha".to_string()
                },
                "the subject names the account"
            );
            assert!(
                detail.contains("never serves OS store references"),
                "says so: {detail}"
            )
        }
        other => panic!("the file backend never serves the store: {other:?}"),
    }
}

/// The dispatcher routes by reference: file references load
/// through the file backend, store references through the OS
/// backend. The absent store item answers with the OS backend's
/// own refusal — item or store, never the file backend's "never
/// serves OS store references" — which is what distinguishes the
/// two paths on every platform (macOS reads `Missing` here, a
/// headless Linux reads `Unavailable`, both name the store).
#[test]
fn dispatcher_routes_store_references_to_the_os_backend() {
    let missing = CredentialRef::OsStore {
        account: "wyrd-test-dispatcher-absent".to_string(),
    };
    match load_credential(&missing) {
        Err(
            CredentialError::Missing { detail, .. }
            | CredentialError::Unavailable { detail, .. }
            | CredentialError::Denied { detail, .. },
        ) => {
            assert!(
                detail.contains("OS credential"),
                "the OS backend answers, not the file backend: {detail}"
            );
            assert!(
                !detail.contains("never serves"),
                "never the file backend's refusal: {detail}"
            );
        }
        other => panic!("a store reference never reports a file error: {other:?}"),
    }
    let file_missing = CredentialRef::File {
        identity: std::path::PathBuf::from("/keys/no-identity"),
        passphrase: None,
    };
    assert!(
        matches!(
            load_credential(&file_missing),
            Err(CredentialError::Missing { .. })
        ),
        "file references still load through the file backend"
    );
}

/// The store-backed read branch honors a migrated
/// configuration: file flags beside it refuse (one backend per
/// drive), the pipe refuses (the store backend never reads
/// it), and a flag-free invocation reaches the OS backend —
/// whose own refusal (item or store, never the file backend's
/// "never serves") proves the dispatch took the store path.
#[test]
fn store_backed_read_branch_refuses_flags_and_reaches_the_store() {
    use crate::tests_harness::TempDir;
    let temp = TempDir::new();
    let config = LoadedConfig {
        path: temp.0.join("default.toml"),
        relays: Vec::new(),
        identity_file: None,
        passphrase_file: None,
        credential_backend: Some("os-store".to_string()),
        credential_account: Some("wyrd-test-store-branch-absent".to_string()),
    };
    // File flags beside a store configuration: ambiguity, not
    // precedence.
    let creds = Credentials {
        identity_file: Some(PathBuf::from("/k/identity")),
        passphrase_file: None,
        passphrase_stdin: false,
        credential_backend: None,
        credential_account: None,
    };
    let err = read_credentials_with(&creds, Some(&config), Cursor::new(b""))
        .expect_err("file flags beside a store config must refuse");
    assert!(
        err.to_string().contains("one backend per drive"),
        "names the rule: {err}"
    );
    // The pipe beside a store configuration refuses before any
    // store access.
    let creds = Credentials {
        identity_file: None,
        passphrase_file: None,
        passphrase_stdin: true,
        credential_backend: None,
        credential_account: None,
    };
    let err = read_credentials_with(&creds, Some(&config), Cursor::new(b""))
        .expect_err("the pipe beside a store config must refuse");
    assert!(
        err.to_string().contains("OS credential store"),
        "names the store: {err}"
    );
    // Flag-free against a store configuration reaches the OS
    // backend: the absent item answers with the store's own
    // refusal, never the file backend's.
    let creds = Credentials {
        identity_file: None,
        passphrase_file: None,
        passphrase_stdin: false,
        credential_backend: None,
        credential_account: None,
    };
    let err = read_credentials_with(&creds, Some(&config), Cursor::new(b""))
        .expect_err("an absent store item must refuse");
    assert!(
        err.to_string().contains("OS credential"),
        "the OS backend answers: {err}"
    );
    assert!(
        !err.to_string().contains("never serves"),
        "never the file backend's refusal: {err}"
    );
    // Provenance for the store path names the store on both
    // surfaces.
    let provenance = CredentialProvenance {
        from_os_store: true,
        ..CredentialProvenance::default()
    };
    assert_eq!(provenance.identity_word(), "OS credential store");
    assert_eq!(provenance.passphrase_word(), "OS credential store");
}

/// Unlock for a store-backed drive loads the identity from the
/// store and takes the session passphrase from the pipe or the
/// prompt — never from a file. A file flag beside a store
/// unlock refuses before anything loads: one backend per drive.
#[test]
fn unlock_refuses_file_flags_beside_the_store() {
    use crate::tests_harness::TempDir;
    let temp = TempDir::new();
    let config = LoadedConfig {
        path: temp.0.join("default.toml"),
        relays: Vec::new(),
        identity_file: None,
        passphrase_file: None,
        credential_backend: Some("os-store".to_string()),
        credential_account: Some("wyrd-drive-alpha".to_string()),
    };
    let mut creds = unlock_creds();
    creds.passphrase_file = Some(PathBuf::from("/k/passphrase"));
    let err = read_unlock_credentials_with(&creds, Some(&config), Cursor::new(b""), true)
        .expect_err("a passphrase file beside the store must refuse");
    assert!(
        err.to_string().contains("never reads a passphrase file"),
        "names the rule: {err}"
    );
    let mut creds = unlock_creds();
    creds.identity_file = Some(PathBuf::from("/k/identity"));
    let err = read_unlock_credentials_with(&creds, Some(&config), Cursor::new(b""), true)
        .expect_err("an identity file beside the store must refuse");
    assert!(
        err.to_string().contains("omit --identity-file"),
        "names the rule: {err}"
    );
}

/// Unlock for a store-backed drive proves the keystore with
/// what the operator types, not with what the manager reads:
/// the identity loads from the store item, the stored
/// passphrase is dropped, and the piped session passphrase is
/// what unlock returns. Where no platform store answers, the
/// provisioning fails closed with `Unavailable` — that refusal
/// is the assertion there, never a silent pass.
#[test]
fn unlock_for_a_store_drive_uses_the_typed_session_passphrase() {
    use wyrd_credential_store::OsStoreBackend;
    use wyrd_sync::keys::{CredentialLoader, CredentialRef, LoadedCredential};
    let account = "wyrd-test-unlock-typed-session";
    let store_ref = CredentialRef::OsStore {
        account: account.to_string(),
    };
    // Converge the fixture item: provision it, or reuse the
    // identical one a previous run left behind. A differing
    // item refuses rather than being overwritten.
    let material = LoadedCredential {
        identity: DeviceIdentitySecret::from_bytes([0x51; 32]).unwrap(),
        passphrase: Some(Zeroizing::new("stored-manager-pass".to_string())),
    };
    match OsStoreBackend.store(&store_ref, &material) {
        Ok(()) => {}
        Err(CredentialError::Invalid { .. }) => {
            let existing = OsStoreBackend.load(&store_ref).expect("a store item reads");
            assert!(
                existing.identity.device_id() == material.identity.device_id(),
                "a foreign item is never overwritten by a test"
            );
        }
        Err(CredentialError::Unavailable { .. }) => return,
        Err(error) => panic!("the fixture item must provision: {error}"),
    }
    let config = LoadedConfig {
        path: PathBuf::from("default.toml"),
        relays: Vec::new(),
        identity_file: None,
        passphrase_file: None,
        credential_backend: Some("os-store".to_string()),
        credential_account: Some(account.to_string()),
    };
    let creds = Credentials {
        identity_file: None,
        passphrase_file: None,
        passphrase_stdin: true,
        credential_backend: None,
        credential_account: None,
    };
    let (identity, passphrase, provenance) = read_unlock_credentials_with(
        &creds,
        Some(&config),
        Cursor::new(b"typed-session-pass\n"),
        false,
    )
    .expect("store unlock reads the identity and pipes the passphrase");
    assert_eq!(identity.device_id(), material.identity.device_id());
    assert_eq!(
        passphrase.as_str(),
        "typed-session-pass",
        "the session proves with what was typed, never the stored passphrase"
    );
    assert!(provenance.from_os_store);
    assert!(provenance.passphrase_from_stdin);
    OsStoreBackend
        .delete(&store_ref)
        .expect("the fixture item cleans up");
}

/// Service definitions for a store-backed drive name the store
/// account — no file paths, no secrets — so the manager starts
/// the backend the drive is configured for.
#[test]
fn service_definition_names_the_store_account() {
    use crate::tests_harness::TempDir;
    use wyrd_daemon::bootstrap::ServiceCredential;
    let temp = TempDir::new();
    let config = LoadedConfig {
        path: temp.0.join("default.toml"),
        relays: Vec::new(),
        identity_file: None,
        passphrase_file: None,
        credential_backend: Some("os-store".to_string()),
        credential_account: Some("wyrd-drive-alpha".to_string()),
    };
    match resolve_service_credential(&service_creds(false, None), Some(&config), true)
        .expect("a store-backed drive resolves its account")
    {
        ServiceCredential::OsStore { account } => {
            assert_eq!(account, "wyrd-drive-alpha");
        }
        ServiceCredential::File { .. } => {
            panic!("a store configuration never resolves files");
        }
    }
}
