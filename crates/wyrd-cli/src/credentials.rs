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
    let identity = read_identity(&identity_file)?;
    if creds.passphrase_stdin {
        let passphrase = read_passphrase_stdin(stdin)?;
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
    let stripped = strip_trailing_newline(&passphrase);
    // Kept scrubbing: the passphrase lives in process memory until the
    // command completes, so it stays in a `Zeroizing` rather than a
    // plain `String`. Callers pass `&passphrase` as `&str` through
    // deref coercion.
    Ok((identity, Zeroizing::new(stripped.to_owned()), provenance))
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
    let identity = read_identity(&identity_file)?;
    if creds.passphrase_stdin {
        let passphrase = read_passphrase_stdin(stdin)?;
        return Ok((identity, passphrase, provenance));
    }
    if !stdin_is_tty {
        return Err(CliError::Usage(
            "not a terminal: pipe the passphrase with --passphrase-stdin or run unlock on a terminal (the owner never starts without it)".to_string(),
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
            "no passphrase typed: the owner never starts without it".to_string(),
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
    let stripped = strip_trailing_newline(&text);
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

    /// Empty pipes fail closed before the keystore opens: immediate
    /// EOF and a bare newline both refuse as usage, never as an
    /// empty passphrase the owner would try.
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
}
