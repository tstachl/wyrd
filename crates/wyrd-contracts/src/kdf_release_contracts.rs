//! Release KDF posture and distribution-build args: the fast KDF
//! table and fixture features stay out of production manifests
//! and out of the flake's distribution build, and every platform
//! the flake ships has exactly one release asset.

use crate::contract_source::*;
use std::collections::BTreeMap;
use std::fs;

/// Release-asset coverage (07): every platform the flake ships has
/// exactly one release asset, no asset names a platform the flake
/// does not ship, and no asset is a tarball for a platform whose
/// delivery is an installer (macOS `.dmg`, Linux `.flatpak`). The
/// four-tarball manifest went unnoticed because nothing compared the
/// two lists, so this compares them structurally — not a substitute
/// for building each installer at release time, which only the
/// release owner can do (signing identity, notarization, bundle
/// signing all live outside this repository).
#[test]
fn release_assets_cover_every_platform_tag() {
    let root = workspace_root();
    let flake = fs::read_to_string(root.join("flake.nix")).expect("flake.nix is readable");
    let systems = flake_string_list(&flake, "systems");
    assert!(
        !systems.is_empty(),
        "flake names no product platforms at all"
    );
    let platform_of: BTreeMap<&str, &str> = flake
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (system, tag) = line.split_once('=')?;
            let system = system.trim().trim_matches('"');
            let tag = tag.trim().trim_matches(|c| c == '"' || c == ';');
            systems.contains(&system).then_some((system, tag))
        })
        .collect();
    assert_eq!(
        platform_of.len(),
        systems.len(),
        "every flake system maps to exactly one ngit platform tag, found: {platform_of:?}"
    );

    let manifest =
        fs::read_to_string(root.join(".ngit/release.yaml")).expect("release.yaml is readable");
    let assets = release_assets(&manifest);
    assert!(!assets.is_empty(), "release.yaml names no assets at all");
    let mut tags: Vec<&str> = assets.iter().map(|asset| asset.2).collect();
    tags.sort_unstable();
    let mut expected: Vec<&str> = platform_of.values().copied().collect();
    expected.sort_unstable();
    assert_eq!(
        tags, expected,
        "one asset per shipped platform, no more and no fewer"
    );
    let mut violations = Vec::new();
    for (file, mime, tag) in &assets {
        let (want_ext, want_mime) = if tag.starts_with("macos-") {
            (".dmg", "application/x-apple-diskimage")
        } else if tag.starts_with("linux-") {
            (".flatpak", "application/vnd.flatpak")
        } else {
            violations.push(format!("{tag} names no known platform family"));
            continue;
        };
        if !file.ends_with(want_ext) {
            violations.push(format!(
                "{tag} delivers as an installer, but its asset is {file} (want *{want_ext})"
            ));
        }
        if *mime != want_mime {
            violations.push(format!(
                "{file} declares {mime} but the installer shape needs {want_mime}"
            ));
        }
        if file.ends_with(".tar.gz") {
            violations.push(format!(
                "{tag} ships a tarball where the runbook promises an installer: {file}"
            ));
        }
        if !file.contains("{version}") {
            violations.push(format!("{file} is not version-templated"));
        }
    }
    assert!(
        violations.is_empty(),
        "release-asset violations:\n- {}",
        violations.join("\n- ")
    );

    // The Linux installer the assets promise has a manifest with the
    // release app-id and the host as its command, each on its own
    // line: a marker embedded in a longer line does not count.
    // Structure only: the bundle itself builds at release time on the
    // publisher's machine.
    let flatpak = fs::read_to_string(root.join("flatpak/md.enesis.wyrd.yaml"))
        .expect("the Flatpak manifest exists");
    for marker in ["app-id: md.enesis.wyrd", "command: wyrd-host"]
        .iter()
        .copied()
    {
        assert!(
            flatpak.lines().any(|line| line.trim() == marker),
            "the Flatpak manifest lost {marker:?} as its own line"
        );
    }
}

