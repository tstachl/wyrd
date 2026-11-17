//! First-run identity onboarding: the desktop's local CLI calls for
//! generation, commit, verification, and credential-pair adoption.
//! Layering: the backend never generates, holds only what it must
//! display, and never interprets secrets — the CLI owns key material,
//! file hardening, and the format (see
//! `wyrd-cli/src/commands/identity.rs`). The process runner is the
//! provisioning adapter's (`provision::run_cli`): structured argv,
//! exit codes only, bounded waits.
//!
//! - `new-passphrase` prints one generated passphrase (writes
//!   nothing); the backend validates its shape before returning it
//!   for write-down, and treats malformed stdout as unknown.
//! - `commit` reads the passphrase from the stdin pipe (never argv),
//!   generates the identity secret in-process, and writes both files
//!   (owner-only, never overwrite). The identity secret never leaves
//!   the CLI; only the passphrase transits backend -> CLI.
//! - `verify` runs the hardened reader over both files (exit codes
//!   only; stderr is opaque display detail, never parsed).
//! - Defaults: the adopted credential pair where one is recorded and
//!   structurally valid, else the platform key paths — plus existence
//!   (no secret reads) and an explicit usability flag for the
//!   home-directory-missing edge.
//!
//! Passphrase transits (CLI stdout for write-down, stdin pipe for
//! commit, IPC fields between the webview and the backend) are the
//! documented onboarding exception to the desktop-shell rule that
//! secret contents never cross IPC or argv: the write-down step
//! requires the frontend to display the text, and the commit step
//! requires the bytes. The exception, its rationale, and its
//! controls live in `docs/design/desktop-shell.md` ("Onboarding
//! passphrase handling"), not just here.
//!
//! All commands are infallible like the other local queries: the
//! answer is a state struct, never the owner taxonomy. A dead worker
//! surfaces as an explicit unknown inside the payload.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::provision::{check_cli_version, run_cli, truncate, CliOutput, PROVISION_TIMEOUT};

/// Unambiguous alphabet mirror (CLI-owned): 32 chars, 5 bits each.
/// The backend validates shape only; generation stays in the CLI.
const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Adopted-pair record: paths only, never secrets. Lives beside the
/// drive registry so the device remembers which credential files it
/// committed or picked, across restarts. A structurally valid record
/// wins over the platform defaults; existence still decides the gate
/// (missing files resolve to welcome/repair, never to a silent
/// generate). A missing or unparsable record falls back to the
/// platform defaults: the files are the source of truth, the record
/// only a pointer.
const ADOPTED_RECORD_FILENAME: &str = "credentials.json";
const ADOPTED_RECORD_VERSION: u8 = 1;

/// Platform key paths plus existence. `usable` is false only where no
/// home directory resolves (paths are then empty and nothing may
/// commit); pre-commit the UI shows usable paths as intended
/// locations, never as existing files; post-commit the same paths
/// with `exists == true` are the actual files Ready shows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct KeyDefaults {
    /// Effective identity file path: adopted pair where recorded, else platform default.
    pub identity_file: String,
    /// Effective passphrase file path: adopted pair where recorded, else platform default.
    pub passphrase_file: String,
    /// Whether the identity file exists (no secret reads).
    pub identity_exists: bool,
    /// Whether the passphrase file exists (no secret reads).
    pub passphrase_exists: bool,
    /// False only where no home directory resolves; paths are then empty and nothing may commit.
    pub usable: bool,
}

/// The per-user configuration directory: the same derivation the
/// registry uses, so the adopted-pair record sits beside the drives
/// file. `None` where no home resolves — the caller reports
/// unusable instead of inventing a relative path.
fn config_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        // Test seam (test builds only; production has no override):
        // unit tests point the record at a scratch directory so no
        // test ever touches the operator's real config dir.
        if let Some(dir) = std::env::var_os("WYRD_ONBOARDING_TEST_DIR") {
            if !dir.is_empty() {
                let dir = PathBuf::from(dir);
                if dir.is_absolute() {
                    return Some(dir);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|home| PathBuf::from(home).join("Library/Application Support/Wyrd"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
            if !dir.is_empty() && dir.starts_with('/') {
                return Some(PathBuf::from(dir).join("wyrd"));
            }
        }
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|home| PathBuf::from(home).join(".local/state/wyrd"))
    }
    #[cfg(not(any(target_os = "macos", unix)))]
    {
        None
    }
}

