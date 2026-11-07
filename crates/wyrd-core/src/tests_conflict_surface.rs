//! Verification tests for the conflict surface (OD-23-A/B/C/D). The
//! protocol facts it renders against (two-head divergence, merge-plan
//! shape, frozen hint, grammar both-branches readability) are pinned
//! where they ship — see the existing `test(conflicted)`,
//! `version_grammar_*`, `merged_dir_*`, and `frozen_hint_*` tests run
//! unchanged; here pins the surface's own honesty.

use std::collections::BTreeMap;

use wyrd_format::{ContentId, DeviceId, Entry, ObjectKind, SnapshotId};
use wyrd_namespace::view::{ConflictVersion, Node, ViewError};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::engine::SnapshotHead;
use wyrd_sync::runtime::{Engine, MergePath, MergePlan};

use super::{
    classify_node, describe_unavailable, inspect_differences, render_conflict_screen,
    render_freeze, render_indicator, render_inspection, BranchHead, DivergenceState, FreezeState,
    InspectionOutcome, NodeDivergence,
};

fn branch(byte: u8, epoch: u64, author_byte: u8) -> BranchHead {
    BranchHead {
        id: SnapshotId::from_bytes([byte; 32]),
        epoch,
        author: DeviceId::from_bytes([author_byte; 32]),
    }
}

fn heads_two() -> Vec<BranchHead> {
    vec![branch(0xA1, 3, 0x11), branch(0xB2, 3, 0x22)]
}

fn file_entry(name: &str, chunk_byte: u8) -> Entry {
    Entry::file(
        name,
        1,
        false,
        vec![ContentId::from_bytes([chunk_byte; 32])],
    )
    .unwrap()
}

fn plan_two_heads_disagreeing() -> (MergePlan, Vec<BranchHead>) {
    let branches = heads_two();
    let (a, b) = (branches[0].id, branches[1].id);
    let mut shared = BTreeMap::new();
    shared.insert(a, Some(file_entry("same.txt", 0x01)));
    shared.insert(b, Some(file_entry("same.txt", 0x01)));
    let mut duel = BTreeMap::new();
    duel.insert(a, Some(file_entry("duel.txt", 0x02)));
    duel.insert(b, Some(file_entry("duel.txt", 0x03)));
    let mut gone = BTreeMap::new();
    gone.insert(a, Some(file_entry("gone.txt", 0x04)));
    gone.insert(b, None);
    let plan = MergePlan {
        heads: vec![a, b],
        paths: vec![
            MergePath {
                path: "duel.txt".into(),
                versions: duel,
            },
            MergePath {
                path: "gone.txt".into(),
                versions: gone,
            },
            MergePath {
                path: "same.txt".into(),
                versions: shared,
            },
        ],
    };
    (plan, branches)
}

