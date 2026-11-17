//! The menu-bar extra (system tray): a non-owning presence that
//! reuses only landed IPC through the registry probe.
//!
//! Discipline, inherited from the shell:
//!
//! - Non-owning client, never opens a drive directory: the menu
//!   builds from [`crate::registry::list_drives`], which validates paths
//!   and probes each owner socket with a bounded `liveness` read —
//!   the same transport-level verdict the lifecycle view shows.
//!   No store opens, no mutation path, no new wire bytes.
//! - No implicit active drive: status lines are disabled menu items
//!   (presentation only, never a selection), and every drive-scoped
//!   command still carries its explicit directory. One dead owner
//!   leaves every other drive's status and connections untouched,
//!   because the probe fans out per drive under its own bound.
//! - The tray routes, never authors: "Add drive" shows the
//!   window and emits `wyrd:select-provision` for the frontend to
//!   render the provision view on the create tab; provisioning
//!   execution stays with runtime provisioning. "Show window"
//!   focuses the existing window.
//!   "Quit" drains in-flight operations per the graceful-shutdown
//!   rule (see `main.rs`) and exits — nothing is reported as
//!   success without an owner answer.
//! - Toolkit edge: `tauri::tray` plus `tauri::menu` ride inside the
//!   reserved `tauri` edge (`tray-icon` + `image-png` features). No
//!   new direct dependency edge — the `image-png` feature pulls the
//!   image stack transitively, which the direct-edge gate does not
//!   count — so the layer gate needs no new allow-list entry.

use tauri::{AppHandle, Manager, Runtime};

use super::registry::{DriveAvailability, RegistryState};

/// One tray menu row: the pure model the builder renders. Status
/// lines are disabled (`enabled: false`); actions are enabled.
/// The builder inserts the separator between the two sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayRow {
    /// Menu item id: drive row, action id, or explanation row id.
    pub id: String,
    /// Menu label: `{name} — {Available|Unavailable|Unknown}` for drives.
    pub label: String,
    /// False for status lines (presentation only), true for the three actions.
    pub enabled: bool,
}

/// Menu action ids: the only events the tray handles.
pub const TRAY_ADD_ID: &str = "wyrd-add-drive";
/// Menu action id for the Show-window action: the only event the tray handles besides setup and quit.
pub const TRAY_SHOW_ID: &str = "wyrd-show";
/// Menu action id for the Quit action: drains in-flight operations per the graceful-shutdown rule, then exits.
pub const TRAY_QUIT_ID: &str = "wyrd-quit";

/// Frontend event the Add-drive action emits: the window shows and
/// the provision view renders on the create tab. The tray never
/// selects a drive.
///
/// Mirrored in `apps/desktop/src/App.svelte` (the tray route
/// listener): no shared build step crosses the Rust/TS boundary, so
/// keep the string in sync on both sides.
pub const SELECT_PROVISION_EVENT: &str = "wyrd:select-provision";

/// Tray icon id: one presence per process.
pub const TRAY_ID: &str = "wyrd-main";

/// One drive's status line: `{name} — {Available|Unavailable|Unknown}`.
/// Availability is transport-level only — the same verdict the
/// lifecycle view shows — never a claim about the drive's contents.
pub fn drive_line(name: &str, availability: &DriveAvailability) -> String {
    let state = match availability {
        DriveAvailability::Available => "Available",
        DriveAvailability::Unavailable { .. } => "Unavailable",
        DriveAvailability::Unknown { .. } => "Unknown",
    };
    format!("{name} — {state}")
}

