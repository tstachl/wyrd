//! The published serving projection: one immutable, generation-tagged
//! snapshot of what presentation backends serve.
//!
//! The sync loop is the single writer: each pass either leaves the
//! published projection untouched or replaces it wholesale with a new
//! generation built from durable state. Backends clone the current
//! [`Arc`](std::sync::Arc) under a short read lock and then serve
//! lock-free from immutable data, so publication is atomic (readers
//! never observe a half-published projection) and readers never block
//! each other on view content.
//!
//! Two counters distinguish "the world changed" from "serving changed":
//! `revision` is the engine's durable commit sequence (every fact commit
//! advances it, empty passes do not), and `generation` counts
//! publications. The loop publishes exactly when the durable revision
//! has advanced past the published one (or a dirty backlog forces it) —
//! no per-report predicate, so a future commit path cannot silently skip
//! publication the way a report-counter gate could.
//!
//! Generic over [`NamespaceView`](crate::view::NamespaceView): the
//! publication slot works against the provider-neutral read surface,
//! never any one presentation's view type.

use std::sync::{Arc, RwLock};

use crate::view::{Head, NamespaceView};

/// The shared publication slot every serving backend reads: one
/// alias for the nested lock shape, so the loop, the split parts, the
/// backend construction, and observation name one type.
pub type SharedProjection<V> = Arc<RwLock<Arc<Projection<V>>>>;

/// One published generation: the serving view plus the versions that
/// produced it. Immutable after construction; the loop publishes by
/// replacing the whole value, never by mutating it.
///
/// The `NamespaceView` bound lives on the constructor and accessor
/// methods, not the struct: holders name their concrete view without
/// repeating its bounds.
pub struct Projection<V> {
    /// Publication count. Bumps on every publish; readers use it to
    /// detect staleness (an older generation is a complete, merely
    /// outdated snapshot — never a torn one).
    generation: u64,
    /// The engine durable commit sequence this generation was built
    /// from. Equal revisions mean provably identical projections, so
    /// the idle loop can skip republication without recomputing heads.
    revision: u64,
    view: V,
}

impl<V: NamespaceView> Projection<V> {
    /// Publish a generation over a shared store handle: the loop and
    /// the backends address the same bytes, each locking only for its
    /// own operation. Heads and facts are fixed at construction.
    pub fn new(
        store: Arc<RwLock<V::Store>>,
        materialization: V::Materialization,
        heads: Vec<Head>,
        generation: u64,
        revision: u64,
    ) -> Self {
        Projection {
            generation,
            revision,
            view: V::open_shared(store, materialization, heads),
        }
    }

    /// Adopt an already-built view as generation zero: the
    /// standalone-backend path passes revision zero (no sync loop owns
    /// a revision counter there); the live handoff passes the engine's
    /// current sequence so the first pass publishes only on real
    /// change. The adopted heads and facts come from the composer's
    /// synchronously refreshed view, so the baseline is exact — the
    /// loop reconciles anything committed after the split.
    pub fn initial(view: V, revision: u64) -> Self {
        Projection {
            generation: 0,
            revision,
            view,
        }
    }

    /// Publish the next generation over a replacement view: the
    /// generation bumps, the durable revision carries over. The
    /// test/simulation path — the production loop builds generations
    /// with [`new`](Self::new), advancing both counters together.
    pub fn successor(current: &Self, view: V) -> Self {
        Projection {
            generation: current.generation + 1,
            revision: current.revision,
            view,
        }
    }

    /// The publication count of this generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The durable revision this generation was built from.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The serving view of this generation.
    pub fn view(&self) -> &V {
        &self.view
    }
}

/// Which of the four consumer-distinguishable projection states the
/// published generation is in (OD-32-C, decided): a fresh valid
/// empty projection, a fresh valid non-empty projection, or a
/// retained last-good generation whose freshness could not be
/// established. The fourth state — no trustworthy projection
/// because initial composition failed — has no slot value by
/// construction: a failed composition never publishes, so the
/// consumer observes it as an absent owner, not as a generation.
///
/// The core reports these facts; the owner exposes them. This is a
/// judgment about the served generation, never a degraded-mode
/// state machine: the loop does not change what it serves on the
/// strength of this value, it only records what the last refresh
/// established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessState {
    /// The published generation is valid and names no heads: a
    /// genuinely empty drive reads the same through any consumer.
    FreshEmpty,
    /// The published generation is valid and names heads: the
    /// consumer serves current state.
    FreshNonEmpty,
    /// A refresh established nothing, so the previous generation
    /// keeps serving: `detail` names the refresh failure that left
    /// it in place. The consumer serves last-good bytes under an
    /// explicit staleness warning, never as current state.
    RetainedLastGood {
        /// The refresh failure rendered as text: failure class and
        /// cause. Diagnostic only — a consumer reconciles against
        /// served generations and revisions, never by parsing this.
        detail: String,
    },
}

/// The freshness judgment over one published generation: which of
/// the states holds, and which durable revision and serving
/// generation the judgment applies to. Written by the live loop on
/// every publication and every retaining failure; read by the
/// owner interface for out-of-process consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionFreshness {
    /// The judged state of the served generation.
    pub state: FreshnessState,
    /// The engine durable revision the judgment applies to.
    pub revision: u64,
    /// The serving generation the judgment applies to.
    pub generation: u64,
}

/// The shared freshness slot: one alias so the loop, the split
/// parts, and the owner interface name one type. Initialized at
/// composition from the baseline heads; replaced wholesale on
/// every judgment, never mutated in place.
pub type SharedFreshness = Arc<RwLock<ProjectionFreshness>>;
