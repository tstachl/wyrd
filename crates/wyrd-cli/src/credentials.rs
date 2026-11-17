//! Credential and identity loading: flag-first, file-second path
//! resolution through the same Unix hardening either way. Called
//! by every subcommand via `load_credentials`.
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::args::Credentials;
use crate::drive_config::{
    load_drive_config, resolve_credential_path, CredentialProvenance, LoadedConfig,
};
use crate::CliError;
use wyrd_sync::keys::DeviceIdentitySecret;
use zeroize::Zeroizing;

#[cfg(unix)]
#[allow(unsafe_code)]
pub(crate) fn current_uid() -> u32 {
    // SAFETY: geteuid has no pointer or aliasing preconditions and only
    // reads the calling process's kernel credential.
    unsafe { libc::geteuid() }
}

/// Load one invocation's secrets: the drive's configuration file
/// first (every subcommand reads the same file, so relay and
/// credential defaults cannot drift between commands), then the
/// credential files the flags or the file name. Returns the secrets
/// with their provenance and the loaded configuration for the
/// dispatch below, which resolves relays and reports provenance
/// from the same value — one read, one source of truth.
pub(crate) fn load_credentials(
    drive_dir: &Path,
    creds: &Credentials,
    override_path: Option<&Path>,
) -> Result<
    (
        DeviceIdentitySecret,
        Zeroizing<String>,
        CredentialProvenance,
        Option<LoadedConfig>,
    ),
    CliError,
> {
    let config = load_drive_config(drive_dir, override_path)?;
    let (identity, passphrase, provenance) = read_credentials(creds, config.as_ref())?;
    Ok((identity, passphrase, provenance, config))
}

/// Read and harden the credential files. The passphrase keeps its
/// trailing newline stripped; the identity may be raw or hex. Paths
/// resolve flags-first, file-second; the resolved paths travel
/// through the same hardening either way — the file defaults the
/// location, never the protection.
pub(crate) fn read_credentials(
    creds: &Credentials,
    config: Option<&LoadedConfig>,
) -> Result<
    (
        DeviceIdentitySecret,
        Zeroizing<String>,
        CredentialProvenance,
    ),
    CliError,
> {
    let identity_file = resolve_credential_path(
        creds.identity_file.clone(),
        config.and_then(|c| c.identity_file.clone()),
        "--identity-file",
        "identity_file",
    )?;
    // Belt and braces beside the clap `conflicts_with`: a
    // programmatically built `Credentials` must also refuse the
    // ambiguous pair rather than silently preferring one surface.
    if creds.passphrase_stdin && creds.passphrase_file.is_some() {
        return Err(CliError::Usage(
            "--passphrase-stdin conflicts with --passphrase-file: pass one passphrase surface"
                .to_string(),
        ));
    }
    let provenance = CredentialProvenance {
        identity_from_file: creds.identity_file.is_none(),
        passphrase_from_file: !creds.passphrase_stdin && creds.passphrase_file.is_none(),
    };
    let identity = read_identity(&identity_file)?;
    if creds.passphrase_stdin {
        let passphrase = read_passphrase_stdin(std::io::stdin().lock())?;
        return Ok((identity, passphrase, provenance));
    }
    let passphrase_file = resolve_credential_path(
        creds.passphrase_file.clone(),
        config.and_then(|c| c.passphrase_file.clone()),
        "--passphrase-file",
        "passphrase_file",
    )?;
    let passphrase_bytes = read_secret_file(&passphrase_file)?;
    let passphrase_text = std::str::from_utf8(&passphrase_bytes)
        .map_err(|_| CliError::Usage("passphrase file must contain UTF-8 text".into()))?;
    let passphrase = Zeroizing::new(passphrase_text.to_owned());
    let passphrase = passphrase
        .strip_suffix("\r\n")
        .or_else(|| passphrase.strip_suffix('\n'))
        .unwrap_or(&passphrase);
    // Kept scrubbing: the passphrase lives in process memory until the
    // command completes, so it stays in a `Zeroizing` rather than a
    // plain `String`. Callers pass `&passphrase` as `&str` through
    // deref coercion.
    Ok((identity, Zeroizing::new(passphrase.to_owned()), provenance))
}

