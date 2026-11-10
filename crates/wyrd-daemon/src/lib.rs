//! Wyrd's daemon: the composition layer the architecture reserves for
//! exactly this role (`docs/architecture.md`): it wires the `wyrd-core`
//! node to a presentation backend and owns the process around it
//! (supervision, signals, mount diagnostics).
//!
//! Two rules shape the layout. The node ([`wyrd_core::node::WyrdNode`])
//! is presentation-agnostic: mobile platforms (Android
//! SAF/DocumentsProvider, iOS file provider) cannot use FUSE, so the
//! platform surface is a pluggable backend over the same view — the
//! same five operations, platform errors instead of POSIX ones. The
//! FUSE adapter (the `fuse` module, behind the default-on `mount`
//! feature) is the first such backend, not a property
//! of the node. This crate's [`core`] module holds the host-side
//! composition tests; [`lifecycle`] owns supervision.

pub mod bootstrap;
pub mod core;
#[cfg(feature = "mount")]
pub mod fuse;
pub mod lifecycle;
pub mod owner;
pub mod posture;
pub mod process;
pub mod settings;
pub mod vault;

pub use core::{
    apply_budget_settings, render_settings_surface, AppliedSettings, BudgetOverrides, FailureClass,
    LiveConfig, LiveError, LiveNode, LiveParts, LiveSummary, NodeError, PendingObligations,
    ResourceBudgets, RunningSettings, RuntimeMaterialization, SyncReport, WyrdNode,
};
pub use settings::{
    default_settings_path, overrides_for, overrides_for_path, read_settings, settings_key_for,
    split_file_overrides, write_settings, AppSettingsFile, DriveSettingsSection,
    APP_SETTINGS_VERSION, DEFAULT_OPEN_TIMEOUT, SETTINGS_FILENAME,
};
// The composed view type, re-exported at the crate root so hosts build
// their node over the same view the backend serves without depending
// on the view crate directly — and without needing the `mount`
// feature: the view exists whether or not any backend is compiled in.
pub use lifecycle::{LoopError, LoopReturn, Supervisor, Wake, WakeSignal};
pub use vault::{
    compose_headless_owner, compose_headless_owner_with_quota, install_role_dir, install_serving,
    run_composed_owner, run_vault, shutdown_transport, ComposedHeadless, OwnerError, OwnerView,
    ServeSurface, TransportDeadlines, TransportShutdown, VaultEvent, VaultLoopEnd, VaultObserver,
    VaultOutcome, VaultRun,
};
// Transport vocabulary for composers: the host (`wyrd-host`) bounds its
// run over these without spelling `wyrd_sync` transport paths, so the
// host composition check sees only the composer surface. The daemon is
// already transport-fluent (lifecycle, vault); this re-export moves no
// machinery, it only names the boundary the host may touch.
pub use wyrd_sync::serving::ServingEndpoint;
pub use wyrd_sync::transport::mailbox::Mailbox;
pub use wyrd_view::DriveView;
