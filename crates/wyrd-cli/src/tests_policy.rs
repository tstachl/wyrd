//! End-to-end policy commands: init, author,
//! policy pin/unpin/evict/status/budgets and policy role
//! set/status through the real `command` entrypoint,
//! proving the wiring the core and daemon tests prove the semantics
//! of. Policy assertions reopen the engine from custody, the way a
//! second CLI invocation would.

use super::tests_harness::{write_secret, TempDir};
use super::*;
use wyrd_format::{Entry, ObjectKind, ObjectStore, Tree};

/// A drive with two authored files, plus the credential flags every
/// policy command needs.
fn authored_drive() -> (TempDir, Vec<String>) {
    let temp = TempDir::new();
    let identity_file = temp.0.join("identity");
    let passphrase_file = temp.0.join("passphrase");
    let drive = temp.0.join("drive");
    write_secret(&identity_file, [0x11; 32]);
    write_secret(&passphrase_file, b"test-pass\n");
    let creds = vec![
        "--identity-file".into(),
        identity_file.display().to_string(),
        "--passphrase-file".into(),
        passphrase_file.display().to_string(),
    ];
    command(vec![
        "init".into(),
        drive.display().to_string(),
        "--identity-file".into(),
        identity_file.display().to_string(),
        "--passphrase-file".into(),
        passphrase_file.display().to_string(),
    ])
    .unwrap();
    let identity = read_identity(&identity_file).unwrap();
    let mut engine = Engine::open_keystore(drive.clone(), "test-pass", identity).unwrap();
    let mut store = FsObjectStore::open(drive.clone()).unwrap();
    let a = store.insert(ObjectKind::Chunk, b"alpha bytes").unwrap();
    let b = store.insert(ObjectKind::Chunk, b"beta bytes").unwrap();
    let root = Tree::from_entries(vec![
        Entry::file("a.txt", 11, false, vec![a]).unwrap(),
        Entry::file("b.txt", 10, false, vec![b]).unwrap(),
    ])
    .unwrap()
    .insert_into(&mut store)
    .unwrap();
    engine.author_snapshot(&store, root).unwrap();
    (temp, creds)
}

fn drive_of(temp: &TempDir) -> String {
    temp.0.join("drive").display().to_string()
}

#[test]
fn pin_status_and_policy_roundtrip() {
    let (temp, creds) = authored_drive();
    let drive = drive_of(&temp);
    let mut pin = vec!["policy".into(), drive.clone(), "pin".into(), "a.txt".into()];
    pin.extend(creds.clone());
    command(pin).unwrap();
    // A second invocation observes the durable promise. The
    // credential flags ride each policy action, so they follow the
    // action's own positional path.
    let mut status = vec![
        "policy".into(),
        drive.clone(),
        "status".into(),
        "a.txt".into(),
    ];
    status.extend(creds.clone());
    command(status).unwrap();
    let mut budgets = vec!["policy".into(), drive.clone(), "budgets".into()];
    budgets.extend(creds);
    command(budgets).unwrap();
    drop(temp);
}

/// `policy budgets` measures a real drive end to end: the object-store
/// walk, the fact-log walk, and the vault walk all run against the
/// authored drive without error. The rendered numbers are asserted in
/// `policy_renderers_name_counts_and_quadrants`; this pins the wiring
/// the renderers ride.
#[test]
fn policy_measures_a_real_drive() {
    let (temp, creds) = authored_drive();
    let drive = drive_of(&temp);
    let mut budgets = vec!["policy".into(), drive.clone(), "budgets".into()];
    budgets.extend(creds);
    command(budgets).unwrap();
    drop(temp);
}

#[test]
fn evict_refuses_pins_until_unpinned() {
    let (temp, creds) = authored_drive();
    let drive = drive_of(&temp);
    let mut pin = vec!["policy".into(), drive.clone(), "pin".into(), "a.txt".into()];
    pin.extend(creds.clone());
    command(pin).unwrap();
    // Same-path evict refuses while pinned.
    let mut evict = vec![
        "policy".into(),
        drive.clone(),
        "evict".into(),
        "a.txt".into(),
    ];
    evict.extend(creds.clone());
    let error = command(evict).unwrap_err();
    assert!(
        matches!(error, CliError::Policy(_)),
        "evict must refuse pinned content, got {error:?}"
    );
    let mut unpin = vec![
        "policy".into(),
        drive.clone(),
        "unpin".into(),
        "a.txt".into(),
    ];
    unpin.extend(creds.clone());
    command(unpin).unwrap();
    let mut evict = vec![
        "policy".into(),
        drive.clone(),
        "evict".into(),
        "a.txt".into(),
    ];
    evict.extend(creds);
    command(evict).unwrap();
    drop(temp);
}

