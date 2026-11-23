use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::{CliError, SHUTDOWN};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::reload;
use tracing_subscriber::EnvFilter;
use wyrd_daemon::DiagnosticLevel;

/// One owner-log file at 5 MiB, three files retained. These are
/// local operator defaults, not protocol constants (T20 retention
/// table in `docs/contracts/trust.md`).
pub(crate) const OWNER_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub(crate) const OWNER_LOG_RETAINED: usize = 3;

/// Cloneable file sink for the mount-log layer: the `fmt` layer needs
/// a `MakeWriter`, and a shared `Arc<Mutex<File>>` is the simplest one
/// that survives into the spawned session thread. A poisoned lock maps
/// to an I/O error rather than panicking the mount.
#[derive(Clone, Debug)]
struct SharedWriter {
    file: Arc<Mutex<fs::File>>,
}

impl SharedWriter {
    fn new(file: fs::File) -> Self {
        SharedWriter {
            file: Arc::new(Mutex::new(file)),
        }
    }
}

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file
            .lock()
            .map_err(|_| std::io::Error::other("mount log lock poisoned"))?
            .write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .lock()
            .map_err(|_| std::io::Error::other("mount log lock poisoned"))?
            .flush()
    }
}

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Build the mount subscriber over an open log file: stderr plus
/// file layers under one filter. Pure construction with no global
/// state, so tests install it thread-scoped via
/// [`tracing::subscriber::with_default`] and assert what lands in the
/// file — including the `--verbose` debug gate — without disturbing
/// the process-global subscriber other tests may have installed.
///
/// Both output layers sit behind a [`ShutdownNoiseGate`]: iroh's relay
/// transport logs `error!` when its receive channel closes, including
/// on a graceful `Endpoint::close` — which is exactly what the
/// post-loop shutdown sequence does for both endpoints. The gate
/// swallows only that one event, and only once [`SHUTDOWN`] is
/// tripped, so a mid-operation relay death still fails visibly.
fn mount_filter(verbose: bool) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Both crates: the binary (`wyrd`, this file) and the library
        // (`wyrd_daemon`, the FUSE backend) emit request and stage
        // events under their own target roots.
        EnvFilter::new(if verbose {
            "info,wyrd=debug,wyrd_daemon=debug"
        } else {
            "info"
        })
    })
}

pub(crate) fn build_mount_subscriber(
    file: Option<fs::File>,
    verbose: bool,
) -> impl tracing::Subscriber {
    let filter = mount_filter(verbose);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false);
    // No file layer without a file: the vault's supervisor captures
    // stderr, and only `--log-file` adds the second stream. `None`
    // layers are no-ops, so the registry shape stays one copy.
    let file_layer = file.map(|file| {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(SharedWriter::new(file))
            .with_ansi(false);
        ShutdownNoiseGate { inner: layer }
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(ShutdownNoiseGate {
            inner: stderr_layer,
        })
        .with(file_layer)
}

/// The mount's reloadable twin of [`build_mount_subscriber`]. The
/// mount exposes the owner interface too, so `diagnostics-set` must
/// be able to widen its filter live. The initial filter still honours
/// `--verbose`; the installed sink replaces it with the app-wide
/// level on the first live set.
#[cfg(any(test, feature = "mount"))]
pub(crate) fn build_reloadable_mount_subscriber(
    file: Option<fs::File>,
    verbose: bool,
) -> (
    impl tracing::Subscriber,
    reload::Handle<EnvFilter, tracing_subscriber::registry::Registry>,
) {
    let (filter, handle) = reload::Layer::new(mount_filter(verbose));
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_ansi(false);
    let file_layer = file.map(|file| {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(SharedWriter::new(file))
            .with_ansi(false);
        ShutdownNoiseGate { inner: layer }
    });
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(ShutdownNoiseGate {
            inner: stderr_layer,
        })
        .with(file_layer);
    (subscriber, handle)
}

/// Suppression wrapper for one output layer: forwards every span and
/// event callback to the inner layer except iroh's relay-transport
/// teardown noise once shutdown is underway (see
/// [`build_mount_subscriber`]). The delegation below is mechanical —
/// every callback except `on_event` passes straight through.
struct ShutdownNoiseGate<L> {
    inner: L,
}

impl<S, L> tracing_subscriber::Layer<S> for ShutdownNoiseGate<L>
where
    S: tracing::Subscriber,
    L: tracing_subscriber::Layer<S>,
{
    fn on_register_dispatch(&self, subscriber: &tracing::Dispatch) {
        self.inner.on_register_dispatch(subscriber);
    }

    fn register_callsite(
        &self,
        callsite: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        self.inner.register_callsite(callsite)
    }

    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        self.inner.enabled(metadata, ctx)
    }

    fn on_layer(&mut self, subscriber: &mut S) {
        self.inner.on_layer(subscriber);
    }

    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.inner.on_new_span(attrs, id, ctx);
    }

    fn on_record(
        &self,
        span: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.inner.on_record(span, values, ctx);
    }

    fn on_follows_from(
        &self,
        span: &tracing::span::Id,
        follows: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.inner.on_follows_from(span, follows, ctx);
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        if SHUTDOWN.load(Ordering::Relaxed) && is_relay_teardown_noise(event) {
            return;
        }
        self.inner.on_event(event, ctx);
    }

    fn on_enter(&self, id: &tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.inner.on_enter(id, ctx);
    }

    fn on_exit(&self, id: &tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.inner.on_exit(id, ctx);
    }

    fn on_close(&self, id: tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.inner.on_close(id, ctx);
    }

    fn on_id_change(
        &self,
        old: &tracing::span::Id,
        new: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.inner.on_id_change(old, new, ctx);
    }
}