/// Platform default key paths: macOS Application Support, Linux
/// XDG state home (fallback `~/.local/state`). `None` where no home
/// resolves — never a literal `~` or a relative guess.
fn default_paths() -> Option<(String, String)> {
    let dir = config_dir()?;
    let keys = dir.join("keys");
    Some((
        keys.join("identity").to_string_lossy().into_owned(),
        keys.join("passphrase").to_string_lossy().into_owned(),
    ))
}

/// The adopted pair where a structurally valid record names one:
/// absolute paths only, anything else falls back to the platform
/// defaults.
fn adopted_paths() -> Option<(String, String)> {
    let dir = config_dir()?;
    adopted_paths_in(&dir)
}

/// The adopted pair inside one directory: version-gated,
/// absolute-paths-only. Missing, unparsable, wrong-version, empty,
/// or relative records all fall back — the credential files are the
/// source of truth, the record only a pointer.
fn adopted_paths_in(dir: &Path) -> Option<(String, String)> {
    let bytes = std::fs::read(dir.join(ADOPTED_RECORD_FILENAME)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("version")?.as_u64()? != u64::from(ADOPTED_RECORD_VERSION) {
        return None;
    }
    let identity = value.get("identity_file")?.as_str()?;
    let passphrase = value.get("passphrase_file")?.as_str()?;
    if identity.is_empty() || passphrase.is_empty() {
        return None;
    }
    if !Path::new(identity).is_absolute() || !Path::new(passphrase).is_absolute() {
        return None;
    }
    Some((identity.to_string(), passphrase.to_string()))
}

/// The effective credential paths: the adopted pair where recorded,
/// else the platform defaults — plus existence (no secret reads) and
/// usability.
pub fn default_key_paths() -> KeyDefaults {
    let Some((identity_file, passphrase_file)) = adopted_paths().or_else(default_paths) else {
        return KeyDefaults {
            identity_file: String::new(),
            passphrase_file: String::new(),
            identity_exists: false,
            passphrase_exists: false,
            usable: false,
        };
    };
    KeyDefaults {
        identity_exists: Path::new(&identity_file).is_file(),
        passphrase_exists: Path::new(&passphrase_file).is_file(),
        identity_file,
        passphrase_file,
        usable: true,
    }
}

/// Record the adopted credential pair (absolute paths only), so the
/// gate and later launches resolve the committed or picked files
/// instead of the platform defaults. The record holds paths, never
/// secrets. Overwrites any previous record: adoption replaces the
/// pointer, never the credential files (which the CLI never
/// overwrites either).
pub fn adopt_paths(identity_file: &Path, passphrase_file: &Path) -> Result<KeyDefaults, String> {
    if !identity_file.is_absolute() || !passphrase_file.is_absolute() {
        return Err("adopted credential paths must be absolute".to_string());
    }
    let dir = config_dir().ok_or_else(|| {
        "cannot record the credential pair: no home directory resolves".to_string()
    })?;
    adopt_paths_in(&dir, identity_file, passphrase_file)?;
    Ok(default_key_paths())
}

/// Record the pair inside one directory: the unit-testable half of
/// [`adopt_paths`]. Absolute paths only; the record holds paths,
/// never secrets.
fn adopt_paths_in(dir: &Path, identity_file: &Path, passphrase_file: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot restrict {}: {error}", dir.display()))?;
    }
    let record = serde_json::json!({
        "version": ADOPTED_RECORD_VERSION,
        "identity_file": identity_file.to_string_lossy(),
        "passphrase_file": passphrase_file.to_string_lossy(),
    });
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|error| format!("cannot encode the credential pair: {error}"))?;
    let path = dir.join(ADOPTED_RECORD_FILENAME);
    let tmp = dir.join(format!(
        "{}.tmp.{}",
        ADOPTED_RECORD_FILENAME,
        std::process::id()
    ));
    let wrote = (|| -> Result<(), String> {
        std::fs::write(&tmp, &bytes)
            .map_err(|error| format!("cannot stage {}: {error}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("cannot restrict {}: {error}", path.display()))?;
            // fsync the staged record before the rename, like the
            // credential writer: the rename then orders a durable
            // record, never a torn one.
            let handle = std::fs::File::open(&tmp)
                .map_err(|error| format!("cannot fsync {}: {error}", path.display()))?;
            handle
                .sync_all()
                .map_err(|error| format!("cannot fsync {}: {error}", path.display()))?;
        }
        std::fs::rename(&tmp, &path)
            .map_err(|error| format!("cannot record {}: {error}", path.display()))?;
        fsync_config_dir(dir)
            .map_err(|error| format!("cannot fsync {}: {error}", dir.display()))?;
        Ok(())
    })();
    if wrote.is_err() {
        // Never leave a staged temp record behind: a crash window
        // with two candidate records must not exist.
        let _ = std::fs::remove_file(&tmp);
    }
    wrote
}

