//! First-run identity onboarding: the desktop's local CLI calls for
//! generation, commit, and verification. Layering: the backend never
//! generates, holds only what it must display, and never interprets
//! secrets — the CLI owns key material, file hardening, and the
//! format (see `wyrd-cli/src/commands/identity.rs`).
//!
//! - `new-passphrase` prints one generated passphrase (writes
//!   nothing); the backend validates its shape before returning it
//!   for write-down, and treats malformed stdout as unknown.
//! - `commit` reads the passphrase from the stdin pipe (never argv),
//!   generates the identity secret in-process, and writes both files
//!   (owner-only, never overwrite). The identity secret never leaves
//!   the CLI; only the passphrase transits backend -> CLI, the
//!   onboarding-only exception the commit boundary requires.
//! - `verify` runs the hardened reader over both files (exit codes
//!   only; stderr is opaque display detail, never parsed).
//! - Defaults: platform key paths plus existence (no secret reads).
//!
//! All commands are infallible like the other local queries: the
//! answer is a state struct, never the owner taxonomy. A dead worker
//! surfaces as an explicit unreadable state inside the payload.

use std::path::Path;
use std::time::Duration;

use super::provision::{check_cli_version, PROVISION_TIMEOUT};

/// Unambiguous alphabet mirror (CLI-owned): 32 chars, 5 bits each.
/// The backend validates shape only; generation stays in the CLI.
const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Platform key paths plus existence. `intended` labels the
/// created-on-finish rule: pre-commit the UI shows these as intended
/// locations, never as existing files; post-commit the same paths
/// with `exists == true` are the actual files Ready shows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct KeyDefaults {
    pub identity_file: String,
    pub passphrase_file: String,
    pub identity_exists: bool,
    pub passphrase_exists: bool,
}

/// Platform default key paths: macOS Application Support, Linux
/// XDG state home (fallback `~/.local/state`). No secret reads —
/// existence only.
pub fn default_key_paths() -> KeyDefaults {
    let (identity_file, passphrase_file) = default_paths();
    KeyDefaults {
        identity_exists: Path::new(&identity_file).is_file(),
        passphrase_exists: Path::new(&passphrase_file).is_file(),
        identity_file,
        passphrase_file,
    }
}

fn default_paths() -> (String, String) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                let base = format!("{home}/Library/Application Support/Wyrd/keys");
                return (format!("{base}/identity"), format!("{base}/passphrase"));
            }
        }
        (
            "~/Library/Application Support/Wyrd/keys/identity".to_string(),
            "~/Library/Application Support/Wyrd/keys/passphrase".to_string(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let base = std::env::var("XDG_STATE_HOME")
            .ok()
            .filter(|v| !v.is_empty() && v.starts_with('/'))
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .filter(|h| !h.is_empty())
                    .map(|h| format!("{h}/.local/state"))
            })
            .map(|b| format!("{b}/wyrd/keys"))
            .unwrap_or_else(|| "~/.local/state/wyrd/keys".to_string());
        (format!("{base}/identity"), format!("{base}/passphrase"))
    }
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
/// 4096 bytes maximum. The backend pre-refuses before spawning; the
/// CLI re-validates before writing.
pub fn custom_passphrase_valid(text: &str) -> bool {
    let chars = text.chars().count();
    chars >= 12 && text.len() <= 4096 && !text.contains('\n') && !text.contains('\r')
}

/// One onboarding operation outcome. Infallible: every path —
/// including timeouts and spawn failures — returns a variant the
/// caller pairs with existence re-reads; no path throws except a dead
/// IPC worker (which the command layer turns into `Unreadable`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum OnboardingResult {
    /// The CLI reported success. `passphrase` carries the generated
    /// text only for `new-passphrase`; every other success carries
    /// `None` (the identity secret never leaves the CLI, and a
    /// committed passphrase is read back from its file, never
    /// echoed).
    Completed {
        detail: String,
        passphrase: Option<String>,
    },
    /// Exit 2 or an adapter-side refusal: usage, expected failure, or
    /// a validation refusal that never spawned a process. Opaque
    /// detail; existence re-reads say what actually exists.
    ExpectedFailure { detail: String },
    /// CLI binary missing or version-mismatched: hard failure, never
    /// silent fallback.
    VersionMismatch { reason: String },
    /// Timeout, spawn failure, or signal death: the outcome is
    /// unknown. Triggers re-observation, never re-execution.
    Unknown { detail: String },
}

/// One completed onboarding call: the outcome plus existence after
/// the run (the commit boundary's read-back lives here, not in the
/// CLI text).
#[derive(Debug, Clone, serde::Serialize)]
pub struct OnboardingReport {
    pub result: OnboardingResult,
    pub defaults: KeyDefaults,
}

