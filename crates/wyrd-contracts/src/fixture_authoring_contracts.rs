//! Retired mutation authority: `WyrdNode` authoring survives only
//! behind the `test-fixtures` feature in dev-dependencies, and
//! snapshot authoring in `wyrd-core` stays confined to the live
//! pass plus those gated fixture bodies.

use crate::contract_source::*;

/// One feature string enables the retired direct-write fixtures when
/// it names the token outright (`test-fixtures`), in `dep/token`
/// form (`wyrd-core/test-fixtures`, weak or strong), or through one
/// level of aliasing: a local feature whose definition lists the
/// token in either form. Anything deeper (an alias of an alias) is
/// out of scope for this structural check — name the token plainly
/// or not at all. Mirrors `feature_enables_fast_kdf`: same shapes,
/// same one-level bound, separate token so each rule reads alone.
fn feature_enables_test_fixtures(name: &str, features: &toml::Table) -> bool {
    if name.contains("test-fixtures") {
        return true;
    }
    features
        .get(name)
        .and_then(|def| def.as_array())
        .is_some_and(|targets| {
            targets
                .iter()
                .any(|target| target.as_str().is_some_and(|s| s.contains("test-fixtures")))
        })
}

/// Collect every manifest edge that enables `test-fixtures` outside
/// test-only tables: `[dependencies]` and `[build-dependencies]` (plus
/// any `[target.*]` nesting) are production edges; only
/// `[dev-dependencies]` may carry the fixture authority. Alias forms
/// count (`dep/token`, or a local feature expanding to the token —
/// see `feature_enables_test_fixtures`). The `[features]` definition
/// section is skipped — declaring the feature in `wyrd-core` is not
/// enabling it. Pure over the manifest so the negative tests below
/// can prove the checker bites without touching the filesystem.
fn check_fixtures_feature_edges(member: &str, manifest: &toml::Table) -> Vec<String> {
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
                            .is_some_and(|name| feature_enables_test_fixtures(name, features))
                    })
                })
            {
                if !dev {
                    out.push(format!(
                        "`{member}` enables `test-fixtures` on `{key}` via `{path}`: \
                         the fixture authority is test-only — keep it in `[dev-dependencies]`"
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

/// No member may enable `test-fixtures` through its default feature
/// set: `default = ["test-fixtures"]` in any `[features]` table would
/// bake the retired authority into default builds while the edge
/// walker above stays green (it skips `[features]` by design).
/// Alias forms count the same way they do on edges. Pure over the
/// manifest so the negative tests below can prove the checker bites
/// without touching the filesystem.
fn check_feature_default_enables_fixtures(member: &str, manifest: &toml::Table) -> Vec<String> {
    let features = member_features(manifest);
    let default_enables = features
        .get("default")
        .and_then(|d| d.as_array())
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.as_str()
                    .is_some_and(|name| feature_enables_test_fixtures(name, &features))
            })
        });
    if default_enables {
        vec![format!(
            "`{member}` enables `test-fixtures` through its default features: \
             the fixture authority must stay opt-in and test-only"
        )]
    } else {
        Vec::new()
    }
}

/// Contract (F2), manifest half: no member's production manifest edge
/// enables `test-fixtures`, and no member (or the workspace root)
/// carries it in a default feature set. The current edges all live in
/// `[dev-dependencies]` (fixture authoring for contract, lifecycle,
/// and daemon/CLI test setups); a production edge would bake the
/// retired direct-write authority into a shipped artifact through
/// feature unification, silently. The resolved-graph backstop stays in
/// CI (`cargo tree -e normal,features` in workflow.yml); this is the
/// in-suite pin that fails the PR gate first.
#[test]
fn release_manifests_exclude_test_fixtures() {
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
        violations.extend(check_fixtures_feature_edges(name, &manifest));
        violations.extend(check_feature_default_enables_fixtures(name, &manifest));
    }
    // The root's `[workspace.dependencies]` is walked too: a feature
    // there is inherited by every `workspace = true` edge.
    violations.extend(check_fixtures_feature_edges(
        "workspace-root",
        &root_manifest,
    ));
    violations.extend(check_feature_default_enables_fixtures(
        "workspace-root",
        &root_manifest,
    ));
    assert!(
        violations.is_empty(),
        "test-fixture production edges:\n- {}",
        violations.join("\n- ")
    );
}

