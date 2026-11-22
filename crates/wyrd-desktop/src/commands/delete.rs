//! Remove a drive's local data through the installed CLI, then
//! re-observe. Exit 0 alone never proves the directory is gone: the
//! report pairs the outcome with a registry re-read and socket probes.

use tauri::{AppHandle, Runtime};

use super::provision::{credential_source, run_provision};
use super::ProvisionReport;

/// Remove one drive's local data. `acknowledged` carries the terminal
/// confirmation to the CLI's `--yes` boundary: a frontend omitting the
/// dialog cannot bypass it.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionDeleteArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Credential path, never contents: required with the file
    /// backend, refused beside a store reference.
    pub identity_file: Option<String>,
    /// Credential path, never contents: required with the file
    /// backend, refused beside a store reference.
    pub passphrase_file: Option<String>,
    /// Credential backend selector: absent or `"file"` reads the
    /// hardened credential files; `"os-store"` resolves the drive
    /// through the OS credential store.
    pub credential_backend: Option<String>,
    /// OS credential-store account naming the drive's item (the
    /// reference, never a secret). Required exactly where the
    /// backend is `"os-store"`, refused everywhere else.
    pub credential_account: Option<String>,
    /// Terminal confirmation carried to the `--yes` boundary.
    pub acknowledged: bool,
}

/// Delete one drive through the installed CLI (`drive delete`), then
/// re-read and re-observe. The CLI authors `Remove(self)` before it
/// removes the directory, refuses a non-owner or a frozen drive, and
/// requires the acknowledgement. Credentials travel as file paths or
/// one store account — never as contents.
#[tauri::command]
pub async fn provision_delete<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionDeleteArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_delete",
        args.drive_dir.clone(),
        // The directory is removed: drop any cached owner connection so
        // the next operation mints a fresh one instead of using a socket
        // that no longer exists.
        true,
        move |state, dir| {
            let credentials = match credential_source(
                args.credential_backend,
                args.credential_account,
                args.identity_file,
                args.passphrase_file,
                false,
            ) {
                Ok(source) => source,
                Err(reason) => {
                    return crate::provision::ProvisionOutcome {
                        result: crate::provision::ProvisionResult::ExpectedFailure {
                            detail: reason,
                        },
                        stderr: String::new(),
                        code: None,
                    };
                }
            };
            let request = crate::provision::DeleteRequest {
                drive_dir: dir,
                credentials,
                acknowledged: args.acknowledged,
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => crate::provision::run_delete(
                    &bin,
                    &request,
                    crate::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => crate::provision::ProvisionOutcome {
                    result: crate::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}
