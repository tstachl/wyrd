//! Device keys: the person-visible account of what this device
//! holds and where it lives (OD-24-A/B/C/D, all decided as
//! recommended).
//!
//! The mechanism is the credential and pairing flow the CLI already
//! runs (`device id`, `pairing-request`, `join`, `member invite`)
//! over the keystore that already exists. What was missing is the
//! honest copy the person asking about their keys is owed at the
//! point of asking — where the secrets live, what cannot be done
//! with them, and what replaces a device. This module owns that
//! copy, and only that copy — no authorization, no transport, no
//! keystore reads, no sequencing, no state inspection, no
//! confirmation, no error propagation. The runner owns all of
//! those; the keystore owns the wrapping.
//!
//! Four settled answers shape every renderer:
//!
//! 1. Where the key lives (OD-24-A, option A): the root key and this
//!    device's encryption secret are wrapped under a passphrase in
//!    files on this device. There is no OS secure storage
//!    integration in this release, and the copy says so rather than
//!    letting the person assume a platform credential store.
//! 2. Unlock memory (OD-24-B, option A with a per-drive opt-in): the
//!    person supplies the passphrase every session. The opt-in lives
//!    with the settings surface that owns the file it would persist
//!    in, not here.
//! 3. Replacement (OD-24-C, option A): the guided owner-mediated
//!    sequence — remove, fresh pairing-request, invite, join — with
//!    the sentence that a lost passphrase has no remedy.
//! 4. Failure (OD-24-D, option A): one message naming both causes. A
//!    mistyped passphrase and a damaged keystore are
//!    indistinguishable, so the message must not pick one. The
//!    message itself lives on `KeystoreError::WrongPassphrase`; this
//!    module only renders panels, never errors.
//!
//! The copy never names any platform credential store: those
//! words would read as promises this release does not keep. It
//! never names the recovery that belongs to its own release, either:
//! a lost passphrase has no remedy, and recovery belongs elsewhere.

use wyrd_format::{DeviceEncryptionKey, DeviceId};

/// Where the secrets live: passphrase-wrapped files on this device,
/// no OS secure storage integration. Names what is true rather than
/// letting the person assume a platform credential store.
pub const KEY_LIVES_HERE: &str = "the root key and this device's encryption secret are wrapped under a passphrase in files on this device; there is no OS secure storage integration in this release";

/// What cannot be done: this device's encryption key cannot be
/// rotated. Replacing a device takes the owner removing it and
/// admitting a fresh identity — `member rotate` mints a fresh epoch
/// secret only and changes no key.
pub const NO_DEVICE_KEY_ROTATION: &str = "this device's encryption key cannot be rotated: replacing a device takes the owner removing it and admitting a fresh identity; member rotate mints a fresh epoch secret only and changes no key";

/// A lost passphrase has no remedy in this release. States the
/// fact without naming the recovery that belongs to its own
/// release, which must never read as available here.
pub const NO_PASSPHRASE_RECOVERY: &str =
    "a lost passphrase has no remedy in this release: nothing recovers it";

/// What an unlock costs: one expensive derivation with no progress
/// signal. The runner prints this before opening the keystore, so
/// the silence during derivation reads as work, never as a hang.
pub const UNLOCK_WORKING: &str =
    "unlocking: deriving keys from the passphrase; this takes a moment and shows no progress";

/// Render the keys panel: the device id, the registered encryption
/// key (`unregistered` until the admission arrives — a fresh join
/// holds genesis only), the held epoch secrets, then the storage,
/// no-rotation, replacement, and no-remedy sentences verbatim. A
/// pure string — the runner owns the unlock, the inspection, and
/// the errors.
pub fn render_key_report(
    device: &DeviceId,
    key: Option<&DeviceEncryptionKey>,
    held: &[u64],
) -> String {
    let mut out = format!("device {device}\n");
    match key {
        Some(key) => out.push_str(&format!("encryption-key {key}\n")),
        None => out.push_str("encryption-key unregistered\n"),
    }
    if held.is_empty() {
        out.push_str("held secrets: none\n");
    } else {
        out.push_str("held secrets:");
        for epoch in held {
            out.push_str(&format!(" {epoch}"));
        }
        out.push('\n');
    }
    out.push_str(&format!("{KEY_LIVES_HERE}\n{NO_DEVICE_KEY_ROTATION}\n"));
    out.push_str(&render_re_enrol(device));
    out.push_str(&format!("{NO_PASSPHRASE_RECOVERY}\n"));
    out
}

/// Render the replacement sequence: the owner-mediated steps in
/// order, naming the exact commands. Step 2 needs a fresh identity
/// file (device identity is single-use within a membership chain),
/// never the removed device's key. The runner never performs the
/// owner half — this is guidance, not automation.
pub fn render_re_enrol(device: &DeviceId) -> String {
    format!(
        "to replace this device:\n1. the owner runs: member remove {device}\n2. with a fresh identity file, this device runs: device pairing-request <out>\n3. the owner runs: member invite <device> <encryption-key> <invitation>\n4. this device runs: device join <invitation>\n"
    )
}

/// Render the unlock notice: printed before the keystore opens, so
/// the expensive derivation carries an observable state. A pure
/// string — the runner owns the print and the open.
pub fn render_unlock_notice() -> String {
    format!("{UNLOCK_WORKING}\n")
}

#[cfg(test)]
#[path = "tests_key_surface.rs"]
mod tests_key_surface;
