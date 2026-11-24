//! Local Finder-integration state for the macOS shell.
//!
//! An ad-hoc dev install leaves the File Provider extension without a
//! team identity or an effective App Group container. Finder registers
//! the domain but the extension cannot reach an owner socket, and
//! Finder reports a generic server/network error. The shell can state
//! the build fact. A provisioning profile is one signal, but a
//! profile-free install can also sign the appex with a team identity;
//! the installer stages that choice in the bundle so the shell does
//! not mislabel it as ad-hoc.
//!
//! This is a build fact, not a drive fact. Nothing here opens a drive,
//! reads a credential, or calls a network service. The state is also
//! included in an explicit local diagnostics export so the "no group
//! container" case stays distinct from a temporary socket failure.

use std::path::Path;

/// The File Provider capability of the installed bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FinderIntegration {
    /// The appex carries a provisioning profile: Finder can resolve
    /// its App Group container and reach a running owner's socket.
    Provisioned,
    /// No appex profile, but the extension is team-signed with a
    /// profile-free identity. It can resolve a container; Finder
    /// behavior depends on the container-path agreement.
    TeamSigned,
    /// No appex profile and no team identity: an ad-hoc dev install.
    /// Finder shows a generic server/network error.
    Unprovisioned,
    /// Not a macOS .app bundle (dev server or other host). There is
    /// no Finder integration to report either way.
    NotApplicable,
}

impl FinderIntegration {
    /// Stable lowercase label for the local diagnostics bundle.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioned => "provisioned",
            Self::TeamSigned => "team-signed",
            Self::Unprovisioned => "unprovisioned",
            Self::NotApplicable => "not-applicable",
        }
    }
}

/// Build fact staged by `install-dev.sh` and mapped into `Resources`.
/// An absent or unknown value is the ad-hoc default, so a hand-built
/// bundle never claims a capability it did not declare.
const CAPABILITY_PATH: &str = "Resources/wyrd-finder-capability.txt";

/// Resolve the state from a bundle's `Contents` directory. Kept pure
/// so tests stage a tree instead of installing an app.
pub fn finder_integration_at(contents: &Path) -> FinderIntegration {
    let profile = contents
        .join("PlugIns")
        .join("WyrdFileProvider.appex")
        .join("Contents")
        .join("embedded.provisionprofile");
    if profile.is_file() {
        return FinderIntegration::Provisioned;
    }
    match std::fs::read_to_string(contents.join(CAPABILITY_PATH)) {
        Ok(text) if text.trim() == "identity" => FinderIntegration::TeamSigned,
        _ => FinderIntegration::Unprovisioned,
    }
}

/// Resolve the state of the running app. `current_exe()` ends in
/// `Contents/MacOS/<binary>` for a bundle; two parents up is
/// `Contents`. Outside a bundle the answer is `NotApplicable`.
pub fn finder_integration() -> FinderIntegration {
    if !cfg!(target_os = "macos") {
        return FinderIntegration::NotApplicable;
    }
    let Some(exe) = std::env::current_exe().ok() else {
        return FinderIntegration::NotApplicable;
    };
    let Some(macos) = exe.parent() else {
        return FinderIntegration::NotApplicable;
    };
    let Some(contents) = macos.parent() else {
        return FinderIntegration::NotApplicable;
    };
    if contents.file_name().is_some_and(|name| name == "Contents") {
        finder_integration_at(contents)
    } else {
        FinderIntegration::NotApplicable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn profile_presence_decides_the_state() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-finder-integration-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let appex = dir
            .join("PlugIns")
            .join("WyrdFileProvider.appex")
            .join("Contents");
        fs::create_dir_all(&appex).unwrap();
        assert_eq!(
            finder_integration_at(&dir),
            FinderIntegration::Unprovisioned
        );
        fs::write(appex.join("embedded.provisionprofile"), b"profile").unwrap();
        assert_eq!(finder_integration_at(&dir), FinderIntegration::Provisioned);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn staged_identity_marker_distinguishes_a_profile_free_signed_appex() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-finder-marker-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let resources = dir.join("Resources");
        fs::create_dir_all(&resources).unwrap();
        assert_eq!(
            finder_integration_at(&dir),
            FinderIntegration::Unprovisioned
        );
        fs::write(resources.join("wyrd-finder-capability.txt"), b"identity\n").unwrap();
        assert_eq!(finder_integration_at(&dir), FinderIntegration::TeamSigned);
        fs::write(resources.join("wyrd-finder-capability.txt"), b"unknown\n").unwrap();
        assert_eq!(
            finder_integration_at(&dir),
            FinderIntegration::Unprovisioned
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn labels_are_stable_lowercase_words() {
        assert_eq!(FinderIntegration::Provisioned.as_str(), "provisioned");
        assert_eq!(FinderIntegration::TeamSigned.as_str(), "team-signed");
        assert_eq!(FinderIntegration::Unprovisioned.as_str(), "unprovisioned");
        assert_eq!(FinderIntegration::NotApplicable.as_str(), "not-applicable");
        assert_eq!(
            serde_json::to_string(&FinderIntegration::Unprovisioned).unwrap(),
            "\"unprovisioned\""
        );
    }
}