#[test]
fn pin_missing_path_names_the_failure() {
    let (temp, creds) = authored_drive();
    let drive = drive_of(&temp);
    let mut pin = vec![
        "policy".into(),
        drive.clone(),
        "pin".into(),
        "nope.txt".into(),
    ];
    pin.extend(creds);
    let error = command(pin).unwrap_err();
    assert!(
        matches!(error, CliError::Policy(_)),
        "missing paths fail as policy errors, got {error:?}"
    );
    drop(temp);
}

#[test]
fn policy_renderers_name_counts_and_quadrants() {
    let report = wyrd_core::policy::PinReport {
        files: 2,
        dirs: 1,
        symlinks_skipped: 1,
        pinned: 3,
        already_pinned: 1,
    };
    let rendered = pin_render("docs", &report);
    assert!(rendered.contains("pinned 3 objects (1 already pinned)"));
    assert!(rendered.contains("2 files"));
    assert!(rendered.contains("1 symlinks skipped"));
    let census = ResidencyCensus {
        files: vec![wyrd_core::policy::FileResidency {
            path: "a.txt".into(),
            chunks: Vec::new(),
            pinned_chunks: Vec::new(),
            policy: RetentionPolicy::Pinned,
            local: LocalPresence::Present,
        }],
        conflicts: vec!["split".into()],
        dirs: 0,
        symlinks_skipped: 0,
    };
    let rendered = policy_status_render("", "docs", &census);
    assert!(rendered.contains("PINNED"));
    assert!(rendered.contains("PRESENT"));
    assert!(rendered.contains("a.txt"));
    assert!(rendered.contains("CONFLICT"));
    assert!(rendered.contains("split"));
    // One posture block, one census: the block the status report
    // prepends is the same renderer `policy role status` composes
    // with the quadrant totals, so the two projections cannot drift.
    let state = wyrd_core::role::RoleState {
        role: wyrd_core::role::Role::Vault,
        transition: wyrd_core::role::Transition::Settled,
    };
    let block = role_section_render(std::path::Path::new("/nowhere"), state, 0);
    assert!(block.contains("role: vault"));
    assert!(block.contains("effective: vault"));
    let composed = policy_status_render(&block, "docs", &census);
    assert!(
        composed.starts_with(&block) && composed.contains("policy status under docs"),
        "the posture block prefixes the table: {composed}"
    );
    let role_only = role_status_render(std::path::Path::new("/nowhere"), state, &census, 0);
    assert!(
        role_only.contains("role: vault") && role_only.contains("files: 1 ("),
        "the role report is the block plus the same census: {role_only}"
    );
    let accounting = RetentionAccounting {
        retained_content: 812,
        fact_log: 37,
        sync_vault: 124,
        quota: None,
        refusals: (0, Vec::new()),
    };
    let budgets = LiveConfig::for_local_sync().budgets;
    let rendered = policy_budgets_render(&census, &accounting, &budgets);
    assert!(rendered.contains("pinned files: 1"));
    // DG-5: no ceiling is stated plainly — never "unlimited" (a
    // ceiling claim by another name), never 0 as a ceiling.
    assert!(
        rendered.contains("retained_bytes_quota: under these offline defaults, no retention ceiling is set on this device"),
        "unset quota must state the absence, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("unlimited"),
        "no ceiling claim by another name:\n{rendered}"
    );
    // OD-26-B option B: the report names each dimension and labels
    // whether it backs enforcement or merely observes. A single total
    // known to be low is not acceptable output.
    assert!(rendered.contains(
        "retained content: 812 (quota-enforced; object store only, not the fact log or sync vault)"
    ));
    assert!(rendered.contains("fact log: 37 (observational)"));
    assert!(rendered.contains("sync vault: 124 (observational)"));
    assert!(rendered.contains("total accounted: 973 (observational;"));
    // OD-26-A option C: with no configured ceiling the report advises
    // one, explicitly non-authoritative — never a silent default.
    assert!(rendered.contains("advisory ceiling (non-authoritative)"));
    assert!(
        rendered.contains("conflicts: 1 skipped (never walked):\n  split"),
        "policy totals name the subtrees they never walked"
    );
}

