//! Process-local local diagnostics state (T20 in
//! `docs/contracts/trust.md`).
//!
//! One owner runs per process, so the applied level lives here as a
//! small atomic and the optional log-filter sink as a process-wide
//! once cell. The authoritative setting is still `settings.json`:
//! this module reads it at startup and writes it through the same
//! restricted transaction as every other settings edit. The in-memory
//! value only governs *subsequent* operations; work already in
//! flight keeps what it started under.
//!
//! Nothing here is a protocol identity, and nothing here is sent on
//! the wire. The session id is a random, ephemeral correlation tag
//! for local records only.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use crate::settings::{
    apply_diagnostics_update, default_diagnostics_mirror_container, default_settings_path,
    diagnostics_level_at, DiagnosticsUpdate, DiagnosticsUpdateError,
};
use wyrd_settings::DiagnosticLevel;

/// The sink notified when the applied level changes. A CLI owner
/// installs one that swaps its tracing filter; a host without a
/// reloadable filter installs none and still persists and reports
/// the level correctly.
type Sink = dyn Fn(DiagnosticLevel) + Send + Sync;

static DIAGNOSTICS_SINK: OnceLock<Arc<Sink>> = OnceLock::new();

/// A second attempt to install the process-wide diagnostics sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a diagnostics sink is already installed")]
pub struct DiagnosticsSinkAlreadyInstalled;

/// Install the process-wide log-filter callback. One owner per
/// process means one sink; a second installation is refused rather
/// than silently replacing the first filter.
pub fn install_diagnostics_sink(sink: Arc<Sink>) -> Result<(), DiagnosticsSinkAlreadyInstalled> {
    DIAGNOSTICS_SINK
        .set(sink)
        .map_err(|_| DiagnosticsSinkAlreadyInstalled)
}

fn current_sink() -> Option<&'static Arc<Sink>> {
    DIAGNOSTICS_SINK.get()
}

fn as_u8(level: DiagnosticLevel) -> u8 {
    match level {
        DiagnosticLevel::Default => 0,
        DiagnosticLevel::Detailed => 1,
    }
}

fn from_u8(raw: u8) -> Result<DiagnosticLevel, String> {
    match raw {
        0 => Ok(DiagnosticLevel::Default),
        1 => Ok(DiagnosticLevel::Detailed),
        other => Err(format!("unknown diagnostics level code {other}")),
    }
}

/// Why a live level change did not take effect. The three outcomes
/// stay distinct: a refused settings write never touched the mirror,
/// a mirror failure means the setting committed but was not applied,
/// and an unconfigured controller never had settings to write.
#[derive(Debug, thiserror::Error)]
pub enum DiagnosticsSetError {
    /// The settings transaction refused or conflicted.
    #[error("diagnostics settings refused: {0}")]
    Settings(String),
    /// The setting committed but the app-group mirror did not publish.
    /// The in-memory level is unchanged.
    #[error("diagnostics setting committed but mirror not published: {0}")]
    Mirror(String),
    /// No settings path was configured for this controller.
    #[error("no settings path configured for live diagnostics")]
    Unconfigured,
}

/// The owner's live diagnostics state.
#[derive(Debug)]
pub struct DiagnosticsController {
    level: AtomicU8,
    session_id: String,
    settings_path: Option<PathBuf>,
    mirror_container: Option<PathBuf>,
}

impl DiagnosticsController {
    /// Build a controller for explicit paths. The startup level is
    /// the persisted one, never the mirror and never a remembered
    /// live override. A settings file that cannot be read is a
    /// startup refusal: running at a guessed verbosity would make
    /// the first operations untrustworthy.
    pub fn for_paths(settings_path: PathBuf, mirror_container: PathBuf) -> Result<Self, String> {
        let level = diagnostics_level_at(&settings_path)?;
        Ok(Self {
            level: AtomicU8::new(as_u8(level)),
            session_id: new_session_id(),
            settings_path: Some(settings_path),
            mirror_container: Some(mirror_container),
        })
    }

    /// Build from the platform's settings path and app-group
    /// container. Used by both owner presentations; the container
    /// need not exist yet (the app creates it at launch), only its
    /// conventional location must be resolvable.
    pub fn from_environment() -> Result<Self, String> {
        let settings = default_settings_path()?;
        let container = default_diagnostics_mirror_container()?;
        Self::for_paths(settings, container)
    }

    /// An in-memory-only controller for servers that never persist
    /// settings (interface fixtures). It still reports and changes a
    /// level, but `set` returns [`DiagnosticsSetError::Unconfigured`]
    /// so a test never mistakes a memory change for a saved edit.
    pub fn inert() -> Self {
        Self {
            level: AtomicU8::new(as_u8(DiagnosticLevel::Default)),
            session_id: new_session_id(),
            settings_path: None,
            mirror_container: None,
        }
    }

    /// The level subsequent operations start under.
    pub fn level(&self) -> DiagnosticLevel {
        // The only writer stores `as_u8` of the closed enum.
        from_u8(self.level.load(Ordering::Acquire))
            .expect("the level slot only ever holds a closed level code")
    }

