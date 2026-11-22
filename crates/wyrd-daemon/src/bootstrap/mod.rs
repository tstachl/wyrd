//! First-drive provisioning: the per-user drive registry, the
//! OS service handoff, and the fixed initialize-then-register order.
//!
//! The contract lives in `docs/design/desktop-shell.md` (bootstrap
//! section): a registry file the installer creates and two writers
//! share — the registration flow and the desktop shell. Never a
//! manager daemon, never a discovery protocol. This module is the
//! registry as code plus the provisioning orchestration the `wyrd
//! bootstrap` command drives. The desktop backend writes the same
//! file through its own backend (which cannot link this crate); both
//! writers serialize on the registry lockfile, so concurrent rewrites
//! converge instead of losing entries.
//!
//! Three rules hold throughout:
//!
//! - The registry holds drive facts, never secrets. Entries name a
//!   directory, a relay set, and a service; key material stays in
//!   the OS keychain and credential files (settings contract).
//! - Registration and service enablement happen only after a
//!   successful initialization. A drive is reported ready only when
//!   its socket verifies this session — never because its registry
//!   entry exists.
//! - Entries are untrusted configuration. Every read validates and
//!   skips what does not parse; every connection still
//!   authenticates through the owner interface's own checks.

mod provision;
mod registry;
mod service;

pub use provision::{
    provision, reconfigure, start_managed_service, Init, ProvisionConfig, ProvisionFailureKind,
    ProvisionState, ReconfigureConfig, StartServiceConfig,
};
pub use registry::{
    default_registry_path, read_registry, registry_lock_path, write_registry, BootstrapError,
    Registry, RegistryEntry, ServiceMode, REGISTRY_FILENAME, REGISTRY_VERSION,
};
pub use service::{
    disable_service, enable_service, install_service, render_launchd_plist, render_systemd_unit,
    service_definition_path_for_service, service_enabled, set_service_enabled, start_service,
    start_service_only, ServiceCredential, ServiceEnabled, SupervisedOwner,
};

// The registry's crate-visible updater is exercised by the tests but
// has no production caller outside its own submodule.
#[cfg(test)]
pub(crate) use registry::modify_registry;

// The original module's imports, now needed by the extracted test
// modules through their `use super::*`; production code imports
// each dependency inside its own submodule.
#[cfg(test)]
use serde::Serialize;
#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use wyrd_sync::keys::DeviceIdentitySecret;
#[cfg(test)]
use wyrd_sync::runtime::{Engine, JoinExpectation};

// Private helpers the extracted test modules reach through `super::*`.
#[cfg(test)]
use provision::{
    classify_post_stop_failure, definition_needs_reinstall, install_service_definition,
    managed_credential, updated_entry,
};
#[cfg(test)]
use registry::validate_entry;
#[cfg(test)]
use service::{
    parse_is_enabled, parse_linger_state, parse_print_disabled, probe_output, run,
    write_service_file,
};

#[cfg(test)]
mod hardening_tests;
#[cfg(test)]
mod tests;
