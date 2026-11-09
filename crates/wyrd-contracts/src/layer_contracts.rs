//! The workspace dependency DAG is a load-bearing architectural invariant:
//! layers depend downward only (`wyrd-format → wyrd-sync → wyrd-core →
//! presentation`), so no backward edge can be smuggled in while staying
//! green. This module checks every member's
//! `[dependencies]` against the declared policy and fails closed on any
//! crate or edge the policy does not know: a new member, a new edge, or
//! a new external dependency is a deliberate policy edit, never an
//! accident.
//!
//! Only production `[dependencies]` are checked. `[dev-dependencies]`
//! stay exempt on purpose: they never ship, and test harnesses
//! legitimately reach further (in-process relays, fast-KDF profiles).
//!
//! Dependencies are resolved to effective package names before any rule
//! applies: a manifest alias (`transport = { package = "wyrd-daemon" }`)
//! is the same edge as the plain name, and a `workspace = true`
//! inheritance consults the workspace table for renames. The manifest
//! key is kept for diagnostics only.
//!
//! One rule needs source, not manifests, and lives here as well:
//! `nostr` use inside `wyrd-core` must stay inside one subsystem
//! directory (the mailbox decision), because manifests cannot scope a
//! dependency to a module. The scanner matches `use nostr`, bare
//! `nostr::` paths, and `extern crate nostr` as seen through a small
//! comment/string-stripping lexer (line and nestable block comments,
//! cooked, byte, and raw strings, char literals) — enough grammar for
//! a convention check, with the residual edge cases named at
//! `code_text`.
//!
//! A second rule needs the transitive closure, not the direct edges,
//! and lives here as well: `wyrd-view`'s production-edge closure must
//! reach neither `wyrd-sync` nor any `iroh*` package. Only the view
//! crate is walked — `wyrd-daemon` and `wyrd-cli` must link the
//! transport, so the gap is load-bearing only here (mobile surfaces
//! mount no FUSE; they build on the same provider-neutral view). The
//! direct check above cannot see reachability — `wyrd-view → wyrd-core
//! → wyrd-sync` is three allowed edges that link iroh into the view
//! crate. Manifests see member-declared edges only: a third-party
//! crate's own transitive dependencies are invisible here, so this
//! pins the exact property that no member-declared path leads to the
//! transport, not a full link-graph audit (that remains `cargo tree`
//! territory). The closure treats an unresolvable edge as a leaf, and
//! may: the direct check already fails closed on any edge outside the
//! exact allow-lists, so leniency here cannot hide a new edge.
//!
//! A third rule pins the verification-proof crossing count (`check_proof_mint`):
//! the unsafe token constructor appears in code exactly twice — its
//! definition and the verification authority's single call — so the
//! audit marker stays honest without review vigilance.
//!
//! A fourth rule pins the release KDF posture (`check_kdf_feature_edges`,
//! `check_release_build_args`): `insecure-fast-kdf` may only appear in
//! `[dev-dependencies]` and never in the flake's distribution-build
//! args, so no shipped artifact derives through the 8 MiB / t=1 table.
//! Manifests see member-declared edges only, so the resolved-graph
//! backstop stays in CI (`cargo tree -e normal,features` in
//! workflow.yml); this pins the declared intent where the PR gate runs
//! it.
//!
//! A fifth rule pins the retired mutation authority
//! (`check_fixtures_feature_edges`, `check_node_authoring_confined`):
//! `WyrdNode::put_file`/`remove` survive only behind the
//! `test-fixtures` feature, enabled in `[dev-dependencies]` alone, and
//! snapshot authoring in `wyrd-core` stays confined to the live pass
//! (`live/apply.rs`) plus those two gated fixture bodies — so a new
//! public mutation entry point with weaker invariants fails the suite
//! instead of shipping beside the queue.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// A normal `[dependencies]` edge set for one member: manifest key to
/// the raw value, so aliases resolve through the `package` field.
type DepSet = BTreeMap<String, toml::Value>;

/// Policy for one workspace member: the exact `wyrd-*` crates it may
/// depend on, and the exact third-party crates it may use. Anything
/// else fails closed.
struct MemberPolicy {
    workspace_allow: &'static [&'static str],
    external_allow: &'static [&'static str],
}

fn policy_for(member: &str) -> Option<MemberPolicy> {
    match member {
        // Hard rule from AGENTS.md: plaintext world, closed substrate.
        "wyrd-format" => Some(MemberPolicy {
            workspace_allow: &[],
            external_allow: &["blake3", "hex", "thiserror", "fastcdc", "serde"],
        }),
        // Protocol machinery: format below, a pinned third-party set,
        // nothing upward or sideways. `tracing` is the structured
        // forensics facade (per-pass sync counts, intake verdicts,
        // outbox sends) — events only, no subscriber, no I/O. `libc`
        // is the custody boundary only (`O_NOFOLLOW` directory opens
        // and the euid ownership check in `durable::store`, mirroring
        // the CLI's credential-file hardening) — no transport, no
        // process control, no other call sites.
        "wyrd-sync" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format", "wyrd-namespace"],
            external_allow: &[
                "iroh",
                "iroh-blobs",
                "bao-tree",
                "bytes",
                "n0-future",
                "nostr",
                "secp256k1",
                "chacha20poly1305",
                "hkdf",
                "sha2",
                "argon2",
                "getrandom",
                "blake3",
                "tokio",
                "thiserror",
                "tracing",
                "zeroize",
                "libc",
            ],
        }),
        // The embeddable node: view, sync, and format below; the mailbox
        // subsystem's async and control-plane crates; never presentation
        // (fuser), argument parsing (clap), POSIX errno mapping (libc),
        // or process-signal handling. `tracing` is the
        // structured forensics facade (per-pass sync counts) — events
        // only, no subscriber, no I/O.
        "wyrd-core" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format", "wyrd-namespace", "wyrd-sync"],
            external_allow: &[
                "futures-util",
                "getrandom",
                "nostr",
                "nostr-sdk",
                "thiserror",
                "tokio",
                "tracing",
            ],
        }),
        // The mount-free view: format and the provider-neutral model
        // below, nothing else. No edge on `wyrd-core` by design — the
        // view links no transport, not even transitively (checked by
        // `view_link_graph_excludes_sync_and_iroh` below).
        "wyrd-view" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format", "wyrd-namespace"],
            external_allow: &["thiserror"],
        }),
        // The provider-neutral namespace model: format below, error
        // reporting only. No sync edge by design — naming verified
        // state must never link the verifier.
        "wyrd-namespace" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format"],
            external_allow: &["thiserror"],
        }),
        // The composer and process host: everything below, never the CLI
        // (presentation depends on the host's public surface, not the
        // reverse) and never the contract suite. The posture sidecar
        // (22c) lives here because every composer writes it and every
        // lock-refused starter reads it — `serde`/`serde_json` are the
        // sidecar schema and encoding, nothing else may use them.
        "wyrd-daemon" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format", "wyrd-sync", "wyrd-view", "wyrd-core"],
            external_allow: &[
                "fuser",
                "futures-util",
                "getrandom",
                "hex",
                "libc",
                "clap",
                "nostr",
                "nostr-sdk",
                "serde",
                "serde_json",
                "thiserror",
                "tokio",
                "tracing",
                "tracing-subscriber",
                "tracing-log",
                "zeroize",
            ],
        }),
        // The `wyrd` process host: argument parsing, credential
        // files, mount orchestration, diagnostics, and exit codes over
        // the daemon's public surface. It mounts through the FUSE
        // adapter, so the host crates (fuser, libc, tracing) and the
        // credential/control-plane crates (hex, nostr) are its own —
        // never the sync transport internals or the contract suite.
        // The posture sidecar (22c) adds three deliberate edges, all
        // confined to the binary: `serde`/`serde_json` for the
        // schema-versioned sidecar file, `getrandom` for the fresh
        // per-process instance id. `wyrd update` (29) adds `blake3`:
        // the artifact digest that names the archive in reports and
        // the transaction log — an identity, never trust. The drive
        // deployment configuration (27) adds `toml`: strict parsing
        // of the relay list and credential paths from `default.toml`
        // — parse-only, the file holds no secrets. No other
        // crate may follow.
        "wyrd-cli" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format", "wyrd-sync", "wyrd-core", "wyrd-daemon"],
            external_allow: &[
                "blake3",
                "clap",
                "fuser",
                "getrandom",
                "hex",
                "libc",
                "nostr",
                "serde",
                "serde_json",
                "thiserror",
                "toml",
                "tracing",
                "tracing-subscriber",
                "tracing-log",
                "zeroize",
            ],
        }),
        // The suite itself: leaf, depends on every crate, and the
        // reverse edge is checked separately below. No third-party
        // production deps today; any is a deliberate edit.
        "wyrd-contracts" => Some(MemberPolicy {
            workspace_allow: &[
                "wyrd-format",
                "wyrd-sync",
                "wyrd-view",
                "wyrd-core",
                "wyrd-daemon",
                "wyrd-cli",
            ],
            // The suite itself names `fuser::Errno` in its
            // mount-coupled tests, behind the default-on `mount`
            // feature (see the `08` libfuse gate): the edge is in
            // `[dependencies]` because Cargo forbids optional
            // dev-dependencies, and the checker reads it statically
            // either way.
            external_allow: &["fuser"],
        }),
        // The embedder-shaped consumer: everything the embedded
        // lifecycle needs (format, transport, node, the mount-free
        // view) and nothing presentation-shaped — no daemon (which
        // carries `fuser`), no CLI (which carries `clap`/`libc`), no
        // contract suite. The transitive half of the boundary is
        // `consumer_link_graph_excludes_presentation`.
        "wyrd-embed" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format", "wyrd-sync", "wyrd-view", "wyrd-core"],
            external_allow: &[],
        }),
        // The desktop host: the owner process. It opens the store
        // through `wyrd-sync`'s durable side only (`durable`,
        // `runtime::Engine`, `keys` — never `transport`, `bulk`, or
        // `serving`; that half is pinned by the host composition
        // check), composes the node through `wyrd-core`, and
        // supervises and tears down through `wyrd-daemon`. `hex` is
        // the serving-id rendering, `libc` the Unix signal arming,
        // `tracing` the stage-log facade. A GUI toolkit joins this
        // set only under OD-06-C's exclusion rule.
        "wyrd-host" => Some(MemberPolicy {
            workspace_allow: &["wyrd-format", "wyrd-sync", "wyrd-core", "wyrd-daemon"],
            external_allow: &["hex", "libc", "thiserror", "tracing"],
        }),
        _ => None,
    }
}

/// Effective package name for one manifest entry: the `package` rename
/// wins; a `workspace = true` inheritance consults the workspace table
/// for the rename; otherwise the key is the name.
fn effective_name(key: &str, value: &toml::Value, workspace_deps: &toml::Table) -> String {
    if let Some(table) = value.as_table() {
        if let Some(package) = table.get("package").and_then(|p| p.as_str()) {
            return package.to_owned();
        }
        if table.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
            if let Some(renamed) = workspace_deps
                .get(key)
                .and_then(|w| w.as_table())
                .and_then(|w| w.get("package"))
                .and_then(|p| p.as_str())
            {
                return renamed.to_owned();
            }
        }
    }
    key.to_owned()
}

