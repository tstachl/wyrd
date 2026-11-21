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
//! backends open the same keystore. Provisioning, replacement, and
//! deletion write this encoding under the loader's commit boundary:
//! every write reads back through the production reader before
//! reporting success.
//!
//! ## Failure mapping
//!
//! The platform's answers map onto the loader taxonomy without
//! fallback and without secret bytes in the evidence:
//!
//! | Platform answer | Loader fact |
//! |---|
//! | No such item | `missing` (re-provision, never auto-create) |
//! | Store reachable, access refused (locked keychain, ACL, macOS auth/interaction/cancel) | `denied` (fix access, retry) |
//! | Store unreachable (no D-Bus session, no Secret Service, other platform failures, future variants) | `unavailable` (retry; never a weaker substitute) |
//! | Item present but shape-broken (non-UTF-8, bad JSON, bad identity, over-long) | `invalid` (fail closed; never guess) |
//!
//! A file reference served here is `invalid`: this backend serves
//! store references only, and a file fallback would cross the
//! backend boundary the reference already chose.
//!
//! ## Platform notes
//!
//! - **macOS**: the launchd agent context shares the login keychain,
//!   so the owner retrieves with no desktop process alive. Items
//!   carry default (NULL) access — no per-application trust list,
//!   no biometric flags — so the first agent read needs no prior
//!   GUI visit while the login keychain is unlocked (verified by
//!   hand with the two-phase probe); a locked keychain reads as
//!   `denied`.
//! - **Linux**: Secret Service in the systemd user-unit context. No
//!   D-Bus session is assumed or synthesized: where none answers,
//!   the drive reads `unavailable`.
//! - **Elsewhere**: Windows credential storage is out of v0.4 with
//!   the Windows platform (`docs/architecture.md`), so non-Unix
//!   targets answer `unavailable` before touching any store.
//!
//! Secret lifetime matches the contract: the retrieved blob lives in
//! a `Zeroizing` buffer from read to parse, the parsed fields in
//! `Zeroizing` wrappers from parse to return, and nothing renders —
//! [`LoadedCredential`] redacts. Parser-transient and platform-side
//! copies (serde buffers, keychain/D-Bus buffers, crash reports) are
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

/// Bound on the stored JSON blob: framing plus the identity plus
/// the worst-case encoding of a bounded passphrase. `serde_json`
/// escapes quotes, backslashes (2 bytes) and control characters (6
/// bytes), so a legal 4096-byte passphrase can occupy 24576 bytes on
/// the wire; the bound admits exactly that plus framing slack, and a
/// larger item is shape-broken, not a larger secret. The platform
/// fetch itself is not bounded — `keyring` exposes no streaming
/// read — so the refusal lands after the store answers; a corrupt
/// or hostile item can allocate up to this bound before refusing.
const MAX_STORED_BYTES: usize = 24576 + 256;