    /// The ephemeral local correlation id: never a DriveId, device
    /// identity, or any protocol identity, and never sent on the wire.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Persist a level through the settings backend, publish the
    /// app-group mirror, then apply it in memory and notify the log
    /// sink. A mirror failure leaves the applied level unchanged:
    /// the setting is saved, but reporting success would hide that
    /// the extension still reads its previous level.
    pub fn set(&self, level: DiagnosticLevel) -> Result<DiagnosticLevel, DiagnosticsSetError> {
        let (settings, mirror) = match (&self.settings_path, &self.mirror_container) {
            (Some(settings), Some(mirror)) => (settings, mirror),
            _ => return Err(DiagnosticsSetError::Unconfigured),
        };
        let update = DiagnosticsUpdate { level: Some(level) };
        match apply_diagnostics_update(settings, mirror, &update) {
            Ok(applied) => {
                self.level.store(as_u8(applied), Ordering::Release);
                // Safe summary plus the ephemeral local correlation
                // id: no filename, identity, content id, or file
                // content ever enters this event.
                tracing::info!(
                    stage = "diagnostics",
                    level = applied.as_str(),
                    session = self.session_id(),
                    "diagnostics level applied"
                );
                if let Some(sink) = current_sink() {
                    sink(applied);
                }
                Ok(applied)
            }
            Err(DiagnosticsUpdateError::Settings(error)) => {
                Err(DiagnosticsSetError::Settings(error.to_string()))
            }
            Err(DiagnosticsUpdateError::Mirror(error)) => Err(DiagnosticsSetError::Mirror(error)),
        }
    }
}

fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    match getrandom::getrandom(&mut bytes) {
        Ok(()) => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        Err(_) => {
            // Correlation only: a clock/pid fallback still separates
            // runs, and no protocol decision ever reads this value.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|age| age.as_nanos() as u64)
                .unwrap_or(0);
            format!("{:016x}{:016x}", std::process::id(), nanos)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wyrd-diagnostics-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn startup_level_is_the_persisted_level_not_the_mirror() {
        let dir = scratch("startup");
        let settings = dir.join("settings.json");
        let container = dir.join("container");
        fs::create_dir_all(&container).expect("container");
        fs::write(
            &settings,
            r#"{"version":2,"drives":{},"diagnostics":{"level":"detailed"}}"#,
        )
        .expect("settings");
        let controller = DiagnosticsController::for_paths(settings, container).expect("controller");
        assert_eq!(controller.level(), DiagnosticLevel::Detailed);
        assert_eq!(controller.session_id().len(), 32);
    }

    #[test]
    fn set_persists_publishes_and_applies_in_that_order() {
        let dir = scratch("set");
        let settings = dir.join("settings.json");
        let container = dir.join("container");
        fs::create_dir_all(&container).expect("container");
        fs::write(&settings, r#"{"version":2,"drives":{}}"#).expect("settings");
        let controller = DiagnosticsController::for_paths(settings.clone(), container.clone())
            .expect("controller");
        assert_eq!(controller.level(), DiagnosticLevel::Default);

        assert_eq!(
            controller.set(DiagnosticLevel::Detailed).expect("set"),
            DiagnosticLevel::Detailed
        );
        assert_eq!(controller.level(), DiagnosticLevel::Detailed);
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings).expect("read")).expect("parse");
        assert_eq!(persisted["diagnostics"]["level"], "detailed");
        let mirror: serde_json::Value = serde_json::from_slice(
            &fs::read(container.join("Diagnostics/level.json")).expect("mirror"),
        )
        .expect("parse mirror");
        assert_eq!(mirror["level"], "detailed");

        assert_eq!(
            controller.set(DiagnosticLevel::Default).expect("clear"),
            DiagnosticLevel::Default
        );
        assert_eq!(controller.level(), DiagnosticLevel::Default);
    }

    #[test]
    fn mirror_failure_does_not_apply_the_level() {
        let dir = scratch("mirror");
        let settings = dir.join("settings.json");
        let missing = dir.join("absent");
        fs::write(&settings, r#"{"version":2,"drives":{}}"#).expect("settings");
        let controller = DiagnosticsController::for_paths(settings, missing).expect("controller");
        let error = controller
            .set(DiagnosticLevel::Detailed)
            .expect_err("mirror failure");
        assert!(matches!(error, DiagnosticsSetError::Mirror(_)), "{error}");
        assert_eq!(controller.level(), DiagnosticLevel::Default);
    }

    #[test]
    fn an_inert_controller_never_pretends_to_persist() {
        let controller = DiagnosticsController::inert();
        assert_eq!(controller.level(), DiagnosticLevel::Default);
        assert!(matches!(
            controller.set(DiagnosticLevel::Detailed),
            Err(DiagnosticsSetError::Unconfigured)
        ));
        assert_eq!(controller.level(), DiagnosticLevel::Default);
    }
}
