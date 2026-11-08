use super::*;
use wyrd_format::DeviceId;

fn device(byte: u8) -> DeviceId {
    DeviceId::from_bytes([byte; 32])
}

/// The removal confirmation carries the canonical boundary and
/// history sentences verbatim, and the terminal sentence only when
/// the owner set emptied.
#[test]
fn removal_confirmation_carries_the_canonical_sentences() {
    let out = render_removal_confirmation(&device(0x44), 3, 1, false);
    assert!(
        out.starts_with("removed 4444"),
        "legacy headline first: {out}"
    );
    assert!(out.contains("(1 carried)"), "carry count: {out}");
    assert!(
        out.contains(REMOVAL_BOUNDARY),
        "canonical boundary verbatim: {out}"
    );
    assert!(
        out.contains(HISTORY_NEVER_REENCRYPTED),
        "canonical history sentence: {out}"
    );
    assert!(
        !out.contains(SOLE_OWNER_TERMINAL),
        "no terminal sentence for an ordinary removal: {out}"
    );
    let terminal = render_removal_confirmation(&device(0x44), 2, 0, true);
    assert!(
        terminal.contains(SOLE_OWNER_TERMINAL),
        "terminal removal states the terminal consequence: {terminal}"
    );
}

/// The rotation confirmation names the fresh epoch secret and the
/// unchanged membership, and claims no device-key change.
#[test]
fn rotation_confirmation_names_the_secret_not_a_key() {
    let out = render_rotation_confirmation(3, 1);
    assert!(
        out.starts_with("rotated to epoch 3"),
        "legacy headline: {out}"
    );
    assert!(out.contains("fresh epoch secret"), "what rotated: {out}");
    assert!(out.contains("membership unchanged"), "what did not: {out}");
    assert!(
        !out.contains("device key"),
        "rotation cannot express a key change: {out}"
    );
}

/// The handover confirmation attributes the handover to the outgoing
/// owner and reports the carried head count.
#[test]
fn handover_confirmation_attributes_and_counts() {
    let out = render_handover_confirmation(&device(0x44), &device(0x22), 3, 2);
    assert!(out.contains("owner is now 4444"), "new owner: {out}");
    assert!(
        out.contains("handed over by 2222"),
        "outgoing author: {out}"
    );
    assert!(out.contains("(2 carried)"), "carried count: {out}");
}

/// The frozen notice names the epoch, lists the rivals, and points
/// at the resolver — never at authoring another control.
#[test]
fn frozen_notice_names_epoch_rivals_and_remedy() {
    let rival = TransitionId::from_bytes([0xAB; 32]);
    let out = render_frozen_notice(2, &[rival]);
    assert!(out.contains("frozen at epoch 2"), "freeze: {out}");
    assert!(out.contains(&rival.to_string()), "rivals: {out}");
    assert!(out.contains("member resolve"), "remedy: {out}");
}