/// Bound on a stored passphrase: the same 4096-byte bound the file,
/// stdin, and prompt surfaces enforce, so every backend opens the
/// same keystore. Shared with the CLI's stdin reader: one literal,
/// so a divergence fails compilation, not silently. (The terminal
/// prompt and the identity writer keep their own literals; see the
/// CLI comment at the import site.)
pub const MAX_PASSPHRASE_BYTES: usize = 4096;

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

    /// Provision a fresh store item under the commit boundary:
    /// validate in memory, refuse where an item already exists
    /// (`Invalid` — provisioning never overwrites), write, then
    /// read back through the production reader and compare identity
    /// (by device id) and passphrase byte for byte. Success is
    /// reported only where the read-back matches. File references
    /// are `invalid`, exactly as in [`CredentialLoader::load`].
    fn store(
        &self,
        reference: &CredentialRef,
        material: &LoadedCredential,
    ) -> Result<(), CredentialError> {
        match reference {
            CredentialRef::File { identity, .. } => Err(CredentialError::Invalid {
                subject: CredentialSubject::File {
                    path: identity.clone(),
                },
                detail:
                    "the OS credential-store backend serves store references only: no file fallback"
                        .to_string(),
            }),
            CredentialRef::OsStore { account } => store_account(account, material),
        }
    }

    /// Replace the store item under the same commit boundary as
    /// [`CredentialLoader::store`]: overwrite, read back, and
    /// report success only where the read-back matches. Upserts, so
    /// retried callers stay idempotent; use `store` where
    /// overwriting would mask a bug.
    fn replace(
        &self,
        reference: &CredentialRef,
        material: &LoadedCredential,
    ) -> Result<(), CredentialError> {
        match reference {
            CredentialRef::File { identity, .. } => Err(CredentialError::Invalid {
                subject: CredentialSubject::File {
                    path: identity.clone(),
                },
                detail:
                    "the OS credential-store backend serves store references only: no file fallback"
                        .to_string(),
            }),
            CredentialRef::OsStore { account } => replace_account(account, material),
        }
    }

    /// Delete the store item. Answers `Missing` where no item
    /// exists, and verifies the removal with a read-back where the
    /// store is reachable — a still-present or unverifiable item is
    /// `Invalid`, never a claimed success.
    fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
        match reference {
            CredentialRef::File { identity, .. } => Err(CredentialError::Invalid {
                subject: CredentialSubject::File {
                    path: identity.clone(),
                },
                detail:
                    "the OS credential-store backend serves store references only: no file fallback"
                        .to_string(),
            }),
            CredentialRef::OsStore { account } => delete_account(account),
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

/// Encode secret material into the canonical stored item: identity
/// hex always, passphrase where the drive has one (`None` is the
/// session posture — no item, never an empty secret). The passphrase
/// travels verbatim — the file reader's trailing-newline strip ran
/// once at migration time, never here — under the same bound every
/// other surface enforces, so every backend opens the same keystore.
/// An over-long passphrase is `invalid` naming the bound, never the
/// bytes. Escaping stays the writer's business (no `serde_json::Value`
/// holding plaintext on the way through): the blob is built straight
/// into a `Zeroizing` buffer and returned scrubbed on drop.
fn encode_stored_value(
    account: &str,
    identity: &DeviceIdentitySecret,
    passphrase: Option<&Zeroizing<String>>,
) -> Result<Zeroizing<String>, CredentialError> {
    let subject = || CredentialSubject::Account {
        account: account.to_owned(),
    };
    if let Some(text) = passphrase {
        if text.len() > MAX_PASSPHRASE_BYTES {
            return Err(CredentialError::Invalid {
                subject: subject(),
                detail: format!(
                    "passphrase for account {account} exceeds the {MAX_PASSPHRASE_BYTES}-byte size limit"
                ),
            });
        }
    }
    // Escaping stays the writer's business: build the JSON straight
    // into the `Zeroizing` return, never through a `serde_json::Value`
    // whose `String`s would drop unscrubbed.
    let identity_hex = identity.to_hex();
    let mut blob = Zeroizing::new(String::with_capacity(80));
    blob.push_str("{\"identity_hex\":\"");
    blob.push_str(identity_hex.as_str());
    blob.push('"');
    if let Some(text) = passphrase {
        blob.push_str(",\"passphrase\":\"");
        json_escape_into(&mut blob, text.as_str());
        blob.push('"');
    }
    blob.push('}');
    Ok(blob)
}

/// Append one JSON string body escaped: `"`, `\`, and control
/// characters take short escapes where they have them, `\u00XX`
/// elsewhere — the same language the decoder accepts, so the
/// round trip is byte-identical through any compliant parser.
fn json_escape_into(out: &mut Zeroizing<String>, text: &str) {
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            character if (character as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(&mut **out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
}

/// The write-half commit boundary, shared by provisioning and
/// replacement: write, then read back through the production reader
/// ([`entry_to_material`]) and compare identity (by device id — the
/// public output, never the bytes) and passphrase byte for byte. A
/// mismatch is `invalid`: the write is never reported successful
/// unverified.
fn verify_round_trip(
    entry: &keyring::Entry,
    account: &str,
    material: &LoadedCredential,
) -> Result<(), CredentialError> {
    let loaded = entry_to_material(entry, account)?;
    if loaded.identity.device_id() != material.identity.device_id()
        || loaded.passphrase.as_deref() != material.passphrase.as_deref()
    {
        return Err(CredentialError::Invalid {
            subject: CredentialSubject::Account {
                account: account.to_owned(),
            },
            detail: format!(
                "OS credential-store item for account {account} did not read back: refusing unverified success"
            ),
        });
    }
    Ok(())
}

/// Provisioning on an acquired entry: refuse where an item already
/// exists, then write under [`verify_round_trip`]. Pure over the
/// entry, so tests pin the whole write path — guard, taxonomy, and
/// read-back — with a mock entry, never touching a platform store.
/// The account-level [`store_account`] below adds only the entry
/// acquisition.
fn provision_on_entry(
    entry: &keyring::Entry,
    account: &str,
    blob: &Zeroizing<String>,
    material: &LoadedCredential,
) -> Result<(), CredentialError> {
    match entry.get_password() {
        Ok(existing) => {
            // The guard's read lands in `Zeroizing` at once: the
            // fetched item — another drive's passphrase — scrubs on
            // drop instead of lingering as a plain `String`.
            let _scrub = Zeroizing::new(existing);
            return Err(CredentialError::Invalid {
                subject: CredentialSubject::Account {
                    account: account.to_owned(),
                },
                detail: format!(
                    "OS credential-store item for account {account} already exists: replace, never re-provision"
                ),
            });
        }
        Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(map_keyring_error(account, error)),
    }
    entry
        .set_password(blob)
        .map_err(|error| map_keyring_error(account, error))?;
    verify_round_trip(entry, account, material)
}

/// Replacement on an acquired entry: overwrite, then
/// [`verify_round_trip`]. Upserts, so retried callers stay
/// idempotent.
fn replace_on_entry(
    entry: &keyring::Entry,
    account: &str,
    blob: &Zeroizing<String>,
    material: &LoadedCredential,
) -> Result<(), CredentialError> {
    entry
        .set_password(blob)
        .map_err(|error| map_keyring_error(account, error))?;
    verify_round_trip(entry, account, material)
}

/// Deletion on an acquired entry: `Missing` where nothing exists,
/// then a read-back proving the removal. A still-present or
/// unverifiable item is `invalid`.
fn delete_on_entry(entry: &keyring::Entry, account: &str) -> Result<(), CredentialError> {
    entry
        .delete_credential()
        .map_err(|error| map_keyring_error(account, error))?;
    match entry_to_material(entry, account) {
        Err(CredentialError::Missing { .. }) => Ok(()),
        Err(error) => Err(CredentialError::Invalid {
            subject: CredentialSubject::Account {
                account: account.to_owned(),
            },
            detail: format!(
                "OS credential-store item for account {account} is removed but unverifiable: {error}"
            ),
        }),
        Ok(_) => Err(CredentialError::Invalid {
            subject: CredentialSubject::Account {
                account: account.to_owned(),
            },
            detail: format!(
                "OS credential-store item for account {account} is still present after deletion"
            ),
        }),
    }
}

/// Provision a fresh store item: refuse where one already exists,
/// then write under [`verify_round_trip`]. The pre-write read is
/// the existence guard and the reachability proof at once — an
/// unreachable store fails here as `unavailable`, never as a write
/// against the void.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn store_account(account: &str, material: &LoadedCredential) -> Result<(), CredentialError> {
    let blob = encode_stored_value(account, &material.identity, material.passphrase.as_ref())?;
    let entry = keyring::Entry::new(OS_SERVICE, account)
        .map_err(|error| map_keyring_error(account, error))?;
    provision_on_entry(&entry, account, &blob, material)
}

/// Replace the store item under the same boundary: overwrite, then
/// [`verify_round_trip`]. Upserts, so retried callers stay
/// idempotent.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn replace_account(account: &str, material: &LoadedCredential) -> Result<(), CredentialError> {
    let blob = encode_stored_value(account, &material.identity, material.passphrase.as_ref())?;
    let entry = keyring::Entry::new(OS_SERVICE, account)
        .map_err(|error| map_keyring_error(account, error))?;
    replace_on_entry(&entry, account, &blob, material)
}

/// Delete the store item: `Missing` where nothing exists, then a
/// read-back proving the removal where the store is reachable. A
/// still-present or unverifiable item is `invalid`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn delete_account(account: &str) -> Result<(), CredentialError> {
    let entry = keyring::Entry::new(OS_SERVICE, account)
        .map_err(|error| map_keyring_error(account, error))?;
    delete_on_entry(&entry, account)
}

