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
//! `wyrd-cli` as `FileBackend`, conforming to this trait); OS
//! credential stores (specified, not yet wired). Adding a backend
//! changes no secret format and no caller.

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
    /// session-posture reference — its passphrase arrives via
    /// the unlock pipe, never the loader).
    File {
        /// The identity file to read and harden.
        identity: PathBuf,
        /// The passphrase file to read and harden, if configured.
        passphrase: Option<PathBuf>,
    },
    /// One item (identity plus passphrase) in the platform's
    /// credential store, named by its account label. Specified,
    /// not yet wired: loading answers `Unavailable` until the
    /// OS backend lands.
    OsStore {
        /// The store account label naming the drive's item.
        account: String,
    },
}

/// One drive's secret material as the loader returns it: the
/// device identity plus the keystore passphrase, both scrubbed on
/// drop. Never logged, never rendered, never placed in argv or
/// configuration — the redacted `Debug` is load-bearing, not
/// cosmetic.
pub struct LoadedCredential {
    /// The device's Nostr identity secret (local-key custody).
    pub identity: DeviceIdentitySecret,
    /// The passphrase unwrapping the drive's root key.
    pub passphrase: Zeroizing<String>,
}

impl std::fmt::Debug for LoadedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoadedCredential(REDACTED)")
    }
}

/// Every terminal answer a credential backend may give, with the
/// evidence to diagnose it. Missing, denied, and unavailable are
/// different facts with different remedies; the loader reports
/// which one it saw and changes nothing. Fails closed throughout:
/// an unanswered probe is `Unavailable`, never an implicit
/// fallback to another backend.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CredentialError {
    /// No such item in the backend. Re-provision; never auto-create.
    #[error("credential missing: {detail}")]
    Missing {
        /// Which reference was looked up and where.
        detail: String,
    },
    /// The store is reachable but refused access (ownership,
    /// modes, ACL, locked keychain). Fix access and retry.
    #[error("credential access denied: {detail}")]
    Denied {
        /// Which check refused, with its evidence.
        detail: String,
    },
    /// The store or mechanism itself is unreachable (no D-Bus
    /// session, no Secret Service, non-Unix file hardening,
    /// backend not yet wired). Retry; never substitute a weaker
    /// mechanism.
    #[error("credential store unavailable: {detail}")]
    Unavailable {
        /// What was unreachable and where.
        detail: String,
    },
    /// The item is present but shape-broken (wrong length,
    /// non-hex identity, non-UTF-8 passphrase). Fail closed;
    /// never guess, never repair silently.
    #[error("credential invalid: {detail}")]
    Invalid {
        /// What failed validation, without secret bytes.
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
    /// read.
    fn load(&self, reference: &CredentialRef) -> Result<LoadedCredential, CredentialError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The taxonomy reports which fact it saw: missing, denied,
    /// unavailable, and invalid are four distinct answers with
    /// the evidence attached, so callers diagnose without
    /// knowing the backend.
    #[test]
    fn taxonomy_names_the_fact_with_its_evidence() {
        let missing = CredentialError::Missing {
            detail: "no item for drive-alpha".to_string(),
        };
        assert!(missing.to_string().contains("missing"));
        assert!(missing.to_string().contains("drive-alpha"));
        let denied = CredentialError::Denied {
            detail: "mode 0644".to_string(),
        };
        assert!(denied.to_string().contains("denied"));
        let unavailable = CredentialError::Unavailable {
            detail: "no D-Bus session".to_string(),
        };
        assert!(unavailable.to_string().contains("unavailable"));
        let invalid = CredentialError::Invalid {
            detail: "identity is 31 bytes".to_string(),
        };
        assert!(invalid.to_string().contains("invalid"));
        assert_ne!(missing, denied);
        assert_ne!(unavailable, invalid);
    }

    /// Secret material never renders: the redacted `Debug` is the
    /// guarantee the no-logs rule rests on.
    #[test]
    fn loaded_material_never_renders() {
        let credential = LoadedCredential {
            identity: DeviceIdentitySecret::from_bytes([0x11; 32]).unwrap(),
            passphrase: Zeroizing::new("hunter2-hunter2".to_string()),
        };
        let rendered = format!("{credential:?}");
        assert_eq!(rendered, "LoadedCredential(REDACTED)");
        assert!(!rendered.contains("hunter2"));
    }

    /// References are selectors: file paths and account labels
    /// travel; secret bytes never do.
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