/// fsync one config directory so a fresh record name is durable,
/// not just its bytes. Best-effort where the directory cannot be
/// opened for fsync (non-Unix): the record write above already
/// succeeded, and the rename carries the pointer either way.
fn fsync_config_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let handle = std::fs::File::open(dir)?;
        handle.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Shape check for a generated passphrase: 24 charset chars in
/// 6-6-6-6 groups (27 with dashes). The backend refuses to display
/// anything else as a generated passphrase.
pub fn passphrase_shape_valid(text: &str) -> bool {
    if text.len() != 27 {
        return false;
    }
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 4 {
        return false;
    }
    parts
        .iter()
        .all(|part| part.len() == 6 && part.bytes().all(|b| ALPHABET.contains(&b)))
}

/// Custom passphrase floor mirror (CLI-owned): 12 chars minimum,
/// 4096 bytes maximum, no line breaks. The backend pre-refuses
/// before spawning; the CLI re-validates before writing.
pub fn custom_passphrase_valid(text: &str) -> bool {
    let chars = text.chars().count();
    chars >= 12 && text.len() <= 4096 && !text.contains('\n') && !text.contains('\r')
}

/// One onboarding operation outcome. Infallible: every path —
/// including timeouts and spawn failures — returns a variant the
/// caller pairs with existence re-reads; no path throws except a dead
/// IPC worker (which the command layer turns into `Unknown`).
///
/// `Debug` is redacted: the generated passphrase must never reach a
/// log through a `{:?}`.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum OnboardingResult {
    /// The CLI reported success. `passphrase` carries the generated
    /// text only for `new-passphrase`; every other success carries
    /// `None` (the identity secret never leaves the CLI, and a
    /// committed passphrase is read back from its file, never
    /// echoed).
    Completed {
        /// Opaque display detail; existence re-reads say what actually exists.
        detail: String,
        /// Generated text only for `new-passphrase`; `None` for every other success.
        passphrase: Option<String>,
    },
    /// Exit 2 or an adapter-side refusal: usage, expected failure, or
    /// a validation refusal that never spawned a process. Opaque
    /// detail; existence re-reads say what actually exists.
    ExpectedFailure {
        /// Opaque display detail; existence re-reads say what actually exists.
        detail: String,
    },
    /// CLI binary missing or version-mismatched: hard failure, never
    /// silent fallback.
    VersionMismatch {
        /// Why the CLI binary is unusable; never a silent fallback.
        reason: String,
    },
    /// Timeout, spawn failure, or signal death: the outcome is
    /// unknown. Triggers re-observation, never re-execution.
    Unknown {
        /// Why the outcome is unknown; triggers re-observation, never re-execution.
        detail: String,
    },
}