/// Off-store targets fail the write half closed like the read half:
/// no mechanism, no item — never a silent file fallback.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn store_account(account: &str, _material: &LoadedCredential) -> Result<(), CredentialError> {
    Err(CredentialError::Unavailable {
        subject: CredentialSubject::Account {
            account: account.to_owned(),
        },
        detail: format!(
            "OS credential storage is not supported on this platform: no item for account {account} can be stored"
        ),
    })
}

/// Off-store targets fail the write half closed like the read half.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn replace_account(account: &str, _material: &LoadedCredential) -> Result<(), CredentialError> {
    Err(CredentialError::Unavailable {
        subject: CredentialSubject::Account {
            account: account.to_owned(),
        },
        detail: format!(
            "OS credential storage is not supported on this platform: no item for account {account} can be replaced"
        ),
    })
}

/// Off-store targets fail the write half closed like the read half.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn delete_account(account: &str) -> Result<(), CredentialError> {
    Err(CredentialError::Unavailable {
        subject: CredentialSubject::Account {
            account: account.to_owned(),
        },
        detail: format!(
            "OS credential storage is not supported on this platform: no item for account {account} can be deleted"
        ),
    })
}

/// The canonical stored item: identity hex always, passphrase where
/// the drive has one (`None` is the session posture — the unlock pipe
/// supplies it downstream, never the loader). Fields land in
/// `Zeroizing` wrappers at the serde boundary, so a refusal below
/// scrubs what it held. No `Debug`: the evidence slot carries the
/// diagnosis, never the bytes.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPayload {
    identity_hex: Zeroizing<String>,
    passphrase: Option<Zeroizing<String>>,
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
    // The file reader trims surrounding ASCII whitespace around
    // hex identities; the store mirrors it exactly (not Unicode
    // `trim`, which the file reader never accepted) so migrated
    // material parses exactly like the file it came from.
    let trimmed = payload
        .identity_hex
        .trim_matches(|character: char| character.is_ascii_whitespace());
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
        // The over-long text is already `Zeroizing`, so the refusal
        // scrubs it on the way out — unlike a plain `String`, which
        // `drop` would leave in memory.
        None => None,
        Some(text) => {
            if text.len() > MAX_PASSPHRASE_BYTES {
                return Err(invalid(&format!(
                    "stored credential for account {account} exceeds the {MAX_PASSPHRASE_BYTES}-byte passphrase limit"
                )));
            }
            Some(text)
        }
    };
    Ok(LoadedCredential {
        identity,
        passphrase,
    })
}

