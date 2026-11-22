//! Membership commands and the terminal membership re-observation.
//! Exit 0 alone never proves a leave: `provision_leave` pairs its
//! outcome with a fresh owner `observe`, like every lifecycle command.

use tauri::{AppHandle, Runtime};
use wyrd_owner_client::OwnerClient;

use super::provision::{credential_source, run_provision};
use super::ProvisionReport;

/// The operated drive's membership tip as re-observed after the
/// operation: terminal owner answer only, never guessed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MembershipObservation {
    /// Membership tip epoch at re-observation, absent where the owner did not answer.
    pub epoch: Option<u64>,
    /// Member count in the terminal `observe` answer.
    pub known_members: usize,
}

/// Leave one drive. `acknowledged` carries the sole-owner terminal
/// confirmation to the `--yes` boundary.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionLeaveArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Credential path, never contents: required with the file
    /// backend, refused beside a store reference (leave reads the
    /// provisioned credential, never seed inputs).
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
    /// Sole-owner terminal confirmation carried to the `--yes` boundary.
    pub acknowledged: bool,
}

/// Leave the drive through the installed CLI (`member leave`), then
/// re-observe membership and lifecycle. `acknowledged` carries the
/// sole-owner terminal confirmation to the `--yes` boundary — a
/// frontend omitting the dialog cannot bypass it. A non-owner leave
/// authors nothing (exit 0 refusal); the re-observation proves the
/// membership is unchanged.
#[tauri::command]
pub async fn provision_leave<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionLeaveArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_leave",
        args.drive_dir.clone(),
        false,
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
            let request = crate::provision::LeaveRequest {
                drive_dir: dir,
                credentials,
                acknowledged: args.acknowledged,
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => {
                    crate::provision::run_leave(&bin, &request, crate::provision::PROVISION_TIMEOUT)
                }
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

/// Independently re-observe the operated drive's membership: a
/// terminal `observe` answer carries the tip epoch and member count;
/// no transport, no terminal answer, or any refusal yields `None` —
/// membership unknown, never guessed from CLI text.
///
/// Bounded like the registry probe (5 s): a wedged owner must cost
/// seconds on a finished operation, never the client's default
/// response wait. Connects directly with the probe timeout instead
/// of going through the connection map — the same shape as the
/// registry's per-drive probe.
pub(crate) fn observe_membership(drive_dir: &std::path::Path) -> Option<MembershipObservation> {
    const MEMBERSHIP_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let client = OwnerClient::connect_drive(drive_dir).ok()?;
    let mut client = client.with_timeout(MEMBERSHIP_PROBE_TIMEOUT).ok()?;
    let status = client.observe().ok()?;
    Some(MembershipObservation {
        epoch: status.tip.as_ref().map(|tip| tip.epoch),
        known_members: status.known_members,
    })
}