/// `(file, mime, platform)` triples from the `assets:` block of the
/// release manifest. Each asset is a `file:` line, a `mime:` line,
/// then its `platforms:` line; anything else in the block fails the
/// parse loudly. Format-sensitive by design: a cosmetic reformat that
/// moves a marker fails this guard, and that failure is the guard
/// working, not a false positive.
fn release_assets(manifest: &str) -> Vec<(&str, &str, &str)> {
    let mut assets = Vec::new();
    let mut file: Option<&str> = None;
    let mut mime: Option<&str> = None;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("- file: ") {
            assert!(
                file.is_none(),
                "asset {file:?} never closed with its platforms"
            );
            file = Some(value.trim());
        } else if let Some(value) = trimmed.strip_prefix("mime: ") {
            assert!(mime.is_none(), "asset {file:?} declares two MIME types");
            mime = Some(value.trim());
        } else if let Some(value) = trimmed.strip_prefix("platforms: [") {
            let tag = value
                .strip_suffix(']')
                .expect("platforms list closes on its line");
            assets.push((
                file.take().expect("platforms without an asset file"),
                mime.take().expect("platforms without an asset MIME type"),
                tag.trim(),
            ));
        }
    }
    assert!(
        file.is_none(),
        "the last asset never closed with its platforms"
    );
    assets
}

/// One feature string enables the fast KDF profile when it names the
/// token outright (`insecure-fast-kdf`), in `dep/token` form
/// (`wyrd-sync/insecure-fast-kdf`, weak or strong), or through one
/// level of aliasing: a local feature whose definition lists the
/// token in either form. Anything deeper (an alias of an alias) is
/// out of scope for this structural check — name the token plainly
/// or not at all.
fn feature_enables_fast_kdf(name: &str, features: &toml::Table) -> bool {
    if name.contains("insecure-fast-kdf") {
        return true;
    }
    features
        .get(name)
        .and_then(|def| def.as_array())
        .is_some_and(|targets| {
            targets.iter().any(|target| {
                target
                    .as_str()
                    .is_some_and(|s| s.contains("insecure-fast-kdf"))
            })
        })
}

/// Collect every manifest edge that enables `insecure-fast-kdf` outside
/// test-only tables: `[dependencies]` and `[build-dependencies]` (plus
/// any `[target.*]` nesting) are production edges; only
/// `[dev-dependencies]` may carry the fast profile. Alias forms count
/// (`dep/token`, or a local feature expanding to the token — see
/// `feature_enables_fast_kdf`). The `[features]` definition section is
/// skipped — declaring the feature in `wyrd-sync` is not enabling it.
/// Pure over the manifest so the negative tests below can prove the
/// checker bites without touching the filesystem.
fn check_kdf_feature_edges(member: &str, manifest: &toml::Table) -> Vec<String> {
    fn walk(
        table: &toml::Table,
        features: &toml::Table,
        path: &str,
        dev: bool,
        member: &str,
        out: &mut Vec<String>,
    ) {
        for (key, value) in table {
            let Some(inner) = value.as_table() else {
                continue;
            };
            if key == "features" {
                // The feature-definition section, not an edge.
                continue;
            }
            if key == "dev-dependencies" {
                walk(inner, features, &format!("{path}.{key}"), true, member, out);
            } else if key == "dependencies" || key == "build-dependencies" {
                walk(
                    inner,
                    features,
                    &format!("{path}.{key}"),
                    false,
                    member,
                    out,
                );
            } else if inner
                .get("features")
                .and_then(|f| f.as_array())
                .is_some_and(|fs| {
                    fs.iter().any(|f| {
                        f.as_str()
                            .is_some_and(|name| feature_enables_fast_kdf(name, features))
                    })
                })
            {
                if !dev {
                    out.push(format!(
                        "`{member}` enables `insecure-fast-kdf` on `{key}` via `{path}`: \
                         the fast KDF profile is test-only — keep it in `[dev-dependencies]`"
                    ));
                }
            } else {
                walk(inner, features, &format!("{path}.{key}"), dev, member, out);
            }
        }
    }
    let mut violations = Vec::new();
    walk(
        manifest,
        &member_features(manifest),
        "manifest",
        false,
        member,
        &mut violations,
    );
    violations
}