/// Pure argv builders: one structured element per argument, never
/// shell-constructed. Secrets never appear — `commit` carries the
/// passphrase on the stdin pipe, not in argv.
pub fn new_passphrase_argv() -> Vec<std::ffi::OsString> {
    vec!["identity".into(), "new-passphrase".into()]
}

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
        return report(mismatch_into(mismatch), cli_bin);
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
    report(result, cli_bin)
}

/// Run `<cli> identity commit`: the passphrase travels on the stdin
/// pipe (never argv). Version check and pre-validation first; a
/// custom passphrase that fails the floor refuses before any process
/// spawns. Post-run the report carries fresh existence (the
/// commit boundary's read-back signal).
pub fn run_commit(
    cli_bin: &Path,
    identity_file: &Path,
    passphrase_file: &Path,
    passphrase: &str,
    timeout: Duration,
) -> OnboardingReport {
    if !custom_passphrase_valid(passphrase) {
        return report(
            OnboardingResult::ExpectedFailure {
                detail: "passphrase must be at least 12 characters (without line breaks) and at most 4096 bytes".to_string(),
            },
            cli_bin,
        );
    }
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return report(mismatch_into(mismatch), cli_bin);
    }
    let argv = commit_argv(identity_file, passphrase_file);
    let output = run_cli(cli_bin, &argv, Some(passphrase.as_bytes()), timeout);
    let result = map_output(output);
    report(result, cli_bin)
}

/// Run `<cli> identity verify`: exit codes only, stderr opaque.
pub fn run_verify(
    cli_bin: &Path,
    identity_file: &Path,
    passphrase_file: &Path,
    timeout: Duration,
) -> OnboardingReport {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return report(mismatch_into(mismatch), cli_bin);
    }
    let output = run_cli(
        cli_bin,
        &verify_argv(identity_file, passphrase_file),
        None,
        timeout,
    );
    report(map_output(output), cli_bin)
}

fn report(result: OnboardingResult, _cli_bin: &Path) -> OnboardingReport {
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
    let Some(code) = output.code else {
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
    let _ = code;
    OnboardingResult::ExpectedFailure {
        detail: truncate(&format!("{}\n{}", output.stdout, output.stderr), 500),
    }
}

/// Raw completed-process capture. `stdin_bytes`, when present, is
/// piped to the child's stdin (the commit passphrase — the only
/// secret that ever crosses this boundary, and never via argv).
struct CliOutput {
    code: Option<i32>,
    status_success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

fn run_cli(
    cli_bin: &Path,
    argv: &[std::ffi::OsString],
    stdin_bytes: Option<&[u8]>,
    timeout: Duration,
) -> CliOutput {
    use std::io::Write;
    let mut command = std::process::Command::new(cli_bin);
    command.args(argv);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    if stdin_bytes.is_some() {
        command.stdin(std::process::Stdio::piped());
    } else {
        command.stdin(std::process::Stdio::null());
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CliOutput {
                code: None,
                status_success: false,
                stdout: String::new(),
                stderr: format!("cannot execute {}: {error}", cli_bin.display()),
                timed_out: false,
            };
        }
    };
    if let Some(bytes) = stdin_bytes {
        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort write then close: a failed write fails the
            // run, never hangs it (the wait below stays bounded and
            // kills past the timeout).
            let _ = stdin.write_all(bytes);
        }
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output();
                return match output {
                    Ok(output) => CliOutput {
                        code: output.status.code().or(status.code()),
                        status_success: output.status.success(),
                        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                        timed_out: false,
                    },
                    Err(error) => CliOutput {
                        code: status.code(),
                        status_success: status.success(),
                        stdout: String::new(),
                        stderr: format!("cannot collect CLI output: {error}"),
                        timed_out: false,
                    },
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return CliOutput {
                        code: None,
                        status_success: false,
                        stdout: String::new(),
                        stderr: format!(
                            "CLI did not complete within {}s and was stopped; outcome unknown",
                            timeout.as_secs()
                        ),
                        timed_out: true,
                    };
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return CliOutput {
                    code: None,
                    status_success: false,
                    stdout: String::new(),
                    stderr: format!("cannot wait on CLI process: {error}"),
                    timed_out: false,
                };
            }
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut end = max;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", trimmed[..end].trim_end())
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
            },
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({
                "result": {"result": "completed", "detail": "ok", "passphrase": "abcd6e-789abc-d6e789-abcd6e"},
                "defaults": {"identity_file": "/k/identity", "passphrase_file": "/k/passphrase", "identity_exists": true, "passphrase_exists": true},
            })
        );
    }
}
