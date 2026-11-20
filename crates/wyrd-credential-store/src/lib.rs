//! OS credential-store backends behind the platform-neutral loader
//! boundary (`docs/contracts/credential-loading.md`,
//! `wyrd_sync::keys::CredentialLoader`).
//!
//! One backend serves every platform with a real store: Keychain on
//! macOS, Secret Service on Linux, both reached through the `keyring`
//! facade. Consumers receive secret *material* — the device identity
//! always, plus the keystore passphrase where the stored item names
//! one — never files, keychain handles, or D-Bus names. The failure
//! taxonomy below is the whole vocabulary this backend answers in.
//!
//! ## Stored-item encoding
//!
//! One item per drive, named by its account label under the fixed
//! service [`OS_SERVICE`]. The item is UTF-8 JSON, bounded like every
//! other credential surface (`MAX_STORED_BYTES`):
//!
//! ```json
//! {"identity_hex":"<64 hex chars>","passphrase":"<optional>"}
//! ```
//!
//! Unknown fields are refused. The identity hex parses through the
//! same scalar gate as the file backend (64 hex chars decoding to 32
//! bytes forming a valid identity scalar); a present passphrase is
//! bounded (`MAX_PASSPHRASE_BYTES`) and returned verbatim — the
//! single trailing-newline strip is the file reader's line
//! discipline, applied once at migration time, never here, so both
//! backends open the same keystore. There is no writer here:
//! provisioning, replacement, and deletion join with the migration
//! slice, which stores post-strip material in exactly this shape.
//!
//! ## Failure mapping
//!
//! The platform's answers map onto the loader taxonomy without
//! fallback and without secret bytes in the evidence:
//!
//! | Platform answer | Loader fact |
//! |---|
//! | No such item | `missing` (re-provision, never auto-create) |
//! | Store reachable, access refused (locked keychain, ACL) | `denied` (fix access, retry) |
//! | Store unreachable (no D-Bus session, no Secret Service, platform failure, future variants) | `unavailable` (retry; never a weaker substitute) |
//! | Item present but shape-broken (non-UTF-8, bad JSON, bad identity, over-long) | `invalid` (fail closed; never guess) |
//!
//! A file reference served here is `invalid`: this backend serves
//! store references only, and a file fallback would cross the
//! backend boundary the reference already chose.
//!
//! ## Platform notes
//!
//! - **macOS**: the launchd agent context shares the login keychain,
//!   so the owner retrieves with no desktop process alive. Keychain
//!   access is requested at registration (a later slice); a locked
//!   or ACL-refused keychain reads as `denied`.
//! - **Linux**: Secret Service in the systemd user-unit context. No
//!   D-Bus session is assumed or synthesized: where none answers,
//!   the drive reads `unavailable`.
//! - **Elsewhere**: Windows credential storage is out of v0.4 with
//!   the Windows platform (`docs/architecture.md`), so non-Unix
//!   targets answer `unavailable` before touching any store.
//!
//! Secret lifetime matches the contract: the retrieved blob lives in
//! a `Zeroizing` buffer from read to parse, the material in
//! `Zeroizing` buffers from parse to return, and nothing renders —
//! [`LoadedCredential`] redacts.
//! Platform-side copies (keychain/D-Bus buffers, crash reports) are
//! best-effort like every backend, not a loader guarantee.

use wyrd_sync::keys::{
    CredentialError, CredentialLoader, CredentialRef, CredentialSubject, DeviceIdentitySecret,
    LoadedCredential,
};
use zeroize::Zeroizing;

/// Fixed service label naming every Wyrd drive item in the platform
/// store. Drives differ by account label, never by service, so one
/// ACL/request covers the backend and items stay enumerable.
pub const OS_SERVICE: &str = "wyrd-drive-credential";

/// Bound on the stored JSON blob: the identity (64 hex chars) plus a
/// bounded passphrase plus JSON framing never legitimately reach it.
/// Mirrors the file reader's posture (bounded reads everywhere); a
/// larger item is shape-broken, not a larger secret.
const MAX_STORED_BYTES: usize = 8192;

/// Bound on a stored passphrase: the same 4096-byte bound the file,
/// stdin, and prompt surfaces enforce, so every backend opens the
/// same keystore. Owns no constant with the CLI — the two readers
/// pin the same literal, and either side changing it fails its own
/// suite, not silently.
const MAX_PASSPHRASE_BYTES: usize = 4096;

/// The OS credential-store backend: serves `CredentialRef::OsStore`
/// through the platform store, nothing else.
pub struct OsStoreBackend;