pub(crate) fn check_release_build_args(text: &str) -> Vec<String> {
    // Live code only: comments are stripped, so a commented-out
    // `cargoExtraArgs` neither counts as declared nor hides a token.
    let code = strip_nix_comments(text);
    if !code.contains("cargoExtraArgs") {
        return vec![
            "flake names no cargoExtraArgs at all: the distribution build must declare \
             its cargo flags where this gate can see them"
                .to_owned(),
        ];
    }
    let mut violations = Vec::new();
    for token in ["insecure-fast-kdf", "test-fixtures", "--all-features"] {
        if code.contains(token) {
            violations.push(format!(
                "distribution build text enables `{token}`: only default features ship — \
                 all-features, fast-KDF, and fixture builds are never production"
            ));
        }
    }
    violations
}

/// No member may enable `insecure-fast-kdf` through its default
/// feature set: `default = ["insecure-fast-kdf"]` in any `[features]`
/// table would bake the fast table into default builds while the
/// edge walker above stays green (it skips `[features]` by design).
/// Alias forms count the same way they do on edges. Pure over the
/// manifest so the negative tests below can prove the checker bites
/// without touching the filesystem.
fn check_feature_default_enables_kdf(member: &str, manifest: &toml::Table) -> Vec<String> {
    let features = member_features(manifest);
    let default_enables = features
        .get("default")
        .and_then(|d| d.as_array())
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.as_str()
                    .is_some_and(|name| feature_enables_fast_kdf(name, &features))
            })
        });
    if default_enables {
        vec![format!(
            "`{member}` enables `insecure-fast-kdf` through its default features: \
             the fast KDF profile must stay opt-in and test-only"
        )]
    } else {
        Vec::new()
    }
}