/// Whether a `pub fn` definition at `def_idx` carries the
/// test-fixtures gate: a `#[cfg(feature = "test-fixtures")]`
/// attribute between the previous function definition and this one
/// (the convention is the attribute directly above the item; the
/// window covers a short doc tail, never a neighboring item, so a
/// gate on one fixture cannot satisfy the next). Runs on raw lines —
/// the code stripper removes string literals, which would eat the
/// token inside the attribute itself — and only counts lines that
/// open a real attribute, so a commented-out gate never satisfies it.
/// `code_lines` is the stripped twin of `raw_lines` (same length)
/// for item-boundary detection.
fn fn_is_fixture_gated(raw_lines: &[&str], code_lines: &[&str], def_idx: usize) -> bool {
    let prev_def = (0..def_idx).rev().find(|k| is_fn_def(code_lines[*k]));
    let start = (def_idx.saturating_sub(8)).max(prev_def.map_or(0, |k| k + 1));
    raw_lines[start..def_idx]
        .iter()
        .any(|line| line.trim_start().starts_with("#[cfg(") && line.contains("test-fixtures"))
}

/// A `fn` definition line: an optional `pub` visibility (possibly
/// scoped, even with spaces), then `async`/`unsafe`/`const`/`extern`/
/// `default` modifiers in any order, an optional ABI string after
/// `extern` (`extern "C" fn`, `unsafe extern "C-unwind" fn`), then the
/// `fn` keyword and the function name. Matching the trailing
/// `fn <ident>` shape — rather than a visibility-prefix list — keeps a
/// future `pub(crate) async fn` / `pub unsafe fn` / `pub const fn` /
/// `pub extern "C" fn` authoring body attributed to itself instead of
/// the previous function. Runs on stripped code lines, so comments and
/// string literals never match (the ABI string is already gone on
/// production scan lines — the skip keeps direct unstripped calls
/// correct too); `fn`-pointer types (`fn(u32)`) and
/// `Fn` bounds never match either (`fn` must stand alone, followed by
/// a name).
fn is_fn_def(line: &str) -> bool {
    let mut tokens = line.split_whitespace().peekable();
    // Optional visibility: `pub`, `pub(...)`.
    if tokens
        .peek()
        .is_some_and(|t| *t == "pub" || t.starts_with("pub("))
    {
        let vis = tokens.next().unwrap_or("");
        // A scoped visibility may contain spaces (`pub(in crate::x)`):
        // consume through the closing paren.
        if vis.starts_with("pub(") && !vis.ends_with(')') {
            for tok in tokens.by_ref() {
                if tok.ends_with(')') {
                    break;
                }
            }
        }
    }
    // Optional modifiers, in any order. `default` covers specialization
    // shapes (`default fn`); an ABI string literal may follow `extern`
    // (`extern "C" fn`) and is skipped, not mistaken for the name.
    let mut saw_extern = false;
    while tokens
        .peek()
        .is_some_and(|t| matches!(*t, "async" | "unsafe" | "const" | "extern" | "default"))
    {
        if tokens.peek().is_some_and(|t| *t == "extern") {
            saw_extern = true;
        }
        tokens.next();
    }
    if saw_extern && tokens.peek().is_some_and(|t| t.starts_with('"')) {
        tokens.next();
    }
    // The keyword plus the function name.
    match tokens.next() {
        Some("fn") => tokens.next().is_some_and(|name| {
            name.chars()
                .next()
                .is_some_and(|c| c == '_' || c.is_alphabetic())
        }),
        _ => false,
    }
}