/// The pure menu model: one disabled row per drive (registry order),
/// then the three actions. Empty and unreadable registries render
/// one disabled explanation row instead of drive lines — never an
/// empty menu, never a guessed drive.
pub fn tray_rows(state: &RegistryState) -> Vec<TrayRow> {
    let mut rows = Vec::new();
    if state.drives.is_empty() {
        let label = match &state.status {
            super::registry::RegistryStatus::Nofile => "No drives registered".to_string(),
            super::registry::RegistryStatus::Ready => "No drives registered".to_string(),
            super::registry::RegistryStatus::Unreadable { .. } => {
                "Registry unreadable — open the window for details".to_string()
            }
        };
        rows.push(TrayRow {
            id: "wyrd-no-drives".to_string(),
            label,
            enabled: false,
        });
    } else {
        for drive in &state.drives {
            rows.push(TrayRow {
                id: format!("wyrd-drive-{}", drive.name),
                label: drive_line(&drive.name, &drive.availability),
                enabled: false,
            });
        }
    }
    rows.push(TrayRow {
        id: TRAY_ADD_ID.to_string(),
        label: "Add drive".to_string(),
        enabled: true,
    });
    rows.push(TrayRow {
        id: TRAY_SHOW_ID.to_string(),
        label: "Show window".to_string(),
        enabled: true,
    });
    rows.push(TrayRow {
        id: TRAY_QUIT_ID.to_string(),
        label: "Quit".to_string(),
        enabled: true,
    });
    rows
}

/// Build the Tauri menu from the pure model: status rows, then a
/// separator, then the three actions.
pub fn build_tray_menu<R: Runtime, M: Manager<R>>(
    manager: &M,
    state: &RegistryState,
) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{MenuBuilder, MenuItem};
    let rows = tray_rows(state);
    // Status rows are all rows but the last three (the actions).
    let split = rows.len().saturating_sub(3);
    let mut builder = MenuBuilder::new(manager);
    for row in rows.iter().take(split) {
        let item = MenuItem::with_id(manager, &row.id, &row.label, row.enabled, None::<&str>)?;
        builder = builder.item(&item);
    }
    builder = builder.separator();
    for row in rows.iter().skip(split) {
        let item = MenuItem::with_id(manager, &row.id, &row.label, row.enabled, None::<&str>)?;
        builder = builder.item(&item);
    }
    builder.build()
}

/// The pre-probe placeholder: an empty ready registry, replaced by
/// the refresh thread's immediate first read. The setup hook never
/// probes — a wedged owner must never delay first paint — so the
/// first menu briefly reads "No drives registered" until the
/// background refresh (milliseconds later) renders the real state.
pub fn loading_state() -> RegistryState {
    RegistryState {
        status: super::registry::RegistryStatus::Ready,
        drives: Vec::new(),
        skipped: Vec::new(),
    }
}

/// Show and focus the main window. Best-effort: a missing window is
/// not a tray failure, so errors are logged, never propagated as a
/// menu-event failure. Deminiaturizes first: `set_focus` is a no-op
/// while minimized, so restore before showing and focusing.
pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        if let Err(error) = window.show().and_then(|()| window.set_focus()) {
            eprintln!("wyrd-desktop: tray show window failed: {error}");
        }
    }
}

