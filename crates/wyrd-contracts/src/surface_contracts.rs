//! Presentation-surface and host-composition scans: the host
//! links no transport above `wyrd-core`, owner flows run through
//! the daemon composer and the shared socket service, and no
//! surface authors a conflict resolution or offers a role choice.

use crate::contract_source::*;
use std::fs;

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

/// Check the two owner-mode composers bind the socket through the
/// shared service (P2, option A): the mount and the headless runner
/// each name `OwnerService::bind`, and neither hand-rolls the server
/// (`OwnerServer::new`, `bind_listener`, `serve_on`) ad hoc. The
/// service definition itself (`owner/server.rs`) and the interface
/// tests own those markers legitimately, so this checker runs only
/// on the two composer files — the filesystem test below pins the
/// file set, and the negative tests prove the checker bites.
/// Pure over the file set so the negatives run without touching the
/// filesystem.
fn check_owner_socket_through_shared_service(files: &[(&str, &str)]) -> Vec<String> {
    const AD_HOC: &[&str] = &["OwnerServer::new", "bind_listener", "serve_on("];
    let mut violations = Vec::new();
    for (path, text) in files {
        let code = code_text(text);
        let has_shared = code.contains("OwnerService::bind");
        if !has_shared {
            violations.push(format!(
                "{path} never binds the owner socket through the shared service \
                 (`OwnerService::bind`): mount and headless bind the same surface"
            ));
        }
        for (index, line) in code.lines().enumerate() {
            for marker in AD_HOC {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names the socket server directly (`{marker}`): bind \
                         through OwnerService::bind instead of hand-rolling the server",
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

/// Contract (P2, option A): both owner-mode composers bind the socket
/// through the shared service. Reads exactly the two composer files
/// — the mount command and the daemon's headless runner — and runs
/// the shared-service checker on them. The behavioral half is the
/// `mounted_owner_serves_socket_alongside_backend` daemon test, which
/// serves a client through the drive socket while the backend holds
/// the same arcs.
#[test]
fn owner_socket_goes_through_the_shared_service() {
    let root = workspace_root();
    let mut refs: Vec<(String, String)> = Vec::new();
    for file in [
        "crates/wyrd-cli/src/commands/mount.rs",
        "crates/wyrd-daemon/src/vault.rs",
    ] {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|_| panic!("composer file {file} reads"));
        refs.push((file.to_owned(), text));
    }
    let borrowed: Vec<(&str, &str)> = refs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let violations = check_owner_socket_through_shared_service(&borrowed);
    assert!(
        violations.is_empty(),
        "owner-socket composition violations:\n- {}",
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

#[cfg(test)]
mod policy_tests {
    use super::*;
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
    fn owner_socket_check_rejects_a_hand_rolled_server() {
        let files = [(
            "crates/wyrd-cli/src/commands/mount.rs",
            "let server = OwnerServer::new(handles);\nlet _svc = OwnerService::bind(handles, &dir);",
        )];
        let violations = check_owner_socket_through_shared_service(&files);
        assert_eq!(violations.len(), 1, "hand-rolled server: {violations:?}");
        assert!(
            violations[0].contains("OwnerServer::new"),
            "names the marker: {violations:?}"
        );
    }

    #[test]
    fn owner_socket_check_rejects_a_missing_bind() {
        let files = [(
            "crates/wyrd-daemon/src/vault.rs",
            "let server = OwnerServer::new(handles);",
        )];
        let violations = check_owner_socket_through_shared_service(&files);
        assert!(
            violations.iter().any(|v| v.contains("OwnerService::bind")),
            "a composer that never binds through the service fails: {violations:?}"
        );
    }

    #[test]
    fn owner_socket_check_ignores_comments_and_strings() {
        let files = [
            (
                "crates/wyrd-cli/src/commands/mount.rs",
                "// OwnerServer::new used to live here, now shared\nlet _svc = OwnerService::bind(h, &d);",
            ),
            (
                "crates/wyrd-daemon/src/vault.rs",
                "let note = \"bind_listener reclaims the stale socket\";\nlet _svc = OwnerService::bind(h, &d);",
            ),
        ];
        assert!(
            check_owner_socket_through_shared_service(&files).is_empty(),
            "comments and strings are not socket composition"
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