/// Check that snapshot authoring in `wyrd-core` stays confined to the
/// live pass plus the two gated fixtures: every
/// `author_pending_write(`/`author_snapshot(`/`author_recovery_snapshot(`
/// call in `node.rs` must sit inside a `test-fixtures`-gated
/// function block (the retired `put_file`/`remove` bodies — a new
/// ungated authoring entry point fails here whatever its name), and
/// nowhere else in `live/` may author but `apply.rs` (the traced
/// seam the loop drives). Test files (`tests_*.rs`) are out of scope
/// by construction: fixture setup authors through `Engine` directly,
/// which is what the gated node fixtures exist to serve. Pure over
/// the file set so the negative tests below can prove the checker
/// bites without touching the filesystem.
fn check_node_authoring_confined(files: &[(&str, &str)]) -> Vec<String> {
    const AUTHORING: &[&str] = &[
        "author_pending_write(",
        "author_snapshot(",
        "author_recovery_snapshot(",
    ];
    let mut violations = Vec::new();
    let mut saw_gated_fixture_def = false;
    for (path, text) in files {
        // Raw lines for gate pairing (the stripper eats the token
        // inside the attribute's own string literal); stripped lines
        // for everything else, so comments and strings never name an
        // entry or an authoring call. A backslash-newline inside a
        // string literal would otherwise cost the stripped text a
        // line the raw text keeps, so continuations join up front —
        // semantically neutral for scanning, and both sides then
        // agree line for line (asserted below).
        let joined = text.replace("\\\n", "");
        let raw_lines: Vec<&str> = joined.lines().collect();
        let code = code_text(&joined);
        let lines: Vec<&str> = code.lines().collect();
        debug_assert_eq!(
            raw_lines.len(),
            lines.len(),
            "the stripper must preserve line breaks in {path}"
        );
        let in_node = path.ends_with("wyrd-core/src/node.rs");
        let in_live = path.contains("wyrd-core/src/live/");
        if !in_node && !in_live {
            continue;
        }
        // Test files name the engine directly as setup; the rule
        // pins the production surface, not its fixtures.
        if path.contains("tests_") || path.contains("/tests/") {
            continue;
        }
        for (idx, line) in lines.iter().enumerate() {
            // The retired authority's gated definitions: the pin's
            // own liveness witness (the default build must not see
            // these names ungated).
            if in_node && (line.contains("pub fn put_file") || line.contains("pub fn remove(")) {
                if fn_is_fixture_gated(&raw_lines, &lines, idx) {
                    saw_gated_fixture_def = true;
                } else {
                    violations.push(format!(
                        "{path}:{} names a direct-write entry without the test-fixtures gate: \
                         author through the live channel instead",
                        idx + 1
                    ));
                }
            }
            if AUTHORING.iter().any(|marker| line.contains(marker)) {
                if in_node {
                    // Attribute the call to its enclosing function:
                    // the nearest preceding `fn` definition must
                    // carry the fixtures gate.
                    let gated = (0..idx).rev().find_map(|j| {
                        is_fn_def(lines[j]).then(|| fn_is_fixture_gated(&raw_lines, &lines, j))
                    });
                    match gated {
                        Some(true) => {}
                        _ => violations.push(format!(
                            "{path}:{} authors a snapshot outside a test-fixtures-gated \
                             function: production authoring flows through live/apply.rs",
                            idx + 1
                        )),
                    }
                } else if !path.ends_with("wyrd-core/src/live/apply.rs") {
                    violations.push(format!(
                        "{path}:{} authors a snapshot outside live/apply.rs: \
                         the loop's traced seam is the single production authority",
                        idx + 1
                    ));
                }
            }
        }
    }
    if !saw_gated_fixture_def {
        violations.push(
            "wyrd-core/src/node.rs names no test-fixtures-gated put_file/remove: \
             the confinement pin is vacuous without its fixture witness"
                .to_owned(),
        );
    }
    violations
}