/// Rebuild the tray menu where the registry state changed since the
/// last render. No tray, no work: a failed build leaves no consumer,
/// so the probe is skipped entirely instead of connecting per drive.
/// An unchanged state skips the rebuild, so a long-running app never
/// churns (or dismisses) the native menu. One slow owner never wedges
/// the refresh: the registry probe bounds each drive, and a failed
/// rebuild keeps the previous menu with `last` untouched.
pub fn refresh_tray_if_changed<R: Runtime>(app: &AppHandle<R>, last: &mut Option<RegistryState>) {
    if app.tray_by_id(TRAY_ID).is_none() {
        return;
    }
    let state = super::registry::list_drives();
    if last.as_ref() == Some(&state) {
        return;
    }
    match build_tray_menu(app, &state) {
        Ok(menu) => {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                match tray.set_menu(Some(menu)) {
                    Ok(()) => {
                        *last = Some(state);
                    }
                    Err(error) => eprintln!("wyrd-desktop: tray menu refresh failed: {error}"),
                }
            }
        }
        Err(error) => eprintln!("wyrd-desktop: tray menu build failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::registry::{DriveView, RegistryStatus};
    use super::*;

    fn drive(name: &str, availability: DriveAvailability) -> DriveView {
        DriveView {
            name: name.to_string(),
            drive_dir: format!("/drives/{name}"),
            relays: vec!["wss://relay.example".to_string()],
            service: format!("drive-{name}"),
            mode: "vault".to_string(),
            mountpoint: None,
            availability,
            service_state: super::super::service::ServiceState::Unknown {
                detail: "tray tests do not probe the manager".to_string(),
            },
        }
    }

    fn ready(drives: Vec<DriveView>) -> RegistryState {
        RegistryState {
            status: RegistryStatus::Ready,
            drives,
            skipped: Vec::new(),
        }
    }

    /// Two live owners and one dead one: the menu shows both healthy
    /// drives and the third explicitly unavailable — never omitting
    /// the dead drive, never merging two outcomes into one line.
    #[test]
    fn two_live_and_one_dead_render_explicitly() {
        let state = ready(vec![
            drive("alpha", DriveAvailability::Available),
            drive("beta", DriveAvailability::Available),
            drive(
                "gamma",
                DriveAvailability::Unavailable {
                    detail: "no owner".to_string(),
                },
            ),
        ]);
        let rows = tray_rows(&state);
        assert_eq!(rows.len(), 6, "three status lines plus three actions");
        assert_eq!(rows[0].label, "alpha — Available");
        assert_eq!(rows[1].label, "beta — Available");
        assert_eq!(rows[2].label, "gamma — Unavailable");
        assert!(
            rows[..3].iter().all(|row| !row.enabled),
            "status lines are presentation only, never a selection"
        );
        assert_eq!(rows[3].id, TRAY_ADD_ID);
        assert_eq!(rows[3].label, "Add drive");
        assert_eq!(rows[4].id, TRAY_SHOW_ID);
        assert_eq!(rows[5].id, TRAY_QUIT_ID);
        assert!(
            rows[3..].iter().all(|row| row.enabled),
            "the three actions stay enabled"
        );
    }

    /// Unknown (transport exists, no terminal answer) renders
    /// distinctly from unavailable: the retry decision depends on
    /// which one came back.
    #[test]
    fn unknown_renders_distinctly_from_unavailable() {
        let state = ready(vec![drive(
            "alpha",
            DriveAvailability::Unknown {
                detail: "torn frame".to_string(),
            },
        )]);
        let rows = tray_rows(&state);
        assert_eq!(rows[0].label, "alpha — Unknown");
        assert!(!rows[0].enabled);
    }

    /// An empty registry renders one explanation row, never an
    /// empty menu and never a guessed drive.
    #[test]
    fn empty_registry_renders_one_explanation_row() {
        let state = ready(Vec::new());
        let rows = tray_rows(&state);
        assert_eq!(rows[0].label, "No drives registered");
        assert!(!rows[0].enabled);
        assert_eq!(rows.len(), 4, "one explanation plus three actions");
    }

    /// The pre-probe placeholder renders the empty menu until the
    /// refresh thread's immediate first read replaces it.
    #[test]
    fn loading_state_renders_the_empty_menu_until_the_first_refresh() {
        let rows = tray_rows(&loading_state());
        assert_eq!(rows[0].label, "No drives registered");
        assert!(!rows[0].enabled);
    }

    /// The tray route string is mirrored in the frontend with no
    /// shared build step, so pin it here: if either side renames
    /// its literal, this fails instead of silently diverging.
    #[test]
    fn provision_event_string_matches_the_frontend_listener() {
        let app_svelte = include_str!("../../../apps/desktop/src/App.svelte");
        assert!(
            app_svelte.contains(SELECT_PROVISION_EVENT),
            "App.svelte must listen for {SELECT_PROVISION_EVENT}"
        );
    }

    /// An unreadable registry names itself loudly in the disabled
    /// row; the full reason stays in the window, not the menu.
    #[test]
    fn unreadable_registry_names_itself() {
        let state = RegistryState {
            status: RegistryStatus::Unreadable {
                reason: "registry /x/drives.json is version 2, this shell reads 1".to_string(),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        };
        let rows = tray_rows(&state);
        assert!(
            rows[0].label.contains("Registry unreadable"),
            "label: {}",
            rows[0].label
        );
        assert!(!rows[0].enabled);
    }
}
