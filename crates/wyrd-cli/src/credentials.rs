//! Credential and identity loading: flag-first, file-second path
//! resolution through the same Unix hardening either way. Called
//! by every subcommand via `load_credentials`.
//!
//! The file backend below also conforms to the platform-neutral
//! loader boundary (`wyrd_sync::keys::CredentialLoader`,
//! `docs/contracts/credential-loading.md`): `FileBackend::load`
//! serves resolved references through the same readers the flag
//! paths use, so the boundary and the backend cannot drift.
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::args::Credentials;
use crate::drive_config::{
    load_drive_config, resolve_credential_path, CredentialProvenance, LoadedConfig,
};
use crate::CliError;
use wyrd_sync::keys::{
    CredentialError, CredentialLoader, CredentialRef, CredentialSubject, DeviceIdentitySecret,
    LoadedCredential,
};
use zeroize::Zeroizing;

/// Frozen wording (product-terminology.md R-C1): every
/// empty-passphrase refusal ends with this sentence.
const NO_PASSPHRASE_TAIL: &str = "This drive's owner cannot start without it";

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
/// location, never the protection. `--passphrase-stdin` reads the
/// passphrase from the process stdin instead of any file.
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
    read_credentials_with(creds, config, std::io::stdin().lock())
}

/// The testable half of [`read_credentials`]: the stdin surface
/// reads from the injected reader, so tests pipe passphrases
/// without touching the process stdin. Production passes the
/// locked stdin; the bound, discipline, and failure modes are
/// identical either way.
fn read_credentials_with(
    creds: &Credentials,
    config: Option<&LoadedConfig>,
    stdin: impl std::io::Read,
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
        passphrase_from_stdin: creds.passphrase_stdin,
        passphrase_from_prompt: false,
    };
    if creds.passphrase_stdin {
        // The session posture loads the identity through the
        // boundary with no passphrase item; the pipe supplies
        // the passphrase downstream, never the loader.
        let loaded = FileBackend
            .load(&CredentialRef::File {
                identity: identity_file,
                passphrase: None,
            })
            .map_err(loader_to_cli)?;
        let passphrase = read_passphrase_stdin(stdin)?;
        return Ok((loaded.identity, passphrase, provenance));
    }
    let passphrase_file = resolve_credential_path(
        creds.passphrase_file.clone(),
        config.and_then(|c| c.passphrase_file.clone()),
        "--passphrase-file",
        "passphrase_file",
    )?;
    // The file branches serve through the loader boundary in one
    // call: the backend runs the production readers, and each
    // taxonomy detail reproduces the `CliError` `Display` it came
    // from, so every single failure reports with identical
    // wording and the same exit code. Resolution order per file
    // is unchanged; where the passphrase names nothing the
    // resolution reports before the single read runs (either
    // order names a real problem at the same exit status).
    let loaded = FileBackend
        .load(&CredentialRef::File {
            identity: identity_file,
            passphrase: Some(passphrase_file),
        })
        .map_err(loader_to_cli)?;
    let passphrase = loaded.passphrase.expect(
        "a file reference naming a passphrase item loads one: the backend read it or failed",
    );
    Ok((loaded.identity, passphrase, provenance))
}

/// Read one passphrase file: UTF-8 text with one trailing newline
/// stripped (the line discipline, not part of the passphrase).
/// Kept scrubbing: the passphrase lives in process memory until
/// the command completes, so it stays in a `Zeroizing` rather
/// than a plain `String`. Callers pass `&passphrase` as `&str`
/// through deref coercion. Read by the [`FileBackend`] loader
/// below, so the boundary and the flag path open the same
/// keystore.
fn read_passphrase_file(path: &Path) -> Result<Zeroizing<String>, CliError> {
    let passphrase_bytes = read_secret_file(path)?;
    let passphrase_text = std::str::from_utf8(&passphrase_bytes)
        .map_err(|_| CliError::Usage("passphrase file must contain UTF-8 text".into()))?;
    let passphrase = Zeroizing::new(passphrase_text.to_owned());
    let stripped = strip_trailing_newline(&passphrase);
    Ok(Zeroizing::new(stripped.to_owned()))
}