/// Read the per-session unlock passphrase from a reader, bounded
/// like the credential file (4096 bytes): a hostile or corrupt pipe
/// cannot exhaust memory. One trailing newline is the line
/// discipline, not part of the passphrase — the same stripping the
/// file path applies, so file and stdin are interchangeable. Empty
/// input (immediate EOF), over-long input, and non-UTF-8 input
/// refuse before anything opens: the owner never starts partially
/// initialized. The buffer stays in a `Zeroizing`, scrubbed on drop.
fn read_passphrase_stdin(reader: impl std::io::Read) -> Result<Zeroizing<String>, CliError> {
    use std::io::Read;
    const MAX_PASSPHRASE_BYTES: usize = 4096;
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .take((MAX_PASSPHRASE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| CliError::Io {
            path: PathBuf::from("<stdin>"),
            source,
        })?;
    if bytes.is_empty() {
        return Err(CliError::Usage(
            "no passphrase on stdin: pipe one line (the owner never starts without it)".to_string(),
        ));
    }
    if bytes.len() > MAX_PASSPHRASE_BYTES {
        return Err(CliError::Usage(
            "passphrase exceeds the 4096-byte size limit".to_string(),
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| CliError::Usage("passphrase on stdin must contain UTF-8 text".into()))?;
    let text = Zeroizing::new(text.to_owned());
    let stripped = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(&text);
    if stripped.is_empty() {
        return Err(CliError::Usage(
            "no passphrase on stdin: pipe one line (the owner never starts without it)".to_string(),
        ));
    }
    Ok(Zeroizing::new(stripped.to_owned()))
}

/// Read a bounded credential file without following symlinks. On Unix the
/// file must belong to the current user and not grant group/other access.
pub(crate) fn read_secret_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, CliError> {
    const MAX_BYTES: usize = 4096;
    #[cfg(not(unix))]
    return Err(CliError::Credential {
        path: path.to_path_buf(),
        reason: "credential-file protection is only implemented on Unix",
    });

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_CLOEXEC,
    );
    let mut file = options.open(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(CliError::Credential {
            path: path.to_path_buf(),
            reason: "not a regular file",
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != current_uid() {
            return Err(CliError::Credential {
                path: path.to_path_buf(),
                reason: "must be owned by the current user",
            });
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(CliError::Credential {
                path: path.to_path_buf(),
                reason: "must not be accessible by group or other users",
            });
        }
    }
    let mut bytes = Zeroizing::new(Vec::new());
    std::io::Read::by_ref(&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| CliError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > MAX_BYTES {
        return Err(CliError::Credential {
            path: path.to_path_buf(),
            reason: "exceeds the 4096-byte size limit",
        });
    }
    Ok(bytes)
}

pub(crate) fn read_identity(path: &Path) -> Result<DeviceIdentitySecret, CliError> {
    let bytes = read_secret_file(path)?;
    let raw = if bytes.len() == 32 {
        let mut raw = Zeroizing::new([0; 32]);
        raw.copy_from_slice(&bytes);
        raw
    } else if let Ok(text) = std::str::from_utf8(&bytes) {
        let text = text.trim_matches(|character: char| character.is_ascii_whitespace());
        if text.len() != 64 {
            return Err(CliError::IdentityFormat);
        }
        let decoded = Zeroizing::new(hex::decode(text).map_err(|_| CliError::IdentityFormat)?);
        let mut raw = Zeroizing::new([0; 32]);
        raw.copy_from_slice(&decoded);
        raw
    } else {
        return Err(CliError::IdentityFormat);
    };
    DeviceIdentitySecret::from_bytes(*raw).map_err(CliError::Identity)
}

#[cfg(test)]
mod tests {
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

    /// Empty pipes fail closed before anything opens: immediate EOF
    /// and a bare newline both refuse as usage, never as an empty
    /// passphrase the owner would try.
    #[test]
    fn empty_stdin_fails_closed() {
        assert!(read_passphrase_stdin(Cursor::new(b"")).is_err());
        assert!(read_passphrase_stdin(Cursor::new(b"\n")).is_err());
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
        };
        assert!(read_credentials(&creds, None).is_err());
    }

    /// Stdin mode never resolves a passphrase file: no config
    /// default is consulted and none is required.
    #[test]
    fn stdin_mode_needs_no_passphrase_file() {
        let config = LoadedConfig {
            path: PathBuf::from("/drive/default.toml"),
            relays: Vec::new(),
            identity_file: Some(PathBuf::from("/cfg/identity")),
            passphrase_file: Some(PathBuf::from("/cfg/passphrase")),
        };
        // Identity still resolves (flag wins); the passphrase file
        // surface is irrelevant — assert resolution directly by
        // observing the provenance refuses nothing at this layer.
        // The read itself would need a real identity file, so pin
        // the conflict rule only: stdin + no file flag is not the
        // ambiguous pair.
        let creds = Credentials {
            identity_file: Some(PathBuf::from("/k/identity")),
            passphrase_file: None,
            passphrase_stdin: true,
        };
        // No ambiguous-pair error: the failure below must be the
        // missing identity file on disk, never the passphrase pair.
        match read_credentials(&creds, Some(&config)) {
            Err(CliError::Usage(message)) if message.contains("conflicts") => {
                panic!("stdin mode must not trip the pair conflict: {message}")
            }
            _ => {}
        }
    }
}