#[test]
fn role_set_status_roundtrip_survives_reinvocation() {
    use wyrd_core::role::{self, Role};

    let (temp, creds) = authored_drive();
    let drive = drive_of(&temp);
    // A fresh device names the mirror default explicitly, never silent.
    let mut status = vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "status".into(),
    ];
    status.extend(creds.clone());
    command(status).unwrap();
    let state = role::load(std::path::Path::new(&drive)).unwrap();
    assert_eq!(state.role, Role::Mirror);
    assert!(!role::is_configured(std::path::Path::new(&drive)));
    // Widening is immediate and durable — and needs no credentials,
    // so a role is settable with the keystore unavailable. A second
    // invocation — the restart equivalent, with no in-memory state
    // carried over — observes the same role from the file.
    command(vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "set".into(),
        "vault".into(),
    ])
    .unwrap();
    let mut status = vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "status".into(),
    ];
    status.extend(creds.clone());
    command(status).unwrap();
    let state = role::load(std::path::Path::new(&drive)).unwrap();
    assert_eq!(state.role, Role::Vault);
    assert_eq!(state.effective(), Role::Vault);
    drop(temp);
}

#[test]
fn role_set_rejects_garbage() {
    let (temp, _creds) = authored_drive();
    let drive = drive_of(&temp);
    let error = command(vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "set".into(),
        "archivist".into(),
    ])
    .unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "unknown roles fail as usage errors, got {error:?}"
    );
    assert!(
        error.to_string().contains("want \"mirror\" or \"vault\""),
        "the real role parse refuses — not clap's unknown-subcommand error: {error}"
    );
    drop(temp);
}

#[test]
fn narrowing_requires_explicit_acknowledgement() {
    use wyrd_core::role::{self, Role, Transition};

    let (temp, _creds) = authored_drive();
    let drive = drive_of(&temp);
    command(vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "set".into(),
        "vault".into(),
    ])
    .unwrap();
    // Leaving vault without --yes refuses: stopping being a
    // provider must be explicit.
    let error = command(vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "set".into(),
        "mirror".into(),
    ])
    .unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "unacknowledged narrowing must refuse, got {error:?}"
    );
    // Acknowledged narrowing records intent but keeps vault
    // effective: acknowledgement is not observability.
    command(vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "set".into(),
        "mirror".into(),
        "--yes".into(),
    ])
    .unwrap();
    let state = role::load(std::path::Path::new(&drive)).unwrap();
    assert_eq!(state.role, Role::Mirror);
    assert_eq!(state.transition, Transition::NarrowingAcknowledged);
    assert_eq!(state.effective(), Role::Vault);
    // Re-narrowing an acknowledged transition names the way out
    // instead of silently succeeding: widen first, then narrow.
    let error = command(vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "set".into(),
        "mirror".into(),
        "--yes".into(),
    ])
    .unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "re-narrowing must name the escape hatch, got {error:?}"
    );
    drop(temp);
}

#[test]
fn evict_refuses_under_vault() {
    let (temp, creds) = authored_drive();
    let drive = drive_of(&temp);
    command(vec![
        "policy".into(),
        drive.clone(),
        "role".into(),
        "set".into(),
        "vault".into(),
    ])
    .unwrap();
    let mut evict = vec![
        "policy".into(),
        drive.clone(),
        "evict".into(),
        "a.txt".into(),
    ];
    evict.extend(creds);
    let error = command(evict).unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "evict under vault must refuse naming the role, got {error:?}"
    );
    drop(temp);
}

