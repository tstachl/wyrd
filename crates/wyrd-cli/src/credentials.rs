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
use wyrd_credential_store::OsStoreBackend;
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

/// Route one credential reference to the backend that serves it:
/// file references to the hardened file reader below, store
/// references to the OS credential-store backend
/// (`wyrd-credential-store`). The reference already chose its
/// backend; the dispatcher never retries across them and never
/// prompts — a wrong-backend answer stays terminal, exactly as the
/// backend reported it.
pub(crate) fn load_credential(
    reference: &CredentialRef,
) -> Result<LoadedCredential, CredentialError> {
    match reference {
        CredentialRef::File { .. } => FileBackend.load(reference),
        CredentialRef::OsStore { .. } => OsStoreBackend.load(reference),
    }
}

/// Which backend one invocation resolves to: exactly one per
/// drive, from the `--credential-backend` flag (winning where
/// present) or the drive configuration. A flag that contradicts
/// the configuration refuses — never a silent fallback — so the
/// service definition and the configuration can never name
/// different backends without the operator hearing about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EffectiveBackend {
    /// The hardened file backend: flags, then configuration.
    File,
    /// The OS credential store: the account label, from the flag
    /// where present, else the configuration. Always validated at
    /// parse shape (see `validate_credential_account`).
    OsStore {
        /// The store account label naming the drive's item.
        account: String,
    },
}

