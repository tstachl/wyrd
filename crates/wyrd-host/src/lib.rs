//! The desktop host: the process that owns a drive.
//!
//! One owner process holds the store's exclusive lock for its lifetime,
//! runs the live loop and the serving surface under the daemon's
//! supervision, and quits in the mount's teardown order
//! (`docs/design/cli.md:48-56`), folding every outcome into the exit status.
//! This crate is the owner process, not the management UI and not a
//! platform adapter: the window attaches later as a non-owner consumer
//! of the drive-owner interface (issue 16), and the File Provider
//! adapter is a separate adapter outside Cargo (issue 17; layout
//! proposed in `packaging/macos/README.md`, pending decision).
//!
//! Dependency rule, enforced by
//! `wyrd-contracts`' host composition check: this crate's production
//! code names no `wyrd_sync` transport machinery (`transport`, `bulk`,
//! `serving`), no `iroh`, and no relay types. It opens the store
//! through `wyrd_sync`'s durable side (`durable`, `runtime::Engine`,
//! `keys`), composes the node through `wyrd-core`, and supervises and
//! tears down through `wyrd-daemon`. Transport construction for the
//! developer-run binary goes through the daemon's shared plumbing, so
//! the rule holds for every target in this crate. Test fakes live in
//! `tests/`, never here.

pub mod host;
pub mod run;

pub use host::{open_host, HostError, HostView, OpenHost};
pub use run::{compose, exit_code, loop_result, telemetry_emit};
pub use wyrd_daemon::ComposedHeadless;
