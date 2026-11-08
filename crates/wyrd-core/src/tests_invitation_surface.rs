//! Copy tests for the invitation-and-join surface (OD-13-A/B/C).
//! The protocol facts the copy states (live-chain-only admission
//! walk, statement-driven learning, genesis-plus-grant join) are
//! pinned where they ship — the late-joiner engine test, the
//! admission fill, the bootstrap ordering — unchanged; here pins the
//! surface's own honesty.

use wyrd_format::{DeviceId, DriveId};

use super::{render_join_confirmation, CATCH_UP_NEXT_SYNC, HISTORY_BOUNDARY, RECOVERY_IS_REISSUE};

fn device(byte: u8) -> DeviceId {
    DeviceId::from_bytes([byte; 32])
}

fn drive(byte: u8) -> DriveId {
    DriveId::from_bytes([byte; 32])
}

/// The confirmation keeps the legacy headline: existing readers of
/// the first line (scripts grepping `joined`, the E2E join log)
/// see no churn; the new sentences ride below it.
#[test]
fn join_confirmation_keeps_the_joined_headline() {
    let confirmation = render_join_confirmation(&device(0x22), &drive(0x44), 2);
    let mut lines = confirmation.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!("joined {} drive {} at epoch 2", device(0x22), drive(0x44)),
        "first line is the legacy headline"
    );
}

/// The boundary states the limit and the mechanism: pre-admission
/// content may not be reachable, the admission covers the live
/// chain only, and reconciliation is never promised.
#[test]
fn join_confirmation_states_the_history_boundary() {
    let confirmation = render_join_confirmation(&device(0x22), &drive(0x44), 2);
    assert!(
        confirmation.contains(HISTORY_BOUNDARY),
        "boundary sentence renders: {confirmation}"
    );
    assert!(
        confirmation.contains("may not be reachable"),
        "names the limit: {confirmation}"
    );
    assert!(
        confirmation.contains("live chain only"),
        "names the mechanism: {confirmation}"
    );
}

/// Catch-up arrives on sync, not at join — and the confirmation
/// never claims completeness in any casing.
#[test]
fn join_confirmation_promises_catch_up_on_sync_never_completeness() {
    let confirmation = render_join_confirmation(&device(0x22), &drive(0x44), 2);
    assert!(
        confirmation.contains(CATCH_UP_NEXT_SYNC),
        "catch-up cadence renders: {confirmation}"
    );
    for promised in [
        "complete",
        "up to date",
        "up-to-date",
        "in sync",
        "whole drive",
        "entire drive",
        "full history",
    ] {
        assert!(
            !confirmation.to_lowercase().contains(promised),
            "no completeness promise ({promised}): {confirmation}"
        );
    }
}

/// The recovery pointer names reissue as the path and refuses the
/// second admission in the same breath.
#[test]
fn join_confirmation_points_recovery_at_reissue() {
    let confirmation = render_join_confirmation(&device(0x22), &drive(0x44), 2);
    assert!(
        confirmation.contains(RECOVERY_IS_REISSUE),
        "recovery sentence renders: {confirmation}"
    );
    assert!(
        confirmation.contains("never with a second admission"),
        "re-invite is a resume, not a duplicate: {confirmation}"
    );
}

/// The epoch rides the headline: a newcomer admitted epochs later
/// sees which epoch admitted it, so the boundary has a number.
#[test]
fn join_confirmation_names_the_admission_epoch() {
    let confirmation = render_join_confirmation(&device(0x22), &drive(0x44), 7);
    assert!(
        confirmation.contains("at epoch 7"),
        "admission epoch is visible: {confirmation}"
    );
}
