//! Read-only credential-backend observation through the installed
//! CLI: the shell asks the version-matched binary what backend the
//! drive's deployment config names, instead of guessing from the
//! registry name. The CLI is the one parser of `default.toml`; this
//! module adds no Cargo edge and never opens a keystore.
//!
//! The answer is a reference — backend kind plus store account or
//! credential file paths. No secret contents cross this boundary,
//! and every failure is an explicit `Unknown` with a reason so the
//! UI keeps an operator choice rather than silently assuming the
//! store backend.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

#[cfg(test)]
use std::path::PathBuf;

/// One drive's effective credential reference as observed from its
/// deployment config. Serialized straight to the frontend; the
/// struct variants pin the tag vocabulary there.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "backend", rename_all = "kebab-case")]
pub enum CredentialReference {
    /// Store-backed: the account label, never the stored secret.
    OsStore {
        /// The OS credential-store item name.
        account: String,
    },
    /// File-backed: the configured paths, never their contents.
    File {
        /// Identity path from the deployment config.
        #[serde(rename = "identityFile")]
        identity_file: String,
        /// Passphrase path from the deployment config.
        #[serde(rename = "passphraseFile")]
        passphrase_file: String,
    },
    /// No usable observation: the reason travels so the shell can
    /// keep its explicit selector and say why.
    Unknown {
        /// Why the backend could not be observed.
        reason: String,
    },
}

/// Bound on the read-only query. A wedged CLI is an unknown answer,
/// not a retry — the same fail-closed rule as the other CLI adapters.
pub const CREDENTIAL_REF_TIMEOUT: Duration = Duration::from_secs(15);

/// Pure argv builder for `wyrd credential-reference <drive-dir>`:
/// one drive identity as one exact argv element, never a shell.
pub fn credential_ref_argv(drive_dir: &Path) -> Vec<OsString> {
    vec![
        "credential-reference".into(),
        drive_dir.as_os_str().to_owned(),
    ]
}

/// Run the query once and parse its single JSON line. Only exit 0
/// with a recognized shape is an observation; anything else is
/// `Unknown` with opaque display detail, never a guessed backend.
pub fn run_credential_reference(
    cli_bin: &Path,
    drive_dir: &Path,
    timeout: Duration,
) -> CredentialReference {
    let argv = credential_ref_argv(drive_dir);
    let output = crate::provision::run_cli(cli_bin, &argv, None, timeout);
    if !output.status_success {
        // Display detail only: never parsed into a backend. Prefer
        // stderr, then stdout, then the bare exit status.
        let detail = if !output.stderr.trim().is_empty() {
            output.stderr.as_str()
        } else if !output.stdout.trim().is_empty() {
            output.stdout.as_str()
        } else {
            ""
        };
        let reason = if detail.is_empty() {
            format!("credential query exited {:?}", output.code)
        } else {
            crate::provision::truncate(detail, 300)
        };
        return CredentialReference::Unknown { reason };
    }
    match serde_json::from_str::<CredentialReference>(output.stdout.trim()) {
        Ok(reference) => reference,
        Err(_) => CredentialReference::Unknown {
            reason: "credential query returned an unrecognized reference".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_names_one_drive_as_one_element() {
        let argv = credential_ref_argv(Path::new("/drives/alpha"));
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0], "credential-reference");
        assert_eq!(argv[1], "/drives/alpha");
    }

    #[test]
    fn each_reference_shape_deserializes_from_the_cli_line() {
        let store: CredentialReference =
            serde_json::from_str(r#"{"backend":"os-store","account":"wyrd-drive-alpha"}"#).unwrap();
        assert_eq!(
            store,
            CredentialReference::OsStore {
                account: "wyrd-drive-alpha".to_string()
            }
        );
        let file: CredentialReference = serde_json::from_str(
            r#"{"backend":"file","identityFile":"/keys/id","passphraseFile":"/keys/pass"}"#,
        )
        .unwrap();
        assert_eq!(
            file,
            CredentialReference::File {
                identity_file: "/keys/id".to_string(),
                passphrase_file: "/keys/pass".to_string(),
            }
        );
        let unknown: CredentialReference =
            serde_json::from_str(r#"{"backend":"unknown","reason":"no configuration file"}"#)
                .unwrap();
        assert_eq!(
            unknown,
            CredentialReference::Unknown {
                reason: "no configuration file".to_string()
            }
        );
    }

    #[test]
    fn a_malformed_line_is_unknown_not_a_guess() {
        let parsed = serde_json::from_str::<CredentialReference>(r#"{"backend":"file"}"#);
        assert!(parsed.is_err(), "missing paths must not parse as a backend");
    }

    /// A fake CLI script, run through the same `run_cli` as the real
    /// binary: no mock, no special path.
    #[cfg(unix)]
    fn fake_cli(tag: &str, body: &str, code: i32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wyrd-credref-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wyrd");
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s' '{body}'\nexit {code}\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_fake_cli_reference_round_trips_through_the_adapter() {
        let bin = fake_cli(
            "ok",
            r#"{"backend":"os-store","account":"wyrd-drive-alpha"}"#,
            0,
        );
        let reference =
            run_credential_reference(&bin, Path::new("/drives/alpha"), Duration::from_secs(5));
        assert_eq!(
            reference,
            CredentialReference::OsStore {
                account: "wyrd-drive-alpha".to_string()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_query_is_unknown_with_opaque_detail() {
        let bin = fake_cli("fail", "credential query unavailable", 2);
        let reference =
            run_credential_reference(&bin, Path::new("/drives/alpha"), Duration::from_secs(5));
        match reference {
            CredentialReference::Unknown { reason } => {
                assert!(reason.contains("credential query unavailable"), "{reason}");
            }
            other => panic!("a failed query cannot name a backend: {other:?}"),
        }
    }
}