/// DG-3's negative invariant, executable: every action the surface
/// offers leaves the durable log byte-identical, the head set
/// unchanged, and no new snapshot id behind. The surface takes no
/// engine and nothing mutable, so no forked two-head fixture is needed
/// to prove this — exercising every action against a live engine and
/// asserting log-identity is the whole proof, and it fails only if
/// someone later adds an authoring affordance, which is the point.
#[test]
fn conflict_surface_authors_no_snapshot() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-core-conflict-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::generate().unwrap();
    let engine = Engine::create(dir.clone(), "core-test-pass", identity).unwrap();

    let revision_before = engine.current();
    let heads_before: Vec<SnapshotHead> = engine.snapshot_heads().unwrap();
    let tip_before = engine
        .membership_log()
        .known_state()
        .map(|tip| tip.transition_id);

    // Every action the surface offers, against a real engine.
    let branches = heads_two();
    let state = DivergenceState::from_branches(&branches);
    let _ = render_indicator(&state);
    let _ = render_freeze(&FreezeState::from_frozen_epoch(None));
    let _ = render_freeze(&FreezeState::from_frozen_epoch(Some(2)));
    let _ = classify_node(&Node::Dir {
        subtree: ContentId::from_bytes([0xDD; 32]),
    });
    let (plan, _) = plan_two_heads_disagreeing();
    let outcome = inspect_differences(&plan, &branches, 64);
    let _ = render_inspection(&outcome);
    let _ = describe_unavailable(&ViewError::NotMaterialized {
        content: ContentId::from_bytes([0xCC; 32]),
    });
    let _ = render_conflict_screen(&state, &FreezeState::Clear, Some(&outcome));

    assert_eq!(engine.current(), revision_before, "no commit landed");
    let heads_after: Vec<SnapshotHead> = engine.snapshot_heads().unwrap();
    assert_eq!(
        heads_after.iter().map(|head| head.id).collect::<Vec<_>>(),
        heads_before.iter().map(|head| head.id).collect::<Vec<_>>(),
        "head set unchanged"
    );
    assert_eq!(
        engine
            .membership_log()
            .known_state()
            .map(|tip| tip.transition_id),
        tip_before,
        "membership untouched"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// The three divergences produce three distinct surfaces, and a fresh
/// drive is none of them: path conflict (a `Conflict` node), a
/// `MergedDir` with no differing child path (the standing indicator
/// still diverges — directory-content divergence has no per-path
/// marker), and a frozen membership epoch.
#[test]
fn conflict_surface_distinguishes_the_three_divergences() {
    // 1. Path conflict: the node itself says so.
    let conflict = Node::Conflict {
        versions: vec![ConflictVersion {
            snapshot: SnapshotId::from_bytes([0xA1; 32]),
            node: Node::File {
                size: 1,
                executable: false,
                chunks: vec![],
            },
        }],
    };
    assert_eq!(classify_node(&conflict), NodeDivergence::PathConflict);

    // 2. Directory-content divergence: every head resolves a
    // directory, contents differ — no `Conflict` anywhere.
    let merged = Node::MergedDir {
        subtrees: vec![
            (
                SnapshotId::from_bytes([0xA1; 32]),
                ContentId::from_bytes([0xD1; 32]),
            ),
            (
                SnapshotId::from_bytes([0xB2; 32]),
                ContentId::from_bytes([0xD2; 32]),
            ),
        ],
    };
    assert_eq!(classify_node(&merged), NodeDivergence::DirectoryContent);

    // 3. Frozen membership epoch: not a node at all — a drive-level
    // freeze with its remedy named.
    let frozen = render_freeze(&FreezeState::Frozen { epoch: 2 });
    assert!(frozen.contains("frozen at epoch 2"));
    assert!(frozen.contains("member resolve"));

    // The three are not conflated: the frozen copy names no branch,
    // the node kinds name no epoch.
    let diverged = render_indicator(&DivergenceState::from_branches(&heads_two()));
    assert_ne!(diverged, frozen);
    assert!(!frozen.contains("branch:"));

    // A fresh drive with zero heads is none of the three.
    let fresh = DivergenceState::from_branches(&[]);
    assert_eq!(fresh, DivergenceState::Converged);
    let fresh_render = render_indicator(&fresh);
    assert!(fresh_render.contains("none observed"));
    assert!(!fresh_render.contains("diverged"));
    let single = DivergenceState::from_branches(&heads_two()[..1]);
    assert_eq!(single, DivergenceState::Converged);
}

/// OD-23-A as a test: no branch is ever named unreadably. No `@`
/// followed by a digit in any branch label or plan column the surface
/// emits (stored root paths pass through verbatim — see
/// `stored_names_render_verbatim` — so the scan below covers only
/// renderings with benign paths); every branch label carries author,
/// epoch, and snapshot facts; no label claims a stored name.
#[test]
fn conflict_surface_never_names_a_branch_unreadably() {
    let branches = heads_two();
    let state = DivergenceState::from_branches(&branches);
    let (plan, _) = plan_two_heads_disagreeing();
    let full = inspect_differences(&plan, &branches, 64);
    let partial = inspect_differences(&plan, &branches, 1);
    let renderings = [
        render_indicator(&state),
        render_indicator(&DivergenceState::Converged),
        render_freeze(&FreezeState::Clear),
        render_freeze(&FreezeState::Frozen { epoch: 2 }),
        render_inspection(&full),
        render_inspection(&partial),
        render_conflict_screen(&state, &FreezeState::Frozen { epoch: 2 }, Some(&full)),
        render_conflict_screen(&state, &FreezeState::Clear, None),
    ];
    for rendering in &renderings {
        assert!(
            !has_at_number(rendering),
            "no version numbering in any rendering:\n{rendering}"
        );
    }
    // Every diverged branch label carries the three facts.
    let indicator = render_indicator(&state);
    for branch in &branches {
        assert!(indicator.contains(&branch.short_id()), "short id present");
        assert!(indicator.contains(&format!("epoch {}", branch.epoch)));
        assert!(indicator.contains(&branch.author.to_string()));
    }
    // The plan columns label heads by facts, never by position: the
    // first column is not "head 1" and no column is a bare number.
    let inspected = render_inspection(&full);
    assert!(!inspected.contains("head 1") && !inspected.contains("head 2"));
}

/// The ordering guarantee behind the no-reimplementation test: the
/// inspection consumes the plan's order — path order ascending,
/// columns in plan-heads order — and never renumbers by SnapshotId
/// byte order. Heads given highest-first stay highest-first.
#[test]
fn conflict_surface_uses_the_plan_order_not_a_renumbering() {
    let low = branch(0x11, 3, 0x11);
    let high = branch(0xFF, 3, 0x22);
    // Plan heads highest-first: byte order would sort them the other way.
    let mut versions = BTreeMap::new();
    versions.insert(high.id, Some(file_entry("z.txt", 0x01)));
    versions.insert(low.id, None);
    let plan = MergePlan {
        heads: vec![high.id, low.id],
        paths: vec![MergePath {
            path: "z.txt".into(),
            versions,
        }],
    };
    let outcome = inspect_differences(&plan, &[low, high], 64);
    let rendered = render_inspection(&outcome);
    let high_pos = rendered.find(&high.short_id()).expect("high column");
    let low_pos = rendered.find(&low.short_id()).expect("low column");
    assert!(
        high_pos < low_pos,
        "columns follow plan-heads order, not byte order:\n{rendered}"
    );
    // Rows follow plan (ascending path) order.
    let (plan, branches) = plan_two_heads_disagreeing();
    let rendered = render_inspection(&inspect_differences(&plan, &branches, 64));
    let duel = rendered.find("duel.txt").expect("duel row");
    let gone = rendered.find("gone.txt").expect("gone row");
    assert!(duel < gone, "rows follow plan order:\n{rendered}");
    // Present-vs-absent is divergence data, not unavailability.
    assert!(rendered.contains("absent"));
    assert!(!rendered.contains("unavailable"));
}

/// OD-23-D as a test: an unreadable branch renders unavailable with
/// its metadata intact — never an empty document, never a zero-byte
/// file, never evidence of absence. Structural outcomes (absent,
/// conflicted, bad paths) are not availability failures and map to
/// `None`, so absence is never confused with unavailability.
#[test]
fn conflict_inspection_reports_unfetched_rather_than_empty() {
    let remote = describe_unavailable(&ViewError::NotMaterialized {
        content: ContentId::from_bytes([0xCC; 32]),
    })
    .expect("remote-only maps to unavailable");
    let stranded = describe_unavailable(&ViewError::Unavailable {
        content: ContentId::from_bytes([0xCC; 32]),
    })
    .expect("unreachable maps to unavailable");
    for copy in [&remote, &stranded] {
        assert!(copy.contains("unavailable"));
        assert!(copy.contains("metadata"), "metadata still stands:\n{copy}");
        for empty_claim in [
            "0 bytes",
            "empty document",
            "zero-byte",
            "no content",
            "holds nothing",
        ] {
            assert!(
                !copy.contains(empty_claim),
                "never an empty-branch claim ({empty_claim}):\n{copy}"
            );
        }
    }
    // Damaged representations are unavailable too, with the same honesty.
    assert!(describe_unavailable(&ViewError::Corrupt)
        .expect("corrupt maps")
        .contains("unavailable"));
    assert!(describe_unavailable(&ViewError::LostRepresentation {
        content: ContentId::from_bytes([0xCC; 32]),
        kind: ObjectKind::Chunk,
    })
    .expect("lost maps")
    .contains("unavailable"));
    // Structural outcomes are not availability failures.
    for structural in [
        ViewError::NotFound,
        ViewError::Conflict,
        ViewError::InvalidPath,
    ] {
        assert_eq!(
            describe_unavailable(&structural),
            None,
            "structural is not unavailable: {structural:?}"
        );
    }
}

/// The decider's extra acceptance condition: budget exhaustion yields
/// an explicitly incomplete result — totals that prove the set is not
/// complete — and never a partial set presented as the whole one. The
/// scope sentence rides every inspection rendering.
#[test]
fn inspection_budget_exhaustion_is_incomplete_never_partial_as_complete() {
    let (plan, branches) = plan_two_heads_disagreeing();
    // Two differing root paths ("duel.txt", "gone.txt"); budget 1.
    let partial = inspect_differences(&plan, &branches, 1);
    assert!(
        matches!(
            partial,
            InspectionOutcome::Incomplete {
                total_differing: 2,
                budget: 1,
                ..
            }
        ),
        "budget 1 over 2 differences is incomplete: {partial:?}"
    );
    let rendered = render_inspection(&partial);
    assert!(
        rendered.contains("root paths only"),
        "scope sentence:\n{rendered}"
    );
    assert!(
        rendered.contains("incomplete"),
        "incomplete marker:\n{rendered}"
    );
    assert!(
        rendered.contains("not the complete set"),
        "never partial-as-complete:\n{rendered}"
    );
    assert!(
        rendered.contains("1 of 2"),
        "totals prove incompleteness:\n{rendered}"
    );

    // Zero budget with differences present is incomplete, not empty.
    let zero = inspect_differences(&plan, &branches, 0);
    assert!(!zero.is_complete());
    let zero_render = render_inspection(&zero);
    assert!(zero_render.contains("incomplete"));

    // A budget that covers the set is complete and says nothing about
    // incompleteness.
    let whole = inspect_differences(&plan, &branches, 64);
    assert!(whole.is_complete());
    let whole_render = render_inspection(&whole);
    assert!(whole_render.contains("root paths only"));
    assert!(!whole_render.contains("incomplete"));

    // An agreeing drive is completely empty of differences — and that
    // completeness is stated, not implied.
    let agreed_plan = MergePlan {
        heads: vec![branches[0].id, branches[1].id],
        paths: vec![MergePath {
            path: "same.txt".into(),
            versions: {
                let mut versions = BTreeMap::new();
                versions.insert(branches[0].id, Some(file_entry("same.txt", 0x01)));
                versions.insert(branches[1].id, Some(file_entry("same.txt", 0x01)));
                versions
            },
        }],
    };
    let empty = inspect_differences(&agreed_plan, &branches, 64);
    assert!(empty.is_complete());
    assert!(
        render_inspection(&empty).contains("no differing root paths"),
        "agreed plans render their emptiness"
    );
}

/// Duplicate submissions collapse: passing the same head twice is one
/// head, not divergence. The verdict counts distinct snapshot ids, so
/// a caller holding a duplicated handle cannot report a diverged drive
/// that is not diverged over eligible heads.
#[test]
fn duplicate_heads_do_not_diverge() {
    let head = branch(0xA1, 3, 0x11);
    assert_eq!(
        DivergenceState::from_branches(&[head, head]),
        DivergenceState::Converged,
        "same snapshot twice is one head"
    );
    assert_eq!(
        DivergenceState::from_branches(&[head, head, head]),
        DivergenceState::Converged
    );
    assert!(
        DivergenceState::from_branches(&heads_two()).is_diverged(),
        "distinct heads still diverge"
    );
}

/// Stored names pass through verbatim: a root entry legitimately named
/// `foo@1` renders as `foo@1` because the store's name wins — the
/// lookup grammar's rule, never a version number this surface invented.
/// OD-23-A governs labels and columns, not suppression of stored names.
#[test]
fn stored_names_render_verbatim() {
    let branches = heads_two();
    let mut versions = BTreeMap::new();
    versions.insert(branches[0].id, Some(file_entry("entry", 0x01)));
    versions.insert(branches[1].id, None);
    let plan = MergePlan {
        heads: vec![branches[0].id, branches[1].id],
        paths: vec![MergePath {
            path: "foo@1".into(),
            versions,
        }],
    };
    let rendered = render_inspection(&inspect_differences(&plan, &branches, 64));
    assert!(
        rendered.contains("foo@1"),
        "stored name verbatim, not suppressed:\n{rendered}"
    );
}

/// The whole screen composes the three responsibilities: indicator,
/// freeze, inspection — or the explicit not-requested state when the
/// on-demand comparison was not run.
#[test]
fn one_screen_composes_indicator_freeze_and_inspection() {
    let branches = heads_two();
    let state = DivergenceState::from_branches(&branches);
    let (plan, _) = plan_two_heads_disagreeing();
    let outcome = inspect_differences(&plan, &branches, 64);
    let screen = render_conflict_screen(&state, &FreezeState::Clear, Some(&outcome));
    assert!(screen.contains("diverged"));
    assert!(screen.contains("not frozen"));
    assert!(screen.contains("root paths only"));

    let bare = render_conflict_screen(&state, &FreezeState::Clear, None);
    assert!(bare.contains("not requested in this view"));
}

fn has_at_number(rendering: &str) -> bool {
    let bytes = rendering.as_bytes();
    bytes
        .windows(2)
        .any(|pair| pair[0] == b'@' && (pair[1].is_ascii_digit() || pair[1] == b'N'))
}
