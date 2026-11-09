//! The materialization vocabulary names exactly two policy values
//! and two presence values, plus the two canonical sentences the
//! surfaces print verbatim.

use crate::materialization_surface::{
    EVICT_CONFIRM, FETCHED_STILL_REMOTE_ONLY, POLICY_PINNED_WORD, POLICY_REMOTE_ONLY_WORD,
    PRESENCE_ABSENT_WORD, PRESENCE_PRESENT_WORD,
};

#[test]
fn vocabulary_is_two_policy_values_and_two_presence_values() {
    assert_eq!(POLICY_PINNED_WORD, "PINNED");
    assert_eq!(POLICY_REMOTE_ONLY_WORD, "REMOTE_ONLY");
    assert_eq!(PRESENCE_PRESENT_WORD, "PRESENT");
    assert_eq!(PRESENCE_ABSENT_WORD, "ABSENT");
}

#[test]
fn fetched_content_carries_no_retention_promise() {
    assert_eq!(
        FETCHED_STILL_REMOTE_ONLY,
        "Content fetched through access is still REMOTE_ONLY. Its presence on this device does not constitute a retention promise."
    );
}

#[test]
fn evict_confirmation_names_release_and_kept_bytes() {
    assert_eq!(
        EVICT_CONFIRM,
        "Evict this content from the device's retention policy? This releases the device's retention promise. The action does not delete the bytes currently stored on this device."
    );
}