/// Check one member's production deps against its policy. Pure over the
/// edge sets so the negative tests below can prove the checker bites
/// without touching the filesystem.
fn check_member(member: &str, deps: &DepSet, workspace_deps: &toml::Table) -> Vec<String> {
    let Some(policy) = policy_for(member) else {
        return vec![format!(
            "workspace member `{member}` is not in the dependency-DAG policy: \
             add it deliberately in `layer_contracts.rs`, do not work around this test"
        )];
    };
    let workspace_allowed: BTreeSet<&str> = policy.workspace_allow.iter().copied().collect();
    let external_allowed: BTreeSet<&str> = policy.external_allow.iter().copied().collect();
    let mut violations = Vec::new();
    for (key, value) in deps {
        let name = effective_name(key, value, workspace_deps);
        if name == "wyrd-contracts" && member != "wyrd-contracts" {
            violations.push(format!(
                "`{member}` depends on `wyrd-contracts` (as `{key}`): the suite is a \
                 leaf, nothing depends on it"
            ));
            continue;
        }
        if name.starts_with("wyrd-") {
            if !workspace_allowed.contains(name.as_str()) {
                violations.push(format!(
                    "`{member}` depends on `{name}` (as `{key}`), outside its allowed set \
                     `{workspace_allowed:?}`: point the edge downward or amend the policy"
                ));
            }
            continue;
        }
        if !external_allowed.contains(name.as_str()) {
            violations.push(format!(
                "`{member}` depends on `{name}` (as `{key}`), outside its allowed \
                 third-party set `{external_allowed:?}`: amend the policy deliberately \
                 or drop the edge"
            ));
        }
    }
    violations
}

/// Collect production `[dependencies]` entries from a member manifest.
/// Any table named exactly `dependencies` counts (covers future
/// `[target.*.dependencies]`); `dev-dependencies` never does.
fn production_deps(manifest: &toml::Table) -> DepSet {
    let mut deps = DepSet::new();
    fn walk(table: &toml::Table, deps: &mut DepSet) {
        for (key, value) in table {
            if key == "dependencies" {
                if let Some(table) = value.as_table() {
                    deps.extend(table.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            } else if let Some(inner) = value.as_table() {
                walk(inner, deps);
            }
        }
    }
    walk(manifest, &mut deps);
    deps
}

/// Collect `[dev-dependencies]` entries from a member manifest. Any
/// table named exactly `dev-dependencies` counts (covers future
/// `[target.*.dev-dependencies]`); production deps never do.
fn dev_deps(manifest: &toml::Table) -> DepSet {
    let mut deps = DepSet::new();
    fn walk(table: &toml::Table, deps: &mut DepSet) {
        for (key, value) in table {
            if key == "dev-dependencies" {
                if let Some(table) = value.as_table() {
                    deps.extend(table.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            } else if let Some(inner) = value.as_table() {
                walk(inner, deps);
            }
        }
    }
    walk(manifest, &mut deps);
    deps
}

/// `wyrd-view`'s test-only edge set, pinned exactly: the view tests
/// build export fixtures against `wyrd-core` and nothing else. The
/// direct check exempts dev-dependencies by design, so this pin is
/// what makes a widening deliberate instead of silent. Pure over the
/// edge set so the negative test proves it bites.
fn check_view_dev_deps(deps: &DepSet, workspace_deps: &toml::Table) -> Vec<String> {
    let names: BTreeSet<String> = deps
        .iter()
        .map(|(key, value)| effective_name(key, value, workspace_deps))
        .collect();
    if names != BTreeSet::from(["wyrd-core".to_owned()]) {
        return vec![format!(
            "`wyrd-view` dev-dependencies drifted to `{names:?}` (want exactly \
             `wyrd-core` for export fixtures): widen deliberately here, never silently"
        )];
    }
    Vec::new()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read_manifest(dir: &Path) -> toml::Table {
    let text = fs::read_to_string(dir.join("Cargo.toml")).expect("member manifest is readable");
    text.parse().expect("member manifest parses as TOML")
}

fn workspace_dep_table(root_manifest: &toml::Table) -> toml::Table {
    root_manifest
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
        .cloned()
        .unwrap_or_default()
}

/// Strip comments and string/char literals from whole file text,
/// preserving newlines so per-line matching below stays meaningful.
/// Handles `//` anywhere outside literals, nestable `/* */` block
/// comments, `"..."` with escapes, `b"..."`, raw `r"..."`/`r#"..."#`
/// forms, and `'x'` char literals (a `'` skips to its line's closing
/// quote; lifetimes without one pass through untouched). A small lexer
/// for a convention check, not a grammar: pathological nesting of
/// quotes inside lifetimes is out of scope.
fn code_text(text: &str) -> String {
    let mut code = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Line comment: `//` in normal state runs to (not past) `\n`.
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                code.push(' ');
                i += 1;
            }
            continue;
        }
        // Block comment, nestable per the Rust grammar.
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 1;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                if chars[i] == '\n' {
                    code.push('\n');
                } else {
                    code.push(' ');
                }
                i += 1;
            }
            continue;
        }
        // Raw string: `r"..."`, `r#"..."#`, ... (and `br` variants).
        if (c == 'r' || (c == 'b' && chars.get(i + 1) == Some(&'r'))) && {
            let mut j = if c == 'b' { i + 1 } else { i };
            j += 1;
            while chars.get(j) == Some(&'#') {
                j += 1;
            }
            chars.get(j) == Some(&'"')
        } {
            let mut hashes = 0;
            let mut j = if c == 'b' { i + 2 } else { i + 1 };
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            j += 1; // opening quote
            loop {
                if j >= chars.len() {
                    i = j;
                    break;
                }
                if chars[j] == '"' {
                    let mut k = j + 1;
                    let mut seen = 0;
                    while seen < hashes && chars.get(k) == Some(&'#') {
                        seen += 1;
                        k += 1;
                    }
                    if seen == hashes {
                        j = k;
                        i = j;
                        break;
                    }
                }
                if chars[j] == '\n' {
                    code.push('\n');
                }
                j += 1;
            }
            continue;
        }
        // Ordinary or byte string.
        if c == '"' || (c == 'b' && chars.get(i + 1) == Some(&'"')) {
            i += if c == 'b' { 2 } else { 1 };
            while i < chars.len() && chars[i] != '"' && chars[i] != '\n' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += usize::from(i < chars.len() && chars[i] == '"');
            continue;
        }
        // Char literal: skip to the line's closing quote when there is
        // one; otherwise this is a lifetime tick, pass it through.
        if c == '\'' {
            let rest: String = chars[i + 1..].iter().collect();
            let closes = rest.find('\'').is_some_and(|p| !rest[..p].contains('\n'));
            if closes {
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            code.push(c);
            i += 1;
            continue;
        }
        code.push(c);
        i += 1;
    }
    code
}

/// Does one stripped source line use a `nostr*` crate.
fn line_uses_nostr(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("use nostr")
        || trimmed.starts_with("extern crate nostr")
        || line.contains("nostr::")
}

/// Subsystem directories (top-level `src/` children) holding `nostr*`
/// use, from an explicit file list. Pure so tests can pin the scanner
/// without touching the filesystem.
fn nostr_holding_subsystems(files: &[(&str, &str)]) -> BTreeSet<String> {
    files
        .iter()
        .filter(|(_, text)| code_text(text).lines().any(line_uses_nostr))
        .filter_map(|(path, _)| {
            Path::new(path)
                .components()
                .nth(1)
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
        })
        .collect()
}

/// Collect every `.rs` file under `dir`, as paths relative to
/// `root`. Build output (`target/`) is never descended into. Fails
/// closed: unreadable entries are errors, never silent skips. An
/// empty collection is returned, not failed, here — an empty walk
/// proves nothing, so each rule names its own non-vacuity verdict.
fn collect_rs_files(dir: &Path, root: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, String)>, errors: &mut Vec<String>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                errors.push(format!("cannot read directory `{}`: {err}", dir.display()));
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    errors.push(format!("cannot list entry in `{}`: {err}", dir.display()));
                    continue;
                }
            };
            let path = entry.path();
            if path.is_dir() {
                // Never descend into build output.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&path, root, files, errors);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                match fs::read_to_string(&path) {
                    Ok(text) => files.push((rel, text)),
                    Err(err) => {
                        errors.push(format!("cannot read file `{}`: {err}", path.display()));
                    }
                }
            }
        }
    }
    walk(dir, root, &mut files, &mut errors);
    files.sort();
    (files, errors)
}