impl std::fmt::Debug for OnboardingResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OnboardingResult::Completed { detail, passphrase } => f
                .debug_struct("Completed")
                .field("detail", detail)
                .field("passphrase", &passphrase.as_ref().map(|_| "REDACTED"))
                .finish(),
            OnboardingResult::ExpectedFailure { detail } => f
                .debug_struct("ExpectedFailure")
                .field("detail", detail)
                .finish(),
            OnboardingResult::VersionMismatch { reason } => f
                .debug_struct("VersionMismatch")
                .field("reason", reason)
                .finish(),
            OnboardingResult::Unknown { detail } => {
                f.debug_struct("Unknown").field("detail", detail).finish()
            }
        }
    }
}

/// One completed onboarding call: the outcome plus existence after
/// the run (the commit boundary's read-back lives here, not in the
/// CLI text). `Debug` is redacted for the same reason as the result.
#[derive(Clone, serde::Serialize)]
pub struct OnboardingReport {
    /// Operation outcome; existence re-reads say what actually exists.
    pub result: OnboardingResult,
    /// Effective credential paths plus existence after the run.
    pub defaults: KeyDefaults,
}

impl std::fmt::Debug for OnboardingReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingReport")
            .field("result", &self.result)
            .field("defaults", &self.defaults)
            .finish()
    }
}

/// Pure argv builders: one structured element per argument, never
/// shell-constructed. Secrets never appear — `commit` carries the
/// passphrase on the stdin pipe, not in argv.
pub fn new_passphrase_argv() -> Vec<std::ffi::OsString> {
    vec!["identity".into(), "new-passphrase".into()]
}

/// Pure argv builder for `<cli> identity commit`: one structured element per
/// argument, never shell-constructed. Secrets never appear — the passphrase
/// rides the stdin pipe, not argv.
pub fn commit_argv(identity_file: &Path, passphrase_file: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "identity".into(),
        "commit".into(),
        "--identity-file".into(),
        identity_file.as_os_str().to_owned(),
        "--passphrase-file".into(),
        passphrase_file.as_os_str().to_owned(),
    ]
}

/// Pure argv builder for `<cli> identity verify`: one structured element per
/// argument, never shell-constructed. Paths only, never secret contents.
pub fn verify_argv(identity_file: &Path, passphrase_file: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "identity".into(),
        "verify".into(),
        "--identity-file".into(),
        identity_file.as_os_str().to_owned(),
        "--passphrase-file".into(),
        passphrase_file.as_os_str().to_owned(),
    ]
}

/// Run `<cli> identity new-passphrase`: version check first, then the
/// structured argv run. Stdout must be exactly one shape-valid
/// passphrase; anything else resolves `Unknown` (never display
/// garbage as a passphrase).
pub fn run_new_passphrase(cli_bin: &Path, timeout: Duration) -> OnboardingReport {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return report(mismatch_into(mismatch));
    }
    let output = run_cli(cli_bin, &new_passphrase_argv(), None, timeout);
    let result = match output {
        CliOutput {
            timed_out: true,
            stderr,
            ..
        } => OnboardingResult::Unknown { detail: stderr },
        CliOutput {
            code: None, stderr, ..
        } => OnboardingResult::Unknown { detail: stderr },
        CliOutput {
            code: Some(0),
            stdout,
            status_success: true,
            ..
        } => {
            let text = stdout.trim().to_string();
            if passphrase_shape_valid(&text) {
                OnboardingResult::Completed {
                    detail: "generated passphrase ready for write-down".to_string(),
                    passphrase: Some(text),
                }
            } else {
                OnboardingResult::Unknown {
                    detail: "the CLI answered successfully but its passphrase had an unexpected shape — re-reading observed state".to_string(),
                }
            }
        }
        CliOutput {
            code: Some(_),
            stderr,
            stdout,
            ..
        } => OnboardingResult::ExpectedFailure {
            detail: truncate(&format!("{stdout}\n{stderr}"), 500),
        },
    };
    report(result)
}