/// Load one unlock invocation's secrets: the identity from its
/// file (flags-first, file-second, same hardening as every other
/// command), the passphrase from the terminal prompt or the stdin
/// pipe — never from a file. Unlock is the per-session command:
/// a passphrase file would make it file-backed by accident, so a
/// `--passphrase-file` flag here refuses instead of being read.
pub(crate) fn read_unlock_credentials(
    creds: &Credentials,
    config: Option<&LoadedConfig>,
    stdin_is_tty: bool,
) -> Result<
    (
        DeviceIdentitySecret,
        Zeroizing<String>,
        CredentialProvenance,
    ),
    CliError,
> {
    read_unlock_credentials_with(creds, config, std::io::stdin().lock(), stdin_is_tty)
}

/// The testable half of [`read_unlock_credentials`]: the prompt
/// decision runs on the injected TTY bit and the pipe reads from
/// the injected reader, so tests cover every refusal without a
/// terminal. The prompt itself (termios echo-off) is the only
/// untested line — it runs only where a human sits.
fn read_unlock_credentials_with(
    creds: &Credentials,
    config: Option<&LoadedConfig>,
    stdin: impl std::io::Read,
    stdin_is_tty: bool,
) -> Result<
    (
        DeviceIdentitySecret,
        Zeroizing<String>,
        CredentialProvenance,
    ),
    CliError,
> {
    // Belt and braces beside the clap `conflicts_with`: a
    // programmatically built `Credentials` must also refuse the
    // ambiguous pair rather than silently preferring one surface.
    if creds.passphrase_stdin && creds.passphrase_file.is_some() {
        return Err(CliError::Usage(
            "--passphrase-stdin conflicts with --passphrase-file: pass one passphrase surface"
                .to_string(),
        ));
    }
    if creds.passphrase_file.is_some() {
        return Err(CliError::Usage(
            "unlock never reads a passphrase file: omit --passphrase-file and type at the prompt (or pipe with --passphrase-stdin); file-backed starts use `owner start` or `drive service-start`"
                .to_string(),
        ));
    }
    let provenance = CredentialProvenance {
        identity_from_file: creds.identity_file.is_none(),
        passphrase_from_file: false,
        passphrase_from_stdin: creds.passphrase_stdin,
        passphrase_from_prompt: !creds.passphrase_stdin,
    };
    let identity_file = resolve_credential_path(
        creds.identity_file.clone(),
        config.and_then(|c| c.identity_file.clone()),
        "--identity-file",
        "identity_file",
    )?;
    // Unlock's identity loads through the boundary with no
    // passphrase item; the pipe or prompt supplies the
    // passphrase downstream, never the loader. Interactive
    // surfaces stay outside the trait by design.
    let loaded = FileBackend
        .load(&CredentialRef::File {
            identity: identity_file,
            passphrase: None,
        })
        .map_err(loader_to_cli)?;
    let identity = loaded.identity;
    if creds.passphrase_stdin {
        let passphrase = read_passphrase_stdin(stdin)?;
        return Ok((identity, passphrase, provenance));
    }
    if !stdin_is_tty {
        return Err(CliError::Usage(
            format!("not a terminal: pipe the passphrase with --passphrase-stdin or run unlock on a terminal. {NO_PASSPHRASE_TAIL}").to_string(),
        ));
    }
    let passphrase = prompt_passphrase_tty()?;
    Ok((identity, passphrase, provenance))
}

