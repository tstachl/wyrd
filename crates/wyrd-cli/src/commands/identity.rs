//! First-run identity onboarding: generate and verify the device's
//! credential files. The desktop calls in through its constrained CLI
//! adapter with paths only; secrets never travel as argv contents.
//!
//! - `generate` mints a fresh identity secret (CSPRNG, scalar-validated)
//!   plus a generated passphrase (120-bit, chunked unambiguous
//!   alphabet), writes both files (0700 dirs, 0600 files, O_NOFOLLOW,
//!   never overwrite), and prints the passphrase on stdout — the single
//!   line the desktop displays for write-down. Nothing else goes to
//!   stdout, so the adapter captures verbatim.
//! - `verify` runs the production hardened reader over both files
//!   (owner-only, no symlinks, bounded, identity format + UTF-8
//!   passphrase) and exits 0/2. Stderr is opaque display detail;
//!   only the exit code decides.
//!
//! Fail-closed on non-Unix: the credential reader the verification
//! reuses is Unix-only, so generation refuses there too — Windows is
//! out of v0.4 entirely.

use std::path::{Path, PathBuf};

use wyrd_sync::keys::DeviceIdentitySecret;
use zeroize::Zeroizing;

use crate::credentials::{read_identity, read_secret_file};
use crate::CliError;

/// Unambiguous alphabet: lowercase Crockford without I/L/O/U — 32
/// chars, 5 bits each, no look-alikes (no 0/O, 1/I/L). Display and
/// file share the chunked form, so a transcribed backup matches.
const PASSPHRASE_ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Generated passphrase body: 24 chars = 120 bits, chunked 6-6-6-6.
const PASSPHRASE_CHARS: usize = 24;
const PASSPHRASE_GROUP: usize = 6;

/// Mint one generated passphrase: 120-bit CSPRNG through the audited
/// `getrandom` substrate (via `DeviceIdentitySecret::generate`'s
/// retry discipline — scalar validity is irrelevant here, but the
/// randomness source is the same audited one). Modulo bias is
/// negligible at 32-way division of a uniform byte (256 = 8 × 32),
/// so `% 32` is exact. Held in `Zeroizing`: the text scrubs on drop.
fn mint_passphrase() -> Result<Zeroizing<String>, CliError> {
    let mut body = Zeroizing::new(String::with_capacity(PASSPHRASE_CHARS + 3));
    let mut buf = [0u8; 32];
    let mut produced = 0usize;
    while produced < PASSPHRASE_CHARS {
        getrandom::getrandom(&mut buf).map_err(|_| {
            CliError::Usage(
                "secure randomness unavailable: cannot generate a passphrase".to_string(),
            )
        })?;
        for byte in buf {
            if produced >= PASSPHRASE_CHARS {
                break;
            }
            let idx = (byte % 32) as usize;
            if produced > 0 && produced.is_multiple_of(PASSPHRASE_GROUP) {
                body.push('-');
            }
            body.push(PASSPHRASE_ALPHABET[idx] as char);
            produced += 1;
        }
    }
    Ok(body)
}

/// Parent directory: created 0700, verified owner-only. Fails closed
/// on non-Unix like the reader.
#[cfg(unix)]
fn ensure_parent(dir: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(
                |source| CliError::Io {
                    path: parent.to_path_buf(),
                    source,
                },
            )?;
        }
    }
    Ok(())
}

/// fsync the parent directory so a fresh directory entry is durable
/// before the caller reports success. The file fsync in `write_new`
/// orders the bytes; this orders the name. Best-effort where there
/// is nothing to report (rollback), hard-error on the commit path.
#[cfg(unix)]
fn fsync_parent(path: &Path) -> Result<(), CliError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let Some(dir) = parent else {
        return Ok(());
    };
    let handle = std::fs::File::open(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    handle.sync_all().map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })
}

/// Write one credential file: O_NOFOLLOW | O_EXCL | 0600 — never
/// overwrite, never follow a symlink — then fsync the parent
/// directory so the new name is durable, not just the bytes.
/// Presence resolves to a usage error (the desktop's Repair state),
/// not a silent replace.
#[cfg(unix)]
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(CliError::Usage(format!(
            "credential file {} already exists: refusing to overwrite (choose Repair to inspect it)",
            path.display()
        )));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options.mode(0o600);
    let mut file = options.open(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::AlreadyExists {
            CliError::Usage(format!(
                "credential file {} already exists: refusing to overwrite (choose Repair to inspect it)",
                path.display()
            ))
        } else {
            CliError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    use std::io::Write;
    file.write_all(bytes).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        CliError::Io {
            path: path.to_path_buf(),
            source,
        }
    })?;
    fsync_parent(path)?;
    Ok(())
}