/// Run `<cli> identity commit` as one atomic onboarding step: the
/// passphrase travels on the stdin pipe (never argv). Version check
/// and pre-validation first; a custom passphrase that fails the
/// floor refuses before any process spawns. On a successful write
/// the same binary verifies the pair through the hardened reader
/// and then records it as adopted — all before the report, so no
/// Ready-close window can orphan a committed pair. A verify or
/// record failure names itself: "written but not readable" and
/// "record not written" are distinct from identity failures, and
/// the view retries verification without re-running the
/// never-overwrite commit.
pub fn run_commit(
    cli_bin: &Path,
    identity_file: &Path,
    passphrase_file: &Path,
    passphrase: &str,
    timeout: Duration,
) -> OnboardingReport {
    if !custom_passphrase_valid(passphrase) {
        return report(OnboardingResult::ExpectedFailure {
            detail: "passphrase must be at least 12 characters (without line breaks) and at most 4096 bytes".to_string(),
        });
    }
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return report(mismatch_into(mismatch));
    }
    let argv = commit_argv(identity_file, passphrase_file);
    let output = run_cli(cli_bin, &argv, Some(passphrase.as_bytes()), timeout);
    if !output.status_success {
        return report(map_output(output));
    }
    let verify = run_cli(
        cli_bin,
        &verify_argv(identity_file, passphrase_file),
        None,
        timeout,
    );
    if !verify.status_success {
        return report(OnboardingResult::ExpectedFailure {
            detail: format!(
                "the files were written but are not readable yet: {}",
                truncate(&format!("{}\n{}", verify.stdout, verify.stderr), 300)
            ),
        });
    }
    if let Err(reason) = adopt_paths(identity_file, passphrase_file) {
        return report(OnboardingResult::ExpectedFailure { detail: reason });
    }
    report(map_output(output))
}

/// Run `<cli> identity verify`: exit codes only, stderr opaque.
pub fn run_verify(
    cli_bin: &Path,
    identity_file: &Path,
    passphrase_file: &Path,
    timeout: Duration,
) -> OnboardingReport {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return report(mismatch_into(mismatch));
    }
    let output = run_cli(
        cli_bin,
        &verify_argv(identity_file, passphrase_file),
        None,
        timeout,
    );
    report(map_output(output))
}

/// Verify the pair through the CLI, then record it as the adopted
/// credential pair. The record is what the gate and later launches
/// resolve; without it a restore or repair choice would be orphaned
/// after the session.
pub fn run_adopt(
    cli_bin: &Path,
    identity_file: &Path,
    passphrase_file: &Path,
    timeout: Duration,
) -> OnboardingReport {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return report(mismatch_into(mismatch));
    }
    let output = run_cli(
        cli_bin,
        &verify_argv(identity_file, passphrase_file),
        None,
        timeout,
    );
    if output.status_success {
        match adopt_paths(identity_file, passphrase_file) {
            Ok(_) => {}
            Err(reason) => {
                return report(OnboardingResult::ExpectedFailure { detail: reason });
            }
        }
    }
    report(map_output(output))
}

fn report(result: OnboardingResult) -> OnboardingReport {
    OnboardingReport {
        result,
        defaults: default_key_paths(),
    }
}

fn mismatch_into(result: super::provision::ProvisionResult) -> OnboardingResult {
    match result {
        super::provision::ProvisionResult::VersionMismatch { reason } => {
            OnboardingResult::VersionMismatch { reason }
        }
        super::provision::ProvisionResult::Unknown { detail } => {
            OnboardingResult::Unknown { detail }
        }
        other => OnboardingResult::Unknown {
            detail: format!("version check answered unexpectedly: {other:?}"),
        },
    }
}

fn map_output(output: CliOutput) -> OnboardingResult {
    if output.timed_out {
        return OnboardingResult::Unknown {
            detail: output.stderr,
        };
    }
    let Some(_code) = output.code else {
        return OnboardingResult::Unknown {
            detail: output.stderr,
        };
    };
    if output.status_success {
        return OnboardingResult::Completed {
            detail: truncate(&format!("{}\n{}", output.stdout, output.stderr), 500),
            passphrase: None,
        };
    }
    OnboardingResult::ExpectedFailure {
        detail: truncate(&format!("{}\n{}", output.stdout, output.stderr), 500),
    }
}

