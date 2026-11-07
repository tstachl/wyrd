//! Verification tests for the storage screen (OD-31-A/B/C/D). The
//! contract tests it renders against (`every_storage_figure_maps_to_a_dg5_row`,
//! `storage_surface_never_says_free_space`) live in `wyrd-cli` next
//! to the DG-5 table's other consumer; the quota-refusal and I/O
//! classification facts are pinned where they ship (see below).

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use wyrd_format::{ContentId, FetchStatus, MemoryObjectStore, RetainedBytes};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

use super::{
    assess_refusal, explain_refusal, render_census, render_storage_screen, CensusReading,
    RefusalAssessment, StandingStorage, StorageReading,
};
use crate::live::{LiveConfig, LiveNode};
use crate::policy::{
    FileResidency, LocalPresence, ResidencyCensus, RetentionPolicy, StorageFigure,
    FORBIDDEN_STORAGE_WORDS,
};
use crate::view::{
    Attr, DirEntry, Head, NamespaceView, Node, OpenFile, RuntimeMaterialization, ViewError,
    ViewLockError,
};

/// Composition-only view: enough [`NamespaceView`] to run
/// [`LiveNode::split`], never serves. Lookup always misses — these
/// tests compose and read the accountant, never resolve a path.
struct CompositionView {
    store: Arc<RwLock<MemoryObjectStore>>,
    materialization: RuntimeMaterialization,
    heads: Vec<Head>,
}

impl NamespaceView for CompositionView {
    type Store = MemoryObjectStore;
    type Materialization = RuntimeMaterialization;

    fn open(
        store: MemoryObjectStore,
        materialization: RuntimeMaterialization,
        heads: Vec<Head>,
    ) -> Self {
        Self::open_shared(Arc::new(RwLock::new(store)), materialization, heads)
    }

    fn open_shared(
        store: Arc<RwLock<MemoryObjectStore>>,
        materialization: RuntimeMaterialization,
        heads: Vec<Head>,
    ) -> Self {
        Self {
            store,
            materialization,
            heads,
        }
    }

    fn store_handle(&self) -> Arc<RwLock<MemoryObjectStore>> {
        Arc::clone(&self.store)
    }

    fn store_read(&self) -> Result<RwLockReadGuard<'_, MemoryObjectStore>, ViewLockError> {
        self.store.read().map_err(|_| ViewLockError)
    }

    fn store_write(&self) -> Result<RwLockWriteGuard<'_, MemoryObjectStore>, ViewLockError> {
        self.store.write().map_err(|_| ViewLockError)
    }

    fn set_heads(&mut self, heads: Vec<Head>) {
        self.heads = heads;
    }

    fn set_materialization(&mut self, materialization: RuntimeMaterialization) {
        self.materialization = materialization;
    }

    fn status(&self, _id: &ContentId) -> FetchStatus {
        FetchStatus::RemoteOnly
    }

    fn stat(&self, _path: &str) -> Result<Attr, ViewError> {
        Err(ViewError::NotFound)
    }

    fn readdir(&self, _node: &Node) -> Result<Vec<DirEntry>, ViewError> {
        Err(ViewError::NotFound)
    }

    fn open_file(&self, _node: &Node) -> Result<OpenFile, ViewError> {
        Err(ViewError::NotFound)
    }

    fn read(&self, _file: &OpenFile, _offset: u64, _len: usize) -> Result<Vec<u8>, ViewError> {
        Err(ViewError::NotFound)
    }

    fn lookup(&self, _path: &str) -> Result<Node, ViewError> {
        Err(ViewError::NotFound)
    }
}

