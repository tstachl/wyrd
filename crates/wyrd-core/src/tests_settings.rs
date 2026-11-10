//! Settings surface verification (issue 25): the surface reports the
//! running composition, changes apply on the next start, and the
//! settings path never constructs the quota-without-accountant
//! mismatch.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use wyrd_format::{ContentId, FetchStatus, MemoryObjectStore, RetainedBytes};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

use super::{apply_budget_settings, render_settings_surface, BudgetOverrides, RunningSettings};
use crate::budgets::ResourceBudgets;
use crate::live::{LiveConfig, LiveNode};
use crate::view::{
    Attr, DirEntry, Head, NamespaceView, Node, OpenFile, RuntimeMaterialization, ViewError,
    ViewLockError,
};

/// Composition-only view: enough [`NamespaceView`] to run
/// [`LiveNode::split`], never serves. Mirrors the storage-screen
/// fixture; each surface owns its stub so a shared helper change
/// cannot move two surfaces at once.
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

fn scratch_engine() -> (Engine, std::path::PathBuf) {
    let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("wyrd-core-settings-{seq}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::generate().unwrap();
    let engine = Engine::create(dir.clone(), "settings-test-pass", identity).unwrap();
    (engine, dir)
}

fn scratch_composed(
    config: &LiveConfig,
    open_timeout: Duration,
) -> (
    LiveNode<CompositionView>,
    crate::live::LiveParts<CompositionView>,
    std::path::PathBuf,
) {
    let (engine, dir) = scratch_engine();
    let revision = engine.current();
    let materialization = RuntimeMaterialization {
        runtime: engine.runtime_state().unwrap(),
        terminal: engine.terminal_snapshot(),
    };
    let store = Arc::new(RwLock::new(MemoryObjectStore::default()));
    let baseline = CompositionView::open_shared(Arc::clone(&store), materialization, Vec::new());
    let (node, parts) = LiveNode::split(engine, store, baseline, revision, 0, open_timeout, config)
        .expect("test configs compose");
    (node, parts, dir)
}

fn running_of(
    node: &LiveNode<CompositionView>,
    parts: &crate::live::LiveParts<CompositionView>,
) -> RunningSettings {
    RunningSettings::snapshot(
        parts,
        node.fetch_pass_budget(),
        node.max_mutation_wait(),
        node.configured_relay_count(),
    )
}

/// The issue's load-bearing test: two nodes composed with different
/// configs report their own running budgets. A surface built from
/// [`LiveConfig::for_local_sync`] would show the default beside a
/// configured drive and be wrong on any configured drive — this
/// fails if the snapshot ever reads a fresh default instead of the
/// composed parts.
#[test]
fn settings_show_the_running_budgets_not_the_defaults() {
    let (quota_config, _retained) = LiveConfig::with_retained_quota(8192);
    let mut quota_config = quota_config;
    quota_config.budgets.max_admit_per_pass = 17;
    quota_config.fetch_pass_budget = Duration::from_secs(11);
    let (quota_node, quota_parts, quota_dir) =
        scratch_composed(&quota_config, Duration::from_secs(31));
    let (plain_node, plain_parts, plain_dir) =
        scratch_composed(&LiveConfig::for_local_sync(), Duration::from_secs(30));

    let quota_running = running_of(&quota_node, &quota_parts);
    let plain_running = running_of(&plain_node, &plain_parts);
    assert_eq!(quota_running.budgets, quota_config.budgets);
    assert_eq!(plain_running.budgets, LiveConfig::for_local_sync().budgets);
    assert_ne!(
        quota_running.budgets, plain_running.budgets,
        "the two compositions run different budgets"
    );
    assert_ne!(
        quota_running.budgets,
        LiveConfig::for_local_sync().budgets,
        "the configured composition does not report the default"
    );

    let quota_render = render_settings_surface(&quota_running);
    let plain_render = render_settings_surface(&plain_running);
    assert!(
        quota_render.contains("8192"),
        "the quota composition shows its own ceiling:\n{quota_render}"
    );
    assert!(
        quota_render.contains("max_admit_per_pass: 17"),
        "the quota composition shows its own admission bound:\n{quota_render}"
    );
    assert!(
        quota_render.contains("fetch_pass_budget: 11s"),
        "the quota composition shows its own fetch budget:\n{quota_render}"
    );
    assert!(
        plain_render.contains("no retention ceiling is set"),
        "the default composition says so plainly:\n{plain_render}"
    );
    assert!(
        plain_render.contains("max_admit_per_pass: 1024"),
        "the default composition shows the default admission bound:\n{plain_render}"
    );
    assert_ne!(quota_render, plain_render);

    let _ = std::fs::remove_dir_all(quota_dir);
    let _ = std::fs::remove_dir_all(plain_dir);
}

/// The documented bounds table, machine-checked: every field of
/// [`ResourceBudgets::default`] equals the bound the
/// `resource-limits.md` Bounds table publishes, before any screen
/// shows one of them to a person. Companion to the pinned
/// `resource_budgets_defaults_are_pinned` contract in `budgets.rs`;
/// this is the issue-named referent the settings surface depends
/// on.
#[test]
fn resource_budget_defaults_match_the_documented_table() {
    let budgets = ResourceBudgets::default();
    assert_eq!(budgets.max_pending_wants, 4096);
    assert_eq!(budgets.max_pending_mutations, 4096);
    assert_eq!(budgets.max_parent_tokens, 4096);
    assert_eq!(budgets.max_admit_per_pass, 1024);
    assert_eq!(budgets.max_quarantine_per_pass, 64);
    assert_eq!(budgets.max_scrub_per_pass, 64);
    assert_eq!(budgets.write_per_handle_bytes, 64 * 1024 * 1024);
    assert_eq!(budgets.write_aggregate_bytes, 256 * 1024 * 1024);
    assert_eq!(budgets.write_dirty_handles, 64);
    assert_eq!(budgets.max_open_handles, 4096);
    assert_eq!(budgets.max_open_capture_bytes, 256 * 1024 * 1024);
    assert_eq!(budgets.retained_bytes_quota, None);
}

/// Budgets are composition-read: changing the settings leaves the
/// running node's values untouched and the next composed node picks
/// them up. Pins `live/config.rs` ("compose and run with the same
/// config value") as the fact the surface's copy depends on, and
/// fails if someone makes budgets hot-reloadable without deciding
/// what that means for the registries the composition already
/// wired.
#[test]
fn a_budget_change_takes_effect_on_the_next_start() {
    let base = LiveConfig::for_local_sync();
    let (node, parts, dir) = scratch_composed(&base, Duration::from_secs(30));
    let running_before = running_of(&node, &parts);
    assert_eq!(running_before.budgets.max_admit_per_pass, 1024);

    let applied = apply_budget_settings(
        &base,
        Duration::from_secs(30),
        &BudgetOverrides {
            max_admit_per_pass: Some(17),
            fetch_pass_budget: Some(Duration::from_secs(11)),
            max_mutation_wait: Some(Duration::from_secs(31)),
            open_timeout: Some(Duration::from_secs(32)),
            retained_bytes_quota: None,
        },
    )
    .expect("valid overrides apply");

    // The running node is untouched by the settings change.
    let running_still = running_of(&node, &parts);
    assert_eq!(running_still, running_before);
    assert_eq!(parts.budgets.max_admit_per_pass, 1024);

    // The next composition carries the change.
    let (next_node, next_parts, next_dir) = scratch_composed(&applied.config, applied.open_timeout);
    let next_running = running_of(&next_node, &next_parts);
    assert_eq!(next_running.budgets.max_admit_per_pass, 17);
    assert_eq!(next_running.fetch_pass_budget, Duration::from_secs(11));
    assert_eq!(next_running.max_mutation_wait, Duration::from_secs(31));
    assert_eq!(next_running.open_timeout, Duration::from_secs(32));
    assert_ne!(next_running, running_before);
    assert!(applied.retained_bytes.is_none());

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(next_dir);
}

/// The settings path pairs a quota with its accountant from one
/// call, so the composition refusal for a quota without an
/// accountant is unreachable through it — while the refusal itself
/// still stands for hand-built configs.
#[test]
fn quota_setting_wires_one_accountant() {
    // A quota through the settings path wires the accountant and
    // composes: the tally the config points at is the tally the
    // result hands the store constructor.
    let base = LiveConfig::for_local_sync();
    let applied = apply_budget_settings(
        &base,
        Duration::from_secs(30),
        &BudgetOverrides {
            retained_bytes_quota: Some(4096),
            ..BudgetOverrides::default()
        },
    )
    .expect("a quota applies");
    let retained = applied
        .retained_bytes
        .as_ref()
        .expect("a quota wires an accountant");
    assert_eq!(applied.config.budgets.retained_bytes_quota, Some(4096));
    assert!(Arc::ptr_eq(
        retained,
        applied
            .config
            .retained_bytes
            .as_ref()
            .expect("the config shares the same tally")
    ));
    let (engine, dir) = scratch_engine();
    let revision = engine.current();
    let materialization = RuntimeMaterialization {
        runtime: engine.runtime_state().unwrap(),
        terminal: engine.terminal_snapshot(),
    };
    // The store charges the same tally the config compares
    // against: a seed visible through the config's handle proves
    // the pairing, not just the presence.
    let store = Arc::new(RwLock::new(
        MemoryObjectStore::default().with_retained(Arc::clone(retained)),
    ));
    retained.add(100);
    assert_eq!(retained.get(), 100);
    let baseline = CompositionView::open_shared(Arc::clone(&store), materialization, Vec::new());
    let (_node, parts) = LiveNode::split(
        engine,
        store,
        baseline,
        revision,
        0,
        applied.open_timeout,
        &applied.config,
    )
    .expect("the paired composition is accepted");
    assert_eq!(parts.budgets.retained_bytes_quota, Some(4096));
    assert_eq!(
        parts
            .retained_bytes
            .as_ref()
            .expect("parts carry the accountant")
            .get(),
        100
    );

    // No quota means no accountant: the default stays default.
    let plain = apply_budget_settings(&base, Duration::from_secs(30), &BudgetOverrides::default())
        .expect("empty overrides apply");
    assert_eq!(plain.config.budgets.retained_bytes_quota, None);
    assert!(plain.retained_bytes.is_none());

    // The mismatch is still refused when built by hand: the
    // settings path avoids it, it does not remove the guard.
    let mut mismatched = LiveConfig::for_local_sync();
    mismatched.budgets.retained_bytes_quota = Some(1024);
    assert!(mismatched.retained_bytes.is_none());
    let (engine, dir2) = scratch_engine();
    let revision = engine.current();
    let materialization = RuntimeMaterialization {
        runtime: engine.runtime_state().unwrap(),
        terminal: engine.terminal_snapshot(),
    };
    let store = Arc::new(RwLock::new(MemoryObjectStore::default()));
    let baseline = CompositionView::open_shared(Arc::clone(&store), materialization, Vec::new());
    let refused = LiveNode::split(
        engine,
        store,
        baseline,
        revision,
        0,
        Duration::from_secs(30),
        &mismatched,
    )
    .err()
    .expect("a quota without an accountant refuses to compose");
    assert!(
        refused.to_string().contains("retained_bytes_quota"),
        "the refusal names the mismatch: {refused}"
    );

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

/// Every surfaced field says when it takes effect, and the narrow
/// claim ("now") appears nowhere: all five bounds are
/// composition-read, so all five say "applies on the next start"
/// and the relay row says read-only.
#[test]
fn settings_say_which_changes_are_next_start() {
    let (config, _retained) = LiveConfig::with_retained_quota(4096);
    let (node, parts, dir) = scratch_composed(&config, Duration::from_secs(30));
    let mut running = running_of(&node, &parts);
    running.configured_relays = 2;
    let rendered = render_settings_surface(&running);
    for field in [
        "retained_bytes_quota",
        "max_admit_per_pass",
        "fetch_pass_budget",
        "max_mutation_wait",
        "open_timeout",
    ] {
        let row = rendered
            .lines()
            .find(|line| line.contains(field))
            .unwrap_or_else(|| panic!("the surface names {field}:\n{rendered}"));
        assert!(
            row.contains("applies on the next start"),
            "the {field} row says next start: {row}"
        );
    }
    let relay_row = rendered
        .lines()
        .find(|line| line.contains("configured relays"))
        .expect("the surface names the relay count");
    assert!(
        relay_row.contains("read-only"),
        "the relay row says read-only: {relay_row}"
    );
    assert!(
        !rendered.contains("takes effect now") && !rendered.contains("effective immediately"),
        "no field promises immediacy:\n{rendered}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Overrides refuse the absurd instead of composing it: a zero
/// admission, a zero duration, and a day-plus stall all fail before
/// anything composes.
#[test]
fn settings_overrides_refuse_absurd_values() {
    let base = LiveConfig::for_local_sync();
    let zero_admit = match apply_budget_settings(
        &base,
        Duration::from_secs(30),
        &BudgetOverrides {
            max_admit_per_pass: Some(0),
            ..BudgetOverrides::default()
        },
    ) {
        Ok(_) => panic!("a zero admission must refuse"),
        Err(message) => message,
    };
    assert!(zero_admit.contains("max_admit_per_pass"), "{zero_admit}");
    let zero_timeout = match apply_budget_settings(
        &base,
        Duration::from_secs(30),
        &BudgetOverrides {
            open_timeout: Some(Duration::ZERO),
            ..BudgetOverrides::default()
        },
    ) {
        Ok(_) => panic!("a zero timeout must refuse"),
        Err(message) => message,
    };
    assert!(zero_timeout.contains("open_timeout"), "{zero_timeout}");
    let huge = match apply_budget_settings(
        &base,
        Duration::from_secs(30),
        &BudgetOverrides {
            fetch_pass_budget: Some(Duration::from_secs(1_000_000)),
            ..BudgetOverrides::default()
        },
    ) {
        Ok(_) => panic!("a day-plus stall must refuse"),
        Err(message) => message,
    };
    assert!(huge.contains("fetch_pass_budget"), "{huge}");
    // The imported constant is load-bearing below: the accountant
    // type stays wired to the store's counter type.
    let counter = RetainedBytes::new();
    counter.add(1);
    assert_eq!(counter.get(), 1);
}
