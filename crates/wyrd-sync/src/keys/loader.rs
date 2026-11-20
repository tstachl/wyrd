//! Platform-neutral credential-loading boundary
//! (`docs/contracts/credential-loading.md`).
//!
//! Consumers receive secret *material* — the device identity secret
//! plus the keystore passphrase — never files, modes, keychain
//! handles, or OS facilities. Backends implement how that material
//! is kept and retrieved; the failure taxonomy below is the whole
//! vocabulary a backend may answer in, so callers diagnose without
//! knowing which backend served them.
//!
//! Backends: the hardened Unix file reader (implemented in
//! `wyrd-cli` as `FileBackend`, conforming to this trait); the OS
//! credential-store backend (implemented in
//! `wyrd-credential-store` as `OsStoreBackend`: Keychain on macOS,
//! Secret Service on Linux). Adding a backend changes no secret
//! format and no caller.

use std::path::PathBuf;

use thiserror::Error;
use zeroize::Zeroizing;

use super::device::DeviceIdentitySecret;

/// Stable selector for one drive's credential. A reference names
/// the backend and the item to load — never secret bytes. Paths
/// are selectors here, resolved through the same hardening the
/// file backend always applied; the item's protection never
/// depends on which surface named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// Files on disk: the identity file always, the passphrase
    /// file only where one is configured (`None` is the
    /// session-posture reference — unlock's passphrase arrives
    /// via the pipe or prompt, never the loader, so the
    /// material loads with no passphrase).
    File {
        /// The identity file to read and harden.
        identity: PathBuf,
        /// The passphrase file to read and harden, if configured.
        passphrase: Option<PathBuf>,
    },
    /// One item (identity plus passphrase) in the platform's
    /// credential store, named by its account label. Served by the
    /// OS credential-store backend (`wyrd-credential-store`):
    /// Keychain on macOS, Secret Service on Linux.
    OsStore {
        /// The store account label naming the drive's item.
        account: String,
    },
}

/// What failed to load, for the taxonomy's evidence slot: the
/// file named, or the store account named. Typed so callers map
/// without parsing prose; the human-readable evidence rides in
/// the variant's `detail` alongside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSubject {
    /// A file the backend tried to read.
    File {
        /// The file that was read, or refused.
        path: PathBuf,
    },
    /// A store account the backend tried to resolve.
    Account {
        /// The account label that was resolved, or refused.
        account: String,
    },
}

/// One drive's secret material as the loader returns it: the
/// device identity always, the keystore passphrase where the
/// reference names one (`None` is the session posture — the
/// unlock pipe or prompt supplies it downstream, never the
/// loader). Members scrub on drop and the rendering is redacted;
/// never logged, never placed in argv or configuration.
pub struct LoadedCredential {
    /// The device's Nostr identity secret (local-key custody).
    pub identity: DeviceIdentitySecret,
    /// The passphrase unwrapping the drive's root key, if the
    /// reference names one.
    pub passphrase: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for LoadedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoadedCredential(REDACTED)")
    }
}

/// Every terminal answer a credential backend may give, with the
/// failing subject and the evidence to diagnose it. Missing,
/// denied, and unavailable are different facts with different
/// remedies; the loader reports which one it saw and changes
/// nothing. Fails closed throughout: an unanswered probe is
/// `Unavailable`, never an implicit fallback to another backend.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CredentialError {
    /// No such item in the backend. Re-provision; never auto-create.
    #[error("credential missing: {detail}")]
    Missing {
        /// What was looked up.
        subject: CredentialSubject,
        /// Where, and what the backend saw.
        detail: String,
    },
    /// The store is reachable but refused access (ownership,
    /// modes, ACL, locked keychain, a symlink the backend
    /// refuses to follow). Fix access and retry.
    #[error("credential access denied: {detail}")]
    Denied {
        /// What refused, and which check fired.
        subject: CredentialSubject,
        /// The check and its evidence.
        detail: String,
    },
    /// The store or mechanism itself is unreachable (no D-Bus
    /// session, no Secret Service, non-Unix file hardening, a
    /// reference served by the wrong backend). Retry; never
    /// substitute a weaker mechanism.
    #[error("credential store unavailable: {detail}")]
    Unavailable {
        /// What was unreachable and where.
        subject: CredentialSubject,
        /// What was missing or unanswered.
        detail: String,
    },
    /// The item is present but shape-broken (wrong length,
    /// non-hex identity, non-UTF-8 passphrase). Fail closed;
    /// never guess, never repair silently.
    #[error("credential invalid: {detail}")]
    Invalid {
        /// What failed validation.
        subject: CredentialSubject,
        /// What was wrong, without secret bytes.
        detail: String,
    },
}

