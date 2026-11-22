//! First-run onboarding commands: defaults, passphrase generation,
//! commit, verify, adopt, and the file-manager reveal used by the
//! key write-down view. Every CLI exchange runs off-runtime and
//! resolves `Unknown` inside the report, never a throw.

use std::path::Path;
use tauri::{AppHandle, Manager, Runtime};

/// Platform key defaults plus existence (no secret reads). A local
/// query: infallible, counted in flight. Pre-commit the UI treats the
/// paths as intended locations; post-commit the same paths with
/// existence set are the actual files. Trivial cost (two metadata
/// probes), so no blocking pool — the flight count still covers the
/// dispatch for the quit drain.
#[tauri::command]
pub async fn onboarding_defaults<R: Runtime>(app: AppHandle<R>) -> crate::onboarding::KeyDefaults {
    let _flight = app.state::<crate::FlightTracker>().enter();
    crate::onboarding::default_key_paths()
}

/// One generated passphrase for write-down: writes nothing. Infallible
/// like the other local queries — the report carries the outcome plus
/// fresh existence, and a dead worker resolves `Unknown` inside the
/// payload, never a throw.
#[tauri::command]
pub async fn onboarding_new_passphrase<R: Runtime>(
    app: AppHandle<R>,
) -> crate::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_new_passphrase", |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => {
                crate::onboarding::run_new_passphrase(&bin, crate::onboarding::onboarding_timeout())
            }
            Err(reason) => crate::onboarding::OnboardingReport {
                result: crate::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: crate::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Commit arguments: credential paths plus the passphrase text. The
/// text travels backend -> CLI on the stdin pipe (never argv); the
/// frontend holds it only from write-down to Finish. `Debug` is
/// redacted so no log line carries the secret.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingCommitArgs {
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
    /// Passphrase text piped to the CLI on stdin, never argv.
    pub passphrase: String,
}

impl std::fmt::Debug for OnboardingCommitArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingCommitArgs")
            .field("identity_file", &self.identity_file)
            .field("passphrase_file", &self.passphrase_file)
            .field("passphrase", &"REDACTED")
            .finish()
    }
}

/// Commit both credential files (generate + atomic write + read-back
/// signal via fresh existence). Infallible; dead workers resolve
/// `Unknown`.
#[tauri::command]
pub async fn onboarding_commit<R: Runtime>(
    app: AppHandle<R>,
    args: OnboardingCommitArgs,
) -> crate::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_commit", move |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => crate::onboarding::run_commit(
                &bin,
                Path::new(&args.identity_file),
                Path::new(&args.passphrase_file),
                &args.passphrase,
                crate::onboarding::onboarding_timeout(),
            ),
            Err(reason) => crate::onboarding::OnboardingReport {
                result: crate::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: crate::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Verify arguments: credential paths only, never contents.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingVerifyArgs {
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
}

/// Verify both files through the hardened reader (exit codes only).
/// Infallible; the report's fresh existence is the read-back signal.
#[tauri::command]
pub async fn onboarding_verify<R: Runtime>(
    app: AppHandle<R>,
    args: OnboardingVerifyArgs,
) -> crate::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_verify", move |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => crate::onboarding::run_verify(
                &bin,
                Path::new(&args.identity_file),
                Path::new(&args.passphrase_file),
                crate::onboarding::onboarding_timeout(),
            ),
            Err(reason) => crate::onboarding::OnboardingReport {
                result: crate::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: crate::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Adopt arguments: credential paths only, never contents. The
/// backend verifies the pair first, then records it as the device's
/// credential pair for this and later launches.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingAdoptArgs {
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
}

/// Verify the pair, then record it as adopted. Infallible; without
/// the record a restore or repair choice would be orphaned after the
/// session — the gate resolves the adopted pair, not just the
/// platform defaults.
#[tauri::command]
pub async fn onboarding_adopt<R: Runtime>(
    app: AppHandle<R>,
    args: OnboardingAdoptArgs,
) -> crate::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_adopt", move |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => crate::onboarding::run_adopt(
                &bin,
                Path::new(&args.identity_file),
                Path::new(&args.passphrase_file),
                crate::onboarding::onboarding_timeout(),
            ),
            Err(reason) => crate::onboarding::OnboardingReport {
                result: crate::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: crate::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Reveal arguments: one path to show in the platform file manager.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealArgs {
    /// One path to show in the platform file manager.
    pub path: String,
}

/// Best-effort reveal of one path in the platform file manager
/// (`open -R` on macOS, `xdg-open` on the directory elsewhere).
/// Returns `true` where the opener spawned; never throws — a missing
/// opener is not an onboarding failure.
#[tauri::command]
pub async fn reveal_in_folder<R: Runtime>(app: AppHandle<R>, args: RevealArgs) -> bool {
    let _flight = app.state::<crate::FlightTracker>().enter();
    let path = args.path;
    tauri::async_runtime::spawn_blocking(move || reveal_path(&path))
        .await
        .unwrap_or(false)
}

fn reveal_path(path: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.as_os_str().to_owned())
            .unwrap_or_else(|| std::ffi::OsString::from(path));
        std::process::Command::new("xdg-open")
            .arg(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }
}

/// Run one onboarding CLI exchange on the blocking pool and count it
/// in flight. Infallible: a dead worker resolves `Unknown` with fresh
/// existence, never success and never a throw.
async fn run_onboarding<R, F>(
    app: AppHandle<R>,
    op: &'static str,
    call: F,
) -> crate::onboarding::OnboardingReport
where
    F: FnOnce(&crate::AppState) -> crate::onboarding::OnboardingReport + Send + 'static,
    R: Runtime,
{
    let _flight = app.state::<crate::FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<crate::AppState>();
        call(&state)
    })
    .await
    .unwrap_or_else(|join| crate::onboarding::OnboardingReport {
        result: crate::onboarding::OnboardingResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        defaults: crate::onboarding::default_key_paths(),
    })
}