impl CredentialLoader for OsStoreBackend {
    /// Resolve a store reference to secret material, or a terminal
    /// [`CredentialError`]. Pure retrieval: no registration, no
    /// keystore open, no side effects beyond the backend's own read.
    /// File references are `invalid` here — the reference already
    /// chose its backend, and this one never falls back.
    fn load(&self, reference: &CredentialRef) -> Result<LoadedCredential, CredentialError> {
        match reference {
            CredentialRef::File { identity, .. } => Err(CredentialError::Invalid {
                subject: CredentialSubject::File {
                    path: identity.clone(),
                },
                detail:
                    "the OS credential-store backend serves store references only: no file fallback"
                        .to_string(),
            }),
            CredentialRef::OsStore { account } => load_account(account),
        }
    }
}

/// Read one account's item from the platform store on targets that
/// have one: acquire the entry, then resolve it through the shared
/// [`entry_to_material`] below — the same two steps the tests drive
/// with a mock entry, so the seam is the entry, never a global.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn load_account(account: &str) -> Result<LoadedCredential, CredentialError> {
    let entry = keyring::Entry::new(OS_SERVICE, account)
        .map_err(|error| map_keyring_error(account, error))?;
    entry_to_material(&entry, account)
}

/// Resolve one acquired entry to secret material. The password lands
/// in a `Zeroizing` buffer at once; the parse below moves it into
/// zeroized material before returning. Pure over the entry, so tests
/// pin the whole read path — store answer, taxonomy, and shape gate —
/// without touching any platform store.
fn entry_to_material(
    entry: &keyring::Entry,
    account: &str,
) -> Result<LoadedCredential, CredentialError> {
    let raw = Zeroizing::new(
        entry
            .get_password()
            .map_err(|error| map_keyring_error(account, error))?,
    );
    decode_stored_value(account, &raw)
}

/// Non-Unix targets carry no store: fail closed as `unavailable`
/// before touching any facade, so a Windows build never reports a
/// missing item where the mechanism itself is absent.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn load_account(account: &str) -> Result<LoadedCredential, CredentialError> {
    Err(CredentialError::Unavailable {
        subject: CredentialSubject::Account {
            account: account.to_owned(),
        },
        detail: format!(
            "OS credential storage is not supported on this platform: no item for account {account} can be loaded"
        ),
    })
}

/// The canonical stored item: identity hex always, passphrase where
/// the drive has one (`None` is the session posture — the unlock pipe
/// supplies it downstream, never the loader). No `Debug`: the
/// evidence slot carries the diagnosis, never the bytes.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPayload {
    identity_hex: String,
    passphrase: Option<String>,
}

/// Parse one stored blob into secret material. Every refusal is
/// `invalid` with a secret-free detail: the shape, never the bytes.
fn decode_stored_value(account: &str, blob: &str) -> Result<LoadedCredential, CredentialError> {
    let subject = || CredentialSubject::Account {
        account: account.to_owned(),
    };
    let invalid = |detail: &str| CredentialError::Invalid {
        subject: subject(),
        detail: detail.to_string(),
    };
    if blob.len() > MAX_STORED_BYTES {
        return Err(invalid(&format!(
            "stored credential for account {account} exceeds the {MAX_STORED_BYTES}-byte size limit"
        )));
    }
    // The parse error itself never reaches the detail: `serde_json`
    // diagnostics can quote the offending input, which here is secret
    // material. A malformed item reports its shape, not its bytes.
    let payload: StoredPayload = serde_json::from_str(blob).map_err(|_| {
        invalid(&format!(
            "stored credential for account {account} is not a valid credential record"
        ))
    })?;
    // The file reader trims surrounding whitespace around hex
    // identities; the store mirrors it so migrated material parses
    // exactly like the file it came from.
    let trimmed = payload.identity_hex.trim();
    if trimmed.len() != 64 {
        return Err(invalid(&format!(
            "stored credential for account {account} names an identity that is not 64 hex characters"
        )));
    }
    let mut raw = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(trimmed, &mut *raw).map_err(|_| {
        invalid(&format!(
            "stored credential for account {account} names an identity that is not hex"
        ))
    })?;
    let identity = DeviceIdentitySecret::from_bytes(*raw).map_err(|_| {
        invalid(&format!(
            "stored credential for account {account} names an identity that is not a valid scalar"
        ))
    })?;
    let passphrase = match payload.passphrase {
        None => None,
        Some(text) => {
            if text.len() > MAX_PASSPHRASE_BYTES {
                return Err(invalid(&format!(
                    "stored credential for account {account} exceeds the {MAX_PASSPHRASE_BYTES}-byte passphrase limit"
                )));
            }
            Some(Zeroizing::new(text))
        }
    };
    Ok(LoadedCredential {
        identity,
        passphrase,
    })
}