/// Generate both credential files and print the passphrase on stdout.
pub(crate) fn generate(identity_file: PathBuf, passphrase_file: PathBuf) -> Result<(), CliError> {
    #[cfg(not(unix))]
    {
        let _ = (&identity_file, &passphrase_file);
        return Err(CliError::Usage(
            "identity generation is only implemented on Unix".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        ensure_parent(&identity_file)?;
        ensure_parent(&passphrase_file)?;
        let (raw, _secret) = generate_scalar()?;
        // Raw 32 bytes: the production reader accepts raw or hex, and
        // raw keeps the file exactly the keystore expects. Both the
        // bytes and the wrapper scrub on drop at scope end.
        let passphrase = mint_passphrase()?;
        // Identity first, then passphrase: the file fsync plus the
        // parent-directory fsync in `write_new` make the order
        // durable — a crash between the two leaves the Repair state
        // (identity without passphrase), never a passphrase pointing
        // at nothing.
        write_new(&identity_file, &raw[..])?;
        let outcome = write_new(&passphrase_file, passphrase.as_bytes());
        if outcome.is_err() {
            let _ = std::fs::remove_file(&identity_file);
            let _ = fsync_parent(&identity_file);
            return outcome;
        }
        // The single stdout line: the desktop captures verbatim for
        // write-down. Nothing else may go to stdout on this path.
        print!("{}", passphrase.as_str());
        Ok(())
    }
}

/// Mint one generated passphrase for display: writes nothing.
pub(crate) fn new_passphrase() -> Result<(), CliError> {
    let passphrase = mint_passphrase()?;
    print!("{}", passphrase.as_str());
    Ok(())
}

/// Commit both credential files: the passphrase arrives on stdin (one
/// line — the onboarding pipe, never argv), the identity secret is
/// generated in-process and never leaves it. Writes owner-only, never
/// overwrite; identity first, passphrase second, with rollback of the
/// identity when the passphrase write fails.
pub(crate) fn commit(identity_file: PathBuf, passphrase_file: PathBuf) -> Result<(), CliError> {
    #[cfg(not(unix))]
    {
        let _ = (&identity_file, &passphrase_file);
        return Err(CliError::Usage(
            "identity commit is only implemented on Unix".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        // Bounded like the credential reader (4096 bytes): a hostile
        // or corrupt pipe cannot exhaust memory before validation
        // refuses it. One trailing newline is the line discipline,
        // not part of the passphrase.
        let mut text = Zeroizing::new(String::new());
        std::io::stdin()
            .take((MAX_PASSPHRASE_BYTES + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|source| CliError::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
        if text.len() > MAX_PASSPHRASE_BYTES {
            return Err(CliError::Usage(
                "passphrase exceeds the 4096-byte size limit".to_string(),
            ));
        }
        let stripped = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(&text);
        let passphrase = Zeroizing::new(stripped.to_owned());
        validate_passphrase(&passphrase)?;
        ensure_parent(&identity_file)?;
        ensure_parent(&passphrase_file)?;
        let (raw, _secret) = generate_scalar()?;
        write_new(&identity_file, &raw[..])?;
        let outcome = write_new(&passphrase_file, passphrase.as_bytes());
        if outcome.is_err() {
            let _ = std::fs::remove_file(&identity_file);
            let _ = fsync_parent(&identity_file);
            return outcome;
        }
        Ok(())
    }
}

/// Passphrase bounds, mirrored by the desktop pre-check: 12 chars
/// minimum (the UI states the same floor), 4096 bytes maximum like
/// the credential reader, no line breaks (one trailing newline is
/// stripped on input, never stored). Empty, short, overlong, and
/// multi-line inputs refuse before anything is written.
const MAX_PASSPHRASE_BYTES: usize = 4096;

fn validate_passphrase(passphrase: &str) -> Result<(), CliError> {
    if passphrase.chars().count() < 12 {
        return Err(CliError::Usage(
            "passphrase must be at least 12 characters".to_string(),
        ));
    }
    if passphrase.len() > MAX_PASSPHRASE_BYTES {
        return Err(CliError::Usage(
            "passphrase exceeds the 4096-byte size limit".to_string(),
        ));
    }
    if passphrase.contains('\n') || passphrase.contains('\r') {
        return Err(CliError::Usage(
            "passphrase must not contain line breaks".to_string(),
        ));
    }
    Ok(())
}
/// Verify both files through the production hardened reader.
pub(crate) fn verify(identity_file: PathBuf, passphrase_file: PathBuf) -> Result<(), CliError> {
    let _identity = read_identity(&identity_file)?;
    let passphrase_bytes = read_secret_file(&passphrase_file)?;
    if std::str::from_utf8(&passphrase_bytes).is_err() {
        return Err(CliError::Usage(
            "passphrase file must contain UTF-8 text".to_string(),
        ));
    }
    Ok(())
}

/// Generate one identity scalar with its raw bytes: random
/// CSPRNG bytes validated through `from_bytes` (same gate as
/// `DeviceIdentitySecret::generate`), retrying the negligible
/// non-scalar fraction. The bytes ride in `Zeroizing` beside the
/// wrapper; both scrub on drop.
fn generate_scalar() -> Result<(Zeroizing<[u8; 32]>, DeviceIdentitySecret), CliError> {
    loop {
        let mut raw = Zeroizing::new([0u8; 32]);
        getrandom::getrandom(&mut *raw)
            .map_err(|_| CliError::Usage("secure randomness unavailable".to_string()))?;
        if let Ok(secret) = DeviceIdentitySecret::from_bytes(*raw) {
            return Ok((raw, secret));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Alphabet pins: 32 unambiguous chars, no look-alikes.
    #[test]
    fn alphabet_is_32_unambiguous_chars() {
        assert_eq!(PASSPHRASE_ALPHABET.len(), 32);
        // Ambiguous pairs are split: 0 stays because O is gone, 1
        // stays because I/L are gone. Ban the letter halves (both
        // cases) plus U (excluded to hold 32 chars).
        for banned in ['o', 'i', 'l', 'u', 'O', 'I', 'L', 'U'] {
            assert!(
                !PASSPHRASE_ALPHABET.contains(&(banned as u8)),
                "alphabet must not contain {banned:?}"
            );
        }
    }

    /// Format pins: 24 chars + 3 dashes, grouped 6-6-6-6, 120 bits.
    #[test]
    fn passphrase_format_is_chunked_120_bits() {
        let phrase = mint_passphrase().unwrap();
        let text = phrase.as_str();
        let parts: Vec<&str> = text.split('-').collect();
        assert_eq!(parts.len(), 4, "four groups: {text}");
        for part in &parts {
            assert_eq!(part.len(), 6, "six chars per group: {text}");
            for ch in part.chars() {
                assert!(
                    PASSPHRASE_ALPHABET.contains(&(ch as u8)),
                    "charset violation {ch:?} in {text}"
                );
            }
        }
        assert_eq!(text.len(), PASSPHRASE_CHARS + 3);
    }

    /// Uniqueness smoke: two mints differ (CSPRNG, not a counter).
    #[test]
    fn passphrases_differ() {
        assert_ne!(mint_passphrase().unwrap(), mint_passphrase().unwrap());
    }

    /// Round-trip through temp files: generate writes 0600 files the
    /// production reader accepts, and refuses to overwrite.
    #[test]
    fn generate_verify_round_trip() {
        let dir = std::env::temp_dir().join(format!("wyrd-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let identity = dir.join("identity");
        let passphrase = dir.join("passphrase");
        generate(identity.clone(), passphrase.clone()).unwrap();
        verify(identity.clone(), passphrase.clone()).unwrap();
        // Never overwrite: second generate refuses.
        assert!(generate(identity.clone(), passphrase.clone()).is_err());
        // Permissions: owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&identity).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "identity file mode");
            let mode = std::fs::metadata(&passphrase).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "passphrase file mode");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Floor pins: 11 chars refuse, 12 pass, overlong and
    /// line-break inputs refuse — all before anything is written.
    #[test]
    fn passphrase_floor_is_12_chars() {
        assert!(validate_passphrase("short-11-ch").is_err());
        assert!(validate_passphrase("exactly-12ch").is_ok());
        assert!(validate_passphrase(&"x".repeat(4097)).is_err());
        assert!(validate_passphrase("twelve-chars\n").is_err());
        assert!(validate_passphrase("twelve-chars\r\n").is_err());
    }
}