/// Contract 56, manifest half: no member's production manifest edge
/// enables `insecure-fast-kdf`, and no member (or the workspace root)
/// carries it in a default feature set. The current edges all live in
/// `[dev-dependencies]` (fast per-test `Engine`s); a production edge
/// would bake the 8 MiB / t=1 table into a shipped artifact through
/// feature unification, silently. The resolved-graph backstop stays in
/// CI (`cargo tree -e normal,features` in workflow.yml); this is the
/// in-suite pin that fails the PR gate first.
#[test]
fn release_manifests_exclude_insecure_fast_kdf() {
    let root = workspace_root();
    let root_manifest = read_manifest(&root);
    let members: Vec<String> = root_manifest
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .expect("workspace members list")
        .iter()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect();
    // Never pass vacuously: an empty member list would check nothing.
    assert!(!members.is_empty(), "workspace declares no members");
    let mut violations = Vec::new();
    for member in &members {
        let name = member.rsplit('/').next().unwrap_or(member);
        let manifest = read_manifest(&root.join(member));
        violations.extend(check_kdf_feature_edges(name, &manifest));
        violations.extend(check_feature_default_enables_kdf(name, &manifest));
    }
    // The root's `[workspace.dependencies]` is walked too: a feature
    // there is inherited by every `workspace = true` edge.
    violations.extend(check_kdf_feature_edges("workspace-root", &root_manifest));
    violations.extend(check_feature_default_enables_kdf(
        "workspace-root",
        &root_manifest,
    ));
    assert!(
        violations.is_empty(),
        "fast-KDF production edges:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract 56, flake half: the distribution build compiles `-p
/// wyrd-cli` with default features, so `kdf_key` takes the production
/// arm (keystore.rs) and derives with the pinned 64 MiB / t=3 table
/// (pinned in turn by `kdf_parameters_are_pinned`). Fails closed on a
/// `cargoExtraArgs` line that enables the feature or passes
/// `--all-features`.
#[test]
fn release_build_args_exclude_insecure_fast_kdf() {
    let text =
        fs::read_to_string(workspace_root().join("flake.nix")).expect("flake.nix is readable");
    let violations = check_release_build_args(&text);
    assert!(
        violations.is_empty(),
        "release-build violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract (F2): the release build compiles `-p
/// wyrd-cli` with default features, so the `test-fixtures` token must
/// never reach the distribution-build args either — a fixture build
/// would ship the retired direct-write authority as public API.
#[test]
fn release_build_args_exclude_test_fixtures() {
    let text =
        fs::read_to_string(workspace_root().join("flake.nix")).expect("flake.nix is readable");
    for token in ["test-fixtures", "--all-features"] {
        assert!(
            !strip_nix_comments(&text).contains(token),
            "distribution build text must not enable `{token}`"
        );
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    fn kdf_manifest(manifest: &str) -> toml::Table {
        manifest.parse().unwrap()
    }

    #[test]
    fn prod_kdf_edge_is_rejected() {
        let violations = check_kdf_feature_edges(
            "wyrd-core",
            &kdf_manifest(
                r#"
                [dependencies]
                wyrd-sync = { workspace = true, features = ["insecure-fast-kdf"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-core"));
        assert!(violations[0].contains("wyrd-sync"));
    }

    #[test]
    fn target_scoped_prod_kdf_edge_is_rejected() {
        let violations = check_kdf_feature_edges(
            "wyrd-daemon",
            &kdf_manifest(
                r#"
                [target.'cfg(unix)'.dependencies]
                wyrd-sync = { workspace = true, features = ["insecure-fast-kdf"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-daemon"));
    }

    #[test]
    fn build_dep_kdf_edge_is_rejected() {
        let violations = check_kdf_feature_edges(
            "wyrd-cli",
            &kdf_manifest(
                r#"
                [build-dependencies]
                wyrd-sync = { workspace = true, features = ["insecure-fast-kdf"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-cli"));
    }

    #[test]
    fn dev_only_kdf_edge_is_allowed() {
        let violations = check_kdf_feature_edges(
            "wyrd-core",
            &kdf_manifest(
                r#"
                [dependencies]
                wyrd-sync.workspace = true

                [dev-dependencies]
                wyrd-sync = { workspace = true, features = ["insecure-fast-kdf"] }
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "dev-only fast-KDF edge must stay allowed: {violations:?}"
        );
    }

    #[test]
    fn kdf_feature_definition_is_not_an_edge() {
        // `wyrd-sync` declares the feature itself; the declaration is
        // not an enablement and must not trip the checker.
        let violations = check_kdf_feature_edges(
            "wyrd-sync",
            &kdf_manifest(
                r#"
                [features]
                insecure-fast-kdf = []
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "feature definition must not read as an edge: {violations:?}"
        );
    }

    #[test]
    fn flake_all_features_is_rejected() {
        let violations =
            check_release_build_args("cargoExtraArgs = \"-p wyrd-cli --all-features\";\n");
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("--all-features"));
    }

    #[test]
    fn flake_kdf_feature_is_rejected() {
        let violations = check_release_build_args(
            "cargoExtraArgs = \"-p wyrd-cli --features insecure-fast-kdf\";\n",
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("insecure-fast-kdf"));
    }

    #[test]
    fn flake_clean_args_pass() {
        let violations = check_release_build_args("cargoExtraArgs = \"-p wyrd-cli\";\n");
        assert!(
            violations.is_empty(),
            "default-feature release args must pass: {violations:?}"
        );
    }

    #[test]
    fn flake_multiline_args_with_all_features_are_rejected() {
        // F1's bypass shape: the token rides a continuation line of a
        // multiline Nix string, which a line-literal scan never sees.
        let violations =
            check_release_build_args("cargoExtraArgs = ''\n  -p wyrd-cli\n  --all-features\n'';\n");
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("--all-features"));
    }

    #[test]
    fn flake_commented_tokens_are_ignored() {
        let violations = check_release_build_args(
            "# never enable insecure-fast-kdf or --all-features here\n\
             cargoExtraArgs = \"-p wyrd-cli\";\n",
        );
        assert!(
            violations.is_empty(),
            "commented tokens must not trip the gate: {violations:?}"
        );
    }

    #[test]
    fn flake_commented_out_args_fail_closed() {
        // Only a commented-out declaration: the live build is
        // undeclared, so the gate must fail, not pass.
        let violations = check_release_build_args("# cargoExtraArgs = \"-p wyrd-cli\";\n");
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn flake_hash_inside_a_string_is_not_a_comment() {
        // The `#` rides inside the string, so the args line stays
        // live and the token scan sees the whole value.
        let violations = check_release_build_args("cargoExtraArgs = \"-p wyrd-cli # pinned\";\n");
        assert!(
            violations.is_empty(),
            "a hash inside a string must not start a comment: {violations:?}"
        );
    }

    #[test]
    fn flake_hash_inside_a_string_hides_no_token() {
        // Same quoting shape, but the string carries the feature: the
        // token is live text and must trip the gate.
        let violations =
            check_release_build_args("cargoExtraArgs = \"-p wyrd-cli # --all-features\";\n");
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("--all-features"));
    }

    #[test]
    fn flake_missing_args_fails_closed() {
        let violations = check_release_build_args("# no cargo flags here\n");
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn workspace_root_prod_kdf_edge_is_rejected() {
        // A feature on the root `[workspace.dependencies]` entry is
        // inherited by every `workspace = true` edge.
        let violations = check_kdf_feature_edges(
            "workspace-root",
            &kdf_manifest(
                r#"
                [workspace.dependencies]
                wyrd-sync = { path = "crates/wyrd-sync", features = ["insecure-fast-kdf"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("workspace-root"));
    }

    #[test]
    fn default_feature_enabling_kdf_is_rejected() {
        let violations = check_feature_default_enables_kdf(
            "wyrd-sync",
            &kdf_manifest(
                r#"
                [features]
                default = ["insecure-fast-kdf"]
                insecure-fast-kdf = []
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-sync"));
    }

    #[test]
    fn default_feature_without_kdf_is_allowed() {
        let violations = check_feature_default_enables_kdf(
            "wyrd-sync",
            &kdf_manifest(
                r#"
                [features]
                default = ["std"]
                insecure-fast-kdf = []
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "unrelated defaults must stay allowed: {violations:?}"
        );
    }

    #[test]
    fn dep_slash_token_edge_is_rejected() {
        // The `dep/token` form on an edge names the token without the
        // literal string standing alone.
        let violations = check_kdf_feature_edges(
            "wyrd-core",
            &kdf_manifest(
                r#"
                [dependencies]
                wyrd-sync = { workspace = true, features = ["wyrd-sync/insecure-fast-kdf"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-core"));
    }

    #[test]
    fn aliased_kdf_edge_is_rejected() {
        // A local feature expanding to the token must not smuggle it
        // onto a production edge.
        let violations = check_kdf_feature_edges(
            "wyrd-core",
            &kdf_manifest(
                r#"
                [dependencies]
                wyrd-sync = { workspace = true, features = ["fast-tests"] }

                [features]
                fast-tests = ["insecure-fast-kdf"]
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-core"));
    }

    #[test]
    fn unrelated_alias_is_allowed() {
        let violations = check_kdf_feature_edges(
            "wyrd-core",
            &kdf_manifest(
                r#"
                [dependencies]
                wyrd-sync = { workspace = true, features = ["fast-tests"] }

                [features]
                fast-tests = ["std"]
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "unrelated aliases must stay allowed: {violations:?}"
        );
    }

    #[test]
    fn aliased_default_enabling_kdf_is_rejected() {
        let violations = check_feature_default_enables_kdf(
            "wyrd-cli",
            &kdf_manifest(
                r#"
                [features]
                default = ["fast-tests"]
                fast-tests = ["wyrd-sync/insecure-fast-kdf"]
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-cli"));
    }
}