/// Contract (F2), source half: within `node.rs` and `live/`, no
/// production authoring sits outside the live pass or a gated fixture.
/// `WyrdNode::put_file` and `remove` survive only as
/// `test-fixtures`-gated bodies in `node.rs`; the loop authors only
/// through `live/apply.rs`. A new second authority inside that scope —
/// a renamed direct-write entry, an ungated helper, a `LiveNode`
/// shortcut around the queue — fails here before it ships beside the
/// quota/fold/staleness channel. The manifest half above keeps the
/// gate itself test-only; this is the source check that makes the
/// retirement a gate rather than a note.
#[test]
fn node_authoring_stays_in_the_live_pass_or_fixtures() {
    let root = workspace_root();
    // The whole `wyrd-core` source tree, not a hardcoded file list: a
    // new submodule inside `live/` cannot evade the pin by name. The
    // checker itself scopes to `node.rs` plus `live/` (test files out
    // of scope), so production files elsewhere ride along unchecked —
    // confinement outside `node.rs`/`live/` rests on the engine owning
    // the only authoring methods, not on this scan. Paths stay
    // relative to the workspace root to match the checker's
    // `wyrd-core/src/...` shapes.
    let (files, errors) = collect_rs_files(&root.join("crates/wyrd-core/src"), &root);
    assert!(
        errors.is_empty(),
        "the source walk must read cleanly: {}",
        errors.join("; ")
    );
    assert!(!files.is_empty(), "the node surface holds sources to check");
    assert!(
        files
            .iter()
            .any(|(p, _)| p.ends_with("wyrd-core/src/node.rs")),
        "node.rs must be in the walked set"
    );
    assert!(
        files.iter().any(|(p, _)| p.contains("wyrd-core/src/live/")),
        "live/ must be in the walked set"
    );
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let violations = check_node_authoring_confined(&borrowed);
    assert!(
        violations.is_empty(),
        "mutation-authority violations:\n- {}",
        violations.join("\n- ")
    );
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::kdf_release_contracts::check_release_build_args;
    fn fixtures_manifest(manifest: &str) -> toml::Table {
        manifest.parse().unwrap()
    }

    #[test]
    fn prod_fixtures_edge_is_rejected() {
        let violations = check_fixtures_feature_edges(
            "wyrd-daemon",
            &fixtures_manifest(
                r#"
                [dependencies]
                wyrd-core = { workspace = true, features = ["test-fixtures"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-daemon"));
        assert!(violations[0].contains("wyrd-core"));
    }

    #[test]
    fn target_scoped_prod_fixtures_edge_is_rejected() {
        // A `test-fixtures` edge under `[target.*.dependencies]` is a
        // production edge: the walker descends through `target`
        // nesting, so only `[dev-dependencies]` (at any depth) stays
        // allowed.
        let violations = check_fixtures_feature_edges(
            "wyrd-daemon",
            &fixtures_manifest(
                r#"
                [target.'cfg(unix)'.dependencies]
                wyrd-core = { workspace = true, features = ["test-fixtures"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-daemon"));
        assert!(violations[0].contains("wyrd-core"));
    }

    #[test]
    fn dev_fixtures_edge_is_allowed() {
        // The four test suites author setup state through the gated
        // fixtures; the dev-dep edge is the sanctioned shape.
        let violations = check_fixtures_feature_edges(
            "wyrd-daemon",
            &fixtures_manifest(
                r#"
                [dev-dependencies]
                wyrd-core = { workspace = true, features = ["test-fixtures"] }
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "dev-dep fixture edges must stay allowed: {violations:?}"
        );
    }

    #[test]
    fn target_scoped_dev_fixtures_edge_is_allowed() {
        // `dev` propagates through `[target.*]` nesting exactly like
        // production sections do: a target-scoped dev-dep edge is still
        // a dev edge, never a shipped enablement.
        let violations = check_fixtures_feature_edges(
            "wyrd-daemon",
            &fixtures_manifest(
                r#"
                [target.'cfg(unix)'.dev-dependencies]
                wyrd-core = { workspace = true, features = ["test-fixtures"] }
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "target-scoped dev-dep fixture edges must stay allowed: {violations:?}"
        );
    }

    #[test]
    fn fixtures_feature_definition_is_not_an_edge() {
        // `wyrd-core` declares the feature itself; the declaration is
        // not an enablement and must not trip the checker.
        let violations = check_fixtures_feature_edges(
            "wyrd-core",
            &fixtures_manifest(
                r#"
                [features]
                test-fixtures = []
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "feature definition must not read as an edge: {violations:?}"
        );
    }

    #[test]
    fn dep_slash_fixtures_edge_is_rejected() {
        // The `dep/token` form on an edge names the token without the
        // literal string standing alone.
        let violations = check_fixtures_feature_edges(
            "wyrd-cli",
            &fixtures_manifest(
                r#"
                [dependencies]
                wyrd-core = { workspace = true, features = ["wyrd-core/test-fixtures"] }
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-cli"));
    }

    #[test]
    fn default_feature_enabling_fixtures_is_rejected() {
        let violations = check_feature_default_enables_fixtures(
            "wyrd-core",
            &fixtures_manifest(
                r#"
                [features]
                default = ["test-fixtures"]
                test-fixtures = []
            "#,
            ),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-core"));
    }

    #[test]
    fn default_feature_without_fixtures_is_allowed() {
        let violations = check_feature_default_enables_fixtures(
            "wyrd-core",
            &fixtures_manifest(
                r#"
                [features]
                default = ["std"]
                test-fixtures = []
            "#,
            ),
        );
        assert!(
            violations.is_empty(),
            "unrelated defaults must stay allowed: {violations:?}"
        );
    }

    #[test]
    fn flake_fixtures_feature_is_rejected() {
        let violations = check_release_build_args(
            "cargoExtraArgs = \"-p wyrd-cli --features test-fixtures\";\n",
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("test-fixtures"));
    }

    #[test]
    fn ungated_direct_write_entry_is_rejected() {
        // A `pub fn put_file` without the fixtures gate is the retired
        // authority returning: the pin must name it.
        let files = [(
            "crates/wyrd-core/src/node.rs",
            "impl<V> WyrdNode<V> {\n    pub fn put_file(&mut self) {\n        let a = engine.author_pending_write(&store, root, pending);\n    }\n}",
        )];
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, *t)).collect();
        let violations = check_node_authoring_confined(&borrowed);
        assert!(
            violations.iter().any(|v| v.contains("put_file")),
            "the ungated entry must fail: {violations:?}"
        );
    }

    #[test]
    fn gated_fixture_body_passes() {
        // The sanctioned shape: gated definitions authoring inside
        // their own bodies, nothing else.
        let files = [(
            "crates/wyrd-core/src/node.rs",
            "impl<V> WyrdNode<V> {\n    /// docs\n    #[cfg(feature = \"test-fixtures\")]\n    pub fn put_file(&mut self) {\n        let a = engine.author_pending_write(&store, root, pending);\n    }\n    /// docs\n    #[cfg(feature = \"test-fixtures\")]\n    pub fn remove(&mut self) {\n        let b = engine.author_pending_write(&store, root, pending);\n    }\n    pub fn view(&self) {}\n}",
        )];
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, *t)).collect();
        let violations = check_node_authoring_confined(&borrowed);
        assert!(
            violations.is_empty(),
            "the gated fixture shape must pass: {violations:?}"
        );
    }

    #[test]
    fn renamed_ungated_authoring_entry_is_rejected() {
        // Renaming the entry evades the name check but not the
        // authoring confinement: the call's block is ungated.
        let files = [(
            "crates/wyrd-core/src/node.rs",
            "impl<V> WyrdNode<V> {\n    /// docs\n    #[cfg(feature = \"test-fixtures\")]\n    pub fn put_file(&mut self) {}\n    pub fn write_file(&mut self) {\n        let a = engine.author_pending_write(&store, root, pending);\n    }\n}",
        )];
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, *t)).collect();
        let violations = check_node_authoring_confined(&borrowed);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("outside a test-fixtures-gated"));
    }

    #[test]
    fn authoring_outside_apply_is_rejected() {
        // A `LiveNode` shortcut around the queue: authoring in
        // `live/pass.rs` is outside the traced seam.
        let files = [
            (
                "crates/wyrd-core/src/node.rs",
                "impl<V> WyrdNode<V> {\n    /// docs\n    #[cfg(feature = \"test-fixtures\")]\n    pub fn put_file(&mut self) {}\n}",
            ),
            (
                "crates/wyrd-core/src/live/pass.rs",
                "impl<V> LiveNode<V> {\n    pub fn sync_once(&mut self) {\n        let s = engine.author_snapshot(store, root);\n    }\n}",
            ),
        ];
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, *t)).collect();
        let violations = check_node_authoring_confined(&borrowed);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("live/apply.rs"));
    }

    #[test]
    fn authoring_in_apply_passes() {
        // The traced seam itself: the loop's single production
        // authority stays allowed.
        let files = [
            (
                "crates/wyrd-core/src/node.rs",
                "impl<V> WyrdNode<V> {\n    /// docs\n    #[cfg(feature = \"test-fixtures\")]\n    pub fn put_file(&mut self) {}\n}",
            ),
            (
                "crates/wyrd-core/src/live/apply.rs",
                "impl<V> LiveNode<V> {\n    fn author_traced() {\n        let s = engine.author_snapshot(store, root);\n    }\n}",
            ),
        ];
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, *t)).collect();
        let violations = check_node_authoring_confined(&borrowed);
        assert!(
            violations.is_empty(),
            "the traced seam must pass: {violations:?}"
        );
    }

    #[test]
    fn fn_def_matches_any_visibility_and_modifier_shape() {
        // The retirement pin attributes authoring calls to the
        // enclosing function; a future modifier shape must resolve to
        // its own definition, never the previous one.
        for line in [
            "fn plain() {",
            "pub fn public() {",
            "pub(crate) fn scoped() {",
            "pub(super) fn parent() {",
            "pub(in crate::live) fn area() {",
            "async fn async_plain() {",
            "pub async fn async_public() {",
            "pub(crate) async fn async_scoped() {",
            "pub unsafe fn unsafe_public() {",
            "pub const fn const_public() {",
            "unsafe extern fn foreign() {",
            "pub extern \"C\" fn foreign_c() {",
            "unsafe extern \"C-unwind\" fn foreign_unwind() {",
            "default fn specialized() {",
        ] {
            assert!(is_fn_def(line), "{line:?} is a definition");
        }
        for line in [
            "let f: fn(u32) = g;",
            "let cb: Box<dyn Fn()> = x;",
            "    // fn commented() {",
            "fn_call();",
            "    -> fn() {",
        ] {
            assert!(!is_fn_def(line), "{line:?} is not a definition");
        }
    }

    #[test]
    fn test_fixture_files_are_out_of_scope() {
        // Fixture setup authors through `Engine` directly; the rule
        // pins the production surface, not its tests. The node stub
        // rides along so the pin's liveness witness stays satisfied.
        let files = [
            (
                "crates/wyrd-core/src/node.rs",
                "impl<V> WyrdNode<V> {\n    /// docs\n    #[cfg(feature = \"test-fixtures\")]\n    pub fn put_file(&mut self) {}\n}",
            ),
            (
                "crates/wyrd-core/src/live/tests_pause.rs",
                "#[test]\nfn x() {\n    engine.author_snapshot(&store, root).unwrap();\n}",
            ),
        ];
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, *t)).collect();
        assert!(
            check_node_authoring_confined(&borrowed).is_empty(),
            "test files must stay out of scope"
        );
    }
}