/// Map one platform answer onto the loader taxonomy. Details carry
/// the account label and the platform's status text — never the item
/// bytes: `BadEncoding` drops its buffer and `Ambiguous` drops its
/// handles uninspected.
fn map_keyring_error(account: &str, error: keyring::Error) -> CredentialError {
    let subject = || CredentialSubject::Account {
        account: account.to_owned(),
    };
    match error {
        keyring::Error::NoEntry => CredentialError::Missing {
            subject: subject(),
            detail: format!("no OS credential-store item for account {account}"),
        },
        keyring::Error::NoStorageAccess(source) => CredentialError::Denied {
            subject: subject(),
            detail: format!("OS credential store refused access for account {account}: {source}"),
        },
        keyring::Error::PlatformFailure(source) => CredentialError::Unavailable {
            subject: subject(),
            detail: format!("OS credential store unreachable for account {account}: {source}"),
        },
        keyring::Error::BadEncoding(_) => CredentialError::Invalid {
            subject: subject(),
            detail: format!("stored credential for account {account} is not UTF-8 text"),
        },
        keyring::Error::TooLong(name, limit) => CredentialError::Invalid {
            subject: subject(),
            detail: format!(
                "OS credential store refused account {account}: {name} exceeds the {limit} limit"
            ),
        },
        keyring::Error::Invalid(name, reason) => CredentialError::Invalid {
            subject: subject(),
            detail: format!("OS credential store refused account {account}: {name} {reason}"),
        },
        keyring::Error::Ambiguous(_) => CredentialError::Invalid {
            subject: subject(),
            detail: format!("more than one OS credential-store item matches account {account}"),
        },
        // The error enum is non-exhaustive: future platform answers
        // fail closed as an unreachable store, never as material.
        other => CredentialError::Unavailable {
            subject: subject(),
            detail: format!("OS credential store failed for account {account}: {other}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wyrd_sync::keys::CredentialSubject;

    /// Canonical test fixture: the exact JSON the future provisioner
    /// will write, built through `serde_json` so escaping stays the
    /// writer's business, never string surgery in a test.
    fn encode_for_tests(identity_hex: &str, passphrase: Option<&str>) -> String {
        let mut item = serde_json::json!({ "identity_hex": identity_hex });
        if let Some(passphrase) = passphrase {
            item["passphrase"] = serde_json::json!(passphrase);
        }
        item.to_string()
    }

    /// A valid identity scalar for fixtures: 0x22 beat the curve the
    /// way 0x11 does in the CLI suite, and 0xFF... never does.
    fn fixture_hex() -> String {
        "22".repeat(32)
    }

    /// The taxonomy pins each platform answer by rule: missing,
    /// denied, unavailable, and invalid are four distinct facts with
    /// the account in a typed slot, and no detail carries secret
    /// bytes — even `BadEncoding`, which arrives holding them.
    #[test]
    fn every_platform_answer_maps_onto_its_arm() {
        let arm = |error: &CredentialError| match error {
            CredentialError::Missing { .. } => "Missing",
            CredentialError::Denied { .. } => "Denied",
            CredentialError::Unavailable { .. } => "Unavailable",
            CredentialError::Invalid { .. } => "Invalid",
        };
        let account = "wyrd-drive-alpha";
        let cases: Vec<(keyring::Error, &str)> = vec![
            (keyring::Error::NoEntry, "Missing"),
            (
                keyring::Error::NoStorageAccess(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "locked",
                ))),
                "Denied",
            ),
            (
                keyring::Error::PlatformFailure(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "no bus",
                ))),
                "Unavailable",
            ),
            (keyring::Error::BadEncoding(vec![0xff, 0xfe]), "Invalid"),
            (keyring::Error::TooLong("user".to_string(), 32), "Invalid"),
            (
                keyring::Error::Invalid("user".to_string(), "empty".to_string()),
                "Invalid",
            ),
            (keyring::Error::Ambiguous(Vec::new()), "Invalid"),
        ];
        for (error, expected) in cases {
            let mapped = map_keyring_error(account, error);
            assert_eq!(arm(&mapped), expected, "platform answer maps by rule");
            match mapped {
                CredentialError::Missing { subject, detail }
                | CredentialError::Denied { subject, detail }
                | CredentialError::Unavailable { subject, detail }
                | CredentialError::Invalid { subject, detail } => {
                    assert_eq!(
                        subject,
                        CredentialSubject::Account {
                            account: account.to_string()
                        },
                        "the subject names the account without parsing"
                    );
                    assert!(
                        detail.contains(account),
                        "the evidence names the account: {detail}"
                    );
                    assert!(
                        !detail.contains("hunter2"),
                        "details never carry secrets: {detail}"
                    );
                }
            }
        }
    }

    /// The shape gate mirrors the file reader: a valid item loads
    /// with the newline already stripped upstream (verbatim here),
    /// session posture carries no passphrase, and every broken shape
    /// — bad JSON, unknown fields, oversize blobs, non-hex, wrong
    /// length, non-scalar, over-long passphrase — is `Invalid`.
    #[test]
    fn stored_shapes_parse_like_the_file_reader() {
        let account = "wyrd-drive-alpha";
        // Happy path: identity plus passphrase, verbatim.
        let blob = encode_for_tests(&fixture_hex(), Some("hunter2-hunter2"));
        let loaded = decode_stored_value(account, &blob).expect("a valid item loads");
        assert_eq!(
            loaded.passphrase.expect("a named item loads one").as_str(),
            "hunter2-hunter2"
        );
        // Session posture: no passphrase item, no empty material.
        let blob = encode_for_tests(&fixture_hex(), None);
        let loaded = decode_stored_value(account, &blob).expect("a session item loads");
        assert!(
            loaded.passphrase.is_none(),
            "no item means no material, not an empty secret"
        );
        // Every broken shape below is Invalid.
        for blob in [
            "not json".to_string(),
            encode_for_tests(&fixture_hex(), Some("ok")) + "trailing",
            serde_json::json!({ "identity_hex": fixture_hex(), "passphrase": "x", "extra": 1 })
                .to_string(),
            "x".repeat(MAX_STORED_BYTES + 1),
            encode_for_tests("too short", Some("x")),
            encode_for_tests(&"zz".repeat(32), Some("x")),
            encode_for_tests(&"ff".repeat(32), Some("x")),
            encode_for_tests(&fixture_hex(), Some(&"x".repeat(MAX_PASSPHRASE_BYTES + 1))),
        ] {
            assert!(
                matches!(
                    decode_stored_value(account, &blob),
                    Err(CredentialError::Invalid { .. })
                ),
                "a broken shape is Invalid: {blob:.64}"
            );
        }
        // Surrounding whitespace around the hex is tolerated, like
        // the file reader's trim — migrated material parses exactly
        // like the file it came from.
        let padded = encode_for_tests(&format!("  {}  ", fixture_hex()), None);
        assert!(
            decode_stored_value(account, &padded).is_ok(),
            "whitespace-padded hex parses"
        );
    }

    /// The backend never serves file references: a file where a
    /// store item belongs is `Invalid` naming the file, never an
    /// implicit read of it.
    #[test]
    fn file_references_are_invalid_not_a_fallback() {
        let backend = OsStoreBackend;
        let path = std::path::PathBuf::from("/keys/identity");
        let outcome = backend.load(&CredentialRef::File {
            identity: path.clone(),
            passphrase: None,
        });
        match outcome {
            Err(CredentialError::Invalid { subject, detail }) => {
                assert_eq!(subject, CredentialSubject::File { path });
                assert!(
                    detail.contains("store references only"),
                    "says so: {detail}"
                );
            }
            other => panic!("a file reference is Invalid here: {other:?}"),
        }
    }

    /// The entry round trip proves the read path talks to the store
    /// facade end to end: store through one entry, resolve through
    /// the shared [`entry_to_material`], then a never-stored entry
    /// reads `Missing` — the re-provision signal, never
    /// auto-creation. One mock entry per case, so no globals and no
    /// cross-test store to race on.
    #[test]
    fn mock_store_round_trip_then_missing() {
        use keyring::mock::MockCredential;
        let account = "wyrd-drive-alpha";
        let blob = encode_for_tests(&fixture_hex(), Some("round-trip-pass"));
        let entry = keyring::Entry::new_with_credential(Box::new(MockCredential::default()));
        entry.set_password(&blob).expect("mock item stores");
        let loaded = entry_to_material(&entry, account).expect("a stored item loads");
        assert_eq!(
            loaded.passphrase.expect("named item loads").as_str(),
            "round-trip-pass"
        );
        let absent = keyring::Entry::new_with_credential(Box::new(MockCredential::default()));
        assert!(
            matches!(
                entry_to_material(&absent, account),
                Err(CredentialError::Missing { .. })
            ),
            "a never-stored entry is Missing"
        );
    }

    /// Secret material never renders, and refused shapes never echo
    /// bytes: the redaction the no-logs rule rests on.
    #[test]
    fn refused_shapes_never_echo_bytes() {
        let account = "wyrd-drive-alpha";
        let secret_pass = "hunter2-hunter2";
        let blob = encode_for_tests("not-hex-at-all", Some(secret_pass));
        let Err(CredentialError::Invalid { detail, .. }) = decode_stored_value(account, &blob)
        else {
            panic!("a broken identity is Invalid");
        };
        assert!(!detail.contains(secret_pass), "no secret echo: {detail}");
        assert!(!detail.contains("not-hex"), "no blob echo: {detail}");
    }
}