static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Compose a live node over an empty tree and keep the parts: the
/// screen's read path starts where this fixture ends.
fn scratch_composed(
    config: &LiveConfig,
) -> (
    LiveNode<CompositionView>,
    crate::live::LiveParts<CompositionView>,
    std::path::PathBuf,
) {
    let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("wyrd-core-storage-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::generate().unwrap();
    let engine = Engine::create(dir.clone(), "core-test-pass", identity).unwrap();
    let revision = engine.current();
    let materialization = RuntimeMaterialization {
        runtime: engine.runtime_state().unwrap(),
        terminal: engine.terminal_snapshot(),
    };
    let store = Arc::new(RwLock::new(MemoryObjectStore::default()));
    let baseline = CompositionView::open_shared(Arc::clone(&store), materialization, Vec::new());
    let (node, parts) = LiveNode::split(
        engine,
        store,
        baseline,
        revision,
        0,
        Duration::from_secs(30),
        config,
    )
    .expect("test configs compose");
    (node, parts, dir)
}

fn file(policy: RetentionPolicy, local: LocalPresence, chunks: Vec<ContentId>) -> FileResidency {
    FileResidency {
        path: "f".into(),
        pinned_chunks: if policy == RetentionPolicy::Pinned {
            chunks.clone()
        } else {
            Vec::new()
        },
        chunks,
        policy,
        local,
    }
}

fn census_fixture() -> ResidencyCensus {
    ResidencyCensus {
        files: vec![
            file(
                RetentionPolicy::Pinned,
                LocalPresence::Present,
                vec![ContentId::from_bytes([0xC0; 32])],
            ),
            file(RetentionPolicy::RemoteOnly, LocalPresence::Absent, vec![]),
        ],
        conflicts: vec!["conflicted".into()],
        dirs: 1,
        symlinks_skipped: 2,
    }
}

/// The one piece of library work: the accountant crosses
/// composition into `LiveParts`, so the screen reads the live atomic
/// count through the owner instead of opening the store. A quota
/// composition wires `Some`; the default wires `None` (which the
/// screen renders unmeasured, never zero).
#[test]
fn composition_carries_the_accountant_into_live_parts() {
    let (config, retained) = LiveConfig::with_retained_quota(4096);
    let (_node, parts, dir) = scratch_composed(&config);
    let carried = parts
        .retained_bytes
        .as_ref()
        .expect("quota composition wires the accountant");
    assert_eq!(parts.budgets.retained_bytes_quota, Some(4096));
    retained.add(100);
    assert_eq!(
        carried.get(),
        100,
        "parts share the live counter, not a copy"
    );
    let standing = parts.standing_storage();
    match standing.reading {
        StorageReading::Measured { bytes, .. } => assert_eq!(bytes, 100),
        other => panic!("quota composition measures, got {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();

    let (_node, parts, dir) = scratch_composed(&LiveConfig::default());
    assert!(
        parts.retained_bytes.is_none(),
        "default composition wires no accountant"
    );
    let standing = parts.standing_storage();
    assert_eq!(standing.reading, StorageReading::Unmeasured);
    assert_eq!(standing.ceiling, None);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The brief's fact (a): a wired screen renders the counter's value,
/// an unwired screen renders an explicit unmeasured state, and the
/// two are not textually confusable. No zero is ever rendered that
/// was not measured.
#[test]
fn storage_screen_never_renders_a_zero_it_did_not_measure() {
    let counter = RetainedBytes::new();
    counter.add(12345);
    let wired = StandingStorage::read(Some(&*counter), Some(65536));
    let wired_render = render_storage_screen(&wired, None, None);
    assert!(
        wired_render.contains("retained content: 12345 bytes"),
        "wired screen renders the counter:\n{wired_render}"
    );

    let unwired = StandingStorage::read(None, None);
    let unwired_render = render_storage_screen(&unwired, None, None);
    assert!(
        unwired_render.contains("retained content: not measured"),
        "unwired screen says so:\n{unwired_render}"
    );
    assert!(
        !unwired_render.contains("retained content: 0"),
        "no zero that was not measured:\n{unwired_render}"
    );
    assert_ne!(
        wired_render, unwired_render,
        "measured and unmeasured are not confusable"
    );

    // A genuine measured zero stays renderable: the counter, not the
    // absence, decides.
    let fresh = RetainedBytes::new();
    let zero = StandingStorage::read(Some(&*fresh), Some(65536));
    let zero_render = render_storage_screen(&zero, None, None);
    assert!(zero_render.contains("retained content: 0 bytes"));
}

/// OD-31-B as a test: the screen's copy differs across the three
/// conditions that collapse into one errno, the case it can identify
/// names count, ceiling, and scope, and the cases it cannot say so
/// instead of choosing. The negative ceiling check never becomes a
/// positive host diagnosis, and no copy suggests freeing space.
#[test]
fn storage_screen_names_which_full_it_ruled_out() {
    let now = 1_794_060_000u64;
    let measured = |bytes: u64| StorageReading::Measured {
        bytes,
        observed_unix_secs: now,
    };

    // 1. Ceiling configured and reached: the one identified case.
    // The estimate rides along: a count below the ceiling whose
    // estimate crosses it is still the ceiling's refusal (the live
    // gate refuses at count-plus-estimate).
    let reached = assess_refusal(measured(70000), Some(65536), 0);
    assert_eq!(
        reached,
        RefusalAssessment::CeilingRefused {
            bytes: 70000,
            ceiling: 65536
        }
    );
    let reached_copy = explain_refusal(reached);
    assert!(reached_copy.contains("70000") && reached_copy.contains("65536"));
    assert!(reached_copy.contains("operator-selected refusal boundary over retained content only"));

    // The B1 regression: retained 60000, ceiling 65536, estimate
    // 10000 — the commit gate refuses (70000 >= 65536), so the
    // screen must identify the ceiling, never exonerate it. With
    // no estimate the same count exonerates.
    assert_eq!(
        assess_refusal(measured(60000), Some(65536), 10000),
        RefusalAssessment::CeilingRefused {
            bytes: 60000,
            ceiling: 65536
        }
    );
    assert_eq!(
        assess_refusal(measured(60000), Some(65536), 0),
        RefusalAssessment::CeilingExonerated {
            bytes: 60000,
            ceiling: 65536
        }
    );

    // 2. No ceiling: unattributable, and the copy says the two host
    // causes cannot be distinguished.
    let no_ceiling = assess_refusal(measured(70000), None, 0);
    assert_eq!(no_ceiling, RefusalAssessment::Unattributable);
    let no_ceiling_copy = explain_refusal(no_ceiling);
    assert!(no_ceiling_copy.contains("no retention ceiling is configured"));
    assert!(no_ceiling_copy.contains("host filesystem capacity or a host filesystem quota"));
    assert!(no_ceiling_copy.contains("cannot distinguish"));

    // 3. Ceiling configured but not reached: the ceiling is
    // exonerated, and the copy refuses the positive host diagnosis.
    let exonerated = assess_refusal(measured(100), Some(65536), 0);
    assert_eq!(
        exonerated,
        RefusalAssessment::CeilingExonerated {
            bytes: 100,
            ceiling: 65536
        }
    );
    let exonerated_copy = explain_refusal(exonerated);
    assert!(exonerated_copy.contains("does not explain this refusal"));
    assert!(exonerated_copy.contains("not established by the available measurements"));
    for host_claim in ["host filesystem is full", "host quota", "disk is full"] {
        assert!(
            !exonerated_copy.contains(host_claim),
            "no positive host diagnosis ({host_claim}):\n{exonerated_copy}"
        );
    }

    // Unmeasured or unreachable counts compare against nothing.
    assert_eq!(
        assess_refusal(StorageReading::Unmeasured, Some(65536), 10000),
        RefusalAssessment::CannotCompare
    );
    assert_eq!(
        assess_refusal(StorageReading::Unavailable, Some(65536), 10000),
        RefusalAssessment::CannotCompare
    );
    let cannot_copy = explain_refusal(RefusalAssessment::CannotCompare);
    assert!(cannot_copy.contains("not measured"));

    // The copies differ across the three conditions, and none
    // suggests deletion as reclamation.
    assert_ne!(reached_copy, no_ceiling_copy);
    assert_ne!(no_ceiling_copy, exonerated_copy);
    for copy in [
        &reached_copy,
        &no_ceiling_copy,
        &exonerated_copy,
        &cannot_copy,
    ] {
        for advice in ["delete", "free up", "freeing space", "reclaim"] {
            assert!(
                !copy.to_lowercase().contains(advice),
                "no deletion-as-reclamation ({advice}):\n{copy}"
            );
        }
    }
}

/// Every figure the screen can emit names its population from the
/// DG-5 closed vocabulary, and no forbidden word renders in any
/// casing. Covers summary, census, refusal, unmeasured, and
/// unavailable renderings.
#[test]
fn storage_screen_shows_only_figures_with_a_named_population() {
    let census = census_fixture();
    let reading = CensusReading {
        census: &census,
        observed_unix_secs: 1_794_060_000,
    };
    let counter = RetainedBytes::new();
    counter.add(500);
    let standing = StandingStorage::read(Some(&*counter), Some(1024));
    let renderings = [
        render_storage_screen(&standing, Some(reading), None),
        render_storage_screen(
            &standing,
            None,
            Some(RefusalAssessment::CeilingExonerated {
                bytes: 500,
                ceiling: 1024,
            }),
        ),
        render_storage_screen(&StandingStorage::read(None, None), None, None),
        render_storage_screen(
            &StandingStorage {
                reading: StorageReading::Unavailable,
                ceiling: Some(1024),
            },
            None,
            Some(RefusalAssessment::CannotCompare),
        ),
        explain_refusal(RefusalAssessment::CeilingRefused {
            bytes: 2048,
            ceiling: 1024,
        }),
        explain_refusal(RefusalAssessment::Unattributable),
    ];

    // The closed vocabulary: every figure the screen names is a
    // StorageFigure label, so a new figure without a variant has no
    // label to print.
    let labels: Vec<&str> = StorageFigure::all()
        .iter()
        .map(|figure| figure.label())
        .collect();
    for expected in [
        "retained content",
        "retained_bytes_quota",
        "pinned files",
        "pinned chunks",
        "files:",
        "conflicts",
        "symlinks skipped",
    ] {
        assert!(
            labels.contains(&expected),
            "{expected} is a closed-vocabulary label"
        );
    }
    let full = renderings.join("\n");
    for expected in [
        "retained content",
        "retained_bytes_quota",
        "pinned files",
        "pinned chunks",
    ] {
        assert!(
            full.contains(expected),
            "figure {expected} names its population"
        );
    }
    for forbidden in FORBIDDEN_STORAGE_WORDS {
        assert!(
            !full.to_lowercase().contains(&forbidden.to_lowercase()),
            "forbidden word ({forbidden}) renders nowhere"
        );
    }
    // "Unlimited" is a ceiling claim by another name: the no-ceiling
    // state never renders it.
    assert!(!full.to_lowercase().contains("unlimited"));
    // No device total is ever claimed: the resident dimensions stay
    // separate, never summed into the device's usage.
    for claim in ["device total", "total device", "disk usage"] {
        assert!(
            !full.to_lowercase().contains(claim),
            "no device-total claim ({claim})"
        );
    }
}

/// OD-31-A's freshness discipline: the standing figure carries its
/// observation time, the census carries its own, and an unreachable
/// owner shows unavailable with no stale number beside it.
#[test]
fn standing_and_census_carry_their_own_observation_times() {
    let counter = RetainedBytes::new();
    counter.add(7);
    let standing = StandingStorage::read(Some(&*counter), Some(64));
    let rendered = standing.render();
    assert!(
        rendered.contains("observed unix"),
        "standing figure timestamps itself:\n{rendered}"
    );
    assert!(
        rendered.contains("distance to refusal: 57 bytes of retention headroom"),
        "live distance, not a cached verdict:\n{rendered}"
    );

    let census = census_fixture();
    let reading = CensusReading {
        census: &census,
        observed_unix_secs: 9_999,
    };
    let census_render = render_census(Some(reading));
    assert!(census_render.contains("observed unix 9999"));

    let unavailable = StandingStorage {
        reading: StorageReading::Unavailable,
        ceiling: Some(64),
    }
    .render();
    assert!(unavailable.contains("unavailable"));
    assert!(unavailable.contains("no old observation is shown as current"));
    assert!(
        !unavailable.contains("observed unix"),
        "unavailable shows no stale number:\n{unavailable}"
    );
}

/// OD-31-C option A: one screen, census above and quota half below.
#[test]
fn one_screen_orders_census_above_quota() {
    let census = census_fixture();
    let counter = RetainedBytes::new();
    counter.add(1);
    let standing = StandingStorage::read(Some(&*counter), Some(64));
    let screen = render_storage_screen(
        &standing,
        Some(CensusReading {
            census: &census,
            observed_unix_secs: 1,
        }),
        None,
    );
    let census_pos = screen.find("pinned files").expect("census half");
    let quota_pos = screen.find("retained content").expect("quota half");
    assert!(
        census_pos < quota_pos,
        "census above, quota below:\n{screen}"
    );
    // On-demand census absent: the screen says it was not requested,
    // never a zero.
    let bare = render_storage_screen(&standing, None, None);
    assert!(bare.contains("not requested in this view"));
}

/// NB2/NB3 follow-ups: the quadrant figure's label already ends in a
/// colon, so the render must not add a second; and an
/// `Unmeasured`-with-ceiling pair renders the known ceiling row,
/// never a no-ceiling sentence.
#[test]
fn census_and_unmeasured_render_without_false_claims() {
    let census = census_fixture();
    let rendered = render_census(Some(CensusReading {
        census: &census,
        observed_unix_secs: 1,
    }));
    assert!(
        !rendered.contains("files::"),
        "the label carries its own colon:\n{rendered}"
    );
    assert!(rendered.contains("files: 1 pinned present"));

    // A directly-constructed inconsistent pair cannot render a false
    // ceiling claim: the ceiling row states what is known.
    let paired = StandingStorage {
        reading: StorageReading::Unmeasured,
        ceiling: Some(1024),
    }
    .render();
    assert!(paired.contains("retained_bytes_quota: 1024"));
    assert!(
        !paired.contains("no retention ceiling is set"),
        "known ceiling never renders as no-ceiling:\n{paired}"
    );
}