/// Prompt for the unlock passphrase on the terminal with echo
/// off. Unix-only: credential files are Unix-only too, so unlock
/// has no non-Unix surface to prompt on. The prompt goes to
/// stderr (stdout stays clean for the owner diagnostics contract
/// below); echo restores through a Drop guard, so every return
/// path — including a read error — leaves the terminal as found.
/// A signal mid-prompt can still kill the process with echo off;
/// shells re-arm echo on their next prompt, and no secret is at
/// risk either way (the buffer is `Zeroizing`, never logged).
#[cfg(unix)]
#[allow(unsafe_code)]
fn prompt_passphrase_tty() -> Result<Zeroizing<String>, CliError> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    struct EchoGuard {
        fd: std::os::unix::io::RawFd,
        previous: libc::termios,
    }
    impl Drop for EchoGuard {
        fn drop(&mut self) {
            // Best effort: nothing left to report to, and the
            // terminal state matters more than the error.
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &self.previous);
            }
        }
    }

    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();
    let mut previous: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: tcgetattr only writes through its third argument on
    // success (checked below) and reads no other memory.
    if unsafe { libc::tcgetattr(fd, &mut previous) } != 0 {
        return Err(CliError::Usage(
            "cannot prepare the terminal prompt: stdin is not a usable terminal".to_string(),
        ));
    }
    let mut noecho = previous;
    noecho.c_lflag &= !libc::ECHO;
    // SAFETY: `noecho` is a copy of a valid termios with one flag
    // cleared; TCSANOW applies it to the open stdin descriptor.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &noecho) } != 0 {
        return Err(CliError::Usage(
            "cannot silence the terminal prompt: echo stays on rather than risk showing the passphrase"
                .to_string(),
        ));
    }
    let _restore = EchoGuard { fd, previous };

    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(b"Passphrase: ");
    let _ = stderr.flush();
    // Bounded like the pipe: stop at the first newline (a typed
    // passphrase is one line; the pipe stays the multi-line
    // surface) and refuse past one line of 4096 bytes plus its
    // newline — the same boundary the file and pipe readers
    // enforce, on the read itself. Cutting at the newline first
    // also removes the off-by-one: a 4096-byte passphrase plus
    // Enter fits, exactly like 4096 piped bytes without one.
    let mut bytes = Zeroizing::new(Vec::new());
    let mut stdin = std::io::stdin().lock();
    loop {
        if bytes.len() >= 4097 {
            return Err(CliError::Usage(
                "passphrase exceeds the 4096-byte size limit".to_string(),
            ));
        }
        let mut chunk = [0u8; 64];
        let n = std::io::Read::read(&mut stdin, &mut chunk).map_err(|source| CliError::Io {
            path: PathBuf::from("<stdin>"),
            source,
        })?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.contains(&b'\n') {
            break;
        }
    }
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
    if let Some(pos) = bytes.iter().position(|byte| *byte == b'\n') {
        bytes.truncate(pos);
    }
    let mut line = Zeroizing::new(
        std::str::from_utf8(&bytes)
            .map_err(|_| CliError::Usage("passphrase must contain UTF-8 text".into()))?
            .to_owned(),
    );
    if line.ends_with('\r') {
        let shortened = line.len() - 1;
        line.truncate(shortened);
    }
    if line.len() > 4096 {
        return Err(CliError::Usage(
            "passphrase exceeds the 4096-byte size limit".to_string(),
        ));
    }
    if line.is_empty() {
        return Err(CliError::Usage(
            format!("No passphrase provided. {NO_PASSPHRASE_TAIL}").to_string(),
        ));
    }
    Ok(line)
}

/// No terminal surface off Unix: credential files are Unix-only,
/// so unlock refuses rather than prompting somewhere unhardened.
#[cfg(not(unix))]
fn prompt_passphrase_tty() -> Result<Zeroizing<String>, CliError> {
    Err(CliError::Usage(
        "terminal unlock is only implemented on Unix: pipe the passphrase with --passphrase-stdin"
            .to_string(),
    ))
}
/// Resolve the credential *paths* a provision or reconfigure hands
/// the daemon: the identity file always (every posture reads it),
/// the passphrase file only for a managed service. A piped or
/// prompted passphrase with `manage_service` refuses — a definition
/// embedding no readable file would install cleanly and then
/// crash-loop under the manager. Session posture is `--no-service`
/// (no definition rendered) plus `drive unlock` for starts.
pub(crate) fn resolve_service_credential_files(
    creds: &Credentials,
    config: Option<&LoadedConfig>,
    manage_service: bool,
) -> Result<(PathBuf, Option<PathBuf>), CliError> {
    if creds.passphrase_stdin && manage_service {
        return Err(CliError::Usage(
            "managed service needs a passphrase file the manager can read: omit --passphrase-stdin (or provision with --no-service and start sessions with `drive unlock`)"
                .to_string(),
        ));
    }
    let identity_file = resolve_credential_path(
        creds.identity_file.clone(),
        config.and_then(|c| c.identity_file.clone()),
        "--identity-file",
        "identity_file",
    )?;
    let passphrase_file = if manage_service {
        Some(resolve_credential_path(
            creds.passphrase_file.clone(),
            config.and_then(|c| c.passphrase_file.clone()),
            "--passphrase-file",
            "passphrase_file",
        )?)
    } else {
        None
    };
    Ok((identity_file, passphrase_file))
}
/// One trailing newline is the line discipline, not part of the
/// passphrase — shared by the file, stdin, and prompt surfaces, so
/// all three open the same keystore.
fn strip_trailing_newline(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
}