/// Whether an event is iroh `=1.1.0`'s relay-transport teardown log:
/// `error!` on target `iroh::socket::transports::relay` with the
/// `poll_recv_queue` channel-closed message. Matching is fail-open —
/// if upstream ever rewords the message, the noise returns instead
/// of anything going silent.
fn is_relay_teardown_noise(event: &tracing::Event<'_>) -> bool {
    if event.metadata().target() != "iroh::socket::transports::relay" {
        return false;
    }
    if *event.metadata().level() != tracing::Level::ERROR {
        return false;
    }
    let mut probe = MessageProbe { message: None };
    event.record(&mut probe);
    probe.message.as_deref() == Some("relay_recv_channel closed")
}

/// Collects an event's `message` field: `tracing` records formatted
/// messages through `record_debug`, so both arms are needed.
struct MessageProbe {
    message: Option<String>,
}

impl tracing::field::Visit for MessageProbe {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }
}

/// Mount diagnostics: structured events to stderr plus a per-mount
/// file, and a bridge so fuser's `log` records land in the same
/// stream.
///
/// The file (`drive_dir/mount.log`, truncated per mount) is the record
/// a dead mount leaves behind — terminal scrollback is gone when the
/// terminal is. Truncation keeps it bounded with zero rotation code:
/// one mount, one log. Logs never contain secret bytes (same rule as
/// CLI errors): stages, ids, errnos, latencies. They do record the
/// drive and mountpoint paths as diagnostic metadata.
///
/// Call first in [`crate::commands::mount::mount`], before preflight or the session thread
/// exists. The daemon binary serves exactly one mount per process, so
/// the first install wins by design. A second init in the same process
/// (tests sharing one test binary) reuses the installed subscriber and
/// says so on the existing stream instead of claiming a fresh install;
/// it still truncates and returns the caller's own log path.
/// Mount-scoped with the diagnostics tests that pin it: without the
/// mount command no non-test caller remains, but the tests still run.
#[cfg(any(test, feature = "mount"))]
pub(crate) fn init_mount_diagnostics(drive_dir: &Path, verbose: bool) -> Result<PathBuf, CliError> {
    let log_path = drive_dir.join("mount.log");
    let file = fs::File::create(&log_path).map_err(|source| CliError::Io {
        path: log_path.clone(),
        source,
    })?;
    // fuser's handshake errors and iroh internals log via the `log`
    // crate; without this bridge those records vanish because no
    // logger is ever initialized.
    let _ = tracing_log::LogTracer::init();
    let (subscriber, handle) = build_reloadable_mount_subscriber(Some(file), verbose);
    if tracing_subscriber::util::SubscriberInitExt::try_init(subscriber).is_err() {
        tracing::warn!(
            stage = "start",
            log = %log_path.display(),
            "diagnostics already initialized; reusing the installed subscriber",
        );
    }
    install_filter_sink(handle);
    Ok(log_path)
}