/// `nostr*` use inside `wyrd-core` must stay within a single top-level
/// `src/` subsystem directory (the mailbox decision: control-plane
/// framing lives with sync control, never ambient across the node).
/// The walk fails closed: unreadable directories and files are
/// violations naming the path (never silent skips), and a walk that
/// examines no `.rs` files at all fails too — an empty file set
/// proves nothing, so "no `nostr*` references" and "nothing examined"
/// are different verdicts.
fn check_core_nostr_scope(core_dir: &Path) -> Vec<String> {
    let src = core_dir.join("src");
    // Paths stay relative to the crate dir, so the subsystem is
    // always at component index 1, matching the test convention.
    let (files, errors) = collect_rs_files(&src, core_dir);
    if !errors.is_empty() {
        return errors;
    }
    if files.is_empty() {
        return vec![format!(
            "`nostr*` scope check examined no `.rs` files under `{}`: \
             an empty walk proves nothing, failing closed",
            src.display()
        )];
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let subsystems = nostr_holding_subsystems(&refs);
    if subsystems.len() > 1 {
        vec![format!(
            "`nostr*` use in `wyrd-core` spans subsystems {subsystems:?}: \
             keep it inside the mailbox subsystem"
        )]
    } else {
        Vec::new()
    }
}

/// The token definition site: the one file allowed to hold the
/// unsafe constructor in code. Every other code occurrence is a mint
/// and fails below.
const PROOF_DEFINITION: &str = "crates/wyrd-namespace/src/view.rs";

/// The verification authority: the one file allowed to call the
/// unsafe constructor in code (after the BIP-340 check).
const PROOF_AUTHORITY: &str = "crates/wyrd-sync/src/durable/mod.rs";

/// Files holding code (not comment/string) occurrences of the proof
/// token's unsafe constructor, from an explicit file list: path to
/// occurrence count. Counts, not presence flags, so a second mint in
/// the same file is visible. Pure so tests can pin the rule without
/// touching the filesystem.
fn mint_holding_files(files: &[(&str, &str)]) -> Vec<(String, usize)> {
    let mut holders: Vec<(String, usize)> = files
        .iter()
        .map(|(path, text)| {
            (
                path.to_string(),
                code_text(text).matches("from_verified_unchecked").count(),
            )
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    holders.sort();
    holders
}

/// The `AuthorizedSnapshot` crossing count: the unsafe constructor
/// must appear in code exactly twice workspace-wide — its definition
/// and the verification authority's single call — so the `#[allow]`
/// audit marker stays honest without review vigilance. Any new mint
/// (production or test) fails here. Pure over the holder list so the
/// negative tests prove it bites.
fn check_proof_mint(holders: &[(String, usize)]) -> Vec<String> {
    let mut violations = Vec::new();
    let definitions: usize = holders
        .iter()
        .filter(|(holder, _)| holder.as_str() == PROOF_DEFINITION)
        .map(|(_, count)| count)
        .sum();
    if definitions != 1 {
        violations.push(format!(
            "the proof token definition `{PROOF_DEFINITION}` holds {definitions} code \
             occurrences of the unsafe constructor (want exactly the definition): \
             the mint moved or duplicated"
        ));
    }
    let mut unexpected: Vec<(&String, usize)> = holders
        .iter()
        .filter(|(holder, _)| {
            holder.as_str() != PROOF_DEFINITION && holder.as_str() != PROOF_AUTHORITY
        })
        .map(|(holder, count)| (holder, *count))
        .collect();
    unexpected.sort();
    for (holder, count) in unexpected {
        violations.push(format!(
            "`{holder}` mints the verification-proof token {count} time(s) outside the \
             verification authority: route through `AuthorizeSnapshot::authorize` or amend \
             this rule deliberately"
        ));
    }
    let authority: usize = holders
        .iter()
        .filter(|(holder, _)| holder.as_str() == PROOF_AUTHORITY)
        .map(|(_, count)| count)
        .sum();
    if authority != 1 {
        violations.push(format!(
            "the verification authority `{PROOF_AUTHORITY}` holds {authority} code \
             occurrences of the unsafe constructor (want exactly the single crossing): \
             the authority stopped minting or mints twice"
        ));
    }
    violations
}

/// Contract 34, mint-count half: the verification-proof token's
/// unsafe constructor appears in code exactly twice — the definition
/// in `wyrd-namespace` and sync's single authorization crossing — so
/// "verified state" cannot be minted anywhere else without the suite
/// failing.
#[test]
fn verification_proof_has_one_mint() {
    let root = workspace_root();
    let (files, errors) = collect_rs_files(&root.join("crates"), &root);
    assert!(
        errors.is_empty(),
        "source walk errors:\n- {}",
        errors.join("\n- ")
    );
    // Never pass vacuously: an empty file set would check nothing.
    assert!(!files.is_empty(), "workspace holds no `.rs` files");
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let violations = check_proof_mint(&mint_holding_files(&refs));
    assert!(
        violations.is_empty(),
        "proof-mint violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract 34 (packaging half): the distribution build compiles the
/// user-facing binary from `wyrd-cli`. The Phase 4 move broke this
/// once (the flake built `-p wyrd-daemon`, which installs no binary,
/// and `wyrd-dist` failed packing `bin/wyrd`), so it is asserted, not
/// remembered. A structural guard over the flake text — not a
/// substitute for a release `nix build` — so it matches every
/// `cargoExtraArgs` line rather than trusting line layout, and fails
/// closed if the flag ever names another package.
#[test]
fn flake_builds_the_binary_from_wyrd_cli() {
    let text =
        fs::read_to_string(workspace_root().join("flake.nix")).expect("flake.nix is readable");
    let args: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("cargoExtraArgs"))
        .collect();
    assert!(
        !args.is_empty(),
        "flake names no binary source package at all"
    );
    assert!(
        args.iter().any(|line| line.contains("-p wyrd-cli")),
        "distribution must build the binary from wyrd-cli, found: {args:?}"
    );
    assert!(
        !args.iter().any(|line| line.contains("-p wyrd-daemon")),
        "distribution must not build the binary from the library-only daemon: {args:?}"
    );
}

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

/// Service supervision (34): one declaration names exactly one
/// owner per drive, and the supervisor contract distinguishes the
/// terminal lock code from restart-worthy ends on both platforms.
/// A structural pin over the module sources, the wrapper, and the
/// probe — the evaluation tests in `tests/harness/test-wyrd-module.sh`
/// prove the generated units; this proves the sources keep the
/// load-bearing clauses. Most load-bearing: the wrapper's terminal
/// code and the CLI's drive-locked code are the same number, read
/// from both files, so neither side can drift alone.
#[test]
fn the_nix_module_declares_exactly_one_owner_per_drive() {
    let root = workspace_root();
    let options = fs::read_to_string(root.join("nix/drive/options.nix")).expect("options readable");
    let nixos =
        fs::read_to_string(root.join("nix/drive/nixos.nix")).expect("nixos module readable");
    let darwin =
        fs::read_to_string(root.join("nix/drive/darwin.nix")).expect("darwin module readable");
    let wrapper =
        fs::read_to_string(root.join("nix/drive/supervise.py")).expect("wrapper readable");
    let probe = fs::read_to_string(root.join("nix/drive/probe.py")).expect("probe readable");
    let cli = fs::read_to_string(root.join("crates/wyrd-cli/src/main.rs")).expect("cli readable");

    // One mode per declaration is structural (an enum); two
    // declarations for one drive are refused at eval with the drive
    // path in the message, and secrets never enter the store.
    for marker in [
        "One owner process per drive directory",
        "duplicateDirs",
        "Secrets are paths, never store strings",
        "storeSecrets",
        "mountpoint belongs to mount mode",
        "belongs to vault mode",
    ] {
        assert!(
            options.contains(marker),
            "the options module lost {marker:?}"
        );
    }
    // The terminal code is one number on both sides of the boundary.
    let cli_code = cli
        .lines()
        .find_map(|line| {
            let rest = line
                .trim()
                .strip_prefix("pub const EXIT_DRIVE_LOCKED: i32 = ")?;
            rest.strip_suffix(';')
        })
        .expect("the CLI names its drive-locked code");
    let wrap_code = wrapper
        .lines()
        .find_map(|line| line.trim().strip_prefix("DRIVE_LOCKED_CODE = "))
        .expect("the wrapper names its terminal code");
    assert_eq!(
        cli_code, wrap_code,
        "the wrapper's terminal code must be the CLI's drive-locked code"
    );
    assert!(
        wrapper.contains("WIFEXITED") && wrapper.contains("WIFSIGNALED"),
        "the wrapper maps the wait status, not the exit code alone"
    );
    // systemd restarts failures rate-limited, never the lock code;
    // the probe refuses the switch on a foreign holder.
    for marker in [
        "RestartPreventExitStatus",
        "78",
        "Restart = \"on-failure\"",
        "startLimitBurst",
        "wyrd-drive-probe",
        "--restart-status=1",
        "MainPID",
    ] {
        assert!(nixos.contains(marker), "the NixOS unit lost {marker:?}");
    }
    // launchd restarts on zero, never on the terminal code; the
    // probe compares against the daemon PID; readiness stays wired
    // to the posture path.
    for marker in [
        "SuccessfulExit",
        "ThrottleInterval",
        "--restart-status=0",
        "launchctl list",
        "--posture-path=",
        "wyrd-drive-probe",
    ] {
        assert!(darwin.contains(marker), "the darwin daemon lost {marker:?}");
    }
    // The probe takes the lock non-blocking and refuses with the
    // drive and the holder facts when a foreign process holds it,
    // while its own previous generation rolls the switch.
    assert!(
        probe.contains("LOCK_EX") && probe.contains("LOCK_NB"),
        "the probe takes the lock non-blocking"
    );
    assert!(
        probe.contains("return 1"),
        "a foreign holder refuses the switch"
    );
    assert!(
        probe.contains("previous generation"),
        "the probe rolls the service's own previous generation"
    );

    // The freshness bound is one number in three spellings (finding
    // 1): the two Python mirrors must agree with each other, and the
    // Rust side must keep the three-interval relationship the
    // mirrors document. A Python script cannot name a Rust
    // constant, so the test pins the literals instead of sharing
    // them.
    let wrap_fresh = wrapper
        .lines()
        .find_map(|line| line.trim().strip_prefix("POSTURE_FRESH_SECS = "))
        .expect("the wrapper names its freshness bound");
    let probe_fresh = probe
        .lines()
        .find_map(|line| line.trim().strip_prefix("POSTURE_FRESH_SECS = "))
        .expect("the probe names its freshness bound");
    assert_eq!(
        wrap_fresh, probe_fresh,
        "supervisor and probe must share one freshness bound"
    );
    let daemon = fs::read_to_string(root.join("crates/wyrd-daemon/src/posture.rs"))
        .expect("posture readable");
    assert!(
        daemon.contains("POSTURE_INTERVAL.as_secs() * 3"),
        "the Rust threshold stays three posture intervals"
    );

    // The sidecar filenames are one vocabulary across the boundary
    // (finding 3): the daemon's constants and the module's paths
    // must name the same files.
    for name in ["vault.posture", "mount.posture"] {
        assert!(daemon.contains(name), "the daemon defines {name}");
        assert!(
            nixos.contains(name) && darwin.contains(name),
            "both supervisors reference {name}"
        );
    }
}

/// The quoted strings a `name = [ ... ]` flake binding holds, in
/// order. Structural parsing for a structural guard: the test fails
/// closed on layout drift rather than silently reading nothing. The
/// list may sit on one line or span several; parsing stops at `]`.
fn flake_string_list<'a>(flake: &'a str, name: &str) -> Vec<&'a str> {
    let mut items = Vec::new();
    let mut inside = false;
    for line in flake.lines() {
        let trimmed = line.trim();
        if !inside {
            let rest = trimmed.strip_prefix(name).unwrap_or("");
            let rest = rest.trim_start().strip_prefix('=').unwrap_or("");
            if !rest.trim_start().starts_with('[') {
                continue;
            }
            inside = true;
        }
        let mut parts = trimmed.split('"');
        while let (Some(_), Some(quoted)) = (parts.next(), parts.next()) {
            if !quoted.is_empty() {
                items.push(quoted);
            }
        }
        if trimmed.contains(']') {
            break;
        }
    }
    items
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

/// Contract 34 (membership half): the workspace member set is exact.
/// Additions fail closed through the unknown-member policy; removals
/// would silently narrow every member-wide check, so the set itself
/// is pinned. Order is not an invariant (Cargo ignores it), so the
/// comparison is order-insensitive and a reorder never fails this.
#[test]
fn workspace_members_are_exact() {
    let root_manifest = read_manifest(&workspace_root());
    let mut members: Vec<String> = root_manifest
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .expect("workspace members list")
        .iter()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect();
    members.sort();
    let mut expected = [
        "crates/wyrd-format",
        "crates/wyrd-sync",
        "crates/wyrd-namespace",
        "crates/wyrd-view",
        "crates/wyrd-core",
        "crates/wyrd-daemon",
        "crates/wyrd-cli",
        "crates/wyrd-contracts",
        "crates/wyrd-embed",
        // The desktop host (06, first of the OD-8 pair): the owner
        // process with no presentation backend. The File Provider
        // adapter (17) is the second member and amends this list again.
        "crates/wyrd-host",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(
        members, expected,
        "workspace membership drifted: add or remove the member deliberately here"
    );
}

/// Contract 34: workspace dependency edges point downward along the
/// declared layering, and the policy fails closed on unknown crates,
/// edges, and third-party additions. The `wyrd-core`/`wyrd-cli`
/// clauses activate when those crates join the workspace.
#[test]
fn crate_dependencies_follow_the_layered_dag() {
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
    let workspace_deps = workspace_dep_table(&root_manifest);

    let mut violations = Vec::new();
    for member in &members {
        let name = member.rsplit('/').next().unwrap_or(member);
        let deps = production_deps(&read_manifest(&root.join(member)));
        violations.extend(check_member(name, &deps, &workspace_deps));
        if name == "wyrd-core" {
            violations.extend(check_core_nostr_scope(&root.join(member)));
        }
    }
    assert!(
        violations.is_empty(),
        "dependency-DAG violations:\n- {}",
        violations.join("\n- ")
    );
}

/// A member's production edges resolved to effective package names:
/// the `wyrd-*` members it names, and the third-party packages it
/// declares. Built with the same alias/workspace resolution as the
/// direct check, so the closure sees exactly what the checker sees.
#[derive(Debug)]
struct MemberEdges {
    workspace: BTreeSet<String>,
    external: BTreeSet<String>,
}

fn member_edges(deps: &DepSet, workspace_deps: &toml::Table) -> MemberEdges {
    let mut edges = MemberEdges {
        workspace: BTreeSet::new(),
        external: BTreeSet::new(),
    };
    for (key, value) in deps {
        let name = effective_name(key, value, workspace_deps);
        if name.starts_with("wyrd-") {
            edges.workspace.insert(name);
        } else {
            edges.external.insert(name);
        }
    }
    edges
}

/// One witness path from `member` to `target` through workspace edges,
/// for diagnostics. Breadth-first over the sorted edge sets, so the
/// reported path is deterministic.
fn witness_path(member: &str, target: &str, graph: &BTreeMap<String, MemberEdges>) -> Vec<String> {
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([member.to_owned()]);
    parent.insert(member.to_owned(), String::new());
    while let Some(current) = queue.pop_front() {
        if current == target {
            let mut path = vec![current];
            loop {
                let previous = parent[path.last().expect("nonempty path")].clone();
                if previous.is_empty() {
                    break;
                }
                path.push(previous);
            }
            path.reverse();
            return path;
        }
        if let Some(edges) = graph.get(&current) {
            for next in &edges.workspace {
                if !parent.contains_key(next) {
                    parent.insert(next.clone(), current.clone());
                    queue.push_back(next.clone());
                }
            }
        }
    }
    Vec::new()
}

/// Check one member's transitive production closure: every workspace
/// member reachable through production edges, plus every external
/// package any reached member declares. Flags `wyrd-sync`
/// reachability and any `iroh*` package in the reached externals as
/// separate violations. Pure over the edge sets so the negative tests
/// below can prove the checker bites without touching the filesystem.
fn check_link_graph(member: &str, graph: &BTreeMap<String, MemberEdges>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![member.to_owned()];
    while let Some(current) = stack.pop() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(edges) = graph.get(&current) {
            stack.extend(edges.workspace.iter().cloned());
        }
    }
    let mut violations = Vec::new();
    if seen.contains("wyrd-sync") {
        violations.push(format!(
            "`{member}` reaches `wyrd-sync` through production edges ({}): the view \
             crate's link graph must exclude the transport — move the needed surface \
             into a narrower crate or amend the policy deliberately",
            witness_path(member, "wyrd-sync", graph).join(" → ")
        ));
    }
    let mut iroh_hits: BTreeSet<(String, String)> = BTreeSet::new();
    for reached in &seen {
        if let Some(edges) = graph.get(reached) {
            iroh_hits.extend(
                edges
                    .external
                    .iter()
                    .filter(|name| name.starts_with("iroh"))
                    .map(|name| (reached.clone(), name.clone())),
            );
        }
    }
    if !iroh_hits.is_empty() {
        let via = iroh_hits
            .iter()
            .map(|(reached, name)| format!("`{name}` via `{reached}`"))
            .collect::<Vec<_>>()
            .join(", ");
        violations.push(format!(
            "`{member}`'s link graph includes iroh-family packages ({via}): the view \
             crate must never link the transport, even transitively"
        ));
    }
    violations
}

/// Contract 34, transitive half: `wyrd-view` is the crate the mobile
/// story depends on staying transport-neutral, so its
/// production-edge closure must reach neither `wyrd-sync` nor any
/// `iroh*` package. Resolves every member's normal edges from the
/// manifests (dev-dependencies excluded, as in the direct check) and
/// walks the closure from the view crate.
#[test]
fn view_link_graph_excludes_sync_and_iroh() {
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
    let workspace_deps = workspace_dep_table(&root_manifest);

    let mut graph = BTreeMap::new();
    for member in &members {
        let name = member.rsplit('/').next().unwrap_or(member);
        let deps = production_deps(&read_manifest(&root.join(member)));
        graph.insert(name.to_owned(), member_edges(&deps, &workspace_deps));
    }
    let violations = check_link_graph("wyrd-view", &graph);
    assert!(
        violations.is_empty(),
        "link-graph violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract 34, dev-dep pin: `wyrd-view`'s test-only edges stay
/// exactly `wyrd-core` (export fixtures). The direct check exempts
/// dev-dependencies, and the transitive check ignores them, so
/// without this pin a transport edge could hide in the test profile
/// while every suite stays green.
#[test]
fn view_dev_deps_stay_pinned() {
    let root = workspace_root();
    let root_manifest = read_manifest(&root);
    let workspace_deps = workspace_dep_table(&root_manifest);
    let view = read_manifest(&root.join("crates/wyrd-view"));
    let violations = check_view_dev_deps(&dev_deps(&view), &workspace_deps);
    assert!(
        violations.is_empty(),
        "dev-dep violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Check that every headless owner run goes through the daemon's
/// shared composition: `run_vault` and its input struct `VaultRun`
/// may appear only inside `wyrd-daemon` itself. `wyrd vault`, `wyrd
/// mount` (session path excluded — see below), `wyrd-host`, and every
/// future owner-mode entry point compose through
/// `compose_headless_owner` / `run_composed_owner` instead of wiring
/// the runner ad hoc. `Supervisor::spawn_loop` is deliberately NOT
/// in this list: the mount's session supervision is a legitimate
/// second caller (its session join bounds destroy's submits, unlike
/// a headless teardown), and forbidding it would force the mount
/// through a shape that is not its own. Result and event vocabulary
/// (`VaultOutcome`, `VaultEvent`, `VaultLoopEnd`) stays free: naming
/// the outcome is observing the run, not owning the composition.
/// Pure over the file set so the negative test below can prove the
/// checker bites without touching the filesystem.
fn check_owner_runs_through_daemon(files: &[(&str, &str)]) -> Vec<String> {
    const FORBIDDEN: &[&str] = &["run_vault(", "VaultRun"];
    let mut violations = Vec::new();
    for (path, text) in files {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names the headless runner directly (`{marker}`): compose \
                         through wyrd_daemon::compose_headless_owner / run_composed_owner instead",
                        index + 1
                    ));
                }
            }
        }
    }
    violations
}

/// Check the host crate's own production sources for transport
/// machinery: the host composes through `wyrd-core` and `wyrd-daemon`
/// and opens through `wyrd-sync`'s durable side, so a `transport::`,
/// `bulk::`, or `serving::` path segment — or an `iroh`/`n0` crate,
/// type, or module ident — in `crates/wyrd-host/src` is a second
/// transport implementation starting. Matched as path segments and
/// ident prefixes (`iroh::`, `Iroh`, `n0_`, `n0::`), so a field that
/// merely mentions the transport (`iroh_id` in stage logs, kept
/// comparable with the vault's) is not a violation. Comments and
/// string literals strip first via `code_text`, the same scanner the
/// proof-mint check uses. Pure over the file set so the negative
/// test below can prove the checker bites without touching the
/// filesystem.
fn check_host_transport_free(files: &[(&str, &str)]) -> Vec<String> {
    const FORBIDDEN: &[&str] = &[
        "transport::",
        "bulk::",
        "serving::",
        "iroh::",
        "Iroh",
        "n0_",
        "n0::",
    ];
    let mut violations = Vec::new();
    for (path, text) in files {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names transport machinery (`{marker}`): the host reaches \
                         transport only through the daemon's composer vocabulary",
                        index + 1
                    ));
                }
            }
        }
    }
    violations
}