/// Load one drive's secret material from a backend. One method:
/// provisioning (store), replacement, and deletion join the
/// boundary with the migration slice; until then backends serve
/// reads only. Implementations must not retry across backends —
/// the reference already chose one — and must not prompt.
pub trait CredentialLoader {
    /// Resolve `reference` to secret material or a terminal
    /// [`CredentialError`]. Pure retrieval: no registration, no
    /// keystore open, no side effects beyond the backend's own
    /// read. The passphrase is present exactly where the
    /// reference names a passphrase item; a session-posture
    /// reference loads the identity with no passphrase.
    fn load(&self, reference: &CredentialRef) -> Result<LoadedCredential, CredentialError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The taxonomy reports which fact it saw, naming the
    /// subject in a typed slot: missing, denied, unavailable,
    /// and invalid are four distinct answers with the evidence
    /// attached, so callers diagnose without knowing the
    /// backend or parsing prose.
    #[test]
    fn taxonomy_names_the_fact_with_its_subject() {
        let missing = CredentialError::Missing {
            subject: CredentialSubject::File {
                path: PathBuf::from("/keys/identity"),
            },
            detail: "no such file".to_string(),
        };
        assert!(missing.to_string().contains("missing"));
        assert!(missing.to_string().contains("no such file"));
        assert_ne!(
            missing,
            CredentialError::Denied {
                subject: CredentialSubject::File {
                    path: PathBuf::from("/keys/identity"),
                },
                detail: "no such file".to_string(),
            },
            "the subject is shared; the fact differs"
        );
        let unavailable = CredentialError::Unavailable {
            subject: CredentialSubject::Account {
                account: "wyrd-drive-alpha".to_string(),
            },
            detail: "backend not wired".to_string(),
        };
        assert!(unavailable.to_string().contains("unavailable"));
        let invalid = CredentialError::Invalid {
            subject: CredentialSubject::File {
                path: PathBuf::from("/keys/identity"),
            },
            detail: "31 bytes".to_string(),
        };
        assert!(invalid.to_string().contains("invalid"));
        assert_ne!(unavailable, invalid);
    }

    /// Secret material never renders: the redaction is the
    /// guarantee the no-logs rule rests on. The passphrase slot
    /// renders nothing whether present or absent.
    #[test]
    fn loaded_material_never_renders() {
        for passphrase in [Some(Zeroizing::new("hunter2-hunter2".to_string())), None] {
            let credential = LoadedCredential {
                identity: DeviceIdentitySecret::from_bytes([0x11; 32]).unwrap(),
                passphrase,
            };
            let rendered = format!("{credential:?}");
            assert_eq!(rendered, "LoadedCredential(REDACTED)");
            assert!(!rendered.contains("hunter2"));
        }
    }

    /// References are selectors: file paths and account labels
    /// travel; secret bytes never do. The session posture names
    /// no passphrase item and still loads.
    #[test]
    fn references_name_items_not_secrets() {
        let file = CredentialRef::File {
            identity: PathBuf::from("/keys/identity"),
            passphrase: Some(PathBuf::from("/keys/passphrase")),
        };
        let rendered = format!("{file:?}");
        assert!(rendered.contains("/keys/identity"));
        let session = CredentialRef::File {
            identity: PathBuf::from("/keys/identity"),
            passphrase: None,
        };
        assert!(matches!(
            session,
            CredentialRef::File {
                passphrase: None,
                ..
            }
        ));
        let store = CredentialRef::OsStore {
            account: "wyrd-drive-alpha".to_string(),
        };
        assert!(format!("{store:?}").contains("wyrd-drive-alpha"));
    }
}