/// Default commit timeout: the version check plus one Argon2-free
/// write path (no KDF on this path — generation is CSPRNG only, and
/// verification derives nothing). Bounded like every other adapter
/// call; expiry kills and reaps before returning.
pub fn onboarding_timeout() -> Duration {
    PROVISION_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes the record-override tests: the override is
    /// process-global for the test binary, so only one of these
    /// holds it at a time.
    static RECORD_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

    /// Point the record dir at a fresh scratch directory; returns the
    /// guard (dropping it releases the lock) — the caller must
    /// `remove_var` the override before dropping, to leave no global
    /// state for the next test.
    fn override_record_dir(tag: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let guard = RECORD_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-adopt-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("WYRD_ONBOARDING_TEST_DIR", &dir);
        (guard, dir)
    }

    fn unoverride_record_dir(guard: std::sync::MutexGuard<'static, ()>) {
        std::env::remove_var("WYRD_ONBOARDING_TEST_DIR");
        drop(guard);
    }

    /// Shape pins: 6-6-6-6 over the 32-char alphabet, 27 with dashes.
    #[test]
    fn shape_accepts_only_the_generated_form() {
        // A well-formed sample from the allowed alphabet.
        assert!(passphrase_shape_valid("abcd6e-789abc-d6e789-abcd6e"));
        assert!(!passphrase_shape_valid("short"));
        assert!(!passphrase_shape_valid("abcd6e-789abc-d6e789-abcd6O"));
        assert!(!passphrase_shape_valid("abcd6e789abcd6e789abcd6e78"));
    }

    /// Floor pins: 11 refuse, 12 pass, newlines refuse, overlong refuses.
    #[test]
    fn custom_floor_mirrors_the_cli() {
        assert!(!custom_passphrase_valid("short-11-ch"));
        assert!(custom_passphrase_valid("exactly-12ch"));
        assert!(!custom_passphrase_valid("has\nnewline12"));
        assert!(!custom_passphrase_valid(&"x".repeat(4097)));
    }

    /// Argv pins: structured elements, secrets never in argv — commit
    /// carries paths only, the passphrase rides the stdin pipe.
    #[test]
    fn argv_carries_paths_only() {
        let commit = commit_argv(Path::new("/k/identity"), Path::new("/k/passphrase"));
        let rendered: Vec<String> = commit
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "identity",
                "commit",
                "--identity-file",
                "/k/identity",
                "--passphrase-file",
                "/k/passphrase",
            ]
        );
        assert!(!rendered
            .iter()
            .any(|a| a.contains("secret") || a.contains('-') && a.len() > 30));
        let verify = verify_argv(Path::new("/k/identity"), Path::new("/k/passphrase"));
        let rendered: Vec<String> = verify
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rendered[0], "identity");
        assert_eq!(rendered[1], "verify");
    }

    /// Serialization pins verbatim: the frontend mirror must read
    /// exactly this.
    #[test]
    fn report_serializes_verbatim() {
        let report = OnboardingReport {
            result: OnboardingResult::Completed {
                detail: "ok".to_string(),
                passphrase: Some("abcd6e-789abc-d6e789-abcd6e".to_string()),
            },
            defaults: KeyDefaults {
                identity_file: "/k/identity".to_string(),
                passphrase_file: "/k/passphrase".to_string(),
                identity_exists: true,
                passphrase_exists: true,
                usable: true,
            },
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({
                "result": {"result": "completed", "detail": "ok", "passphrase": "abcd6e-789abc-d6e789-abcd6e"},
                "defaults": {"identity_file": "/k/identity", "passphrase_file": "/k/passphrase", "identity_exists": true, "passphrase_exists": true, "usable": true},
            })
        );
    }

    /// The adopted-pair record round-trips through one directory
    /// and resolves: write, read, and no temp file left behind.
    /// Missing, unparsable, wrong-version, empty, and relative
    /// records all fall back — the credential files stay the source
    /// of truth.
    #[test]
    fn adopted_record_round_trips_and_rejects() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-adopt-{}-roundtrip",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        // Missing record falls back.
        assert!(adopted_paths_in(&dir).is_none());
        let identity = dir.join("identity");
        let passphrase = dir.join("passphrase");
        adopt_paths_in(&dir, &identity, &passphrase).unwrap();
        assert_eq!(
            adopted_paths_in(&dir).unwrap(),
            (
                identity.to_string_lossy().into_owned(),
                passphrase.to_string_lossy().into_owned()
            )
        );
        // No staged temp file survives a success.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{ADOPTED_RECORD_FILENAME}.tmp."))
            })
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
        // Unparsable, wrong-version, empty, and relative records
        // fall back instead of resolving.
        std::fs::write(dir.join(ADOPTED_RECORD_FILENAME), b"not json").unwrap();
        assert!(adopted_paths_in(&dir).is_none());
        std::fs::write(
            dir.join(ADOPTED_RECORD_FILENAME),
            serde_json::json!({"version": 99, "identity_file": "/a", "passphrase_file": "/b"})
                .to_string(),
        )
        .unwrap();
        assert!(adopted_paths_in(&dir).is_none());
        std::fs::write(
            dir.join(ADOPTED_RECORD_FILENAME),
            serde_json::json!({"version": 1, "identity_file": "", "passphrase_file": "/b"})
                .to_string(),
        )
        .unwrap();
        assert!(adopted_paths_in(&dir).is_none());
        std::fs::write(
            dir.join(ADOPTED_RECORD_FILENAME),
            serde_json::json!({"version": 1, "identity_file": "relative", "passphrase_file": "/b"})
                .to_string(),
        )
        .unwrap();
        assert!(adopted_paths_in(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The public adoption gate refuses relative paths before
    /// touching the filesystem.
    #[test]
    fn adopt_paths_refuses_relative_paths() {
        assert!(adopt_paths(Path::new("relative"), Path::new("/abs")).is_err());
        assert!(adopt_paths(Path::new("/abs"), Path::new("relative")).is_err());
    }

    /// `Debug` never leaks the passphrase.
    #[test]
    fn debug_redacts_the_passphrase() {
        let result = OnboardingResult::Completed {
            detail: "ok".to_string(),
            passphrase: Some("abcd6e-789abc-d6e789-abcd6e".to_string()),
        };
        let rendered = format!("{result:?}");
        assert!(rendered.contains("REDACTED"), "rendered: {rendered}");
        assert!(!rendered.contains("abcd6e"), "rendered: {rendered}");
    }

    /// Write a fake `wyrd` CLI: answers `__build_info` with the
    /// workspace version and the onboarding verbs with canned
    /// answers. `new-passphrase` prints one shape-valid phrase;
    /// `commit` drains stdin and exits 0 without writing (the
    /// adapter maps exit codes only); `verify` exits 0. The adapter
    /// spawns it exactly like the installed binary — no mock.
    fn fake_cli(tag: &str, verify_code: i32, wedge: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-onboarding-fakecli-{}-{}",
            std::process::id(),
            tag
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wyrd");
        let version = env!("CARGO_PKG_VERSION");
        let script = if wedge {
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nsleep 30\nexit 0\n"
            )
        } else {
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nif [ \"$1\" = \"identity\" ] && [ \"$2\" = \"new-passphrase\" ]; then\necho \"abcd6e-789abc-d6e789-abcd6e\"\nexit 0\nfi\nif [ \"$1\" = \"identity\" ] && [ \"$2\" = \"commit\" ]; then\ncat > /dev/null\nexit 0\nfi\nif [ \"$1\" = \"identity\" ] && [ \"$2\" = \"verify\" ]; then\nexit {verify_code}\nfi\nexit 2\n"
            )
        };
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// `new-passphrase` returns the shape-valid phrase from stdout;
    /// malformed output would resolve unknown instead.
    #[test]
    fn new_passphrase_returns_the_shaped_phrase() {
        let cli = fake_cli("phrase", 0, false);
        let report = run_new_passphrase(&cli, Duration::from_secs(30));
        match &report.result {
            OnboardingResult::Completed { passphrase, .. } => {
                assert_eq!(passphrase.as_deref(), Some("abcd6e-789abc-d6e789-abcd6e"));
            }
            other => panic!("fake new-passphrase completes: {other:?}"),
        }
    }

    /// `commit` pipes the passphrase on stdin (the fake drains it),
    /// maps exit 0 to completed without echoing the text, and then
    /// verifies and adopts atomically: on success the record names
    /// the committed pair, so no Ready-close window can orphan it.
    #[test]
    fn commit_pipes_stdin_and_maps_the_code_only() {
        let (guard, dir) = override_record_dir("commit");
        let cli = fake_cli("commit", 0, false);
        let report = run_commit(
            &cli,
            Path::new("/k/identity"),
            Path::new("/k/passphrase"),
            "exactly-12ch",
            Duration::from_secs(30),
        );
        match &report.result {
            OnboardingResult::Completed { passphrase, .. } => assert!(passphrase.is_none()),
            other => panic!("fake exit-0 commit completes: {other:?}"),
        }
        assert_eq!(
            adopted_paths_in(&dir).unwrap(),
            ("/k/identity".to_string(), "/k/passphrase".to_string())
        );
        unoverride_record_dir(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `verify` maps a nonzero exit to an expected failure.
    #[test]
    fn verify_maps_exit_2_to_expected_failure() {
        let cli = fake_cli("verify-fail", 2, false);
        let report = run_verify(
            &cli,
            Path::new("/k/identity"),
            Path::new("/k/passphrase"),
            Duration::from_secs(30),
        );
        assert!(
            matches!(&report.result, OnboardingResult::ExpectedFailure { .. }),
            "exit 2 fails expectedly: {:?}",
            report.result
        );
    }

    /// `adopt` verifies through the CLI and then records the pair:
    /// the Repair pick path persists in the same call, never in two.
    #[test]
    fn adopt_verifies_then_records() {
        let (guard, dir) = override_record_dir("adopt");
        let cli = fake_cli("adopt", 0, false);
        let report = run_adopt(
            &cli,
            Path::new("/k/identity"),
            Path::new("/k/passphrase"),
            Duration::from_secs(30),
        );
        assert!(
            matches!(
                &report.result,
                OnboardingResult::Completed {
                    passphrase: None,
                    ..
                }
            ),
            "fake exit-0 adopt completes: {:?}",
            report.result
        );
        assert_eq!(
            adopted_paths_in(&dir).unwrap(),
            ("/k/identity".to_string(), "/k/passphrase".to_string())
        );
        unoverride_record_dir(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed verify records nothing: the gate keeps resolving the
    /// previous pair instead of a pair that never checked out.
    #[test]
    fn adopt_records_nothing_on_verify_failure() {
        let (guard, dir) = override_record_dir("adopt-fail");
        let cli = fake_cli("adopt-fail", 2, false);
        let report = run_adopt(
            &cli,
            Path::new("/k/identity"),
            Path::new("/k/passphrase"),
            Duration::from_secs(30),
        );
        assert!(
            matches!(&report.result, OnboardingResult::ExpectedFailure { .. }),
            "exit 2 fails expectedly: {:?}",
            report.result
        );
        assert!(
            !dir.join(ADOPTED_RECORD_FILENAME).exists(),
            "no record on verify failure"
        );
        unoverride_record_dir(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A wedged CLI is killed past the timeout even with stdin
    /// piped: the outcome resolves unknown and the wait stays
    /// bounded.
    #[test]
    fn wedged_commit_resolves_unknown() {
        let cli = fake_cli("wedge", 0, true);
        let start = std::time::Instant::now();
        let report = run_commit(
            &cli,
            Path::new("/k/identity"),
            Path::new("/k/passphrase"),
            "exactly-12ch",
            Duration::from_secs(2),
        );
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the wait is bounded by the timeout"
        );
        assert!(
            matches!(&report.result, OnboardingResult::Unknown { .. }),
            "a wedged CLI resolves unknown: {:?}",
            report.result
        );
    }
}