/// Contract (06): the host embeds `wyrd-core` and links no transport
/// above it. Manifest half: the host edge set names `wyrd-core` and
/// no `iroh*`/`n0*` package. Source half: no transport path segment
/// in the host's own production sources. The mechanical companion,
/// run by hand and pasted into the PR: `cargo tree -p wyrd-host
/// -e normal` reviewed against the same list — transitive `iroh`
/// arrives only through `wyrd-core` and `wyrd-daemon`, never through
/// a host edge.
#[test]
fn host_embeds_wyrd_core_and_links_no_transport_above_it() {
    let root = workspace_root();
    let root_manifest = read_manifest(&root);
    let workspace_deps = workspace_dep_table(&root_manifest);
    let manifest = read_manifest(&root.join("crates/wyrd-host"));
    let deps = production_deps(&manifest);
    let names: Vec<String> = deps
        .iter()
        .map(|(key, value)| effective_name(key, value, &workspace_deps))
        .collect();
    assert!(
        names.iter().any(|name| name == "wyrd-core"),
        "the host embeds wyrd-core, found edges: {names:?}"
    );
    let transport_deps: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with("iroh") || name.starts_with("n0"))
        .collect();
    assert!(
        transport_deps.is_empty(),
        "the host declares no transport package, found: {transport_deps:?}"
    );

    let (files, errors) = collect_rs_files(&root.join("crates/wyrd-host/src"), &root);
    assert!(
        errors.is_empty(),
        "host source walk errors:\n- {}",
        errors.join("\n- ")
    );
    assert!(!files.is_empty(), "the host crate holds sources to check");
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let violations = check_host_transport_free(&refs);
    assert!(
        violations.is_empty(),
        "host transport violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract (06 follow-up): every headless owner run goes through
/// the daemon's shared composition. Scans the CLI's and the host's
/// production sources for direct uses of the headless runner and
/// its input struct; `wyrd vault`, `wyrd-host`, and every future
/// owner-mode entry point must reach `run_vault` only through
/// `compose_headless_owner` / `run_composed_owner`. The mount's
/// session-supervised loop is intentionally out of scope (its
/// `spawn_loop` call bounds destroy's submits — a shape the
/// headless check must not flatten), and so is the daemon itself.
/// The behavioral half lives in the suites that exercise both
/// callers end to end (`host_*` lifecycle tests for the host,
/// vault CLI tests for the vault).
#[test]
fn headless_runs_go_through_the_daemon_composer() {
    let root = workspace_root();
    let mut refs: Vec<(String, String)> = Vec::new();
    for member in ["crates/wyrd-cli/src", "crates/wyrd-host/src"] {
        let (files, errors) = collect_rs_files(&root.join(member), &root);
        assert!(
            errors.is_empty(),
            "{member} source walk errors:\n- {}",
            errors.join("\n- ")
        );
        refs.extend(files);
    }
    assert!(
        !refs.is_empty(),
        "the composer callers hold sources to check"
    );
    let borrowed: Vec<(&str, &str)> = refs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let violations = check_owner_runs_through_daemon(&borrowed);
    assert!(
        violations.is_empty(),
        "owner-composition violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract (conflict floor): no v0.4 surface authors a conflict resolution.
/// Scans the surface crates' production sources for the resolution
/// primitive (`MergeSelection`, `Engine::merge_heads`); the engine
/// library owns the primitive, the CLI's `snapshot` command is the
/// single exempted owner-mediated caller (the CLI exemption, named in the same
/// invariant), and everything
/// else — daemon backends, host, FUSE adapter, every other CLI
/// command — must never name it. The behavioral half is the
/// `an_unresolved_conflict_stays_fully_addressable` view test plus
/// the mounted-write suites; this test is the source check that
/// makes the deferral a gate rather than a note.
#[test]
fn no_surface_authors_a_conflict_resolution() {
    let root = workspace_root();
    let mut refs: Vec<(String, String)> = Vec::new();
    for member in [
        "crates/wyrd-cli/src",
        "crates/wyrd-daemon/src",
        "crates/wyrd-host/src",
        "crates/wyrd-view/src",
    ] {
        let (files, errors) = collect_rs_files(&root.join(member), &root);
        assert!(
            errors.is_empty(),
            "{member} source walk errors:\n- {}",
            errors.join("\n- ")
        );
        refs.extend(files);
    }
    assert!(!refs.is_empty(), "the surface crates hold sources to check");
    // The CLI exemption, pinned to exactly one file: the CLI's
    // snapshot command is the owner-mediated path v0.4 keeps. The
    // check below runs on everything else; this assertion keeps the
    // exemption from silently widening (a second exempted file) or
    // silently lapsing (the primitive renamed out from under it).
    // New CLI command files need no exemption handling: the partition
    // below matches only `commands/snapshot.rs`, so any future surface
    // that names the primitive lands in `rest` and fails the check.
    let (exempt, rest): (Vec<_>, Vec<_>) = refs
        .into_iter()
        .partition(|(p, _)| p.ends_with("commands/snapshot.rs"));
    assert_eq!(
        exempt.len(),
        1,
        "exactly one exempted surface file: {exempt:?}"
    );
    let exempt_code = code_text(&exempt[0].1);
    assert!(
        exempt_code.contains("MergeSelection") && exempt_code.contains("merge_heads"),
        "the exemption is live: snapshot.rs still authors through the primitive"
    );
    let borrowed: Vec<(&str, &str)> = rest.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let violations = check_no_surface_authors_resolutions(&borrowed);
    assert!(
        violations.is_empty(),
        "resolution-authoring violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract (OD-15-A, OD-15-C): no v0.4 surface offers a role choice.
///
/// The membership state machine has no role-change operation — roles
/// change through removal and single-use identity names a fresh device
/// — so a picker would sell a device replacement as a role change.
/// v0.4 sharing is member-only: the reader role stays reachable only
/// through the CLI's existing `member invite --reader`, and no
/// presentation surface may render a role selector.
///
/// The check has two halves. The structural half pins the axis the
/// decision turns on: the `Change` enum carries exactly the five
/// decided kinds (no `SetRole` to hang a picker on), and the single
/// `enum Role` in the workspace is the materialization/serving axis in
/// `wyrd-core/src/role.rs`, not a membership role. The surface half
/// scans the presentation crates for picker vocabulary; the CLI's
/// `member` command is the one exempted owner-mediated path (it keeps
/// `--reader`), so any second surface reaching for role-choice
/// vocabulary fails loudly.
#[test]
fn no_v04_surface_offers_a_role_choice() {
    let root = workspace_root();
    // Structural half: the membership axis has no role change to pick.
    let membership = fs::read_to_string(root.join("crates/wyrd-format/src/membership.rs"))
        .expect("membership readable");
    for variant in [
        "Change::Admit(",
        "Change::Remove(",
        "Change::Rotate",
        "Change::SetOwners(",
        "Change::AdmitReader(",
    ] {
        assert!(
            membership.contains(variant),
            "the five decided change kinds stay: missing {variant}"
        );
    }
    assert!(
        !membership.contains("SetRole") && !membership.contains("ChangeRole"),
        "no role-change variant on the membership axis"
    );
    let mut role_defs = Vec::new();
    for member in [
        "crates/wyrd-core/src",
        "crates/wyrd-format/src",
        "crates/wyrd-sync/src",
    ] {
        let (files, errors) = collect_rs_files(&root.join(member), &root);
        assert!(
            errors.is_empty(),
            "{member} source walk errors:\n- {}",
            errors.join("\n- ")
        );
        for (path, text) in files {
            let code = code_text(&text);
            for (index, line) in code.lines().enumerate() {
                let trimmed = line.trim();
                // Exactly `enum Role`, not `RoleState` / `RoleError` /
                // `Transition`: the next char after `Role` must not be
                // a name char. The single hit is the
                // materialization/serving axis in wyrd-core/src/role.rs.
                for marker in ["enum Role", "struct Role"] {
                    let mut search = 0;
                    while let Some(found) = trimmed[search..].find(marker) {
                        let after = search + found + marker.len();
                        let boundary = trimmed[after..]
                            .chars()
                            .next()
                            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                        if boundary {
                            role_defs.push(format!("{path}:{}", index + 1));
                            break;
                        }
                        search = after;
                    }
                }
            }
        }
    }
    assert_eq!(
        role_defs.len(),
        1,
        "exactly one Role type in the workspace (the materialization axis): {role_defs:?}"
    );
    assert!(
        role_defs[0].ends_with("crates/wyrd-core/src/role.rs:41")
            || role_defs[0].contains("crates/wyrd-core/src/role.rs"),
        "the single Role lives on the materialization axis: {:?}",
        role_defs
    );

    // Surface half: no picker vocabulary outside the CLI member path.
    let mut refs: Vec<(String, String)> = Vec::new();
    for member in [
        "crates/wyrd-host/src",
        "crates/wyrd-daemon/src",
        "crates/wyrd-view/src",
        "crates/wyrd-cli/src",
    ] {
        let (files, errors) = collect_rs_files(&root.join(member), &root);
        assert!(
            errors.is_empty(),
            "{member} source walk errors:\n- {}",
            errors.join("\n- ")
        );
        refs.extend(files);
    }
    assert!(!refs.is_empty(), "the surface crates hold sources to check");
    let (exempt, rest): (Vec<_>, Vec<_>) = refs
        .into_iter()
        .partition(|(p, _)| p.ends_with("commands/member.rs") || p.ends_with("args.rs"));
    assert!(
        !exempt.is_empty(),
        "the CLI member exemption is live: member.rs/args.rs still carry --reader"
    );
    let exempt_code: String = exempt
        .iter()
        .map(|(_, t)| code_text(t))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        exempt_code.contains("--reader") || exempt_code.contains("reader"),
        "the exemption is live: the CLI member path still names the reader flag"
    );
    let borrowed: Vec<(&str, &str)> = rest.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let violations = check_no_surface_offers_role_choice(&borrowed);
    assert!(
        violations.is_empty(),
        "role-choice violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Check that no presentation surface offers a membership role choice:
/// picker widgets (`RolePicker`, `role_picker`, `role-choice`,
/// `role_choice`) and a role-change primitive (`SetRole`,
/// `ChangeRole`) are library-forbidden vocabulary outside the CLI's
/// exempted member path. Pure over the file set so the negative tests
/// below can prove the checker bites without touching the filesystem.
fn check_no_surface_offers_role_choice(files: &[(&str, &str)]) -> Vec<String> {
    const FORBIDDEN: &[&str] = &[
        "RolePicker",
        "role_picker",
        "role-choice",
        "role_choice",
        "SetRole",
        "ChangeRole",
    ];
    let mut violations = Vec::new();
    for (path, text) in files {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names role-choice vocabulary (`{marker}`): v0.4 sharing is \
                         member-only (the CLI --reader path is the single exemption)",
                        index + 1
                    ));
                }
            }
        }
    }
    violations
}

/// Check that no v0.4 surface authors a conflict resolution: the
/// resolution primitive (`MergeSelection`, `Engine::merge_heads`) is
/// library vocabulary, and no surface module may name it. The one
/// exemption is the CLI's `snapshot` command (the CLI exemption: an
/// owner-mediated operator action, not a presentation surface), which
/// the caller filters out before checking — so a second surface
/// reaching the primitive fails loudly rather than being read once.
/// Result vocabulary is untouched: only constructing a selection or
/// calling the merge entry point counts, not naming a snapshot id.
/// Pure over the file set so the negative test below can prove the
/// checker bites without touching the filesystem.
fn check_no_surface_authors_resolutions(files: &[(&str, &str)]) -> Vec<String> {
    const FORBIDDEN: &[&str] = &["MergeSelection", "merge_heads"];
    let mut violations = Vec::new();
    for (path, text) in files {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names the resolution primitive (`{marker}`): surfaces present \
                         conflicts, they never author them (the CLI exemption covers only the CLI's \
                         snapshot command)",
                        index + 1
                    ));
                }
            }
        }
    }
    violations
}

