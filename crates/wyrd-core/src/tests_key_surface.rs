use super::*;
use crate::key_surface::render_unlock_notice;

/// Words this surface must never print: forget euphemisms, the
/// platform stores this release does not integrate with, and the
/// recovery that belongs to its own release.
fn banned(output: &str) -> Vec<&'static str> {
    [
        "deleted",
        "erased",
        "reclaimed",
        "wiped",
        "keychain",
        "Keychain",
        "Secure Enclave",
        "TPM",
        "hardware-backed",
        "encrypted by the OS",
        "root recovery",
    ]
    .into_iter()
    .filter(|word| output.contains(*word))
    .collect()
}

/// The keys panel names where the secrets live: passphrase-wrapped
/// files on this device, no OS secure storage — and none of the
/// platform-store words a person would read as promises.
#[test]
fn key_surface_names_where_the_key_lives() {
    let device = DeviceId::from_bytes([0x44; 32]);
    let key = DeviceEncryptionKey::from_bytes([0x45; 32]);
    let out = render_key_report(&device, Some(&key), &[1, 2]);
    assert!(
        out.contains(KEY_LIVES_HERE),
        "storage sentence verbatim: {out}"
    );
    assert!(out.contains("passphrase"), "names the wrapping: {out}");
    assert!(
        out.contains("files on this device"),
        "names the place: {out}"
    );
    assert!(
        out.contains("no OS secure storage integration"),
        "names the absence: {out}"
    );
    assert!(
        banned(&out).is_empty(),
        "no platform-store or recovery promise, found {banned:?}: {out}",
        banned = banned(&out)
    );
}

/// The keys panel offers no device-key rotation: the action the
/// milestone does not contain must not appear as copy either. The
/// panel states replacement is remove-and-re-admit, and the module
/// authors no membership change of any kind.
#[test]
fn key_surface_offers_no_device_key_rotation() {
    let device = DeviceId::from_bytes([0x44; 32]);
    let out = render_key_report(&device, None, &[]);
    assert!(
        out.contains(NO_DEVICE_KEY_ROTATION),
        "no-rotation sentence verbatim: {out}"
    );
    assert!(
        out.contains("removing it and admitting a fresh identity"),
        "replacement is remove-and-re-admit: {out}"
    );
    assert!(out.contains("member remove"), "owner command named: {out}");
    assert!(
        out.contains("member rotate mints a fresh epoch secret only"),
        "rotate claims no key change: {out}"
    );
    let source = include_str!("key_surface.rs");
    for marker in ["Change::", "author_", "admit_device", "remove_device"] {
        assert!(
            !source.contains(marker),
            "the surface authors nothing ({marker})"
        );
    }
    assert!(banned(&out).is_empty(), "no forget euphemism: {out}");
}

/// The surface never touches the KDF or the credential hardening:
/// it formats strings over ids, keys, and epochs. The credential
/// rules stay implemented once, in the credential reader.
#[test]
fn credential_rules_are_not_reimplemented_by_the_surface() {
    let source = include_str!("key_surface.rs");
    for marker in [
        "kdf_key",
        "unwrap_root",
        "unwrap_device_secret",
        "pairing_request",
        "O_NOFOLLOW",
        "O_CLOEXEC",
        "0o077",
        "metadata()",
    ] {
        assert!(
            !source.contains(marker),
            "the surface reimplements nothing ({marker})"
        );
    }
}

/// The unlock notice is the observable unlocking state: printed
/// before the blocking derivation, which reports no progress of its
/// own. Names the cost and the silence together.
#[test]
fn unlock_shows_progress_before_the_kdf_returns() {
    let out = render_unlock_notice();
    assert!(out.contains("unlocking"), "names the state: {out}");
    assert!(out.contains("takes a moment"), "names the cost: {out}");
    assert!(
        out.contains("shows no progress"),
        "names the silence: {out}"
    );
}

/// The report names the device copyably, the registered key (or the
/// honest `unregistered`), and the held epochs (or the honest
/// `none`) — the three facts the person came to see.
#[test]
fn key_report_names_device_key_and_held_secrets() {
    let device = DeviceId::from_bytes([0x44; 32]);
    let key = DeviceEncryptionKey::from_bytes([0x45; 32]);
    let out = render_key_report(&device, Some(&key), &[1, 2]);
    assert!(
        out.contains(&device.to_string()),
        "device id in copyable form: {out}"
    );
    assert!(
        out.contains(&key.to_string()),
        "registered key named: {out}"
    );
    assert!(out.contains("held secrets: 1 2"), "held named: {out}");
    let fresh = render_key_report(&device, None, &[]);
    assert!(
        fresh.contains("encryption-key unregistered"),
        "unregistered is a state, not an error: {fresh}"
    );
    assert!(
        fresh.contains("held secrets: none"),
        "no secrets is a state, not an error: {fresh}"
    );
}

/// The replacement sequence names the exact commands in order, with
/// the device id copyable, and the no-remedy sentence travels with
/// it — a person who has just lost a passphrase is the least able
/// to reconstruct either from a sentence.
#[test]
fn re_enrol_names_the_owner_mediated_sequence() {
    let device = DeviceId::from_bytes([0x44; 32]);
    let out = render_key_report(&device, None, &[]);
    for step in [
        "member remove",
        "device pairing-request",
        "member invite",
        "device join",
    ] {
        assert!(out.contains(step), "step named ({step}): {out}");
    }
    assert!(
        out.contains(&device.to_string()),
        "device id copyable in the sequence: {out}"
    );
    assert!(
        out.contains(NO_PASSPHRASE_RECOVERY),
        "no-remedy sentence travels with the sequence: {out}"
    );
    assert!(
        banned(&out).is_empty(),
        "no recovery promise beside the no-remedy sentence: {out}"
    );
}