/// The filter directives for one diagnostics level. Default keeps
/// the long-standing verbosity unchanged; detailed adds debug for
/// this binary and the daemon, where local decision records are
/// emitted. `RUST_LOG` still wins when an operator set it, exactly
/// as it does for mount and sync diagnostics.
pub(crate) fn log_filter(level: DiagnosticLevel) -> EnvFilter {
    let base = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match level {
        DiagnosticLevel::Default => base,
        // Detailed adds the local decision records on top of any
        // operator-set RUST_LOG instead of replacing it. The two
        // directives are constants, so parsing them cannot fail.
        DiagnosticLevel::Detailed => base
            .add_directive("wyrd=debug".parse().expect("constant directive"))
            .add_directive("wyrd_daemon=debug".parse().expect("constant directive")),
    }
}

/// Install the process-wide sink that swaps a subscriber's filter on
/// a live level change. Both owner presentations use it: the mount's
/// `--verbose` filter is the starting point, and the app-wide level
/// replaces it after the first `diagnostics-set`. A reload failure is
/// returned to the controller so the caller never sees "applied"
/// when verbosity did not change.
fn install_filter_sink(handle: reload::Handle<EnvFilter, tracing_subscriber::registry::Registry>) {
    let sink = Arc::new(move |level: DiagnosticLevel| {
        handle
            .modify(|current| *current = log_filter(level))
            .map_err(|error| format!("cannot reload log filter: {error}"))
    });
    if wyrd_daemon::install_diagnostics_sink(sink).is_err() {
        tracing::warn!(
            stage = "start",
            "diagnostics sink already installed; live level changes will not change this filter",
        );
    }
}

fn rotated_log_path(path: &Path, generation: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

/// Size-bounded owner log. `fmt` asks for a writer per event; the
/// rotation check lives inside the shared writer so every event sees
/// the same bound. One event larger than the bound is never split or
/// dropped — the next event rotates, so the file may exceed the
/// bound by at most one record.
#[derive(Clone, Debug)]
struct OwnerLogWriter {
    inner: Arc<Mutex<OwnerLogState>>,
}

#[derive(Debug)]
struct OwnerLogState {
    path: PathBuf,
    file: fs::File,
}

impl OwnerLogWriter {
    fn new(path: &Path) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(OwnerLogState {
                path: path.to_path_buf(),
                file,
            })),
        })
    }

    fn rotate(state: &mut OwnerLogState) -> io::Result<()> {
        state.file.flush()?;
        if OWNER_LOG_RETAINED > 1 {
            for generation in (2..OWNER_LOG_RETAINED).rev() {
                let from = rotated_log_path(&state.path, generation - 1);
                let to = rotated_log_path(&state.path, generation);
                if from.exists() {
                    if to.exists() {
                        fs::remove_file(&to)?;
                    }
                    fs::rename(&from, &to)?;
                }
            }
            let first = rotated_log_path(&state.path, 1);
            if first.exists() {
                fs::remove_file(&first)?;
            }
            fs::rename(&state.path, &first)?;
        }
        state.file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&state.path)?;
        Ok(())
    }
}

impl Write for OwnerLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("owner log lock poisoned"))?;
        let length = state.file.metadata().map(|meta| meta.len()).unwrap_or(0);
        if length.saturating_add(buf.len() as u64) > OWNER_LOG_MAX_BYTES {
            Self::rotate(&mut state)?;
        }
        state.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner
            .lock()
            .map_err(|_| io::Error::other("owner log lock poisoned"))?
            .file
            .flush()
    }
}

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for OwnerLogWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Owner subscriber with a reloadable filter. The handle is held by
/// the diagnostics sink so `diagnostics-set` can widen verbosity for
/// subsequent events without a restart. Default verbosity matches
/// the mount and sync subscribers exactly.
fn build_owner_subscriber(
    initial: DiagnosticLevel,
    writer: Option<OwnerLogWriter>,
) -> (
    impl tracing::Subscriber,
    reload::Handle<EnvFilter, tracing_subscriber::registry::Registry>,
) {
    let (filter, handle) = reload::Layer::new(log_filter(initial));
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_ansi(false);
    let file_layer = writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
    });
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(ShutdownNoiseGate {
            inner: stderr_layer,
        })
        .with(file_layer);
    (subscriber, handle)
}