/// Read the per-session unlock passphrase from a reader, bounded
/// like the credential file (4096 bytes): a hostile or corrupt pipe
/// cannot exhaust memory. One trailing newline is the line
/// discipline, not part of the passphrase — the same stripping the
/// file path applies, so file and stdin are interchangeable. Empty
/// input (immediate EOF), over-long input, and non-UTF-8 input
/// refuse before the keystore opens: the owner never starts
/// partially initialized. The buffer stays in a `Zeroizing`,
/// scrubbed on drop.
fn read_passphrase_stdin(reader: impl std::io::Read) -> Result<Zeroizing<String>, CliError> {
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
            format!("No passphrase provided on stdin. {NO_PASSPHRASE_TAIL}").to_string(),
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
    let stripped = strip_trailing_newline(&text);
    if stripped.is_empty() {
        return Err(CliError::Usage(
            format!("No passphrase provided on stdin. {NO_PASSPHRASE_TAIL}").to_string(),
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

/// The file backend conforming to the platform-neutral loader
/// boundary (`wyrd_sync::keys::CredentialLoader`,
/// `docs/contracts/credential-loading.md`). Resolution
/// (flags-first, file-second) stays in `read_credentials*`: the
/// backend loads from resolved references only, through the same
/// readers the flag paths use, so the boundary and the backend
/// cannot drift. Interactive surfaces (stdin pipe, TTY prompt)
/// are not backends and stay outside the trait.
pub struct FileBackend;

/// Map a file-read failure onto the loader taxonomy. Ownership
/// and mode refusals — plus a symlink the reader refuses to
/// follow — are access facts (`Denied`); shape refusals are
/// validity facts (`Invalid`); a missing file is `Missing`;
/// anything else about the read itself is `Unavailable`. Every
/// detail reproduces the `CliError` `Display` it came from, so
/// [`loader_to_cli`] round-trips the wording exactly. The subject
/// rides alongside so callers map without parsing prose.
fn cli_to_loader(error: CliError, subject: CredentialSubject) -> CredentialError {
    match error {
        CliError::Io { path, source } => {
            let detail = format!("{}: {source}", path.display());
            match source.kind() {
                std::io::ErrorKind::NotFound => CredentialError::Missing { subject, detail },
                std::io::ErrorKind::PermissionDenied => CredentialError::Denied { subject, detail },
                _ => {
                    // A symlink the reader refuses to follow surfaces
                    // as ELOOP: the open was refused, so it is an
                    // access fact, not an unreachable store. Matched
                    // by number (not kind) to stay off the unstable
                    // `io_error_more` kinds.
                    #[cfg(unix)]
                    if source.raw_os_error() == Some(libc::ELOOP) {
                        return CredentialError::Denied { subject, detail };
                    }
                    CredentialError::Unavailable { subject, detail }
                }
            }
        }
        CliError::Credential { path, reason } => {
            let detail = format!("credential file {}: {reason}", path.display());
            match reason {
                "must be owned by the current user"
                | "must not be accessible by group or other users" => {
                    CredentialError::Denied { subject, detail }
                }
                "credential-file protection is only implemented on Unix" => {
                    CredentialError::Unavailable { subject, detail }
                }
                _ => CredentialError::Invalid { subject, detail },
            }
        }
        CliError::IdentityFormat => CredentialError::Invalid {
            subject,
            detail: "identity file must contain exactly 32 raw bytes or 64 hex characters"
                .to_string(),
        },
        other => CredentialError::Invalid {
            subject,
            detail: other.to_string(),
        },
    }
}

/// Map the taxonomy back to the CLI errors with identical
/// wording: every detail above reproduces the `CliError`
/// `Display` it came from, so the report on stderr and the exit
/// code (usage-class, exit 2 throughout) are unchanged by the
/// cutover. The variant collapses by design — the taxonomy, not
/// the CLI enum, is the reporting vocabulary from here on.
fn loader_to_cli(error: CredentialError) -> CliError {
    match error {
        CredentialError::Missing { detail, .. }
        | CredentialError::Denied { detail, .. }
        | CredentialError::Unavailable { detail, .. }
        | CredentialError::Invalid { detail, .. } => CliError::Usage(detail),
    }
}

impl CredentialLoader for FileBackend {
    /// Resolve a file reference to secret material through the
    /// production readers in one call — identity first, then the
    /// passphrase where the reference names one. `OsStore`
    /// references answer `Unavailable`: specified, not yet wired
    /// — never an implicit file fallback. A reference naming no
    /// passphrase item loads the identity with no passphrase:
    /// that is the session posture, and the unlock pipe or
    /// prompt supplies the passphrase downstream.
    fn load(&self, reference: &CredentialRef) -> Result<LoadedCredential, CredentialError> {
        match reference {
            CredentialRef::File { identity, passphrase } => {
                let subject = || CredentialSubject::File {
                    path: identity.clone(),
                };
                let identity =
                    read_identity(identity).map_err(|error| cli_to_loader(error, subject()))?;
                let Some(passphrase_path) = passphrase.as_ref() else {
                    return Ok(LoadedCredential {
                        identity,
                        passphrase: None,
                    });
                };
                let subject = || CredentialSubject::File {
                    path: passphrase_path.clone(),
                };
                let passphrase = read_passphrase_file(passphrase_path)
                    .map_err(|error| cli_to_loader(error, subject()))?;
                Ok(LoadedCredential {
                    identity,
                    passphrase: Some(passphrase),
                })
            }
            CredentialRef::OsStore { account } => Err(CredentialError::Unavailable {
                subject: CredentialSubject::Account {
                    account: account.clone(),
                },
                detail: format!(
                    "OS credential store backend is not implemented: no item for account {account} can be loaded"
                ),
            }),
        }
    }
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
        };
        let creds = Credentials {
            identity_file: Some(identity_file),
            passphrase_file: None,
            passphrase_stdin: true,
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
        }
    }

    /// A managed service with a piped passphrase refuses before
    /// anything is created: no definition could embed the pipe, so
    /// installing one would crash-loop under the manager.
    #[test]
    fn managed_service_with_a_pipe_refuses() {
        let err = resolve_service_credential_files(&service_creds(true, None), None, true)
            .expect_err("managed plus stdin must refuse");
        assert!(
            err.to_string()
                .contains("managed service needs a passphrase file"),
            "names the managed requirement: {err}"
        );
    }

    /// Session posture skips the file entirely: `--no-service` with
    /// a pipe resolves the identity and carries no passphrase path
    /// for the daemon to render.
    #[test]
    fn unmanaged_service_with_a_pipe_carries_no_passphrase_file() {
        use crate::tests_harness::{write_secret, TempDir};
        let temp = TempDir::new();
        let identity_file = temp.0.join("identity");
        write_secret(&identity_file, [0x44; 32]);
        let mut creds = service_creds(true, None);
        creds.identity_file = Some(identity_file.clone());
        let (identity, passphrase) = resolve_service_credential_files(&creds, None, false)
            .expect("session posture resolves without a passphrase file");
        assert_eq!(identity, identity_file);
        assert!(passphrase.is_none(), "no file for the daemon to render");
    }

    /// File posture is unchanged: managed resolves both paths for
    /// the definition exactly like before.
    #[test]
    fn managed_service_with_files_resolves_both_paths() {
        let creds = Credentials {
            identity_file: Some(PathBuf::from("/k/identity")),
            passphrase_file: Some(PathBuf::from("/k/passphrase")),
            passphrase_stdin: false,
        };
        let (identity, passphrase) =
            resolve_service_credential_files(&creds, None, true).expect("file posture resolves");
        assert_eq!(identity, PathBuf::from("/k/identity"));
        assert_eq!(passphrase, Some(PathBuf::from("/k/passphrase")));
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
    /// each `Credential` reason and each IO kind maps to the arm
    /// named beside it, so a renamed literal must update
    /// `cli_to_loader` — the table fails otherwise. A refused
    /// symlink (`ELOOP`) is `Denied`: the open was refused, and
    /// retrying the same open cannot fix it.
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
            ("must be owned by the current user", "Denied"),
            ("must not be accessible by group or other users", "Denied"),
            (
                "credential-file protection is only implemented on Unix",
                "Unavailable",
            ),
            ("not a regular file", "Invalid"),
            ("exceeds the 4096-byte size limit", "Invalid"),
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
        // `tests_cli::credential_symlinks_are_rejected_without_following`.
    }

    /// The OS store reference answers `Unavailable`, never an
    /// implicit file fallback: specified, not yet wired. The
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
                assert!(detail.contains("not implemented"), "says so: {detail}")
            }
            other => panic!("an unwired backend is Unavailable: {other:?}"),
        }
    }
}