/// Check one consumer member's transitive production closure against
/// the embedder boundary: no process-host or suite crate reachable
/// (`wyrd-daemon` carries `fuser`, `wyrd-cli` carries `clap`/`libc`,
/// `wyrd-contracts` is the suite itself), and no presentation external
/// (`fuser`, `libc`, `clap`) declared by any reached member. Pure over
/// the edge sets so the negative tests below can prove the checker
/// bites without touching the filesystem.
fn check_consumer_link_graph(member: &str, graph: &BTreeMap<String, MemberEdges>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![member.to_owned()];
    while let Some(current) = stack.pop() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(edges) = graph.get(&current) {
            stack.extend(edges.workspace.iter().cloned());
        }
    }
    let mut violations = Vec::new();
    for host in ["wyrd-daemon", "wyrd-cli", "wyrd-contracts"] {
        if seen.contains(host) {
            violations.push(format!(
                "`{member}` reaches `{host}` through production edges ({}): the \
                 embedded consumer boundary excludes presentation and the process \
                 host — keep the embedded surface in wyrd-core",
                witness_path(member, host, graph).join(" → ")
            ));
        }
    }
    let mut presentation: BTreeSet<(String, String)> = BTreeSet::new();
    for reached in &seen {
        if let Some(edges) = graph.get(reached) {
            presentation.extend(
                edges
                    .external
                    .iter()
                    .filter(|name| matches!(name.as_str(), "fuser" | "libc" | "clap"))
                    // `libc` through `wyrd-sync` is the custody boundary,
                    // not presentation: `O_NOFOLLOW` directory opens and
                    // the euid ownership check in `durable::store`,
                    // mirroring the CLI's credential-file hardening. The
                    // embedded consumer boundary excludes presentation
                    // and the process host, not the C library the
                    // substrate verifies custody with. `libc` through any
                    // other member still bites.
                    .filter(|name| !(reached.as_str() == "wyrd-sync" && name.as_str() == "libc"))
                    .map(|name| (reached.clone(), name.clone())),
            );
        }
    }
    if !presentation.is_empty() {
        let via = presentation
            .iter()
            .map(|(reached, name)| format!("`{name}` via `{reached}`"))
            .collect::<Vec<_>>()
            .join(", ");
        violations.push(format!(
            "`{member}`'s link graph includes a presentation package ({via}): the \
             embedded consumer boundary excludes presentation, even transitively"
        ));
    }
    violations
}