/// Resolve the effective backend for one invocation from the flag
/// and the loaded configuration. The flag wins where present; a
/// flag that contradicts the configuration is a usage refusal
/// naming both, never a precedence rule. The account follows the
/// same rule: the flag wins, a contradiction refuses, and
/// `os-store` without an account refuses naming the missing
/// selector.
pub(crate) fn effective_backend(
    creds: &Credentials,
    config: Option<&LoadedConfig>,
) -> Result<EffectiveBackend, CliError> {
    if let Some(backend) = creds.credential_backend.as_deref() {
        if backend != "file" && backend != "os-store" {
            return Err(CliError::Usage(format!(
                "unknown --credential-backend {backend:?}: expected \"file\" or \"os-store\""
            )));
        }
    }
    let flag_backend = creds.credential_backend.as_deref();
    let config_backend = config.and_then(|c| c.credential_backend.as_deref());
    if let (Some(flag), Some(configured)) = (flag_backend, config_backend) {
        if flag != configured {
            return Err(CliError::Usage(format!(
                "credential backend mismatch: --credential-backend {flag:?} names one backend but the drive configuration names {configured:?}: one backend per drive"
            )));
        }
    }
    let backend = flag_backend.or(config_backend).unwrap_or("file");
    let flag_account = creds.credential_account.as_deref();
    let config_account = config.and_then(|c| c.credential_account.as_deref());
    if let (Some(flag), Some(configured)) = (flag_account, config_account) {
        if flag != configured {
            return Err(CliError::Usage(
                "credential account mismatch: --credential-account names one store item but the drive configuration names another: one backend per drive"
                    .to_string(),
            ));
        }
    }
    match backend {
        "os-store" => {
            let account = flag_account.or(config_account).ok_or_else(|| {
                CliError::Usage(
                    "credential backend is \"os-store\" but no credential account is set: pass --credential-account or set credential_account in the drive configuration"
                        .to_string(),
                )
            })?;
            // The account shape check rides the same rule as the
            // configuration parser: the flag-spelled account is
            // validated before anything loads, so a bad flag
            // refuses with its name.
            crate::drive_config::validate_credential_account(
                std::path::Path::new("--credential-account"),
                account,
            )?;
            Ok(EffectiveBackend::OsStore {
                account: account.to_string(),
            })
        }
        _ => {
            // The configuration parser already refuses an account
            // beside the file backend; the flag needs the same
            // guard for a config-less invocation.
            if let Some(account) = flag_account {
                return Err(CliError::Usage(format!(
                    "--credential-account {account:?} is set but the credential backend is not \"os-store\": one backend per drive"
                )));
            }
            Ok(EffectiveBackend::File)
        }
    }
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
    // A store-backed drive resolves through the OS credential store:
    // file flags beside a store configuration are ambiguity, not
    // precedence — one backend per drive, refused by name. The
    // flag wins where present; a flag that contradicts the
    // configuration refuses, never silently falls back.
    match effective_backend(creds, config)? {
        EffectiveBackend::OsStore { account } => {
            if creds.identity_file.is_some() || creds.passphrase_file.is_some() {
                return Err(CliError::Usage(
                    "drive credentials name the OS credential store but --identity-file/--passphrase-file name files: one backend per drive".to_string(),
                ));
            }
            if creds.passphrase_stdin {
                return Err(CliError::Usage(
                    "drive credentials name the OS credential store: --passphrase-stdin names a pipe the store backend never reads".to_string(),
                ));
            }
            let reference = CredentialRef::OsStore { account };
            let loaded = load_credential(&reference).map_err(loader_to_cli)?;
            let passphrase = loaded.passphrase.ok_or_else(|| {
                CliError::Usage(
                    "OS credential-store item names no passphrase: file-backed session posture has no store equivalent here".to_string(),
                )
            })?;
            let provenance = CredentialProvenance {
                from_os_store: true,
                ..CredentialProvenance::default()
            };
            return Ok((loaded.identity, passphrase, provenance));
        }
        EffectiveBackend::File => {}
    }
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
        from_os_store: false,
    };
    if creds.passphrase_stdin {
        // The session posture loads the identity through the
        // boundary with no passphrase item; the pipe supplies
        // the passphrase downstream, never the loader.
        let loaded = load_credential(&CredentialRef::File {
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
    let loaded = load_credential(&CredentialRef::File {
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
    // The session unlock flow for store-backed drives loads the
    // identity from the store and takes the passphrase from the
    // terminal prompt or the stdin pipe — never from a file, never
    // from the store. The stored item stays the managed credential;
    // unlock borrows only its identity and proves the keystore with
    // a typed session passphrase, so adopting the store never
    // enables unattended startup by accident.
    if let EffectiveBackend::OsStore { account } = effective_backend(creds, config)? {
        // File flags beside a store unlock are ambiguity, not
        // precedence: the passphrase file already refused above
        // for every unlock, and the identity file refuses here —
        // the store names the identity, never a file.
        if creds.identity_file.is_some() {
            return Err(CliError::Usage(
                "unlock for an OS credential-store drive loads the identity from the store: omit --identity-file".to_string(),
            ));
        }
        let loaded = load_credential(&CredentialRef::OsStore { account }).map_err(loader_to_cli)?;
        let identity = loaded.identity;
        // The stored passphrase (where the item names one) is
        // dropped here: the session proves the keystore with what
        // the operator types, not with what the manager reads.
        drop(loaded.passphrase);
        let provenance = CredentialProvenance {
            identity_from_file: false,
            passphrase_from_file: false,
            passphrase_from_stdin: creds.passphrase_stdin,
            passphrase_from_prompt: !creds.passphrase_stdin,
            from_os_store: true,
        };
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
        return Ok((identity, passphrase, provenance));
    }
    let provenance = CredentialProvenance {
        identity_from_file: creds.identity_file.is_none(),
        passphrase_from_file: false,
        passphrase_from_stdin: creds.passphrase_stdin,
        passphrase_from_prompt: !creds.passphrase_stdin,
        from_os_store: false,
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
    let loaded = load_credential(&CredentialRef::File {
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
/// Resolve the credential a provision or reconfigure hands the
/// daemon: exactly one backend, as a reference (paths or the store
/// account) — never secrets. File postures resolve paths the way
/// `load_credentials` resolves them: the identity file always
/// (every posture reads it), the passphrase file only for a
/// managed service. A piped or prompted passphrase with
/// `manage_service` refuses — a definition embedding no readable
/// file would install cleanly and then crash-loop under the
/// manager. Session posture is `--no-service` (no definition
/// rendered) plus `drive unlock` for starts. Store postures name
/// the account; the manager resolves the item, no files involved.
pub(crate) fn resolve_service_credential(
    creds: &Credentials,
    config: Option<&LoadedConfig>,
    manage_service: bool,
) -> Result<wyrd_daemon::bootstrap::ServiceCredential, CliError> {
    use wyrd_daemon::bootstrap::ServiceCredential;
    match effective_backend(creds, config)? {
        EffectiveBackend::OsStore { account } => Ok(ServiceCredential::OsStore { account }),
        EffectiveBackend::File => {
            if creds.passphrase_stdin && manage_service {
                return Err(CliError::Usage(
                    "managed service needs a passphrase file the manager can read: omit --passphrase-stdin (or provision with --no-service and start sessions with `drive unlock`)".to_string(),
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
            Ok(ServiceCredential::File {
                identity_file,
                passphrase_file,
            })
        }
    }
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
pub(crate) fn read_passphrase_stdin(
    reader: impl std::io::Read,
) -> Result<Zeroizing<String>, CliError> {
    // The bound is the OS backend's: the stdin and store surfaces
    // share one literal, so a passphrase the pipe accepts is one
    // the store accepts. (The terminal prompt and the identity
    // writer pin their own 4096; see `prompt_passphrase_tty` and
    // `commands/identity.rs`.)
    use wyrd_credential_store::MAX_PASSPHRASE_BYTES;
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

/// Shared refusal literals: the reader emits these and
/// `cli_to_loader` classifies by them, so a rename touches both
/// sides together — the mapping test pins the arms, and the
/// wording-parity test pins the full render against the
/// `CliError` `Display` impls.
const REASON_UNIX_ONLY: &str = "credential-file protection is only implemented on Unix";
const REASON_NOT_REGULAR: &str = "not a regular file";
const REASON_OWNERSHIP: &str = "must be owned by the current user";
const REASON_MODES: &str = "must not be accessible by group or other users";
const REASON_SIZE_LIMIT: &str = "exceeds the 4096-byte size limit";
/// Identity shape message: `CliError::IdentityFormat` renders
/// this literal (`lib.rs`), and the taxonomy carries the same
/// text — the wording-parity test asserts they stay equal.
const IDENTITY_SHAPE_MSG: &str =
    "identity file must contain exactly 32 raw bytes or 64 hex characters";

/// Read a bounded credential file without following symlinks. On Unix the
/// file must belong to the current user and not grant group/other access.
pub(crate) fn read_secret_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, CliError> {
    const MAX_BYTES: usize = 4096;
    #[cfg(not(unix))]
    return Err(CliError::Credential {
        path: path.to_path_buf(),
        reason: REASON_UNIX_ONLY,
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
            reason: REASON_NOT_REGULAR,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != current_uid() {
            return Err(CliError::Credential {
                path: path.to_path_buf(),
                reason: REASON_OWNERSHIP,
            });
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(CliError::Credential {
                path: path.to_path_buf(),
                reason: REASON_MODES,
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
            reason: REASON_SIZE_LIMIT,
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
                REASON_OWNERSHIP | REASON_MODES => CredentialError::Denied { subject, detail },
                REASON_UNIX_ONLY => CredentialError::Unavailable { subject, detail },
                _ => CredentialError::Invalid { subject, detail },
            }
        }
        CliError::IdentityFormat => CredentialError::Invalid {
            subject,
            detail: IDENTITY_SHAPE_MSG.to_string(),
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
/// Shared with the migration path, which reports backend refusals
/// the same way.
pub(crate) fn loader_to_cli(error: CredentialError) -> CliError {
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
    /// references answer `Unavailable`: the file backend never
    /// serves the store — the CLI dispatcher routes those to the OS
    /// credential-store backend, and this arm stays terminal so no
    /// path silently substitutes one backend for the other. A
    /// reference naming no passphrase item loads the identity with
    /// no passphrase: that is the session posture, and the unlock
    /// pipe or prompt supplies the passphrase downstream.
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
                    "the file backend never serves OS store references: no item for account {account} can be loaded here"
                ),
            }),
        }
    }

    /// Provision fresh credential files under the commit boundary:
    /// validate in memory (reference and material agree on the
    /// passphrase item), write owner-only without overwriting
    /// (presence is `Invalid` — provisioning never overwrites),
    /// then read back through [`FileBackend::load`] and compare.
    /// Success is reported only where the read-back matches. The
    /// identity stores as the canonical hex the reader accepts, so
    /// raw and migrated files open the same keystore. `OsStore`
    /// references answer `Unavailable`, exactly as in `load`.
    fn store(
        &self,
        reference: &CredentialRef,
        material: &LoadedCredential,
    ) -> Result<(), CredentialError> {
        match reference {
            CredentialRef::File { identity, passphrase } => {
                check_posture_agreement(identity, passphrase, material)?;
                store_file_credential(identity, passphrase.as_ref(), material)
            }
            CredentialRef::OsStore { account } => Err(CredentialError::Unavailable {
                subject: CredentialSubject::Account {
                    account: account.clone(),
                },
                detail: format!(
                    "the file backend never serves OS store references: no item for account {account} can be stored here"
                ),
            }),
        }
    }

    /// Replace the credential files under the same commit boundary
    /// as [`FileBackend::store`], with backups: existing files copy
    /// aside to `<path>.bak` first (a stale backup from a killed run
    /// refuses — recover it by hand), then both files overwrite
    /// atomically, read back, and compare. A mismatch restores the
    /// backups and reports `Invalid`; only a verified pair removes
    /// them. Upserts, so retried callers stay idempotent.
    fn replace(
        &self,
        reference: &CredentialRef,
        material: &LoadedCredential,
    ) -> Result<(), CredentialError> {
        match reference {
            CredentialRef::File { identity, passphrase } => {
                check_posture_agreement(identity, passphrase, material)?;
                replace_file_credential(identity, passphrase.as_ref(), material)
            }
            CredentialRef::OsStore { account } => Err(CredentialError::Unavailable {
                subject: CredentialSubject::Account {
                    account: account.clone(),
                },
                detail: format!(
                    "the file backend never serves OS store references: no item for account {account} can be replaced here"
                ),
            }),
        }
    }

    /// Delete the credential files: identity first, then the
    /// passphrase where the reference names one. Answers `Missing`
    /// naming the first absent path, refuses to delete through a
    /// symlink (`Denied`), and verifies the removal with a
    /// read-back — a still-present or unverifiable pair is
    /// `Invalid`, never a claimed success.
    fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
        match reference {
            CredentialRef::File { identity, passphrase } => {
                delete_file_credential(identity, passphrase.as_ref())?;
                match FileBackend.load(reference) {
                    Err(CredentialError::Missing { .. }) => Ok(()),
                    Err(error) => Err(CredentialError::Invalid {
                        subject: CredentialSubject::File {
                            path: identity.clone(),
                        },
                        detail: format!(
                            "credential files for {} are removed but unverifiable: {error}",
                            identity.display()
                        ),
                    }),
                    Ok(_) => Err(CredentialError::Invalid {
                        subject: CredentialSubject::File {
                            path: identity.clone(),
                        },
                        detail: format!(
                            "credential files for {} are still present after deletion",
                            identity.display()
                        ),
                    }),
                }
            }
            CredentialRef::OsStore { account } => Err(CredentialError::Unavailable {
                subject: CredentialSubject::Account {
                    account: account.clone(),
                },
                detail: format!(
                    "the file backend never serves OS store references: no item for account {account} can be deleted here"
                ),
            }),
        }
    }
}

/// Reference and material must agree on the passphrase item: a file
/// reference naming a passphrase file with passphrase-less material
/// (or a bare identity reference with material carrying one) is
/// `Invalid` — the writer never invents or drops an item the
/// reference does not name.
fn check_posture_agreement(
    identity: &Path,
    passphrase: &Option<PathBuf>,
    material: &LoadedCredential,
) -> Result<(), CredentialError> {
    let agrees = passphrase.is_some() == material.passphrase.is_some();
    if agrees {
        Ok(())
    } else {
        Err(CredentialError::Invalid {
            subject: CredentialSubject::File {
                path: identity.to_path_buf(),
            },
            detail: "credential reference and material disagree on the passphrase item: a writer never invents or drops one".to_string(),
        })
    }
}

/// The write-half commit boundary for files, shared by provisioning
/// and replacement: the read-back through the production loader
/// with identity compared by device id (the public output, never
/// the bytes) and passphrase byte for byte. A mismatch is `Invalid`:
/// the write is never reported successful unverified. Material with
/// a trailing-newline passphrase cannot round-trip the file line
/// discipline — that write is `Invalid`, fail closed, never a
/// silent rewrite.
fn verify_file_written(
    identity: &Path,
    passphrase: Option<&PathBuf>,
    material: &LoadedCredential,
) -> Result<(), CredentialError> {
    let reference = CredentialRef::File {
        identity: identity.to_path_buf(),
        passphrase: passphrase.cloned(),
    };
    let loaded = FileBackend.load(&reference)?;
    if loaded.identity.device_id() != material.identity.device_id()
        || loaded.passphrase.as_deref() != material.passphrase.as_deref()
    {
        return Err(CredentialError::Invalid {
            subject: CredentialSubject::File {
                path: identity.to_path_buf(),
            },
            detail: format!(
                "credential files for {} did not read back: refusing unverified success",
                identity.display()
            ),
        });
    }
    Ok(())
}

/// Provision fresh files on Unix: owner-only parents, never
/// overwrite (presence is `Invalid` via `write_new`), identity
/// first with passphrase rollback on failure — the same order and
/// rollback `commit` uses — then [`verify_file_written`].
#[cfg(unix)]
fn store_file_credential(
    identity: &Path,
    passphrase: Option<&PathBuf>,
    material: &LoadedCredential,
) -> Result<(), CredentialError> {
    use crate::commands::identity::{ensure_parent, fsync_parent, write_new};
    let subject = || CredentialSubject::File {
        path: identity.to_path_buf(),
    };
    // Both parents before either write: a refused passphrase
    // parent must not leave a half-provisioned identity behind
    // (a retry would then fail with "already exists").
    ensure_parent(identity).map_err(|error| cli_to_loader(error, subject()))?;
    if let Some(passphrase_path) = passphrase {
        let subject = || CredentialSubject::File {
            path: passphrase_path.clone(),
        };
        ensure_parent(passphrase_path).map_err(|error| cli_to_loader(error, subject()))?;
    }
    let identity_hex = material.identity.to_hex();
    write_new(identity, identity_hex.as_bytes())
        .map_err(|error| cli_to_loader(error, subject()))?;
    if let (Some(passphrase_path), Some(passphrase_material)) =
        (passphrase, material.passphrase.as_ref())
    {
        let subject = || CredentialSubject::File {
            path: passphrase_path.clone(),
        };
        let outcome = write_new(passphrase_path, passphrase_material.as_bytes());
        if let Err(error) = outcome {
            // Rollback the identity: a half-provisioned pair is worse
            // than none — the same rollback `commit` performs.
            let _ = std::fs::remove_file(identity);
            let _ = fsync_parent(identity);
            return Err(cli_to_loader(error, subject()));
        }
    }
    verify_file_written(identity, passphrase, material)
}

/// No file backend off Unix: credential files are Unix-only, so the
/// write half answers `Unavailable` like the read half does.
#[cfg(not(unix))]
fn store_file_credential(
    identity: &Path,
    _passphrase: Option<&PathBuf>,
    _material: &LoadedCredential,
) -> Result<(), CredentialError> {
    Err(cli_to_loader(
        CliError::Credential {
            path: identity.to_path_buf(),
            reason: REASON_UNIX_ONLY,
        },
        CredentialSubject::File {
            path: identity.to_path_buf(),
        },
    ))
}

/// Replace files on Unix under backup: existing files copy aside to
/// `<path>.bak` first (a stale backup refuses — recover it by hand,
/// the writer never destroys the only copy), then both files
/// overwrite atomically, read back, and compare. A mismatch restores
/// the backups and reports `Invalid`; only a verified pair removes
/// them, so an interrupted replace leaves the old pair or the new
/// pair plus backups — never a torn file, never a silent loss.
#[cfg(unix)]
fn replace_file_credential(
    identity: &Path,
    passphrase: Option<&PathBuf>,
    material: &LoadedCredential,
) -> Result<(), CredentialError> {
    use crate::commands::identity::{ensure_parent, fsync_parent, overwrite_file};
    let subject = || CredentialSubject::File {
        path: identity.to_path_buf(),
    };
    ensure_parent(identity).map_err(|error| cli_to_loader(error, subject()))?;
    let identity_hex = material.identity.to_hex();
    let pairs: Vec<(&Path, &[u8])> = match (passphrase, material.passphrase.as_ref()) {
        (Some(passphrase_path), Some(passphrase_material)) => {
            ensure_parent(passphrase_path).map_err(|error| cli_to_loader(error, subject()))?;
            vec![
                (identity, identity_hex.as_bytes()),
                (passphrase_path, passphrase_material.as_bytes()),
            ]
        }
        (None, None) => vec![(identity, identity_hex.as_bytes())],
        // Unreachable: `check_posture_agreement` ran first. The
        // explicit arm keeps a future refactor from silently
        // inventing or dropping an item here.
        _ => {
            return Err(CredentialError::Invalid {
                subject: subject(),
                detail: "credential reference and material disagree on the passphrase item: a writer never invents or drops one".to_string(),
            });
        }
    };
    // Back up what exists; refuse over a stale backup.
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (path, _) in &pairs {
        let backup = match path.extension().and_then(|extension| extension.to_str()) {
            Some(previous) => path.with_extension(format!("{previous}-bak")),
            None => path.with_extension("bak"),
        };
        if std::fs::symlink_metadata(&backup).is_ok() {
            return Err(CredentialError::Invalid {
                subject: CredentialSubject::File { path: backup },
                detail:
                    "a stale credential backup already exists: recover it by hand before replacing"
                        .to_string(),
            });
        }
        if std::fs::symlink_metadata(path).is_ok() {
            std::fs::copy(path, &backup).map_err(|source| {
                cli_to_loader(
                    CliError::Io {
                        path: path.to_path_buf(),
                        source,
                    },
                    CredentialSubject::File {
                        path: path.to_path_buf(),
                    },
                )
            })?;
            backups.push((path.to_path_buf(), backup));
        }
    }
    let outcome = (|| {
        for (path, bytes) in &pairs {
            overwrite_file(path, bytes).map_err(|error| {
                cli_to_loader(
                    error,
                    CredentialSubject::File {
                        path: path.to_path_buf(),
                    },
                )
            })?;
        }
        verify_file_written(identity, passphrase, material)
    })();
    match outcome {
        Ok(()) => {
            for (_, backup) in &backups {
                let _ = std::fs::remove_file(backup);
            }
            let _ = fsync_parent(identity);
            Ok(())
        }
        Err(error) => {
            // Restore what was backed up: the pair returns to the
            // pre-replace state, and the error names the failed
            // verification, never secret bytes. A backup whose
            // restore fails is kept — never destroy the only copy
            // — and named in the error.
            let mut leftovers: Vec<PathBuf> = Vec::new();
            for (path, backup) in &backups {
                if std::fs::copy(backup, path).is_err() {
                    leftovers.push(backup.clone());
                    continue;
                }
                let _ = std::fs::remove_file(backup);
            }
            if leftovers.is_empty() {
                Err(error)
            } else {
                let names = leftovers
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(CredentialError::Invalid {
                    subject: CredentialSubject::File {
                        path: identity.to_path_buf(),
                    },
                    detail: format!(
                        "credential files for {} did not read back and the backup restore failed, backup kept at {names}: {error}",
                        identity.display()
                    ),
                })
            }
        }
    }
}

/// No file backend off Unix: see [`store_file_credential`].
#[cfg(not(unix))]
fn replace_file_credential(
    identity: &Path,
    _passphrase: Option<&PathBuf>,
    _material: &LoadedCredential,
) -> Result<(), CredentialError> {
    Err(cli_to_loader(
        CliError::Credential {
            path: identity.to_path_buf(),
            reason: REASON_UNIX_ONLY,
        },
        CredentialSubject::File {
            path: identity.to_path_buf(),
        },
    ))
}

/// Delete files on Unix: refuse through symlinks, `Missing` where
/// absent, identity first then the passphrase. Removal of a symlink
/// itself would be safe, but a link where a credential file belongs
/// is a fact to refuse (`Denied`), never to clean up quietly.
#[cfg(unix)]
fn delete_file_credential(
    identity: &Path,
    passphrase: Option<&PathBuf>,
) -> Result<(), CredentialError> {
    use crate::commands::identity::fsync_parent;
    remove_one_credential_file(identity)?;
    if let Some(passphrase_path) = passphrase {
        remove_one_credential_file(passphrase_path)?;
    }
    let _ = fsync_parent(identity);
    Ok(())
}

/// Remove one credential file: `Missing` where absent, `Denied`
/// through a symlink (a link where a credential file belongs is a
/// fact to refuse, never to clean up quietly), `Invalid` for a
/// directory.
#[cfg(unix)]
fn remove_one_credential_file(path: &Path) -> Result<(), CredentialError> {
    let subject = || CredentialSubject::File {
        path: path.to_path_buf(),
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(CredentialError::Missing {
                subject: subject(),
                detail: format!("no credential file {}", path.display()),
            });
        }
        Err(source) => {
            return Err(cli_to_loader(
                CliError::Io {
                    path: path.to_path_buf(),
                    source,
                },
                subject(),
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(CredentialError::Denied {
            subject: subject(),
            detail: format!(
                "credential file {} is a symlink: refusing to delete through it",
                path.display()
            ),
        });
    }
    if metadata.is_dir() {
        return Err(CredentialError::Invalid {
            subject: subject(),
            detail: format!("credential file {} is not a regular file", path.display()),
        });
    }
    std::fs::remove_file(path).map_err(|source| {
        cli_to_loader(
            CliError::Io {
                path: path.to_path_buf(),
                source,
            },
            subject(),
        )
    })
}

/// No file backend off Unix: see [`store_file_credential`].
#[cfg(not(unix))]
fn delete_file_credential(
    identity: &Path,
    _passphrase: Option<&PathBuf>,
) -> Result<(), CredentialError> {
    Err(cli_to_loader(
        CliError::Credential {
            path: identity.to_path_buf(),
            reason: REASON_UNIX_ONLY,
        },
        CredentialSubject::File {
            path: identity.to_path_buf(),
        },
    ))
}

#[cfg(test)]
#[path = "tests_credentials.rs"]
mod tests;