/// Owner diagnostics: structured events to stderr always, plus a
/// size-bounded operator-chosen file when `--log-file` is given
/// (OD-22-D option A+C, T20 retention table). Never a drive-resident
/// default: a vault runs for months under a supervisor that captures
/// stderr, and a restart must not silently discard earlier runs —
/// rotation keeps three files instead of appending without bound.
/// Logs never contain secret bytes (same rule as CLI errors).
///
/// Call first in the owner runner, before the serving endpoint
/// or the loop thread exists. Same first-install-wins rule as the
/// mount: one owner per process by construction.
pub(crate) fn init_owner_diagnostics(log_file: Option<&Path>) -> Result<Option<PathBuf>, CliError> {
    let writer = match log_file {
        Some(path) => Some(OwnerLogWriter::new(path).map_err(|source| CliError::Io {
            path: path.to_path_buf(),
            source,
        })?),
        None => None,
    };
    let log_path = log_file.map(Path::to_path_buf);
    // Start at the persisted level; the controller reads the same
    // file and is authoritative for what operations apply. A read
    // failure here only affects this process's filter, never the
    // settings file or the level the controller reports.
    let initial = wyrd_daemon::settings::default_settings_path()
        .ok()
        .and_then(|path| wyrd_daemon::diagnostics_level_at(&path).ok())
        .unwrap_or_default();
    let (subscriber, handle) = build_owner_subscriber(initial, writer);
    let _ = tracing_log::LogTracer::init();
    if tracing_subscriber::util::SubscriberInitExt::try_init(subscriber).is_err() {
        tracing::warn!(
            stage = "start",
            log = ?log_path.as_ref().map(|path| path.display().to_string()),
            "diagnostics already initialized; reusing the installed subscriber",
        );
    }
    install_filter_sink(handle);
    Ok(log_path)
}

/// Install stderr-only diagnostics for `sync now`: the same filter
/// contract as the owner (notably `RUST_LOG` passthrough via
/// [`build_mount_subscriber`]), but no log file — the harness
/// captures stderr per run. Without this the headless path runs
/// blind: intake verdicts, route publication, and fetch dials emit
/// nothing anywhere, and a stuck run's only evidence is its summary
/// counts. Observability only: no pass, verdict, or exit semantics
/// change. Idempotent like the other inits — a second install in one
/// process (tests, `--serve` composition) reuses the subscriber.
pub(crate) fn init_sync_diagnostics() {
    let _ = tracing_log::LogTracer::init();
    let subscriber = build_mount_subscriber(None, false);
    if tracing_subscriber::util::SubscriberInitExt::try_init(subscriber).is_err() {
        tracing::warn!(
            stage = "start",
            "diagnostics already initialized; reusing the installed subscriber",
        );
    }
}

#[cfg(test)]
mod owner_diagnostics_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wyrd-owner-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn owner_log_rotates_at_the_bound_and_keeps_three_files() {
        let dir = scratch("rotation");
        let path = dir.join("owner.log");
        let mut writer = OwnerLogWriter::new(&path).expect("writer");
        let chunk = vec![b'x'; 1024];
        for _ in 0..(3 * (OWNER_LOG_MAX_BYTES / 1024) + 4) {
            writer.write_all(&chunk).expect("write");
        }
        writer.flush().expect("flush");
        assert!(path.exists(), "the current file survives");
        assert!(rotated_log_path(&path, 1).exists(), "one rotation survives");
        assert!(rotated_log_path(&path, 2).exists(), "two rotations survive");
        assert!(
            !rotated_log_path(&path, 3).exists(),
            "the oldest generation is dropped"
        );
        for generation in 1..=OWNER_LOG_RETAINED {
            let rotated = if generation == 1 {
                path.clone()
            } else {
                rotated_log_path(&path, generation - 1)
            };
            assert!(
                fs::metadata(rotated).expect("generation exists").len() <= OWNER_LOG_MAX_BYTES,
                "one record past the bound at most"
            );
        }
    }

    #[test]
    fn the_owner_filter_is_reloadable_without_a_new_subscriber() {
        let dir = scratch("reload");
        let log = dir.join("owner.log");
        let writer = OwnerLogWriter::new(&log).expect("writer");
        let (subscriber, handle) = build_owner_subscriber(DiagnosticLevel::Default, Some(writer));
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "wyrd", "hidden before detailed mode");
            handle
                .modify(|filter| *filter = log_filter(DiagnosticLevel::Detailed))
                .expect("reload");
            tracing::debug!(target: "wyrd", "visible after detailed mode");
        });
        let content = fs::read_to_string(&log).expect("log content");
        assert!(
            !content.contains("hidden before detailed mode"),
            "{content}"
        );
        assert!(content.contains("visible after detailed mode"), "{content}");
    }
}
