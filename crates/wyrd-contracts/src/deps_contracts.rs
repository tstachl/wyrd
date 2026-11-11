//! The workspace dependency DAG is a load-bearing architectural
//! invariant: layers depend downward only, and every member's
//! production edges plus the member-declared link-graph closures
//! fail closed on anything the policy does not know. The
//! verification-proof mint count and the view dev-dependency pin
//! live here as well: both guard declared edges, not source.

use crate::contract_source::*;
use crate::nostr_scope_contracts::check_core_nostr_scope;
use std::collections::{BTreeMap, BTreeSet};

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
        // The drive-owner client: owned protocol types and the
        // non-owning socket client. Domain-free by construction —
        // the daemon performs every domain-to-wire conversion — so
        // desktop and future consumers link the interface without
        // the authoring surface. No `wyrd-*` edge at all, not even
        // `wyrd-format`: identifiers cross as hex strings, never as
        // content types.
        "wyrd-owner-client" => Some(MemberPolicy {
            workspace_allow: &[],
            external_allow: &["hex", "serde", "serde_json"],
        }),
        // The composer and process host: everything below, never the CLI
        // (presentation depends on the host's public surface, not the
        // reverse) and never the contract suite. The posture sidecar
        // (22c) lives here because every composer writes it and every
        // lock-refused starter reads it — `serde`/`serde_json` are the
        // sidecar schema and encoding, nothing else may use them.
        "wyrd-daemon" => Some(MemberPolicy {
            workspace_allow: &[
                "wyrd-format",
                "wyrd-sync",
                "wyrd-view",
                "wyrd-core",
                "wyrd-owner-client",
            ],
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
                "wyrd-owner-client",
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
        // The desktop management UI backend: a non-owning client of
        // the drive-owner interface. Forwards through the shared
        // `wyrd-owner-client` only — never the store, never the
        // authoring surface, never presentation. `tauri` is the
        // reserved toolkit edge for the command layer that lands
        // with the first view; `tauri-plugin-dialog` is the native
        // file picker for credential and invitation paths (first
        // shipped view needing it: runtime provisioning — paths
        // travel, never secret contents); `serde`/`serde_json` are
        // the typed IPC answers, `thiserror` the backend error plumbing.
        // Every other direct workspace edge — `wyrd-core`,
        // `wyrd-sync`, `wyrd-view`, `wyrd-format`, `wyrd-host`,
        // `wyrd-cli`, `wyrd-daemon` — is a violation, and the
        // transitive closure test below pins the reachability half.
        "wyrd-desktop" => Some(MemberPolicy {
            workspace_allow: &["wyrd-owner-client"],
            external_allow: &[
                "tauri",
                "tauri-plugin-dialog",
                "serde",
                "serde_json",
                "thiserror",
            ],
        }),
        _ => None,
    }
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
        // The desktop management UI backend: a non-owning client of
        // the drive-owner interface over `wyrd-owner-client`. No
        // views yet (stage 1); the closure test below is its
        // architectural proof.
        "crates/wyrd-desktop",
        // The drive-owner client: owned protocol types and the
        // non-owning socket client. Domain-free by extraction (the
        // closure test below is its proof); the daemon keeps the
        // server and every domain conversion.
        "crates/wyrd-owner-client",
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

/// Check one consumer member's transitive production closure against
/// the owner-client boundary: no workspace member reachable at all.
/// The client owns the protocol representation and the transport;
/// every domain conversion lives in the daemon, so even a
/// `wyrd-format` edge would be a conceptual leak, not just a build
/// edge. Pure over the edge sets so the negative tests below can
/// prove the checker bites without touching the filesystem.
fn check_owner_client_link_graph(
    member: &str,
    graph: &BTreeMap<String, MemberEdges>,
) -> Vec<String> {
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
    seen.remove(member);
    if seen.is_empty() {
        Vec::new()
    } else {
        let mut reached: Vec<&String> = seen.iter().collect();
        reached.sort();
        vec![format!(
            "`{member}` reaches workspace members through production edges ({}): the \
             owner-client boundary is domain-free — move the conversion into the daemon",
            reached
                .iter()
                .map(|next| witness_path(member, next, graph).join(" → "))
                .collect::<Vec<_>>()
                .join(", ")
        )]
    }
}

/// Check one consumer member's transitive production closure against
/// the desktop boundary: the only workspace member reachable is the
/// domain-free `wyrd-owner-client`. The desktop crate forwards
/// through the shared client and never opens a drive, so even a
/// `wyrd-format` edge would be a conceptual leak — and a
/// `wyrd-daemon` edge would re-link the composition (`desktop →
/// daemon → core`) the extraction removed. Pure over the edge sets
/// so the negative tests below can prove the checker bites without
/// touching the filesystem.
fn check_desktop_link_graph(member: &str, graph: &BTreeMap<String, MemberEdges>) -> Vec<String> {
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
    seen.remove(member);
    seen.remove("wyrd-owner-client");
    if seen.is_empty() {
        Vec::new()
    } else {
        let mut reached: Vec<&String> = seen.iter().collect();
        reached.sort();
        vec![format!(
            "`{member}` reaches workspace members beyond the owner client through production \
             edges ({}): the desktop boundary is the non-owning consumer surface — move the \
             conversion into the daemon or the protocol into the owner client",
            reached
                .iter()
                .map(|next| witness_path(member, next, graph).join(" → "))
                .collect::<Vec<_>>()
                .join(", ")
        )]
    }
}

/// Contract (desktop scaffold): `wyrd-desktop` is the non-owning
/// management UI backend, so its production-edge closure must reach
/// no workspace member but the domain-free `wyrd-owner-client` —
/// never `wyrd-core` or any authoring capability, even transitively.
/// Resolves every member's normal edges from the manifests
/// (dev-dependencies excluded, as in the direct check) and walks the
/// closure from the desktop crate. Test-only harnesses (real-owner
/// IPC proofs) legitimately reach the daemon through
/// `[dev-dependencies]`; production never does.
#[test]
fn desktop_link_graph_excludes_everything_but_the_owner_client() {
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
    assert!(
        graph.contains_key("wyrd-desktop"),
        "the desktop crate is a workspace member with a policy"
    );
    let violations = check_desktop_link_graph("wyrd-desktop", &graph);
    assert!(
        violations.is_empty(),
        "link-graph violations:\n- {}",
        violations.join("\n- ")
    );
}

/// Contract (desktop scaffold), extraction half: `wyrd-owner-client`
/// is the domain-free consumer surface the desktop closure test will
/// depend on, so its own production closure must reach no workspace
/// member at all — not even `wyrd-format`. The stage-1 closure
/// analysis demonstrated the need mechanically
/// (`desktop → daemon → core`); this test pins the extraction's
/// answer in the same member-declared-edge style as the view and
/// embed precedents.
#[test]
fn owner_client_link_graph_is_domain_free() {
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
    assert!(
        graph.contains_key("wyrd-owner-client"),
        "the owner-client crate is a workspace member with a policy"
    );
    let violations = check_owner_client_link_graph("wyrd-owner-client", &graph);
    assert!(
        violations.is_empty(),
        "link-graph violations:\n- {}",
        violations.join("\n- ")
    );
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::collections::BTreeMap;
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
    fn owner_client_closure_rejects_any_workspace_reachability_with_witness() {
        let graph = link_graph(&[
            ("wyrd-owner-client", &["wyrd-format"], &["serde"]),
            ("wyrd-format", &[], &["blake3", "thiserror"]),
        ]);
        let violations = check_owner_client_link_graph("wyrd-owner-client", &graph);
        assert_eq!(violations.len(), 1, "even wyrd-format: {violations:?}");
        assert!(
            violations[0].contains("wyrd-format"),
            "reachability names the target: {violations:?}"
        );
        assert!(
            violations[0].contains("wyrd-owner-client → wyrd-format"),
            "the witness path names every hop: {violations:?}"
        );
    }

    #[test]
    fn owner_client_closure_passes_domain_free() {
        let graph = link_graph(&[("wyrd-owner-client", &[], &["hex", "serde", "serde_json"])]);
        assert!(
            check_owner_client_link_graph("wyrd-owner-client", &graph).is_empty(),
            "no workspace edges must pass: {graph:?}"
        );
    }

    #[test]
    fn desktop_closure_rejects_daemon_composition_with_witness() {
        // The shape the extraction removed: `desktop → daemon →
        // core`. A direct `wyrd-daemon` edge fails even though the
        // daemon is a legitimate composer for owner-mode binaries —
        // the desktop is a consumer, not a composer.
        let graph = link_graph(&[
            ("wyrd-desktop", &["wyrd-daemon"], &["serde"]),
            (
                "wyrd-daemon",
                &["wyrd-core", "wyrd-owner-client"],
                &["serde", "tokio"],
            ),
            ("wyrd-core", &["wyrd-sync"], &["thiserror"]),
            ("wyrd-sync", &["wyrd-format"], &["iroh"]),
            ("wyrd-format", &[], &["blake3"]),
            ("wyrd-owner-client", &[], &["serde", "serde_json"]),
        ]);
        let violations = check_desktop_link_graph("wyrd-desktop", &graph);
        assert_eq!(
            violations.len(),
            1,
            "the daemon reachability: {violations:?}"
        );
        assert!(
            violations[0].contains("wyrd-daemon"),
            "reachability names the target: {violations:?}"
        );
        assert!(
            violations[0].contains("wyrd-desktop → wyrd-daemon"),
            "the witness path names every hop: {violations:?}"
        );
    }

    #[test]
    fn desktop_closure_rejects_core_even_without_the_daemon() {
        // The two clauses are independent: a `wyrd-core` edge fails
        // even with no daemon in the closure.
        let graph = link_graph(&[
            ("wyrd-desktop", &["wyrd-core", "wyrd-owner-client"], &[]),
            ("wyrd-core", &["wyrd-format"], &["thiserror"]),
            ("wyrd-format", &[], &["blake3"]),
            ("wyrd-owner-client", &[], &["serde", "serde_json"]),
        ]);
        let violations = check_desktop_link_graph("wyrd-desktop", &graph);
        assert_eq!(violations.len(), 1, "the core reachability: {violations:?}");
        assert!(
            violations[0].contains("wyrd-desktop → wyrd-core"),
            "names the member and the witness: {violations:?}"
        );
    }

    #[test]
    fn desktop_closure_passes_over_the_owner_client_only() {
        let graph = link_graph(&[
            (
                "wyrd-desktop",
                &["wyrd-owner-client"],
                &["serde", "serde_json"],
            ),
            ("wyrd-owner-client", &[], &["hex", "serde", "serde_json"]),
        ]);
        assert!(
            check_desktop_link_graph("wyrd-desktop", &graph).is_empty(),
            "the owner-client edge must pass: {graph:?}"
        );
    }

    #[test]
    fn desktop_direct_policy_rejects_every_prohibited_workspace_edge() {
        // The policy names each prohibited edge the way the
        // host-composition checks do: no store, no authoring, no
        // presentation, no suite, no sibling consumer.
        for edge in [
            "wyrd-core",
            "wyrd-sync",
            "wyrd-view",
            "wyrd-format",
            "wyrd-host",
            "wyrd-cli",
            "wyrd-daemon",
        ] {
            let violations = check_member("wyrd-desktop", &deps(&[edge]), &empty_ws());
            assert_eq!(
                violations.len(),
                1,
                "the {edge} edge must fail closed: {violations:?}"
            );
            assert!(
                violations[0].contains(edge),
                "the violation names the edge: {violations:?}"
            );
        }
        // The allowed surface passes: the shared client plus the
        // reserved toolkit and plumbing externals.
        assert!(
            check_member(
                "wyrd-desktop",
                &deps(&[
                    "wyrd-owner-client",
                    "tauri",
                    "serde",
                    "serde_json",
                    "thiserror"
                ]),
                &empty_ws()
            )
            .is_empty(),
            "the consumer surface must pass"
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
    fn view_dev_dep_widening_is_rejected() {
        let violations = check_view_dev_deps(&deps(&["wyrd-core", "tokio"]), &empty_ws());
        assert_eq!(violations.len(), 1, "widened dev-deps: {violations:?}");
        assert!(
            violations[0].contains("tokio"),
            "names the added edge: {violations:?}"
        );
    }

    #[test]
    fn view_dev_dep_pin_passes_exact() {
        let violations = check_view_dev_deps(&deps(&["wyrd-core"]), &empty_ws());
        assert!(
            violations.is_empty(),
            "exactly wyrd-core must pass: {violations:?}"
        );
    }
}