/// DG-5 acceptance test, first half: every figure the closed
/// vocabulary names maps to exactly one row of the normative table
/// in `docs/design/storage-growth.md`. A new figure without a table
/// row fails here before it can reach a screen unnamed — the table
/// is the contract, so this is the test that makes it one. The table
/// body is delimited by markers so prose elsewhere in the document
/// can never satisfy the match by accident.
#[test]
fn every_storage_figure_maps_to_a_dg5_row() {
    use wyrd_core::policy::{FigureKind, StorageFigure};

    let doc = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/design/storage-growth.md");
    let text = std::fs::read_to_string(&doc)
        .unwrap_or_else(|error| panic!("DG-5 table doc must be readable: {doc:?}: {error}"));
    let start = text
        .find("<!-- DG-5-TABLE-START -->")
        .expect("DG-5 table start marker");
    let end = text
        .find("<!-- DG-5-TABLE-END -->")
        .expect("DG-5 table end marker");
    assert!(start < end, "DG-5 table markers out of order");
    let table = &text[start..end];
    let table_lower = table.to_lowercase();
    for figure in StorageFigure::all() {
        let cell = format!("`{}`", figure.label());
        assert!(
            table.contains(&cell),
            "storage figure {figure:?} has no DG-5 table row (expected {cell:?} in the table)"
        );
        // The scope text mirrors the row's scope cell: meaning
        // cannot drift from the words while the label stays. Case
        // folds because the table's sentence case need not match the
        // code's fragment case — the words must match, not the caps.
        assert!(
            table_lower.contains(&figure.scope().to_lowercase()),
            "storage figure {figure:?} scope {:?} is not in its DG-5 row",
            figure.scope()
        );
        // The row's Kind cell is the kind word, not a passing
        // mention elsewhere in the row: split the pipe-delimited row
        // and compare the last cell exactly.
        let row = table
            .lines()
            .find(|line| line.contains(&cell))
            .unwrap_or_else(|| panic!("row for {figure:?} vanished between asserts"));
        let kind_cell = row
            .split('|')
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .next_back()
            .unwrap_or_default();
        assert_eq!(
            kind_cell,
            figure.kind().as_str(),
            "storage figure {figure:?} kind cell is {kind_cell:?}, want {:?}",
            figure.kind().as_str()
        );
    }
    // Exactly one figure backs enforcement. Anything else presented
    // as enforced is the overstatement this contract exists to
    // prevent; two enforced figures would disagree about what the
    // ceiling compares against.
    let enforced = StorageFigure::all()
        .iter()
        .filter(|figure| figure.kind() == FigureKind::Enforced)
        .collect::<Vec<_>>();
    assert_eq!(
        enforced,
        vec![&StorageFigure::RetainedContent],
        "exactly one enforced figure: {enforced:?}"
    );
}

