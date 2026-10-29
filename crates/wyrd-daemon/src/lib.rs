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

pub mod core;
#[cfg(feature = "mount")]
pub mod fuse;
pub mod lifecycle;
pub mod posture;
pub mod process;
pub mod vault;

pub use core::{
    FailureClass, LiveConfig, LiveError, LiveNode, LiveParts, LiveSummary, NodeError,
    PendingObligations, ResourceBudgets, RuntimeMaterialization, SyncReport, WyrdNode,
};
// The composed view type, re-exported at the crate root so hosts build
// their node over the same view the backend serves without depending
// on the view crate directly — and without needing the `mount`
// feature: the view exists whether or not any backend is compiled in.
pub use lifecycle::{LoopError, LoopReturn, Supervisor, Wake, WakeSignal};
pub use vault::{
    install_role_dir, install_serving, run_vault, shutdown_transport, ServeSurface,
    TransportDeadlines, TransportShutdown, VaultEvent, VaultLoopEnd, VaultObserver, VaultOutcome,
    VaultRun,
};
// Transport vocabulary for composers: the host (`wyrd-app`) bounds its
// run over these without spelling `wyrd_sync` transport paths, so the
// host composition check sees only the composer surface. The daemon is
// already transport-fluent (lifecycle, vault); this re-export moves no
// machinery, it only names the boundary the host may touch.
pub use wyrd_fuse::DriveView;
pub use wyrd_sync::serving::ServingEndpoint;
pub use wyrd_sync::transport::mailbox::Mailbox;