/// Contract 34, consumer half: `wyrd-embed` is a real consumer of the
/// whole embedded lifecycle, so its production closure must reach no
/// presentation or process surface — resolving its edges from the
/// manifests exactly as the direct check does (dev-dependencies
/// excluded, as there).
#[test]
fn consumer_link_graph_excludes_presentation() {
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
    let workspace_deps = workspace_dep_table(&root_manifest);

    let mut graph = BTreeMap::new();
    for member in &members {
        let name = member.rsplit('/').next().unwrap_or(member);
        let deps = production_deps(&read_manifest(&root.join(member)));
        graph.insert(name.to_owned(), member_edges(&deps, &workspace_deps));
    }
    let violations = check_consumer_link_graph("wyrd-embed", &graph);
    assert!(
        violations.is_empty(),
        "link-graph violations:\n- {}",
        violations.join("\n- ")
    );
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

/// The member's own `[features]` table for alias expansion below.
/// Absent means no aliases to expand through.
fn member_features(manifest: &toml::Table) -> toml::Table {
    manifest
        .get("features")
        .and_then(|f| f.as_table())
        .cloned()
        .unwrap_or_default()
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

/// Check the distribution build's cargo args from the flake text: the
/// release closure builds `-p wyrd-cli` with default features, so the
/// live (non-comment) flake text may contain neither
/// `insecure-fast-kdf` nor `--all-features` — in any string form,
/// single-line or multiline, under any variable name. Comment stripping
/// is string-aware (a `#` inside `"..."` or `''...''` never starts a
/// comment), so a commented-out `cargoExtraArgs` neither satisfies the
/// presence check nor trips the token check, and a token that only
/// reaches the build through string concatenation is out of scope for
/// this structural guard (name it plainly or not at all). Not a
/// substitute for the resolved-graph check in CI
/// (`cargo tree -e normal,features` in workflow.yml) — matching the
/// whole live text rather than trusting line layout, and failing
/// closed when the flag vanishes.
/// Strip Nix `#` comments, string-aware: `"..."` (with `\` escapes)
/// and `''...''` (with `'''` escape) pass through intact, so a `#`
/// inside a string never starts a comment. Newlines are preserved so
/// diagnostics below keep their line numbers. Enough grammar for a
/// convention check; interpolation nesting beyond balanced quotes is
/// out of scope.
fn strip_nix_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"\"") {
            // Double-quoted string: copy through, honoring escapes.
            out.push('"');
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                out.push(c as char);
                i += 1;
                if c == b'\\' && i < bytes.len() {
                    out.push(bytes[i] as char);
                    i += 1;
                } else if c == b'"' {
                    break;
                }
            }
        } else if rest.starts_with(b"''") {
            // Indented string: copy through, honoring `'''`.
            out.push_str("''");
            i += 2;
            while i < bytes.len() {
                if bytes[i..].starts_with(b"'''") {
                    out.push_str("'''");
                    i += 3;
                } else if bytes[i..].starts_with(b"''") {
                    out.push_str("''");
                    i += 2;
                    break;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
        } else if bytes[i] == b'#' {
            // Comment: skip to (not through) the newline.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn check_release_build_args(text: &str) -> Vec<String> {
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
/// scoped, even with spaces), then `async`/`unsafe`/`const`/`extern`
/// modifiers in any order, then the `fn` keyword and the function
/// name. Matching the trailing `fn <ident>` shape — rather than a
/// visibility-prefix list — keeps a future `pub(crate) async fn` /
/// `pub unsafe fn` / `pub const fn` authoring body attributed to
/// itself instead of the previous function. Runs on stripped code
/// lines, so comments and string literals never match; `fn`-pointer
/// types (`fn(u32)`) and `Fn` bounds never match either (`fn` must
/// stand alone, followed by a name).
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
    // Optional modifiers, in any order.
    while tokens
        .peek()
        .is_some_and(|t| matches!(*t, "async" | "unsafe" | "const" | "extern"))
    {
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

/// Contract (F2), source half: no production `wyrd-core` export
/// authors a snapshot outside the live pass. `WyrdNode::put_file` and
/// `remove` survive only as `test-fixtures`-gated bodies in
/// `node.rs`; the loop authors only through `live/apply.rs`. A new
/// second authority — a renamed direct-write entry, an ungated
/// helper, a `LiveNode` shortcut around the queue — fails here before
/// it ships beside the quota/fold/staleness channel. The manifest
/// half above keeps the gate itself test-only; this is the source
/// check that makes the retirement a gate rather than a note.
#[test]
fn node_authoring_stays_in_the_live_pass_or_fixtures() {
    let root = workspace_root();
    // The whole `wyrd-core` source tree, not a hardcoded file list: a
    // new submodule cannot evade the pin by name. The checker itself
    // scopes to `node.rs` plus `live/` (test files out of scope), so
    // production files elsewhere ride along harmlessly. Paths stay
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
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Scratch `wyrd-core`-shaped root (`<root>/src/`) under the temp
    /// dir. The caller owns cleanup; tests remove the tree before
    /// asserting so a failure never leaks a permission-stripped dir.
    fn scratch_core_dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "wyrd-nostr-scope-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).expect("scratch src dir is creatable");
        root
    }

    fn empty_ws() -> toml::Table {
        toml::Table::new()
    }

    fn deps(names: &[&str]) -> DepSet {
        names
            .iter()
            .map(|n| (n.to_string(), toml::Value::Boolean(true)))
            .collect()
    }

    fn manifest_deps(manifest: &str) -> DepSet {
        production_deps(&manifest.parse().unwrap())
    }

    #[test]
    fn upward_edge_is_rejected() {
        let violations = check_member(
            "wyrd-sync",
            &deps(&["wyrd-format", "wyrd-daemon"]),
            &empty_ws(),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-daemon"));
    }

    #[test]
    fn aliased_upward_edge_is_rejected_through_manifest_path() {
        let parsed = manifest_deps(
            r#"
            [dependencies]
            wyrd-format.workspace = true
            transport = { package = "wyrd-daemon", path = "../wyrd-daemon" }
        "#,
        );
        let violations = check_member("wyrd-sync", &parsed, &empty_ws());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-daemon"));
        assert!(violations[0].contains("transport"));
    }

    #[test]
    fn workspace_inherited_rename_resolves() {
        let root: toml::Table = r#"
            [workspace.dependencies]
            renamed = { package = "wyrd-daemon" }
        "#
        .parse()
        .unwrap();
        let parsed = manifest_deps(
            r#"
            [dependencies]
            renamed.workspace = true
        "#,
        );
        let violations = check_member("wyrd-sync", &parsed, &workspace_dep_table(&root));
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-daemon"));
    }

    #[test]
    fn core_never_gains_presentation_deps() {
        for forbidden in ["fuser", "clap", "libc"] {
            let violations = check_member(
                "wyrd-core",
                &deps(&["wyrd-format", "wyrd-sync", forbidden]),
                &empty_ws(),
            );
            assert_eq!(violations.len(), 1, "for {forbidden}");
        }
    }

    #[test]
    fn aliased_forbidden_external_is_rejected() {
        let parsed = manifest_deps(
            r#"
            [dependencies]
            wyrd-sync.workspace = true
            fuse_bindings = { package = "fuser", version = "0.18" }
        "#,
        );
        let violations = check_member("wyrd-core", &parsed, &empty_ws());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("fuser"));
    }

    #[test]
    fn new_external_dep_fails_closed() {
        let violations = check_member(
            "wyrd-sync",
            &deps(&["wyrd-format", "some-new-crate"]),
            &empty_ws(),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("some-new-crate"));
    }

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

    #[test]
    fn cli_never_touches_the_view_crate() {
        let violations = check_member("wyrd-cli", &deps(&["wyrd-core", "wyrd-view"]), &empty_ws());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("wyrd-view"));
    }

    #[test]
    fn cli_host_deps_are_exact() {
        // The process host's full production edge set, pinned so a
        // widening (or a silent policy trim) fails here and not in
        // review vigilance. The live contract above checks the real
        // manifest; this names the expectation.
        let violations = check_member(
            "wyrd-cli",
            &deps(&[
                "wyrd-format",
                "wyrd-sync",
                "wyrd-core",
                "wyrd-daemon",
                "clap",
                "fuser",
                "hex",
                "libc",
                "nostr",
                "thiserror",
                "toml",
                "tracing",
                "tracing-subscriber",
                "tracing-log",
                "zeroize",
            ]),
            &empty_ws(),
        );
        assert!(
            violations.is_empty(),
            "cli allowlist drifted: {violations:?}"
        );
    }

    #[test]
    fn format_substrate_stays_closed() {
        let violations = check_member("wyrd-format", &deps(&["blake3", "tokio"]), &empty_ws());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("tokio"));
    }

    #[test]
    fn unknown_member_fails_closed() {
        let violations = check_member("wyrd-something-new", &deps(&["wyrd-format"]), &empty_ws());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("deliberately"));
    }

    #[test]
    fn contracts_is_a_leaf() {
        let violations = check_member(
            "wyrd-daemon",
            &deps(&["wyrd-sync", "wyrd-contracts"]),
            &empty_ws(),
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("leaf"));
    }

    #[test]
    fn production_deps_include_nested_tables_but_not_dev() {
        let collected = manifest_deps(
            r#"
            [dependencies]
            wyrd-format.workspace = true
            [dev-dependencies]
            wyrd-daemon.workspace = true
            [target.'cfg(unix)'.dependencies]
            libc = "0.2"
        "#,
        );
        assert!(collected.contains_key("wyrd-format"));
        assert!(collected.contains_key("libc"));
        assert!(!collected.contains_key("wyrd-daemon"));
    }

    #[test]
    fn nostr_scope_pins_to_one_subsystem() {
        let single = [
            ("src/mailbox/mod.rs", "use nostr_sdk::prelude::Client;"),
            (
                "src/mailbox/seen_store.rs",
                "let id = nostr::event::EventId::new();",
            ),
            ("src/session.rs", "use std::collections::BTreeMap;"),
        ];
        assert_eq!(
            nostr_holding_subsystems(&single),
            BTreeSet::from(["mailbox".to_owned()])
        );

        let spread = [
            ("src/mailbox/mod.rs", "use nostr_sdk::prelude::Client;"),
            ("src/session/mod.rs", "extern crate nostr;"),
        ];
        assert_eq!(
            nostr_holding_subsystems(&spread),
            BTreeSet::from(["mailbox".to_owned(), "session".to_owned()])
        );
    }

    #[test]
    fn nostr_scope_ignores_comments_and_urls() {
        let files = [
            ("src/session/mod.rs", "// see nostr:: docs for the framing"),
            (
                "src/session/mod.rs",
                "let x = 1;// attached nostr:: comment",
            ),
            (
                "src/session/mod.rs",
                "/* block nostr:: mention\nspanning lines, /* nested */ done */",
            ),
            ("src/node/mod.rs", "let url = \"http://relay/nostr::x\";"),
            ("src/node/mod.rs", "let raw = r#\"nostr:: in raw\"#;"),
            ("src/node/mod.rs", "let tick = &'static str;"),
            ("src/node/mod.rs", "use std::io;"),
        ];
        assert!(nostr_holding_subsystems(&files).is_empty());
    }

    #[test]
    fn nostr_scope_still_detects_real_use_beside_noise() {
        let files = [
            (
                "src/mailbox/mod.rs",
                "/* nostr:: in a comment */\nuse nostr_sdk::prelude::Client;",
            ),
            (
                "src/mailbox/seen_store.rs",
                "let doc = \"nostr:: in a string\";\nlet id = nostr::event::EventId::new();",
            ),
        ];
        assert_eq!(
            nostr_holding_subsystems(&files),
            BTreeSet::from(["mailbox".to_owned()])
        );
    }

    #[test]
    fn nostr_scope_fails_when_source_tree_is_missing() {
        let missing =
            std::env::temp_dir().join(format!("wyrd-nostr-scope-missing-{}", std::process::id()));
        // A crashed earlier run must not leave the tree behind and
        // flip this to the empty-dir case.
        let _ = fs::remove_dir_all(&missing);
        let violations = check_core_nostr_scope(&missing);
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains(&*missing.join("src").to_string_lossy()),
            "failure must name the unreadable tree: {violations:?}"
        );
    }

    #[test]
    fn nostr_scope_fails_when_no_files_are_examined() {
        let root = scratch_core_dir();
        let violations = check_core_nostr_scope(&root);
        fs::remove_dir_all(&root).expect("scratch cleanup");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("no `.rs` files"),
            "an empty walk must fail closed: {violations:?}"
        );
    }

    /// Permission-stripping proves the nested-dir branch on ordinary
    /// runners; on elevated runners (root reads through `0o000`) the
    /// same test asserts the complementary truth instead — a fully
    /// readable fixture passes clean — so it is an assertion everywhere.
    #[cfg(unix)]
    #[test]
    fn nostr_scope_surfaces_an_unreadable_nested_dir() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch_core_dir();
        let mailbox = root.join("src").join("mailbox");
        fs::create_dir_all(&mailbox).expect("scratch mailbox dir");
        fs::write(mailbox.join("mod.rs"), "use nostr_sdk::prelude::Client;").expect("scratch file");
        let opaque = root.join("src").join("opaque");
        fs::create_dir_all(&opaque).expect("scratch opaque dir");
        fs::set_permissions(&opaque, fs::Permissions::from_mode(0o000)).expect("strip permissions");
        // No root-proof way to fail `read_dir` from the filesystem
        // exists, so an elevated runner reads through `0o000`. Rather
        // than print-and-return (nextest hides passing-test output, so
        // a note proves nothing), assert the complementary truth on
        // that path: everything is readable, so the fixture tree must
        // yield a clean scope verdict. That keeps the test an assertion
        // on every runner and doubles as happy-path coverage of the
        // fixture itself.
        if fs::read_dir(&opaque).is_ok() {
            let violations = check_core_nostr_scope(&root);
            fs::remove_dir_all(&root).expect("scratch cleanup");
            assert!(
                violations.is_empty(),
                "fully readable fixture must pass clean: {violations:?}"
            );
            return;
        }
        let violations = check_core_nostr_scope(&root);
        fs::set_permissions(&opaque, fs::Permissions::from_mode(0o755)).expect("restore");
        fs::remove_dir_all(&root).expect("scratch cleanup");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("opaque"),
            "nested read failure must surface: {violations:?}"
        );
    }

    /// A dangling symlink fails `read_to_string` on every runner,
    /// including elevated ones that read through `0o000`, so the
    /// unreadable-file proof is runner-independent. `is_dir` follows
    /// links (false for a dangling link) and the `rs` extension still
    /// selects it, landing in exactly the file-error branch.
    #[cfg(unix)]
    #[test]
    fn nostr_scope_surfaces_an_unreadable_file() {
        let root = scratch_core_dir();
        let mailbox = root.join("src").join("mailbox");
        fs::create_dir_all(&mailbox).expect("scratch dirs");
        let file = mailbox.join("mod.rs");
        std::os::unix::fs::symlink("nowhere.rs", &file).expect("dangling symlink");
        assert!(
            fs::read_to_string(&file).is_err(),
            "the dangling link must stay unreadable"
        );
        let violations = check_core_nostr_scope(&root);
        fs::remove_dir_all(&root).expect("scratch cleanup");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("mod.rs"),
            "unreadable file must surface: {violations:?}"
        );
    }

    /// A scratch workspace-edge graph: `(member, workspace edges,
    /// external packages)`. Edges name members directly (no manifest
    /// parsing) so the closure tests prove the checker bites on shape
    /// alone.
    fn link_graph(members: &[(&str, &[&str], &[&str])]) -> BTreeMap<String, MemberEdges> {
        members
            .iter()
            .map(|(member, workspace, external)| {
                (
                    member.to_string(),
                    MemberEdges {
                        workspace: workspace.iter().map(|s| s.to_string()).collect(),
                        external: external.iter().map(|s| s.to_string()).collect(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn transitive_sync_reachability_is_rejected_with_witness() {
        let graph = link_graph(&[
            ("wyrd-view", &["wyrd-core"], &["thiserror"]),
            (
                "wyrd-core",
                &["wyrd-format", "wyrd-sync"],
                &["thiserror", "tokio"],
            ),
            ("wyrd-sync", &["wyrd-format"], &["iroh", "thiserror"]),
            ("wyrd-format", &[], &["blake3", "thiserror"]),
        ]);
        let violations = check_link_graph("wyrd-view", &graph);
        assert_eq!(violations.len(), 2, "sync and iroh: {violations:?}");
        assert!(
            violations[0].contains("wyrd-sync"),
            "sync reachability names the target: {violations:?}"
        );
        assert!(
            violations[0].contains("wyrd-view → wyrd-core → wyrd-sync"),
            "the witness path names every hop: {violations:?}"
        );
        assert!(
            violations[1].contains("`iroh` via `wyrd-sync`"),
            "the iroh hit names the declaring member: {violations:?}"
        );
    }

    #[test]
    fn iroh_without_sync_is_still_rejected() {
        // The two assertions are independent: a reached member
        // declaring an iroh-family package fails even with no
        // `wyrd-sync` in the closure.
        let graph = link_graph(&[
            ("wyrd-view", &["wyrd-core"], &["thiserror"]),
            ("wyrd-core", &["wyrd-format"], &["iroh-blobs", "thiserror"]),
            ("wyrd-format", &[], &["blake3", "thiserror"]),
        ]);
        let violations = check_link_graph("wyrd-view", &graph);
        assert_eq!(violations.len(), 1, "iroh only: {violations:?}");
        assert!(
            violations[0].contains("`iroh-blobs` via `wyrd-core`"),
            "names the package and the member: {violations:?}"
        );
    }

    #[test]
    fn clean_closure_passes() {
        let graph = link_graph(&[
            ("wyrd-view", &["wyrd-namespace"], &["thiserror"]),
            ("wyrd-namespace", &["wyrd-format"], &["thiserror"]),
            ("wyrd-format", &[], &["blake3", "hex", "thiserror"]),
        ]);
        assert!(
            check_link_graph("wyrd-view", &graph).is_empty(),
            "a transport-free closure must pass"
        );
    }

    #[test]
    fn closure_ignores_unknown_members() {
        // Edges at non-members (a removed crate, a typo) are the
        // direct check's fail-closed territory; the closure walk must
        // not panic on them, just treat them as leaves.
        let graph = link_graph(&[("wyrd-view", &["wyrd-gone"], &["thiserror"])]);
        assert!(
            check_link_graph("wyrd-view", &graph).is_empty(),
            "unknown members are leaves to the closure: {graph:?}"
        );
    }

    #[test]
    fn consumer_closure_rejects_presentation_reachability_with_witness() {
        let graph = link_graph(&[
            ("wyrd-embed", &["wyrd-core"], &[]),
            ("wyrd-core", &["wyrd-format", "wyrd-daemon"], &["thiserror"]),
            ("wyrd-daemon", &["wyrd-view"], &["fuser"]),
            ("wyrd-view", &[], &["thiserror"]),
            ("wyrd-format", &[], &["blake3", "thiserror"]),
        ]);
        let violations = check_consumer_link_graph("wyrd-embed", &graph);
        assert_eq!(
            violations.len(),
            2,
            "the daemon reachability and the fuser external: {violations:?}"
        );
        assert!(
            violations[0].contains("wyrd-daemon"),
            "reachability names the target: {violations:?}"
        );
        assert!(
            violations[0].contains("wyrd-embed → wyrd-core → wyrd-daemon"),
            "the witness path names every hop: {violations:?}"
        );
        assert!(
            violations[1].contains("`fuser` via `wyrd-daemon`"),
            "the presentation external names its declaring member: {violations:?}"
        );
    }

    #[test]
    fn consumer_closure_rejects_the_process_host_even_without_presentation_externals() {
        // The two clauses are independent: a reached host fails even
        // when no reached member declares a presentation external.
        let graph = link_graph(&[
            ("wyrd-embed", &["wyrd-cli"], &[]),
            ("wyrd-cli", &["wyrd-core"], &["hex", "thiserror"]),
            ("wyrd-core", &["wyrd-format"], &["thiserror"]),
            ("wyrd-format", &[], &["blake3", "thiserror"]),
        ]);
        let violations = check_consumer_link_graph("wyrd-embed", &graph);
        assert_eq!(violations.len(), 1, "the host only: {violations:?}");
        assert!(
            violations[0].contains("wyrd-cli"),
            "names the process host: {violations:?}"
        );
    }

    #[test]
    fn consumer_closure_allows_libc_via_sync_only() {
        // `wyrd-sync`'s `libc` is the custody boundary (`O_NOFOLLOW`
        // opens, euid ownership), not a presentation surface.
        let graph = link_graph(&[
            ("wyrd-embed", &["wyrd-core"], &[]),
            ("wyrd-core", &["wyrd-format", "wyrd-sync"], &["thiserror"]),
            ("wyrd-sync", &["wyrd-format"], &["thiserror", "libc"]),
            ("wyrd-format", &[], &["blake3", "thiserror"]),
        ]);
        assert!(
            check_consumer_link_graph("wyrd-embed", &graph).is_empty(),
            "custody libc must pass: {graph:?}"
        );
    }

    #[test]
    fn consumer_closure_rejects_libc_via_other_members() {
        // The exemption is member-scoped: `libc` anywhere but
        // `wyrd-sync` still bites.
        let graph = link_graph(&[
            ("wyrd-embed", &["wyrd-core"], &[]),
            (
                "wyrd-core",
                &["wyrd-format", "wyrd-sync"],
                &["thiserror", "libc"],
            ),
            ("wyrd-sync", &["wyrd-format"], &["thiserror"]),
            ("wyrd-format", &[], &["blake3", "thiserror"]),
        ]);
        let violations = check_consumer_link_graph("wyrd-embed", &graph);
        assert_eq!(violations.len(), 1, "libc via core only: {violations:?}");
        assert!(
            violations[0].contains("`libc` via `wyrd-core`"),
            "the presentation external names its declaring member: {violations:?}"
        );
    }

    #[test]
    fn consumer_clean_closure_passes() {
        let graph = link_graph(&[
            ("wyrd-embed", &["wyrd-view", "wyrd-core"], &[]),
            ("wyrd-view", &["wyrd-namespace"], &["thiserror"]),
            (
                "wyrd-core",
                &["wyrd-format", "wyrd-sync"],
                &["thiserror", "tokio"],
            ),
            (
                "wyrd-sync",
                &["wyrd-format"],
                &["iroh", "nostr", "thiserror"],
            ),
            ("wyrd-namespace", &["wyrd-format"], &["thiserror"]),
            ("wyrd-format", &[], &["blake3", "hex", "thiserror"]),
        ]);
        assert!(
            check_consumer_link_graph("wyrd-embed", &graph).is_empty(),
            "a presentation-free closure must pass, transport and all: {graph:?}"
        );
    }

    #[test]
    fn mint_count_passes_the_clean_pair() {
        let holders = [
            ("crates/wyrd-sync/src/durable/mod.rs".to_owned(), 1),
            ("crates/wyrd-namespace/src/view.rs".to_owned(), 1),
        ];
        assert!(
            check_proof_mint(&holders).is_empty(),
            "definition plus authority must pass"
        );
    }

    #[test]
    fn mint_count_rejects_a_second_mint() {
        let holders = [
            ("crates/wyrd-view/src/view/tests_harness.rs".to_owned(), 1),
            ("crates/wyrd-namespace/src/view.rs".to_owned(), 1),
            ("crates/wyrd-sync/src/durable/mod.rs".to_owned(), 1),
        ];
        let violations = check_proof_mint(&holders);
        assert_eq!(violations.len(), 1, "one rogue mint: {violations:?}");
        assert!(
            violations[0].contains("crates/wyrd-view/src/view/tests_harness.rs"),
            "names the rogue file: {violations:?}"
        );
    }

    #[test]
    fn mint_count_rejects_a_second_mint_in_the_authority_file() {
        // The likeliest real drift: another call inside the authority
        // file. File-granularity would miss it; counts do not.
        let holders = [
            ("crates/wyrd-namespace/src/view.rs".to_owned(), 1),
            ("crates/wyrd-sync/src/durable/mod.rs".to_owned(), 2),
        ];
        let violations = check_proof_mint(&holders);
        assert_eq!(violations.len(), 1, "double mint: {violations:?}");
        assert!(
            violations[0].contains("mints twice"),
            "names the failure mode: {violations:?}"
        );
    }

    #[test]
    fn mint_count_rejects_a_missing_definition() {
        let holders = [("crates/wyrd-sync/src/durable/mod.rs".to_owned(), 1)];
        let violations = check_proof_mint(&holders);
        assert_eq!(violations.len(), 1, "missing definition: {violations:?}");
        assert!(
            violations[0].contains("wyrd-namespace"),
            "names the definition site: {violations:?}"
        );
    }

    #[test]
    fn mint_count_rejects_a_missing_authority() {
        let holders = [("crates/wyrd-namespace/src/view.rs".to_owned(), 1)];
        let violations = check_proof_mint(&holders);
        assert_eq!(violations.len(), 1, "missing authority: {violations:?}");
        assert!(
            violations[0].contains("wyrd-sync"),
            "names the authority site: {violations:?}"
        );
    }

    #[test]
    fn mint_scanner_ignores_comments_and_strings() {
        let files = [
            (
                "crates/wyrd-view/src/view/head.rs",
                "// mint via from_verified_unchecked is forbidden",
            ),
            (
                "crates/wyrd-sync/src/durable/mod.rs",
                "let name = \"from_verified_unchecked\";",
            ),
        ];
        assert!(
            mint_holding_files(&files).is_empty(),
            "comments and strings are not mints"
        );
    }

    #[test]
    fn fuse_dev_dep_widening_is_rejected() {
        let violations = check_view_dev_deps(&deps(&["wyrd-core", "tokio"]), &empty_ws());
        assert_eq!(violations.len(), 1, "widened dev-deps: {violations:?}");
        assert!(
            violations[0].contains("tokio"),
            "names the added edge: {violations:?}"
        );
    }

    #[test]
    fn fuse_dev_dep_pin_passes_exact() {
        let violations = check_view_dev_deps(&deps(&["wyrd-core"]), &empty_ws());
        assert!(
            violations.is_empty(),
            "exactly wyrd-core must pass: {violations:?}"
        );
    }

    #[test]
    fn host_transport_check_rejects_a_transport_path() {
        let files = [(
            "crates/wyrd-host/src/host.rs",
            "use wyrd_sync::transport::mailbox::Mailbox;",
        )];
        let violations = check_host_transport_free(&files);
        assert_eq!(violations.len(), 1, "transport path: {violations:?}");
        assert!(
            violations[0].contains("transport::"),
            "names the marker: {violations:?}"
        );
    }

    #[test]
    fn host_transport_check_rejects_an_iroh_ident() {
        let files = [(
            "crates/wyrd-host/src/main.rs",
            "let endpoint = IrohBulkSource::connect_default();",
        )];
        let violations = check_host_transport_free(&files);
        assert_eq!(violations.len(), 1, "iroh ident: {violations:?}");
    }

    #[test]
    fn host_transport_check_ignores_comments_and_strings() {
        let files = [
            (
                "crates/wyrd-host/src/lib.rs",
                "// the host names no wyrd_sync transport machinery",
            ),
            (
                "crates/wyrd-host/src/run.rs",
                "let note = \"serving installs on the loop\";",
            ),
        ];
        assert!(
            check_host_transport_free(&files).is_empty(),
            "comments and strings are not transport"
        );
    }

    #[test]
    fn host_transport_check_passes_the_durable_side() {
        let files = [(
            "crates/wyrd-host/src/host.rs",
            "use wyrd_sync::{durable::DurableError, runtime::{Engine, EngineError}};",
        )];
        assert!(
            check_host_transport_free(&files).is_empty(),
            "the durable side is the host's own vocabulary"
        );
    }

    #[test]
    fn owner_composition_check_rejects_a_direct_runner_call() {
        let files = [(
            "crates/wyrd-cli/src/commands/vault.rs",
            "let outcome = run_vault(run);",
        )];
        let violations = check_owner_runs_through_daemon(&files);
        assert_eq!(violations.len(), 1, "direct runner call: {violations:?}");
        assert!(
            violations[0].contains("run_vault("),
            "names the marker: {violations:?}"
        );
    }

    #[test]
    fn owner_composition_check_rejects_a_runner_struct_use() {
        let files = [(
            "crates/wyrd-host/src/run.rs",
            "use wyrd_daemon::{run_vault, VaultRun};",
        )];
        let violations = check_owner_runs_through_daemon(&files);
        assert_eq!(violations.len(), 1, "runner struct use: {violations:?}");
        assert!(
            violations[0].contains("VaultRun"),
            "names the marker: {violations:?}"
        );
    }

    #[test]
    fn owner_composition_check_ignores_comments_and_strings() {
        let files = [
            (
                "crates/wyrd-cli/src/commands/vault.rs",
                "// run_vault used to live here, now shared",
            ),
            (
                "crates/wyrd-host/src/run.rs",
                "let note = \"VaultRun carries the live node\";",
            ),
        ];
        assert!(
            check_owner_runs_through_daemon(&files).is_empty(),
            "comments and strings are not composition"
        );
    }

    #[test]
    fn owner_composition_check_passes_the_session_supervisor() {
        // The mount's session-supervised loop is the legitimate
        // second supervisor caller: its session join bounds
        // destroy's submits, so the headless check must not fire
        // on it. Only the headless runner is pinned here.
        let files = [(
            "crates/wyrd-cli/src/commands/mount.rs",
            "let drive = supervisor.spawn_loop(live, mailbox, bulk, config, trigger_tx, on_error);",
        )];
        assert!(
            check_owner_runs_through_daemon(&files).is_empty(),
            "the session supervisor is not the headless runner"
        );
    }

    #[test]
    fn owner_composition_check_passes_event_vocabulary() {
        // Naming the outcome is observing the run, not owning the
        // composition: outcome, event, and loop-end types stay free.
        let files = [(
            "crates/wyrd-host/src/run.rs",
            "fn exit_code(outcome: &VaultOutcome) -> i32 {",
        )];
        assert!(
            check_owner_runs_through_daemon(&files).is_empty(),
            "outcome vocabulary is not composition"
        );
    }

    #[test]
    fn resolution_check_rejects_a_selection_construction() {
        let files = [(
            "crates/wyrd-daemon/src/fuse/backend.rs",
            "let selection = MergeSelection::Take(id);",
        )];
        let violations = check_no_surface_authors_resolutions(&files);
        assert_eq!(violations.len(), 1, "selection use: {violations:?}");
        assert!(
            violations[0].contains("MergeSelection"),
            "names the marker: {violations:?}"
        );
    }

    #[test]
    fn resolution_check_rejects_a_merge_heads_call() {
        let files = [(
            "crates/wyrd-host/src/run.rs",
            "let snapshot = engine.merge_heads(map)?;",
        )];
        let violations = check_no_surface_authors_resolutions(&files);
        assert_eq!(violations.len(), 1, "merge call: {violations:?}");
        assert!(
            violations[0].contains("merge_heads"),
            "names the marker: {violations:?}"
        );
    }

    #[test]
    fn resolution_check_ignores_comments_and_strings() {
        let files = [
            (
                "crates/wyrd-view/src/view/drive.rs",
                "// MergeSelection lives in the engine, not here",
            ),
            (
                "crates/wyrd-cli/src/commands/mount.rs",
                "let note = \"merge_heads resolves both parents\";",
            ),
        ];
        assert!(
            check_no_surface_authors_resolutions(&files).is_empty(),
            "comments and strings are not authoring"
        );
    }

    #[test]
    fn resolution_check_passes_snapshot_id_vocabulary() {
        // Naming a snapshot is reading history, not authoring a
        // resolution: ids and plans stay free.
        let files = [(
            "crates/wyrd-cli/src/commands/snapshot.rs",
            "fn resolve_plan(plan: &MergePlan, id: SnapshotId) {",
        )];
        assert!(
            check_no_surface_authors_resolutions(&files).is_empty(),
            "snapshot vocabulary is not authoring"
        );
    }
}

/// The drive-owner interface's production surface, for the three
/// source checks below: every `.rs` file under the interface
/// module, tests included (the tests must not construct a mailbox
/// either — the fake implements the trait, it never connects one).
fn owner_module_files() -> Vec<(String, String)> {
    let root = workspace_root();
    let dir = root.join("crates/wyrd-daemon/src/owner");
    let (files, errors) = collect_rs_files(&dir, &root);
    assert!(
        errors.is_empty(),
        "owner module source walk errors:\n- {}",
        errors.join("\n- ")
    );
    assert!(!files.is_empty(), "the owner module holds sources to check");
    files
}

/// The interface exposes no store internals (OD-16-C's closure
/// rule): no operation accepts or returns a `DurableStore`, a
/// `RuntimeState`, an `Engine`, or a `wyrd_sync::runtime` type. The
/// check is textual over the module — the forbidden names cannot
/// appear in signatures, bodies, or imports — and the server's
/// construction from shared handles (`OwnerHandles::from_parts`)
/// pins the shape by compilation.
#[test]
fn interface_exposes_no_store_internals() {
    const FORBIDDEN: &[&str] = &["DurableStore", "RuntimeState", "Engine<"];
    let files = owner_module_files();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let mut violations = Vec::new();
    for (path, text) in &borrowed {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names store internals (`{marker}`): the interface serves shared \
                         projections and loop-published slots, never the store or the engine",
                        index + 1
                    ));
                }
            }
        }
    }
    // `wyrd_sync::runtime` types ride the same rule on the
    // operation surface (the server, its handles, and its auth):
    // no operation there names the module. The wire mapping
    // (`wire.rs`) is exempt by design — mapping a value requires
    // naming its source type, and the `From` impls are Rust-side
    // conversion conveniences, invisible on the wire. The tests are
    // exempt because a test composer needs an engine to build a
    // drive worth serving.
    for (path, text) in &borrowed {
        let file = path.rsplit('/').next().unwrap_or(path);
        if file == "wire.rs" || file == "tests_owner.rs" {
            continue;
        }
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            if line.contains("sync::runtime::") || line.contains("sync::runtime,") {
                violations.push(format!(
                    "{path}:{} names `wyrd_sync::runtime` (`{}`): the interface closure stays free \
                     of it",
                    index + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "store-internal leaks:\n- {}",
        violations.join("\n- ")
    );
}

/// The interface is not a second engine: no call site for snapshot
/// authoring, carry, admission, or announcement anywhere in the
/// module. Forwarding a submission to the single queue is the
/// interface's job (child 2); interpreting it is not, and these
/// four names are the interpretation vocabulary. The shipped
/// surface carries this gate; the test file is exempt by name —
/// fixtures compose drives through the engine API exactly like any
/// composer does, and never implement engine semantics.
#[test]
fn the_interface_is_not_a_second_engine() {
    const FORBIDDEN: &[&str] = &[
        "author_snapshot",
        "carry_pending",
        "admit_device",
        "announce_snapshot",
    ];
    let files = owner_module_files();
    let shipped: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .filter(|(path, _)| !path.ends_with("tests_owner.rs"))
        .collect();
    assert!(
        !shipped.is_empty(),
        "the shipped surface holds sources to check"
    );
    let mut violations = Vec::new();
    for (path, text) in shipped {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names the engine vocabulary (`{marker}`): the interface forwards \
                         to the queue, it never implements the engine",
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "second-engine leaks:\n- {}",
        violations.join("\n- ")
    );
}

/// The read side never constructs a mailbox: no live mailbox type,
/// no seen store, no health sampling, no trait use beyond the test
/// fake's implementation. The behavioral half is
/// `interface_read_side_never_constructs_a_mailbox` in the daemon;
/// this test is the source check that makes the status contract's
/// rule (`status.rs:108-114`) a gate rather than a note.
#[test]
fn interface_read_side_never_constructs_a_mailbox() {
    const FORBIDDEN: &[&str] = &[
        "LiveMailbox",
        "SeenStore",
        "health_snapshot",
        "Mailbox::",
        ".send(",
        ".recv(",
        ".settle(",
        ".connect(",
    ];
    let files = owner_module_files();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let mut violations = Vec::new();
    for (path, text) in borrowed {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} reaches the mailbox (`{marker}`): the read side forwards the \
                         loop's observations, it never touches a mailbox",
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "mailbox construction leaks:\n- {}",
        violations.join("\n- ")
    );
}