/// DG-5 acceptance test, second half: no forbidden phrase in any
/// rendered output, and no quota rendering without its refusal
/// scope. The list must include "free space", "disk usage", and
/// "storage used" — a substring check on "free space" alone would
/// miss the device-total claims — plus the bare-quota failure mode
/// the whole issue is about.
#[test]
fn storage_surface_never_says_free_space() {
    use wyrd_core::policy::{StorageFigure, FORBIDDEN_STORAGE_WORDS};

    // The list itself is pinned: the issue requires "free space",
    // "disk usage", and "storage used", and a list that silently
    // shrank to one substring would pass the loop below while missing
    // the device-total claims.
    assert_eq!(
        FORBIDDEN_STORAGE_WORDS,
        &["free space", "disk usage", "storage used"]
    );

    let census = ResidencyCensus {
        files: vec![wyrd_core::policy::FileResidency {
            path: "a.txt".into(),
            chunks: Vec::new(),
            pinned_chunks: Vec::new(),
            policy: RetentionPolicy::Pinned,
            local: LocalPresence::Present,
        }],
        conflicts: vec!["split".into()],
        dirs: 1,
        symlinks_skipped: 1,
    };
    let budgets = LiveConfig::for_local_sync().budgets;
    let quota_none = RetentionAccounting {
        retained_content: 812,
        fact_log: 37,
        sync_vault: 124,
        quota: None,
        refusals: (0, Vec::new()),
    };
    let quota_some = RetentionAccounting {
        quota: Some(1024),
        refusals: (2, vec![1024]),
        ..quota_none
    };
    let status = policy_status_render("", "docs", &census);
    let rendered_none = policy_budgets_render(&census, &quota_none, &budgets);
    let rendered_some = policy_budgets_render(&census, &quota_some, &budgets);
    // The action receipts are storage surfaces too: `evict_render`'s
    // note rides beside byte-adjacent words ("no bytes deleted"), so
    // it is scanned like the reports above rather than trusted by
    // inspection.
    let evicted = evict_render(
        "docs",
        &wyrd_core::policy::EvictReport {
            files: 1,
            dirs: 1,
            symlinks_skipped: 0,
            released: 2,
            already_remote: 0,
        },
    );
    for output in [&status, &rendered_none, &rendered_some, &evicted] {
        let lower = output.to_lowercase();
        for forbidden in FORBIDDEN_STORAGE_WORDS {
            assert!(
                !lower.contains(*forbidden),
                "forbidden phrase {forbidden:?} in storage output:\n{output}"
            );
        }
    }
    // The bare-quota failure mode: a `retained_bytes_quota` number
    // with no refusal scope. The scope rides the same sentence, so
    // assert the sentence, not just the label, on both faces of the
    // one flag — the receive ceiling is a second rendering of the
    // same number and must carry scope too.
    assert!(
        rendered_some.contains(
            "retained_bytes_quota: 1024 (operator-selected refusal boundary over retained content only"
        ),
        "a configured quota must carry its refusal scope:\n{rendered_some}"
    );
    assert!(
        rendered_some.contains(
            "receive_retention_ceiling: 1024 (same flag, fetch admission before storage)"
        ),
        "the receive face must carry its scope too:\n{rendered_some}"
    );
    // An unset quota states the absence (OD-22-A recommendation (A)):
    // never "unlimited", never a zero that would read as protection.
    assert!(
        rendered_none.contains("no retention ceiling is set on this device"),
        "unset quota must state the absence:\n{rendered_none}"
    );
    assert!(
        !rendered_none.contains("unlimited"),
        "no ceiling claim by another name:\n{rendered_none}"
    );
    // A measured zero stays a measurement next to an explicit
    // no-ceiling statement — never protection, never "not measured"
    // confusion: the offline walk always measures, so zero means
    // empty, and the absence that matters is the ceiling's.
    let empty = RetentionAccounting {
        retained_content: 0,
        fact_log: 0,
        sync_vault: 0,
        quota: None,
        refusals: (0, Vec::new()),
    };
    let rendered_empty = policy_budgets_render(&census, &empty, &budgets);
    assert!(
        rendered_empty.contains("retained content: 0 (quota-enforced;")
            && rendered_empty.contains("no retention ceiling is set on this device"),
        "a measured zero with no ceiling must read as empty, not protected:\n{rendered_empty}"
    );
    // Every label the renderer emits for a figure is the contract's
    // label: spot-check the census rows the budgets report shares
    // with the status report.
    for label in [
        StorageFigure::PinnedFiles.label(),
        StorageFigure::QuadrantFiles.label(),
        StorageFigure::ConflictsSkipped.label(),
    ] {
        assert!(
            rendered_none.contains(label),
            "budgets report must use the contract label {label:?}:\n{rendered_none}"
        );
    }
}

/// The offline report names its budgets honestly: this command's
/// defaults, never a live composition's values. `policy budgets`
/// cannot observe a running mount's flags (the quota is not
/// persisted), so calling its numbers "effective" would let the
/// report disagree with a configured drive without either being
/// wrong. The live screen (issue 31) reads `LiveParts.budgets`; this
/// report states its source instead.
#[test]
fn storage_surface_names_its_budgets_offline_defaults() {
    let census = ResidencyCensus {
        files: Vec::new(),
        conflicts: Vec::new(),
        dirs: 0,
        symlinks_skipped: 0,
    };
    let accounting = RetentionAccounting {
        retained_content: 0,
        fact_log: 0,
        sync_vault: 0,
        quota: Some(1024),
        refusals: (0, Vec::new()),
    };
    let budgets = LiveConfig::for_local_sync().budgets;
    let rendered = policy_budgets_render(&census, &accounting, &budgets);
    assert!(
        rendered.contains("budgets (offline defaults"),
        "the report must name its budgets' source:\n{rendered}"
    );
    assert!(
        !rendered.contains("effective"),
        "an offline report must not claim effective budgets:\n{rendered}"
    );
}
