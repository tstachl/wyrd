use super::*;
use crate::member_surface::{
    render_removal_confirmation, HISTORY_NEVER_REENCRYPTED, REMOVAL_BOUNDARY, SOLE_OWNER_TERMINAL,
};

fn banned(output: &str) -> Vec<&'static str> {
    ["deleted", "erased", "reclaimed", "wiped"]
        .into_iter()
        .filter(|word| output.contains(*word))
        .collect()
}

/// The leave confirmation states the removal boundary: the
/// sole-owner path reuses the canonical removal confirmation, and
/// the non-owner refusal carries the same boundary verbatim. Both
/// contain the cli.md sentence and none of the euphemisms a leave
/// must never use.
#[test]
fn leave_confirmation_states_removal_bounds_acquisition() {
    let device = DeviceId::from_bytes([0x44; 32]);
    let sole_owner = render_removal_confirmation(&device, 2, 0, true);
    assert!(
        sole_owner.contains(REMOVAL_BOUNDARY),
        "sole-owner leave states the boundary: {sole_owner}"
    );
    assert!(
        sole_owner.contains(HISTORY_NEVER_REENCRYPTED),
        "sole-owner leave states history is kept: {sole_owner}"
    );
    assert!(
        sole_owner.contains(SOLE_OWNER_TERMINAL),
        "sole-owner leave states the terminal consequence: {sole_owner}"
    );
    let refusal = render_leave_refusal(&device);
    assert!(
        refusal.contains(REMOVAL_BOUNDARY),
        "non-owner refusal states the boundary: {refusal}"
    );
    assert!(
        refusal.contains(HISTORY_NEVER_REENCRYPTED),
        "non-owner refusal states history is kept: {refusal}"
    );
    for (name, output) in [("sole-owner", &sole_owner), ("refusal", &refusal)] {
        assert!(
            banned(output).is_empty(),
            "{name} uses no forget euphemism, found {banned:?}: {output}",
            banned = banned(output)
        );
    }
}

/// The non-owner refusal never authors and never reports success:
/// it names the owner requirement, shows the device id copyably,
/// and states the person is still in the shared membership.
#[test]
fn leave_refusal_names_owner_shows_device_and_denies_departure() {
    let device = DeviceId::from_bytes([0x44; 32]);
    let out = render_leave_refusal(&device);
    assert!(
        out.contains(LEAVE_NEEDS_OWNER),
        "owner requirement first: {out}"
    );
    assert!(
        out.contains(&device.to_string()),
        "device id in copyable form: {out}"
    );
    assert!(
        out.contains("member remove"),
        "exact owner command named: {out}"
    );
    assert!(
        out.contains("still in the shared membership"),
        "local departure distinguished from removal: {out}"
    );
    for word in ["success", "complete", "you left", "has left"] {
        assert!(!out.contains(word), "never reports success ({word}): {out}");
    }
    assert!(banned(&out).is_empty(), "no forget euphemism: {out}");
}

/// The no-seat refusal names the device and offers no transition.
#[test]
fn not_a_member_names_device_and_authors_nothing() {
    let device = DeviceId::from_bytes([0x45; 32]);
    let out = render_not_a_member(&device);
    assert!(out.contains(LEAVE_NOT_A_MEMBER), "reason: {out}");
    assert!(out.contains(&device.to_string()), "device id named: {out}");
}

/// Close quits in the documented teardown order and promises
/// nothing about the directory.
#[test]
fn quit_note_states_order_and_keeps_everything() {
    let out = render_quit_note();
    assert!(
        out.contains("unmount and session join first"),
        "step 1: {out}"
    );
    assert!(
        out.contains("admission close and loop join"),
        "step 2: {out}"
    );
    assert!(
        out.contains("transport under bounded deadlines"),
        "step 3: {out}"
    );
    assert!(
        out.contains("promises nothing about their contents"),
        "no directory promise: {out}"
    );
    for kept in ["content", "membership", "credentials", "metadata"] {
        assert!(out.contains(kept), "close keeps {kept}: {out}");
    }
}

/// Export needs the drive un-mounted and fails closed on the lock.
#[test]
fn export_requirement_names_unmounted_and_fail_closed() {
    let out = render_export_requirement();
    assert!(out.contains("un-mounted"), "precondition: {out}");
    assert!(out.contains("fails closed"), "no retry: {out}");
}
