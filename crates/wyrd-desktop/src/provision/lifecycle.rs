//! Version-first lifecycle runners. Every path returns an outcome
//! the caller pairs with observation; none reports lifecycle state
//! or retries a process. The parent module owns the authority
//! contract.

use std::path::Path;
use std::time::Duration;

use super::argv::{
    bootstrap_argv, delete_argv, leave_argv, reconfigure_argv, service_disable_argv,
    service_enable_argv, service_start_argv,
};
use super::process::{check_cli_version, map_exit_output, run_cli};
use super::{
    DeleteRequest, LeaveRequest, ProvisionOutcome, ProvisionRequest, ProvisionResult,
    ReconfigureRequest, ServiceDisableRequest, ServiceEnableRequest, ServiceStartRequest,
};

/// Run one `bootstrap` provisioning through the version-matched CLI:
/// version check first (hard failure on mismatch), then the structured
/// argv run, then the exit-code map. Every path — including timeouts
/// (killed and reaped) and spawn failures — returns an outcome the
/// caller pairs with a registry re-read, socket probes, and a
/// membership re-observation; no path reports lifecycle state.
pub fn run_bootstrap(
    cli_bin: &Path,
    request: &ProvisionRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match bootstrap_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `member leave` through the version-matched CLI, with the
/// same version-first, observation-after contract as
/// [`run_bootstrap`].
pub fn run_leave(cli_bin: &Path, request: &LeaveRequest, timeout: Duration) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match leave_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive delete` through the version-matched CLI, with the
/// same version-first, observation-after contract as
/// [`run_bootstrap`]. No implicit retry: the CLI may have committed
/// before a timeout, so only the paired observation decides the
/// display.
pub fn run_delete(cli_bin: &Path, request: &DeleteRequest, timeout: Duration) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match delete_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive reconfigure` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read, socket probes, and a membership re-observation; no path
/// reports lifecycle state.
pub fn run_reconfigure(
    cli_bin: &Path,
    request: &ReconfigureRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match reconfigure_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive service-start` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read, socket probes, and a membership re-observation; no path
/// reports lifecycle state. No credentials travel: the CLI opens no
/// keystore for a start.
pub fn run_service_start(
    cli_bin: &Path,
    request: &ServiceStartRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = service_start_argv(request);
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive service-enable` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read and a fresh enablement probe; no path reports
/// enablement state. No credentials travel: the CLI opens no
/// keystore for an enable.
pub fn run_service_enable(
    cli_bin: &Path,
    request: &ServiceEnableRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = service_enable_argv(request);
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive service-disable` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read and a fresh enablement probe; no path reports
/// enablement state. No credentials travel: the CLI opens no
/// keystore for a disable.
pub fn run_service_disable(
    cli_bin: &Path,
    request: &ServiceDisableRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = service_disable_argv(request);
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}