/// Keychain authentication/ACL refusal and interaction outcomes
/// (`SecBase.h`): the access facts keyring's status table never
/// names, so they arrive inside the facade's opaque platform
/// failure. macOS only.
#[cfg(target_os = "macos")]
const ERR_SEC_AUTH_FAILED: i32 = -25293;
/// Locked keychain with no UI to unlock through (the launchd
/// context): `SecBase.h` `errSecInteractionNotAllowed`.
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
/// The operator refused the access prompt: `SecBase.h`
/// `errSecUserCanceled`. A refusal, not an outage.
#[cfg(target_os = "macos")]
const ERR_SEC_USER_CANCELED: i32 = -128;

/// Map one platform answer onto the loader taxonomy. Details carry
/// the account label and the platform's status text — never the item
/// bytes: `BadEncoding` scrubs its buffer before answering and
/// `Ambiguous` drops its handles uninspected.
fn map_keyring_error(account: &str, error: keyring::Error) -> CredentialError {
    use zeroize::Zeroize;
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
        keyring::Error::PlatformFailure(source) => {
            // The facade funnels every unlisted Security status here,
            // including the authentication refusal keyring never
            // names: an ACL or locked-keychain answer is an access
            // fact (`denied`), not an unreachable store. macOS reads
            // the status off the downcast source (the box owns a
            // `'static` object, which is what the downcast needs);
            // elsewhere the facade's own `NoStorageAccess` is the
            // only access signal, so nothing is.
            #[cfg(target_os = "macos")]
            let denied = source
                .downcast_ref::<security_framework::base::Error>()
                .is_some_and(|error| {
                    matches!(
                        error.code(),
                        ERR_SEC_AUTH_FAILED
                            | ERR_SEC_INTERACTION_NOT_ALLOWED
                            | ERR_SEC_USER_CANCELED
                    )
                });
            #[cfg(not(target_os = "macos"))]
            let denied = false;
            if denied {
                CredentialError::Denied {
                    subject: subject(),
                    detail: format!(
                        "OS credential store refused access for account {account}: {source}"
                    ),
                }
            } else {
                CredentialError::Unavailable {
                    subject: subject(),
                    detail: format!(
                        "OS credential store unreachable for account {account}: {source}"
                    ),
                }
            }
        }
        // The buffer holds the raw stored item — identity plus
        // passphrase — so it is scrubbed before the refusal, never
        // dropped as plaintext.
        keyring::Error::BadEncoding(mut raw) => {
            raw.zeroize();
            CredentialError::Invalid {
                subject: subject(),
                detail: format!("stored credential for account {account} is not UTF-8 text"),
            }
        }
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
            // The attached buffer is the raw stored item — this one
            // even names the passphrase, so the detail must not.
            (
                keyring::Error::BadEncoding(b"hunter2-hunter2\xff".to_vec()),
                "Invalid",
            ),
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

    /// macOS names the Keychain's authentication and interaction
    /// refusals inside the facade's opaque platform failure:
    /// `errSecAuthFailed`, `errSecInteractionNotAllowed` (locked
    /// keychain with no UI — the launchd case), and
    /// `errSecUserCanceled` are access facts (`denied`), while any
    /// other Security status stays `unavailable`. Keyring's own table
    /// never names these codes, so the backend reads them off the
    /// downcast source.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_access_refusals_are_denied_not_unavailable() {
        let account = "wyrd-drive-alpha";
        use security_framework::base::Error as SecError;
        for code in [-25293, -25308, -128] {
            let denied = map_keyring_error(
                account,
                keyring::Error::PlatformFailure(Box::new(SecError::from(code))),
            );
            match denied {
                CredentialError::Denied { subject, detail } => {
                    assert_eq!(
                        subject,
                        CredentialSubject::Account {
                            account: account.to_string()
                        }
                    );
                    assert!(detail.contains(account), "names the account: {detail}");
                }
                other => panic!("status {code} is Denied: {other:?}"),
            }
        }
        // Any other Security status is still an unreachable store.
        let unreachable = map_keyring_error(
            account,
            keyring::Error::PlatformFailure(Box::new(SecError::from(-1))),
        );
        assert!(
            matches!(unreachable, CredentialError::Unavailable { .. }),
            "other statuses stay Unavailable: {unreachable:?}"
        );
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
        // A heavily-escaped passphrase still loads: 4096 backslashes
        // escape to two bytes each, so the blob exceeds the old
        // bound but stays inside the encoding-aware one — the file
        // backend's passphrase is the store backend's passphrase
        // byte for byte.
        let escaped = "\\".repeat(MAX_PASSPHRASE_BYTES);
        let blob = encode_for_tests(&fixture_hex(), Some(&escaped));
        let loaded = decode_stored_value(account, &blob).expect("an escaped item loads");
        assert_eq!(
            loaded.passphrase.expect("named item loads").as_str(),
            escaped.as_str(),
            "escaping never changes the material"
        );
        // The derivation's true worst case: control characters
        // escape to six bytes each, 24576 plus framing against the
        // 24832 bound. This pins the bound the derivation rests on,
        // not just the 2x case above.
        let worst = "\u{0}".repeat(MAX_PASSPHRASE_BYTES);
        let blob = encode_for_tests(&fixture_hex(), Some(&worst));
        let loaded = decode_stored_value(account, &blob).expect("a worst-case item loads");
        assert_eq!(
            loaded.passphrase.expect("named item loads").as_str(),
            worst.as_str(),
            "the 6x expansion is admitted"
        );
        // Surrounding ASCII whitespace around the hex is tolerated,
        // like the file reader's trim — migrated material parses
        // exactly like the file it came from.
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

    /// The encoder is the decoder's inverse: identity hex always,
    /// passphrase where named, session posture with none — and the
    /// encoded item parses through the production reader to the
    /// same device id and passphrase. The bound is shared: a
    /// passphrase the pipe accepts is one the store accepts, and an
    /// over-long one is `Invalid` naming the bound, never the bytes.
    #[test]
    fn encode_is_the_decoder_inverse_within_bounds() {
        let account = "wyrd-drive-alpha";
        let identity = DeviceIdentitySecret::from_bytes(
            hex::decode(fixture_hex())
                .expect("fixture hex decodes")
                .try_into()
                .expect("32 bytes"),
        )
        .expect("fixture scalar");
        for passphrase in [Some(Zeroizing::new("hunter2-hunter2".to_string())), None] {
            let blob =
                encode_stored_value(account, &identity, passphrase.as_ref()).expect("encodes");
            let loaded = decode_stored_value(account, &blob).expect("a stored item loads");
            assert_eq!(loaded.identity.device_id(), identity.device_id());
            assert_eq!(loaded.passphrase.as_deref(), passphrase.as_deref());
        }
        let overlong = Zeroizing::new("x".repeat(MAX_PASSPHRASE_BYTES + 1));
        match encode_stored_value(account, &identity, Some(&overlong)) {
            Err(CredentialError::Invalid { subject, detail }) => {
                assert_eq!(
                    subject,
                    CredentialSubject::Account {
                        account: account.to_string()
                    }
                );
                assert!(detail.contains("4096"), "names the bound: {detail}");
                assert!(
                    !detail.contains(&"x".repeat(16)),
                    "no secret echo: {detail}"
                );
            }
            other => panic!("an over-long passphrase is Invalid: {other:?}"),
        }
    }

    /// The production writer pins its own escape branches: a
    /// quote, a backslash, a newline and a NUL round-trip
    /// byte-identical through [`encode_stored_value`] into the
    /// production reader. The decoder tests prove serde output
    /// parses; this proves the writer emits what the decoder
    /// accepts — dropping any escape branch fails here, with the
    /// escaped spellings asserted directly.
    #[test]
    fn encoder_escape_branches_round_trip_verbatim() {
        let account = "wyrd-drive-alpha";
        let identity = DeviceIdentitySecret::from_bytes(
            hex::decode(fixture_hex())
                .expect("fixture hex decodes")
                .try_into()
                .expect("32 bytes"),
        )
        .expect("fixture scalar");
        let passphrase = "all-\"-\\-\n-\u{0}-together";
        let material = LoadedCredential {
            identity,
            passphrase: Some(Zeroizing::new(passphrase.to_string())),
        };
        let blob = encode_stored_value(account, &material.identity, material.passphrase.as_ref())
            .expect("encodes");
        for escaped in ["\\\"", "\\\\", "\\n", "\\u0000"] {
            assert!(
                blob.contains(escaped),
                "the writer emits the {escaped} escape: {}",
                blob.as_str()
            );
        }
        let loaded = decode_stored_value(account, &blob).expect("production output parses");
        assert_eq!(loaded.identity.device_id(), material.identity.device_id());
        assert_eq!(
            loaded.passphrase.expect("named item loads").as_str(),
            passphrase,
            "every escaped byte round-trips"
        );
    }

    /// The write half honors the commit boundary on a mock entry:
    /// provision stores and reads back, a second provision is
    /// `Invalid` (never an overwrite), replacement overwrites the
    /// same item, deletion removes it to `Missing`, and deleting
    /// nothing is `Missing` — never a claimed success.
    #[test]
    fn write_half_obeys_guard_and_commit_boundary() {
        use keyring::mock::MockCredential;
        let account = "wyrd-drive-alpha";
        let material = |passphrase: Option<&str>| LoadedCredential {
            identity: DeviceIdentitySecret::from_bytes(
                hex::decode(fixture_hex())
                    .expect("fixture hex decodes")
                    .try_into()
                    .expect("32 bytes"),
            )
            .expect("fixture scalar"),
            passphrase: passphrase.map(|text| Zeroizing::new(text.to_string())),
        };
        // Provision through the entry-level helper on one shared
        // mock entry: the trait-level `load` would acquire a real
        // platform entry (absent on headless CI), so the read-back
        // here goes through the same production reader the commit
        // boundary uses.
        let entry = keyring::Entry::new_with_credential(Box::new(MockCredential::default()));
        let first = material(Some("first-pass"));
        let blob = encode_stored_value(account, &first.identity, first.passphrase.as_ref())
            .expect("encodes");
        provision_on_entry(&entry, account, &blob, &first).expect("fresh provision stores");
        let loaded = entry_to_material(&entry, account).expect("provisioned item loads");
        assert_eq!(loaded.identity.device_id(), first.identity.device_id());
        assert_eq!(
            loaded.passphrase.expect("named item loads").as_str(),
            "first-pass"
        );
        // Provision again: Invalid, never an overwrite.
        let second = material(Some("second-pass"));
        let blob2 = encode_stored_value(account, &second.identity, second.passphrase.as_ref())
            .expect("encodes");
        match provision_on_entry(&entry, account, &blob2, &second) {
            Err(CredentialError::Invalid { detail, .. }) => assert!(
                detail.contains("already exists"),
                "names the guard: {detail}"
            ),
            other => panic!("re-provisioning is Invalid: {other:?}"),
        }
        // Replacement overwrites under the same boundary.
        replace_on_entry(&entry, account, &blob2, &second).expect("replacement overwrites");
        let loaded = entry_to_material(&entry, account).expect("replaced item loads");
        assert_eq!(
            loaded.passphrase.expect("named item loads").as_str(),
            "second-pass"
        );
        // Deletion removes to Missing.
        delete_on_entry(&entry, account).expect("deletion removes");
        assert!(
            matches!(
                entry_to_material(&entry, account),
                Err(CredentialError::Missing { .. })
            ),
            "a deleted item reads Missing"
        );
        // Deleting nothing is Missing, never success.
        assert!(
            matches!(
                delete_on_entry(&entry, account),
                Err(CredentialError::Missing { .. })
            ),
            "deleting nothing is Missing"
        );
    }

    /// The trait-level write half refuses file references like the
    /// read half does: `Invalid` naming the file, never a write to
    /// the wrong backend.
    #[test]
    fn trait_writes_refuse_file_references() {
        let backend = OsStoreBackend;
        let path = std::path::PathBuf::from("/keys/identity");
        let reference = CredentialRef::File {
            identity: path.clone(),
            passphrase: None,
        };
        let material = LoadedCredential {
            identity: DeviceIdentitySecret::from_bytes(
                hex::decode(fixture_hex())
                    .expect("fixture hex decodes")
                    .try_into()
                    .expect("32 bytes"),
            )
            .expect("fixture scalar"),
            passphrase: None,
        };
        for outcome in [
            backend.store(&reference, &material),
            backend.replace(&reference, &material),
            backend.delete(&reference),
        ] {
            match outcome {
                Err(CredentialError::Invalid { subject, detail }) => {
                    assert_eq!(subject, CredentialSubject::File { path: path.clone() });
                    assert!(
                        detail.contains("store references only"),
                        "says so: {detail}"
                    );
                }
                other => panic!("a file reference is Invalid here: {other:?}"),
            }
        }
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

    /// Manual service-context probe: stores an item through the
    /// real platform store, loads it back through the backend, then
    /// deletes it. Ignored by default — it touches the login
    /// keychain / Secret Service — and run by hand where it matters.
    /// Never runs in CI.
    ///
    /// Two modes. Unset `WYRD_PROBE_PHASE` keeps the legacy
    /// single-process round trip (login session convenience). Split
    /// mode proves first unlock without a prior GUI visit:
    /// - `store` in the login session / GUI context: converges any
    ///   leftover item, then stores through the backend commit
    ///   boundary (including its in-context read-back, exactly like
    ///   production bootstrap) and exits WITHOUT a retrieval — the
    ///   item is provisioned, never yet read out-of-context.
    /// - `load` under the launchd agent / systemd user unit with no
    ///   terminal: loads through the backend (this is the first
    ///   out-of-context retrieval), asserts the material, deletes,
    ///   and asserts `Missing`.
    ///
    /// Both phases share the account in `WYRD_PROBE_ACCOUNT`
    /// (default: pid-based, for the single-process mode); split runs
    /// must export the same account in both phases. A run between
    /// phases leaves the probe item behind by design — re-running
    /// the store phase converges it.
    ///
    /// Hand run (macOS): `WYRD_PROBE_ACCOUNT=wyrd-probe-<n>
    /// cargo test -p wyrd-credential-store --lib -- --ignored
    /// real_store_probe_round_trip` with `WYRD_PROBE_PHASE=store`,
    /// then the same binary under a `RunAtLoad` LaunchAgent plist
    /// with `WYRD_PROBE_PHASE=load` (plus `WYRD_PROBE_ACCOUNT`)
    /// and no terminal; the agent log must show the loaded
    /// material and the post-delete `Missing`.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[ignore = "touches the real platform store; run by hand per the contract's service-context clause"]
    #[test]
    fn real_store_probe_round_trip() {
        /// Deletes the probe item on drop, so a panic between store
        /// and delete cannot leave a residue a later run's distinct
        /// account would never clear. Split-mode store takes no
        /// guard: the item must survive for the load phase.
        struct ProbeGuard<'a> {
            entry: &'a keyring::Entry,
        }
        impl Drop for ProbeGuard<'_> {
            fn drop(&mut self) {
                let _ = self.entry.delete_credential();
            }
        }
        let backend = OsStoreBackend;
        let account = std::env::var("WYRD_PROBE_ACCOUNT")
            .unwrap_or_else(|_| format!("wyrd-probe-{}-{}", std::process::id(), "service-context"));
        let reference = || CredentialRef::OsStore {
            account: account.clone(),
        };
        let material = LoadedCredential {
            identity: DeviceIdentitySecret::from_bytes(
                hex::decode(fixture_hex())
                    .expect("fixture hex decodes")
                    .try_into()
                    .expect("32 bytes"),
            )
            .expect("fixture scalar"),
            passphrase: Some(Zeroizing::new("probe-pass".to_string())),
        };
        let phase = std::env::var("WYRD_PROBE_PHASE").unwrap_or_default();
        match phase.as_str() {
            // Registration: converge any leftover, store through the
            // commit boundary, exit with the item in place and never
            // retrieved out-of-context.
            "store" => {
                match backend.delete(&reference()) {
                    Ok(()) | Err(CredentialError::Missing { .. }) => {}
                    Err(error) => panic!("a leftover probe item deletes: {error:?}"),
                }
                backend
                    .store(&reference(), &material)
                    .expect("probe item stores through the commit boundary");
            }
            // First unlock: the first out-of-context retrieval,
            // then cleanup with the post-delete `Missing` proof.
            "load" => {
                let entry = keyring::Entry::new(OS_SERVICE, &account).expect("probe entry builds");
                let _guard = ProbeGuard { entry: &entry };
                let loaded = backend
                    .load(&reference())
                    .expect("the owner retrieves its material with no prior GUI visit");
                assert_eq!(
                    loaded.passphrase.expect("named item loads").as_str(),
                    "probe-pass"
                );
                backend.delete(&reference()).expect("probe item cleans up");
                assert!(
                    matches!(
                        backend.load(&reference()),
                        Err(CredentialError::Missing { .. })
                    ),
                    "a deleted item reads Missing, never stale material"
                );
            }
            // Single-process round trip: the login-session
            // convenience path, unchanged.
            _ => {
                let blob = encode_for_tests(&fixture_hex(), Some("probe-pass"));
                let entry = keyring::Entry::new(OS_SERVICE, &account).expect("probe entry builds");
                // A previous interrupted run must not poison this one.
                let _ = entry.delete_credential();
                let _guard = ProbeGuard { entry: &entry };
                entry.set_password(&blob).expect("probe item stores");
                let loaded = backend
                    .load(&reference())
                    .expect("the owner retrieves its material");
                assert_eq!(
                    loaded.passphrase.expect("named item loads").as_str(),
                    "probe-pass"
                );
                entry.delete_credential().expect("probe item cleans up");
                assert!(
                    matches!(
                        backend.load(&reference()),
                        Err(CredentialError::Missing { .. })
                    ),
                    "a deleted item reads Missing, never stale material"
                );
            }
        }
    }
}
