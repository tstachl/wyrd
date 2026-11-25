//! Bake the effective App Group id into every Rust binary at compile
//! time. The sandboxed File Provider extension resolves its group
//! from a build-rendered `Info.plist`; the registrar gets it as a
//! build-rendered launchd argv. Both the owner daemon and the
//! desktop diagnostics export resolve the group from
//! `app_group_identifier()` at runtime, but a launchd-spawned
//! process inherits no `WYRD_APP_GROUP` — so reading the env at
//! runtime alone would point them at the compiled default while the
//! Apple-facing artifacts point at a third party's override.
//!
//! Capturing the same resolution at build time removes that split:
//! `DEFAULT_APP_GROUP_IDENTIFIER` becomes the effective build value,
//! so owner, desktop, appex and registrar agree without any runtime
//! env. An explicit runtime `WYRD_APP_GROUP` still wins (dev and
//! manual runs), preserving the existing override contract.
//!
//! Resolution order mirrors `packaging/macos/app-group.sh`:
//!   WYRD_APP_GROUP       full id, explicit override (wins)
//!   APPLE_TEAM_ID   derives <team>.group.md.enesis.wyrd
//!   built-in default     the team-prefixed default
//!
//! Re-runs only when an identity input changes. The raw identity
//! inputs are never emitted into the artifact — only the resulting
//! group id, which the runtime needs.

const DEFAULT_GROUP: &str = "JU2RFHWQPH.group.md.enesis.wyrd";
const GROUP_SUFFIX: &str = "group.md.enesis.wyrd";
const TEAM_LEN: usize = 10;

fn trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn validate_team(team: &str) -> Result<(), String> {
    if team.len() != TEAM_LEN {
        return Err(format!(
            "APPLE_TEAM_ID must be {TEAM_LEN} characters, got {}: {team}",
            team.len()
        ));
    }
    if !team
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(format!(
            "APPLE_TEAM_ID has invalid characters (want A-Z, 0-9): {team}"
        ));
    }
    Ok(())
}

fn main() {
    println!("cargo:rerun-if-env-changed=WYRD_APP_GROUP");
    println!("cargo:rerun-if-env-changed=APPLE_TEAM_ID");

    let effective = match trimmed("WYRD_APP_GROUP") {
        Some(group) => {
            if let Some(bad) = group
                .chars()
                .find(|c| !c.is_ascii_alphanumeric() && !"._-".contains(*c))
            {
                panic!("wyrd-settings: WYRD_APP_GROUP has invalid character {bad:?}");
            }
            group
        }
        None => match trimmed("APPLE_TEAM_ID") {
            Some(team) => {
                if let Err(error) = validate_team(&team) {
                    panic!("wyrd-settings: {error}");
                }
                format!("{team}.{GROUP_SUFFIX}")
            }
            None => DEFAULT_GROUP.to_string(),
        },
    };

    println!("cargo:rustc-env=WYRD_EFFECTIVE_APP_GROUP={effective}");
}
