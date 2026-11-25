//! Snapshot/tree/manifest closure correspondence.
//!
//! A snapshot body names a root tree; that tree's files reference chunks and
//! its directories reference subtrees; the manifest hierarchy is a second
//! declaration of the same content, mapping every reachable chunk to a sealed
//! representation and every subtree to a child manifest. Nothing in the
//! content cryptography ties those two declarations together: a member can
//! sign a body over tree T1 and publish a perfectly valid, snapshot-bound
//! manifest describing the objects of T2. Fetch then believes the manifest
//! while the read side walks T1, producing an authenticated but broken
//! replica.
//!
//! [`verify_snapshot_manifest`] establishes the correspondence invariant:
//! given a snapshot body and the manifest hierarchy for it, the structural
//! tree closure and the manifest closure must describe exactly the same
//! logical content.
//!
//! # Model
//!
//! Tree nodes are **structural**: the root is `Snapshot::tree` and each
//! subtree is a [`ChildManifest::tree`]. Production authoring self-maps every
//! tree node with a `Tree`-kind [`ManifestEntry`], so the closure is
//! fetchable; the entry must agree with the structural reference (the tree
//! must be reachable and the declared size must match its plaintext length).
//! A `Tree` entry is still optional at this verifier's boundary, so closures
//! authored before self-mapping, or by other producers, remain checkable.
//!
//! [`ChildManifest::tree`]: wyrd_format::ChildManifest
//!
//! # Enforcement boundary
//!
//! Every manifest self-maps its own tree node with a `Tree` entry, and those
//! entries are structural: the fetch plan always wants them, so a receiver
//! can assemble the full tree closure. The verifier runs at two boundaries:
//!
//! - authoring self-checks before commit (`crate::runtime::author`);
//! - the daemon verifies each classified head's closure against the local
//!   store before installing it, and a head whose closure is incomplete or
//!   does not correspond is never mounted (see `wyrd-daemon`).
//!
//! [`verify_head_closure`] is the single-head entry point for the
//! second; [`verify_head_closure_with`] is its batch form.
//!
//! A gate run over several heads shares one [`ClosureVerifier`]: it
//! reads, decodes, checks, and scans each distinct tree once and reuses
//! the result for every head that reaches the same tree, instead of
//! paying that cost per head. [`HeadGateBudget`] caps the whole batch's
//! bytes read, entries scanned, and verification steps. Exhaustion is
//! reported as [`ClosureError::WorkBudgetExhausted`] — a scheduling
//! deferral of the entire publication, not a verdict on any head.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use thiserror::Error;
use wyrd_format::{
    ChildManifest, ContentId, EntryContent, Manifest, ManifestEntry, ObjectKind, ObjectStore,
    Snapshot, SnapshotId, StoreError, StoreFailure, Tree,
};

use crate::ingest::{check_manifest, check_tree, IngestError, Limits};

/// Why a snapshot's manifest closure does not correspond to its tree closure.
///
/// Every variant except `ObjectStore` is a fail-closed verdict on
/// evidence, never a transport or cryptographic failure; the bytes
/// involved already authenticated. `ObjectStore` is a store I/O
/// failure carrying its own classification: it is neither pending
/// progress nor permanent damage, and callers route it to the store
/// failure policy.
///
/// Batch policy (raise and count): the head-closure gate verifies
/// every classified head, counts each rejection by
/// [`ClosureError::rejection_class`], and raises the first damage as
/// the batch error — so one damaged head still fails its pass
/// closed while the counts describe the whole batch.
#[derive(Debug, Error)]
pub enum ClosureError {
    #[error("root manifest describes snapshot {found}, not {expected}")]
    /// The root manifest declares a different snapshot than the body being verified.
    RootSnapshotMismatch {
        /// The snapshot id the body commits.
        expected: SnapshotId,
        /// The snapshot id the root manifest declares.
        found: SnapshotId,
    },
    #[error("no root manifest is recorded for snapshot {0}")]
    /// No root manifest is recorded locally for this snapshot (fetch progress, not damage).
    RootManifestMissing(SnapshotId),
    #[error("root manifest id {found} does not match its canonical bytes ({derived})")]
    /// Root manifest bytes do not hash back to the recorded manifest id.
    RootIdentityMismatch {
        /// The recorded manifest id the bytes failed to reproduce.
        found: ContentId,
        /// The manifest id the canonical bytes actually derive to.
        derived: ContentId,
    },
    #[error("child manifest {expected} does not match its canonical bytes ({derived})")]
    /// A child manifest's bytes do not hash back to the id its parent links.
    ChildIdentityMismatch {
        /// The manifest id the parent link names.
        expected: ContentId,
        /// The manifest id the child's canonical bytes actually derive to.
        derived: ContentId,
    },
    #[error("manifest is not canonical: entries or children are not strictly ascending")]
    /// A manifest's entries or children are not strictly ascending.
    NonCanonicalManifest,
    #[error("tree {0} is not available locally")]
    /// A reachable tree object is not present locally (fetch progress, not damage).
    TreeUnavailable(ContentId),
    #[error("tree {0} does not hash back to its content id")]
    /// Stored tree bytes do not hash back to their content id.
    TreeIdentityMismatch(ContentId),
    #[error("tree {0} failed to decode")]
    /// Stored tree bytes fail to decode.
    InvalidTree(ContentId),
    #[error("tree {tree} is mapped by both manifest {first} and manifest {second}")]
    /// One tree is claimed by two different manifests.
    AmbiguousTreeMapping {
        /// The doubly-claimed tree.
        tree: ContentId,
        /// The first claimant manifest.
        first: ContentId,
        /// The second, contradicting claimant manifest.
        second: ContentId,
    },
    #[error("directory {tree} has no child manifest for subtree {child}")]
    /// A directory subtree has no corresponding child manifest link.
    MissingChildManifest {
        /// The tree whose directory names the unlinked subtree.
        tree: ContentId,
        /// The subtree with no corresponding child manifest.
        child: ContentId,
    },
    #[error("child manifest {manifest} for subtree {child} is not available")]
    /// A linked child manifest has no local record (fetch progress, not damage).
    MissingChildManifestRecord {
        /// The subtree whose record is missing.
        child: ContentId,
        /// The linked manifest id with no local record.
        manifest: ContentId,
    },
    #[error("child manifest {manifest} describes a different snapshot")]
    /// A child manifest declares a different snapshot than the body under verification.
    ChildSnapshotMismatch {
        /// The child manifest bound to another snapshot.
        manifest: ContentId,
    },
    #[error("child manifest {child} of tree {tree} is not a directory subtree")]
    /// A child link names a tree that is not a directory subtree here.
    UnexpectedChildManifest {
        /// The tree whose child link is unexpected.
        tree: ContentId,
        /// The linked tree that is not a directory subtree here.
        child: ContentId,
    },
    #[error("file in tree {tree} references chunk {chunk} with no manifest entry")]
    /// A file references a chunk with no manifest entry.
    MissingChunkEntry {
        /// The tree whose file references the unmapped chunk.
        tree: ContentId,
        /// The chunk with no manifest entry.
        chunk: ContentId,
    },
    #[error("manifest entry {content} is not reachable from the snapshot tree")]
    /// A manifest entry is unreachable from the snapshot tree.
    UnrelatedEntry {
        /// The manifest entry unreachable from the snapshot tree.
        content: ContentId,
    },
    #[error("manifest entry {content} has kind {found:?}, expected {expected:?}")]
    /// A manifest entry's kind disagrees with where the tree closure finds it.
    KindMismatch {
        /// The misclassified content id.
        content: ContentId,
        /// The kind the tree closure requires here.
        expected: ObjectKind,
        /// The kind the manifest entry declares.
        found: ObjectKind,
    },
    #[error("tree entry {content} declares {found} bytes but the tree is {expected}")]
    /// An optional tree mapping declares the wrong plaintext size.
    TreeEntrySizeMismatch {
        /// The tree the mismatched entry maps.
        content: ContentId,
        /// The tree's actual plaintext length.
        expected: u64,
        /// The size the tree entry declares.
        found: u64,
    },
    #[error(transparent)]
    /// Ingest limits rejected the tree or manifest under verification.
    Ingest(#[from] IngestError),
    #[error("object store read failed: {detail}")]
    /// The object store failed mid-read; the store's own classification applies.
    ObjectStore {
        /// The store's own classification: a full or unwritable
        /// store is not closure damage, and the failure policy has a
        /// dedicated budget for it.
        failure: StoreFailure,
        /// The store error rendered as text.
        detail: String,
    },
    #[error("head-gate work budget exhausted")]
    /// The aggregate budget for the whole gate run is spent. This is
    /// neither pending progress nor damage on this head — it says the
    /// batch cannot be judged within its bound, so the caller must
    /// defer the entire publication and keep the previous complete
    /// generation serving. It carries no `RejectionClass` for the
    /// same reason `ObjectStore` does not.
    WorkBudgetExhausted,
}

/// Operator-facing class of a head-closure rejection: what the
/// gate's verdict means for the head, not which check produced it.
/// Callers report counts by class so an operator can tell "still
/// fetching" from "invalid remote data" without rerunning the
/// verifier by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionClass {
    /// The closure has not been fetched yet: ordinary fetch
    /// progress, and the head installs once its records land.
    Incomplete,
    /// Stored bytes do not hash back to their content identity:
    /// invalid remote data that will not heal with more fetching.
    IdentityMismatch,
    /// Every other structural verdict: a contradicted mapping, a
    /// non-canonical document, an unreachable entry, an ingest
    /// ceiling. Fail-closed damage, never progress.
    Other,
}

impl ClosureError {
    /// Whether this failure means "the closure has not been fetched
    /// yet" rather than "the closure is wrong". The pending arm is
    /// ordinary fetch progress: the head installs once its records
    /// and trees land, and callers must treat it as no-publication
    /// state, never as a fatal engine error. Every other variant
    /// except `ObjectStore` is permanent damage — a mismatch, a
    /// non-canonical document, a contradicted mapping — and stays
    /// fail-closed; `ObjectStore` is neither, and callers classify it
    /// through its own `StoreFailure`.
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            ClosureError::RootManifestMissing(_)
                | ClosureError::TreeUnavailable(_)
                | ClosureError::MissingChildManifestRecord { .. }
        )
    }

    /// The operator-facing rejection class for this failure, or
    /// `None` when the failure is not a rejection at all: an
    /// `ObjectStore` I/O failure carries the store's own
    /// classification and callers route it to the store failure
    /// policy, never to the closure counts.
    ///
    /// Every variant is listed explicitly — no wildcard — so adding
    /// a variant fails compilation until its class is decided here.
    /// This is the single source of truth for the pending set: keep
    /// it in sync with [`is_pending`](ClosureError::is_pending).
    pub fn rejection_class(&self) -> Option<RejectionClass> {
        match self {
            ClosureError::RootSnapshotMismatch { .. }
            | ClosureError::NonCanonicalManifest
            | ClosureError::InvalidTree(_)
            | ClosureError::AmbiguousTreeMapping { .. }
            | ClosureError::MissingChildManifest { .. }
            | ClosureError::ChildSnapshotMismatch { .. }
            | ClosureError::UnexpectedChildManifest { .. }
            | ClosureError::MissingChunkEntry { .. }
            | ClosureError::UnrelatedEntry { .. }
            | ClosureError::KindMismatch { .. }
            | ClosureError::TreeEntrySizeMismatch { .. }
            | ClosureError::Ingest(_) => Some(RejectionClass::Other),
            ClosureError::RootIdentityMismatch { .. }
            | ClosureError::ChildIdentityMismatch { .. }
            | ClosureError::TreeIdentityMismatch(_) => Some(RejectionClass::IdentityMismatch),
            ClosureError::RootManifestMissing(_)
            | ClosureError::TreeUnavailable(_)
            | ClosureError::MissingChildManifestRecord { .. } => Some(RejectionClass::Incomplete),
            ClosureError::ObjectStore { .. } => None,
            // Neither a per-head rejection nor damage: the whole batch
            // is deferred by the caller, so it is not counted on this head.
            ClosureError::WorkBudgetExhausted => None,
        }
    }
}

/// Resolves a manifest by its logical [`ContentId`].
///
/// Implemented for a plain `BTreeMap<ContentId, Manifest>` (authoring builds
/// one from the records it just sealed) and for [`RuntimeState`], whose
/// recorded manifest hierarchy is the durable read-side source.
///
/// [`RuntimeState`]: crate::runtime::RuntimeState
pub trait ManifestSource {
    /// Resolve a recorded manifest by its logical content id.
    fn manifest(&self, id: &ContentId) -> Option<&Manifest>;
}

impl ManifestSource for BTreeMap<ContentId, Manifest> {
    fn manifest(&self, id: &ContentId) -> Option<&Manifest> {
        self.get(id)
    }
}

impl ManifestSource for BTreeMap<ContentId, &Manifest> {
    fn manifest(&self, id: &ContentId) -> Option<&Manifest> {
        self.get(id).copied()
    }
}

impl ManifestSource for crate::runtime::RuntimeState {
    fn manifest(&self, id: &ContentId) -> Option<&Manifest> {
        self.manifest_record(id).map(|record| &record.manifest)
    }
}

/// Every manifest the verifier accepts must be canonical: entries strictly
/// ascending by `(content_id, kind, version)` and children strictly ascending
/// by tree id, exactly as the format decoder enforces. Checking here keeps the
/// verifier sound for direct in-memory inputs (defense in depth: the
/// [`Manifest`] type already establishes this by construction).
fn check_canonical(manifest: &Manifest) -> Result<(), ClosureError> {
    for pair in manifest.entries().windows(2) {
        let a = (
            pair[0].content_id.as_bytes(),
            pair[0].kind.byte(),
            pair[0].version,
        );
        let b = (
            pair[1].content_id.as_bytes(),
            pair[1].kind.byte(),
            pair[1].version,
        );
        if a >= b {
            return Err(ClosureError::NonCanonicalManifest);
        }
    }
    for pair in manifest.children().windows(2) {
        if pair[0].tree.as_bytes() >= pair[1].tree.as_bytes() {
            return Err(ClosureError::NonCanonicalManifest);
        }
    }
    Ok(())
}

/// Aggregate work budget for one gate run over a batch of heads.
///
/// [`Limits`] bounds one tree or manifest; this bounds the *sum* across the
/// whole batch, so many heads with large closures cannot multiply CPU and
/// read-lock time at a fixed per-object cost. Exhaustion is a scheduling
/// deferral, not closure damage: the caller holds back the entire
/// publication and keeps the previous complete generation serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadGateBudget {
    /// Maximum plaintext tree bytes read and decoded in the batch.
    pub max_bytes: u64,
    /// Maximum tree entries scanned in the batch.
    pub max_entries: u64,
    /// Maximum (tree, manifest) verification steps in the batch — a
    /// deterministic proxy for elapsed work, since bytes and entries
    /// are already charged separately for actual I/O.
    pub max_work: u64,
}

impl HeadGateBudget {
    /// Production bound: generous enough that one lawful head at the
    /// `Limits::V0` ceiling (a 64 MiB tree, 1M tree entries, 580k
    /// manifest entries) and a large multi-head fan-out still verify,
    /// while a hostile fan-out of thousands of distinct large closures
    /// is stopped. Reuse means shared trees cost their bytes and
    /// entries only once, so these caps measure distinct work.
    pub const DEFAULT: Self = Self {
        max_bytes: 4 * 1024 * 1024 * 1024,
        max_entries: 512 * 1024 * 1024,
        max_work: 16 * 1024 * 1024,
    };
    /// No bound: a single self-check at authoring time, where there is
    /// no cross-head fan-out to cap and existing behavior must hold.
    pub const UNLIMITED: Self = Self {
        max_bytes: u64::MAX,
        max_entries: u64::MAX,
        max_work: u64::MAX,
    };
}

/// The derived, immutable result of verifying one tree's bytes. The
/// decoded `Tree` itself is dropped; only what the closure check needs
/// is retained, so a wide tree costs its references once per pass
/// rather than once per head.
#[derive(Debug)]
struct CachedTree {
    byte_len: u64,
    chunks: Vec<ContentId>,
    directories: Vec<ContentId>,
}

/// One verifier for a whole gate run. Tree bytes are read, decoded,
/// checked, and scanned at most once per distinct tree id; the derived
/// result is reused by every head that reaches the same tree. All
/// work is charged against one [`HeadGateBudget`], so distinct large
/// closures cannot exhaust CPU and the store read lock by fanning out.
///
/// The [`Limits`] table is fixed at construction: a cached tree has
/// already passed `check_tree` under that table, so no later call can
/// silently skip a stricter check. The gate uses [`Limits::V0`].
pub struct ClosureVerifier {
    trees: HashMap<ContentId, CachedTree>,
    limits: Limits,
    budget: HeadGateBudget,
    bytes: u64,
    entries: u64,
    work: u64,
}

impl ClosureVerifier {
    /// A verifier with a fresh cache, one fixed limits table, and the
    /// given aggregate budget. The limits live here so a cached tree
    /// can never skip a stricter check on a later call.
    pub fn new(budget: HeadGateBudget, limits: Limits) -> Self {
        ClosureVerifier {
            trees: HashMap::new(),
            limits,
            budget,
            bytes: 0,
            entries: 0,
            work: 0,
        }
    }

    /// Work actually performed: bytes read, entries scanned, and steps.
    /// Exposed for operator logging and the aggregate-cost regression.
    pub fn work(&self) -> (u64, u64, u64) {
        (self.bytes, self.entries, self.work)
    }

    fn charge_bytes(&mut self, bytes: u64) -> Result<(), ClosureError> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > self.budget.max_bytes {
            return Err(ClosureError::WorkBudgetExhausted);
        }
        Ok(())
    }

    fn charge_entries(&mut self, entries: u64) -> Result<(), ClosureError> {
        self.entries = self.entries.saturating_add(entries);
        if self.entries > self.budget.max_entries {
            return Err(ClosureError::WorkBudgetExhausted);
        }
        Ok(())
    }

    fn charge_work(&mut self) -> Result<(), ClosureError> {
        self.work = self.work.saturating_add(1);
        if self.work > self.budget.max_work {
            return Err(ClosureError::WorkBudgetExhausted);
        }
        Ok(())
    }

    /// Verify one snapshot's manifest/tree correspondence using this
    /// verifier's cache and budget. The full check is spelled out here;
    /// [`verify_snapshot_manifest`] is the single-head wrapper over it.
    fn verify_snapshot<S, M>(
        &mut self,
        snapshot: &Snapshot,
        objects: &S,
        root_id: &ContentId,
        root: &Manifest,
        children: &M,
    ) -> Result<(), ClosureError>
    where
        S: ObjectStore,
        S::Error: std::fmt::Debug,
        M: ManifestSource,
    {
        let snapshot_id = snapshot.snapshot_id();
        if root.snapshot() != snapshot_id {
            return Err(ClosureError::RootSnapshotMismatch {
                expected: snapshot_id,
                found: root.snapshot(),
            });
        }
        check_canonical(root)?;
        let derived = ContentId::derive(ObjectKind::Manifest, &root.canonical_bytes());
        if &derived != root_id {
            return Err(ClosureError::RootIdentityMismatch {
                found: *root_id,
                derived,
            });
        }
        check_manifest(&self.limits, root)?;

        // Pair every reachable tree with the one manifest that declares it,
        // so a second, different claimant is a detected contradiction.
        let mut mapping: BTreeMap<ContentId, ContentId> = BTreeMap::new();
        mapping.insert(snapshot.tree, *root_id);
        let mut stack: Vec<(ContentId, &Manifest)> = vec![(snapshot.tree, root)];
        let mut tree_sizes: BTreeMap<ContentId, u64> = BTreeMap::new();
        let mut chunks: BTreeSet<ContentId> = BTreeSet::new();
        let mut visited_manifests: Vec<&Manifest> = Vec::new();

        while let Some((tree_id, manifest)) = stack.pop() {
            // One verification step for this (tree, manifest) pair,
            // charged even on a cache hit: the correspondence work is
            // real per head, only byte I/O and decode are reused.
            self.charge_work()?;
            if !self.trees.contains_key(&tree_id) {
                let bytes = objects
                    .get(&tree_id)
                    .map_err(|error| ClosureError::ObjectStore {
                        failure: error.failure(),
                        detail: format!("{error:?}"),
                    })?
                    .ok_or(ClosureError::TreeUnavailable(tree_id))?;
                if ContentId::derive(ObjectKind::Tree, &bytes) != tree_id {
                    return Err(ClosureError::TreeIdentityMismatch(tree_id));
                }
                // Charge before decoding so a hostile byte string
                // cannot spend unbounded CPU before the budget stops it.
                self.charge_bytes(bytes.len() as u64)?;
                let tree = Tree::decode(&bytes).map_err(|_| ClosureError::InvalidTree(tree_id))?;
                check_tree(&self.limits, &tree)?;
                let mut tree_chunks = Vec::new();
                let mut directories = Vec::new();
                for entry in tree.entries() {
                    match &entry.content {
                        EntryContent::File { chunks: ids, .. } => {
                            tree_chunks.extend(ids.iter().copied())
                        }
                        EntryContent::Dir { subtree } => directories.push(*subtree),
                        EntryContent::Symlink { .. } => {}
                    }
                }
                self.charge_entries(tree.entries().len() as u64)?;
                self.trees.insert(
                    tree_id,
                    CachedTree {
                        byte_len: bytes.len() as u64,
                        chunks: tree_chunks,
                        directories,
                    },
                );
            }
            let cached = &self.trees[&tree_id];
            tree_sizes.insert(tree_id, cached.byte_len);
            visited_manifests.push(manifest);

            // Per-manifest lookups: a tree of a million files against a
            // manifest of a million entries must stay O(n log n).
            let entry_lookup: BTreeMap<ContentId, &ManifestEntry> = manifest
                .entries()
                .iter()
                .map(|entry| (entry.content_id, entry))
                .collect();
            let link_lookup: BTreeMap<ContentId, &ChildManifest> = manifest
                .children()
                .iter()
                .map(|link| (link.tree, link))
                .collect();
            let mut directories: BTreeSet<ContentId> = BTreeSet::new();
            for chunk in &cached.chunks {
                let declared =
                    entry_lookup
                        .get(chunk)
                        .copied()
                        .ok_or(ClosureError::MissingChunkEntry {
                            tree: tree_id,
                            chunk: *chunk,
                        })?;
                if declared.kind != ObjectKind::Chunk {
                    return Err(ClosureError::KindMismatch {
                        content: *chunk,
                        expected: ObjectKind::Chunk,
                        found: declared.kind,
                    });
                }
                chunks.insert(*chunk);
            }
            for subtree in &cached.directories {
                directories.insert(*subtree);
            }
            // A child link the tree does not name is an unrelated declaration.
            for link in manifest.children() {
                if !directories.contains(&link.tree) {
                    return Err(ClosureError::UnexpectedChildManifest {
                        tree: tree_id,
                        child: link.tree,
                    });
                }
            }
            // Every directory names exactly one child manifest, and the
            // tree's manifest claimant must be unique.
            for subtree in &directories {
                let link = link_lookup.get(subtree).copied().ok_or(
                    ClosureError::MissingChildManifest {
                        tree: tree_id,
                        child: *subtree,
                    },
                )?;
                match mapping.get(subtree) {
                    Some(existing) if existing != &link.manifest => {
                        return Err(ClosureError::AmbiguousTreeMapping {
                            tree: *subtree,
                            first: *existing,
                            second: link.manifest,
                        });
                    }
                    Some(_) => continue,
                    None => {
                        mapping.insert(*subtree, link.manifest);
                    }
                }
                let child = children.manifest(&link.manifest).ok_or(
                    ClosureError::MissingChildManifestRecord {
                        child: *subtree,
                        manifest: link.manifest,
                    },
                )?;
                // The source is indexed by the claimed id, but a generic
                // source may hand back bytes that derive to a different
                // one. Bind the child's canonical identity to the id its
                // parent named, exactly as the root identity is bound.
                check_canonical(child)?;
                let derived = ContentId::derive(ObjectKind::Manifest, &child.canonical_bytes());
                if derived != link.manifest {
                    return Err(ClosureError::ChildIdentityMismatch {
                        expected: link.manifest,
                        derived,
                    });
                }
                if child.snapshot() != snapshot_id {
                    return Err(ClosureError::ChildSnapshotMismatch {
                        manifest: link.manifest,
                    });
                }
                check_manifest(&self.limits, child)?;
                stack.push((*subtree, child));
            }
        }

        // With both closures resolved, every entry must land in one of them.
        // Manifest entries are already in memory, but scanning them is
        // still per-head work, so they count against the entry ceiling.
        let trees: BTreeSet<ContentId> = tree_sizes.keys().copied().collect();
        for manifest in visited_manifests {
            self.charge_entries(manifest.entries().len() as u64)?;
            for entry in manifest.entries() {
                let is_chunk = chunks.contains(&entry.content_id);
                let is_tree = trees.contains(&entry.content_id);
                if !is_chunk && !is_tree {
                    return Err(ClosureError::UnrelatedEntry {
                        content: entry.content_id,
                    });
                }
                if is_chunk && entry.kind != ObjectKind::Chunk {
                    return Err(ClosureError::KindMismatch {
                        content: entry.content_id,
                        expected: ObjectKind::Chunk,
                        found: entry.kind,
                    });
                }
                if is_tree {
                    if entry.kind != ObjectKind::Tree {
                        return Err(ClosureError::KindMismatch {
                            content: entry.content_id,
                            expected: ObjectKind::Tree,
                            found: entry.kind,
                        });
                    }
                    let actual = tree_sizes.get(&entry.content_id).copied().unwrap_or(0);
                    if entry.size != actual {
                        return Err(ClosureError::TreeEntrySizeMismatch {
                            content: entry.content_id,
                            expected: actual,
                            found: entry.size,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Verify that the manifest hierarchy rooted at `root` corresponds to the
/// tree closure rooted at `snapshot.tree`.
///
/// `root_id` is `root`'s logical identity (the value `root.canonical_bytes()`
/// derives to), and `children` resolves any descendant manifest by the
/// `ChildManifest::manifest` identity its parent links. The plaintext tree
/// objects are read from `objects`; missing trees fail closed.
///
/// Checks: every manifest is canonical (strictly ascending entries and
/// children) and every child manifest's canonical identity matches the id its
/// parent named; every directory subtree has a corresponding child manifest
/// and vice versa; every file chunk has a manifest entry of kind `Chunk`;
/// every manifest entry is reachable from the tree (a chunk or, optionally, a
/// tree node), with matching kind and, for a `Tree` entry, matching plaintext
/// size; no unreachable mapping is admitted. The tree's own declared file
/// size is deliberately **not** compared to the chunk list: the format
/// permits them to disagree and makes that inconsistency the reader's job
/// (`docs/contracts/object-model.md`), while a chunk representation's declared size is
/// enforced authoritatively by `crate::seal::verify` when it is fetched. Work
/// is bounded by [`Limits`] per tree and manifest and by the distinct
/// reachable tree nodes: the walk is iterative and deduplicated, so a shared
/// or deep DAG cannot explode it.
pub fn verify_snapshot_manifest<S, M>(
    snapshot: &Snapshot,
    objects: &S,
    root_id: &ContentId,
    root: &Manifest,
    children: &M,
    limits: &Limits,
) -> Result<(), ClosureError>
where
    S: ObjectStore,
    S::Error: std::fmt::Debug,
    M: ManifestSource,
{
    // The single-head entry point (authoring self-checks and unit
    // tests) uses an unbounded verifier: there is no cross-head
    // fan-out to cap here, and behavior is unchanged.
    ClosureVerifier::new(HeadGateBudget::UNLIMITED, *limits)
        .verify_snapshot(snapshot, objects, root_id, root, children)
}

/// Verify one classified head's closure against the local store: resolve the
/// recorded root manifest for the head's snapshot and run
/// [`verify_snapshot_manifest`]. `Err` means the head is not materializable —
/// its closure is incomplete or does not correspond — and callers must not
/// install or serve it.
pub fn verify_head_closure<S: ObjectStore>(
    runtime: &crate::runtime::RuntimeState,
    snapshot: &Snapshot,
    objects: &S,
    limits: &Limits,
) -> Result<(), ClosureError>
where
    S::Error: std::fmt::Debug,
{
    let root = runtime
        .root_manifest_record(&snapshot.snapshot_id())
        .ok_or(ClosureError::RootManifestMissing(snapshot.snapshot_id()))?;
    verify_snapshot_manifest(
        snapshot,
        objects,
        &root.manifest_id,
        &root.manifest,
        runtime,
        limits,
    )
}

/// The batch form of [`verify_head_closure`]. One verifier is shared by
/// every head in a gate run, so a tree referenced by several heads is
/// read, decoded, checked, and scanned once, and all work is charged to
/// one [`HeadGateBudget`] and one [`Limits`] table (both fixed at
/// construction). `WorkBudgetExhausted` means the whole batch must be
/// deferred — it is not a verdict on this head.
pub fn verify_head_closure_with<S: ObjectStore>(
    verifier: &mut ClosureVerifier,
    runtime: &crate::runtime::RuntimeState,
    snapshot: &Snapshot,
    objects: &S,
) -> Result<(), ClosureError>
where
    S::Error: std::fmt::Debug,
{
    let root = runtime
        .root_manifest_record(&snapshot.snapshot_id())
        .ok_or(ClosureError::RootManifestMissing(snapshot.snapshot_id()))?;
    verifier.verify_snapshot(
        snapshot,
        objects,
        &root.manifest_id,
        &root.manifest,
        runtime,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use wyrd_format::{
        BaoRoot, ChildManifest, DeviceId, Entry, ManifestEntry, MemoryObjectStore, StorageId,
        TransitionId,
    };

    /// A store that counts reads per content id, so the reuse guardrail
    /// can assert a shared tree is read exactly once per gate run.
    #[derive(Default)]
    struct CountingStore {
        inner: MemoryObjectStore,
        reads: RefCell<HashMap<ContentId, usize>>,
    }
    #[derive(Debug)]
    struct CountingError;
    impl StoreError for CountingError {}
    impl ObjectStore for CountingStore {
        type Error = CountingError;
        fn insert(&mut self, kind: ObjectKind, data: &[u8]) -> Result<ContentId, Self::Error> {
            self.inner.insert(kind, data).map_err(|_| CountingError)
        }
        fn insert_verified(
            &mut self,
            kind: ObjectKind,
            expected: &ContentId,
            data: &[u8],
        ) -> Result<(), Self::Error> {
            self.inner
                .insert_verified(kind, expected, data)
                .map_err(|_| CountingError)
        }
        fn get(&self, id: &ContentId) -> Result<Option<Vec<u8>>, Self::Error> {
            *self.reads.borrow_mut().entry(*id).or_default() += 1;
            self.inner.get(id).map_err(|_| CountingError)
        }
        fn has(&self, id: &ContentId) -> Result<bool, Self::Error> {
            self.inner.has(id).map_err(|_| CountingError)
        }
    }

    fn chunk(data: &[u8]) -> (ContentId, ManifestEntry) {
        let id = ContentId::derive(ObjectKind::Chunk, data);
        let entry = ManifestEntry {
            content_id: id,
            kind: ObjectKind::Chunk,
            version: 0,
            storage_id: StorageId::from_bytes([0xA0; 32]),
            encryption_epoch: 1,
            size: data.len() as u64,
            transport: BaoRoot::from_bytes([0xB0; 32]),
        };
        (id, entry)
    }

    fn store_tree(store: &mut MemoryObjectStore, tree: &Tree) -> ContentId {
        let bytes = tree.encode();
        store.insert(ObjectKind::Tree, &bytes).unwrap()
    }

    fn snapshot(tree: ContentId) -> Snapshot {
        Snapshot::new(
            Vec::new(),
            tree,
            DeviceId::from_bytes([0x01; 32]),
            TransitionId::from_bytes([0x02; 32]),
            1,
            0,
            7,
        )
        .unwrap()
    }

    fn manifest(snapshot: &Snapshot, entries: Vec<ManifestEntry>) -> Manifest {
        Manifest::new(snapshot.snapshot_id(), entries, Vec::new()).unwrap()
    }

    fn manifest_with_children(
        snapshot: &Snapshot,
        entries: Vec<ManifestEntry>,
        children: Vec<ChildManifest>,
    ) -> Manifest {
        Manifest::new(snapshot.snapshot_id(), entries, children).unwrap()
    }

    fn manifest_id(manifest: &Manifest) -> ContentId {
        ContentId::derive(ObjectKind::Manifest, &manifest.canonical_bytes())
    }

    /// A root tree with one file of two chunks, both mapped.
    fn flat() -> (MemoryObjectStore, Snapshot, ContentId, Manifest) {
        let mut store = MemoryObjectStore::default();
        let (c1, e1) = chunk(b"hello ");
        let (c2, e2) = chunk(b"world");
        let tree = Tree::from_entries(vec![
            Entry::file("greeting.txt", 11, false, vec![c1, c2]).unwrap()
        ])
        .unwrap();
        let tree_id = store_tree(&mut store, &tree);
        let snapshot = snapshot(tree_id);
        let manifest = manifest(&snapshot, vec![e1, e2]);
        (store, snapshot, tree_id, manifest)
    }

    #[test]
    fn corresponding_manifest_verifies() {
        let (store, snapshot, _tree_id, root) = flat();
        verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap();
    }

    /// A wide flat tree shared by several distinct heads, plus the
    /// per-head manifest that maps it. Heads differ only in authoring
    /// device so their snapshot ids (and root manifests) differ while
    /// the tree identity is common.
    /// (snapshot, root id, root manifest, manifest-by-id source)
    type SharedHead = (Snapshot, ContentId, Manifest, BTreeMap<ContentId, Manifest>);

    fn wide_shared_heads(
        wide: usize,
        heads: usize,
    ) -> (MemoryObjectStore, ContentId, u64, Vec<SharedHead>) {
        let mut store = MemoryObjectStore::default();
        let mut tree_entries = Vec::with_capacity(wide);
        let mut manifest_entries = Vec::with_capacity(wide);
        for i in 0..wide {
            let data = format!("payload-{i:04}");
            let (chunk, entry) = chunk(data.as_bytes());
            tree_entries.push(
                Entry::file(format!("f-{i:04}"), data.len() as u64, false, vec![chunk]).unwrap(),
            );
            manifest_entries.push(entry);
        }
        manifest_entries.sort_by_key(|entry| entry.content_id);
        let tree = Tree::from_entries(tree_entries).unwrap();
        let bytes = tree.encode();
        let tree_len = bytes.len() as u64;
        let tree_id = store.insert(ObjectKind::Tree, &bytes).unwrap();

        let mut result = Vec::with_capacity(heads);
        for i in 0..heads {
            let snapshot = Snapshot::new(
                Vec::new(),
                tree_id,
                DeviceId::from_bytes([i as u8 + 1; 32]),
                TransitionId::from_bytes([0x02; 32]),
                1,
                0,
                7,
            )
            .unwrap();
            let root = Manifest::new(snapshot.snapshot_id(), manifest_entries.clone(), Vec::new())
                .unwrap();
            let root_id = manifest_id(&root);
            let mut children = BTreeMap::new();
            children.insert(root_id, root.clone());
            result.push((snapshot, root_id, root, children));
        }
        (store, tree_id, tree_len, result)
    }

    /// The aggregate-cost guardrail: eight heads over one wide tree
    /// decode it once, not eight times. The budget is sized to fit one
    /// tree's worth of work exactly, so reuse is what keeps the batch
    /// under budget; a fresh verifier per head would exhaust it.
    #[test]
    fn shared_wide_tree_is_read_and_scanned_once_per_gate_run() {
        const WIDE: usize = 128;
        const HEADS: usize = 8;
        let (inner, tree_id, tree_len, heads) = wide_shared_heads(WIDE, HEADS);
        let tree_bytes = inner.get(&tree_id).unwrap().unwrap();
        let store = CountingStore {
            inner,
            reads: RefCell::new(HashMap::new()),
        };
        // Exactly one tree's worth of bytes/entries, and one step per
        // head: sharing is the only reason this fits.
        let budget = HeadGateBudget {
            max_bytes: tree_len,
            max_entries: WIDE as u64 * (HEADS as u64 + 1),
            max_work: HEADS as u64,
        };
        let mut verifier = ClosureVerifier::new(budget, Limits::V0);
        for (snapshot, root_id, root, children) in &heads {
            verifier
                .verify_snapshot(snapshot, &store, root_id, root, children)
                .expect("shared batch stays within a one-tree budget");
        }
        assert_eq!(
            store.reads.borrow().get(&tree_id).copied().unwrap_or(0),
            1,
            "the shared tree is read once per gate run, not once per head"
        );
        assert_eq!(
            verifier.work(),
            (tree_len, WIDE as u64 * (HEADS as u64 + 1), HEADS as u64),
            "tree bytes and entries are charged once; manifest entries and steps per head"
        );

        // The same heads through a fresh verifier each time read the
        // tree once per head — precisely the behavior this fix replaces.
        let mut fresh_inner = MemoryObjectStore::default();
        fresh_inner.insert(ObjectKind::Tree, &tree_bytes).unwrap();
        let fresh = CountingStore {
            inner: fresh_inner,
            reads: RefCell::new(HashMap::new()),
        };
        for (snapshot, root_id, root, children) in &heads {
            let mut once = ClosureVerifier::new(HeadGateBudget::UNLIMITED, Limits::V0);
            once.verify_snapshot(snapshot, &fresh, root_id, root, children)
                .expect("a single head still verifies");
        }
        assert_eq!(
            fresh.reads.borrow().get(&tree_id).copied().unwrap_or(0),
            HEADS,
            "without reuse the shared tree is read once per head"
        );
    }

    /// The shipped default must never defer a lawful drive: one head
    /// may legally declare `max_manifest_children` subtrees (each a
    /// verification step) and carry `max_tree_entries` plus
    /// `max_manifest_entries` scanned entries, and several max-size
    /// trees must fit in the byte cap. Pin those relationships so a
    /// future tightening cannot turn a valid head into an unpublishable
    /// one on every pass.
    #[test]
    fn default_budget_covers_the_lawful_envelope() {
        let default = HeadGateBudget::DEFAULT;
        assert!(
            default.max_work > Limits::V0.max_manifest_children as u64,
            "one lawful head's subtree fan-out must fit the step cap"
        );
        assert!(
            default.max_entries
                > (Limits::V0.max_tree_entries + Limits::V0.max_manifest_entries) as u64,
            "one lawful head's tree plus manifest entries must fit the entry cap"
        );
        assert!(
            default.max_bytes >= Limits::V0.max_object_bytes as u64 * 8,
            "several max-size trees must fit the byte cap"
        );
    }

    /// Distinct large trees are charged separately, so a fan-out of
    /// heads that do not share content exhausts the aggregate budget
    /// even though every individual tree is within `Limits::V0`.
    #[test]
    fn aggregate_work_budget_exhausts_on_distinct_trees() {
        const WIDE: usize = 64;
        const HEADS: usize = 4;
        let (inner, _tree_id, tree_len, heads) = wide_shared_heads(WIDE, HEADS);
        let store = CountingStore {
            inner,
            reads: RefCell::new(HashMap::new()),
        };
        let budget = HeadGateBudget {
            max_bytes: tree_len,
            max_entries: WIDE as u64 * (HEADS as u64 + 1),
            max_work: HEADS as u64 - 1,
        };
        let mut verifier = ClosureVerifier::new(budget, Limits::V0);
        let mut exhausted = false;
        for (snapshot, root_id, root, children) in &heads {
            match verifier.verify_snapshot(snapshot, &store, root_id, root, children) {
                Ok(()) => {}
                Err(ClosureError::WorkBudgetExhausted) => {
                    exhausted = true;
                    break;
                }
                Err(other) => panic!("unexpected closure verdict: {other:?}"),
            }
        }
        assert!(exhausted, "the aggregate step budget stops the batch");
        assert_eq!(
            verifier.work().2,
            HEADS as u64,
            "the last allowed step is charged before exhaustion"
        );
    }

    #[test]
    fn optional_tree_entry_agrees_when_present() {
        let (store, snapshot, tree_id, root) = flat();
        // A `Tree` entry for the root tree: legitimate, size must match.
        let size = store.get(&tree_id).unwrap().unwrap().len() as u64;
        let mut entries = root.entries().to_vec();
        entries.push(ManifestEntry {
            content_id: tree_id,
            kind: ObjectKind::Tree,
            version: 0,
            storage_id: StorageId::from_bytes([0xC0; 32]),
            encryption_epoch: 1,
            size,
            transport: BaoRoot::from_bytes([0xD0; 32]),
        });
        let root = Manifest::new(snapshot.snapshot_id(), entries, Vec::new()).unwrap();
        verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap();

        // Wrong declared size for the tree entry is rejected.
        let mut entries = root.entries().to_vec();
        entries
            .iter_mut()
            .find(|entry| entry.content_id == tree_id)
            .unwrap()
            .size = size + 1;
        let root = Manifest::new(snapshot.snapshot_id(), entries, Vec::new()).unwrap();
        let err = verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap_err();
        assert!(matches!(err, ClosureError::TreeEntrySizeMismatch { .. }));
    }

    #[test]
    fn mismatched_snapshot_and_manifest_are_rejected() {
        // Snapshot over tree T1; a valid manifest for the *chunks of T2*.
        let (mut store, snapshot, _t1, _root) = flat();
        let (c_other, e_other) = chunk(b"unrelated");
        let other =
            Tree::from_entries(vec![Entry::file("other", 9, false, vec![c_other]).unwrap()])
                .unwrap();
        let other_id = store_tree(&mut store, &other);
        assert_ne!(snapshot.tree, other_id);
        let root = manifest(&snapshot, vec![e_other]);
        let err = verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap_err();
        assert!(
            matches!(err, ClosureError::MissingChunkEntry { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn unrelated_entry_is_rejected_even_with_a_full_closure() {
        // T1's chunks are all mapped, but an extra mapping rides along.
        let (store, snapshot, _t1, root) = flat();
        let (_, e_extra) = chunk(b"trespassing");
        let mut entries = root.entries().to_vec();
        entries.push(e_extra);
        let root = Manifest::new(snapshot.snapshot_id(), entries, Vec::new()).unwrap();
        let err = verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap_err();
        assert!(
            matches!(err, ClosureError::UnrelatedEntry { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn nested_subtree_must_have_a_corresponding_child_manifest() {
        let mut store = MemoryObjectStore::default();
        let (c, e) = chunk(b"leaf");
        let leaf =
            Tree::from_entries(vec![Entry::file("leaf", 4, false, vec![c]).unwrap()]).unwrap();
        let leaf_id = store_tree(&mut store, &leaf);
        let root_tree = Tree::from_entries(vec![Entry::dir("sub", leaf_id).unwrap()]).unwrap();
        let root_tree_id = store_tree(&mut store, &root_tree);
        let snapshot = snapshot(root_tree_id);

        // Missing child link: the directory has no child manifest.
        let root = manifest(&snapshot, Vec::new());
        let err = verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap_err();
        assert!(matches!(err, ClosureError::MissingChildManifest { .. }));

        // A well-formed child link verifies, with the child mapping its chunk.
        let child = manifest(&snapshot, vec![e]);
        let wrong_id = ContentId::from_bytes([0xAB; 32]);
        let root = manifest_with_children(
            &snapshot,
            Vec::new(),
            vec![ChildManifest {
                tree: leaf_id,
                manifest: wrong_id,
                storage: StorageId::from_bytes([0xE0; 32]),
                transport: BaoRoot::from_bytes([0xF0; 32]),
            }],
        );
        let children = BTreeMap::from([(wrong_id, child)]);
        let err = verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &children,
            &Limits::V0,
        )
        .unwrap_err();
        assert!(
            matches!(err, ClosureError::ChildIdentityMismatch { .. }),
            "unexpected error: {err:?}"
        );
    }

    // NOTE: a duplicated-entry manifest is no longer representable as a
    // typed value (`Manifest::new` rejects duplicate keys, as does the
    // decoder), so there is no non-canonical input left for the closure
    // verifier's canonicality check to refuse. Duplicate rejection is
    // covered at the construction boundary in `wyrd-format` instead.

    #[test]
    fn repeated_subtree_reference_verifies() {
        // Two directory names resolving to the same subtree share one child
        // manifest: the walk must dedupe the tree, not reject the repeat.
        let mut store = MemoryObjectStore::default();
        let (c, e) = chunk(b"leaf");
        let leaf =
            Tree::from_entries(vec![Entry::file("leaf", 4, false, vec![c]).unwrap()]).unwrap();
        let leaf_id = store_tree(&mut store, &leaf);
        let root_tree = Tree::from_entries(vec![
            Entry::dir("a", leaf_id).unwrap(),
            Entry::dir("b", leaf_id).unwrap(),
        ])
        .unwrap();
        let root_tree_id = store_tree(&mut store, &root_tree);
        let snapshot = snapshot(root_tree_id);

        let child = manifest(&snapshot, vec![e]);
        let child_id = manifest_id(&child);
        let root = manifest_with_children(
            &snapshot,
            Vec::new(),
            vec![ChildManifest {
                tree: leaf_id,
                manifest: child_id,
                storage: StorageId::from_bytes([0xE0; 32]),
                transport: BaoRoot::from_bytes([0xF0; 32]),
            }],
        );
        let children = BTreeMap::from([(child_id, child)]);
        verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &children,
            &Limits::V0,
        )
        .unwrap();
    }

    #[test]
    fn deep_closure_is_bounded_and_iterative() {
        // A long dir chain walks iteratively: no recursion, no stack
        // growth, one manifest per tree node.
        const DEPTH: usize = 512;
        let mut store = MemoryObjectStore::default();
        let mut tree_ids = vec![store_tree(&mut store, &Tree::empty())];
        for _ in 0..DEPTH {
            let child = *tree_ids.last().expect("chain nonempty");
            let parent = Tree::from_entries(vec![Entry::dir("d", child).unwrap()]).unwrap();
            tree_ids.push(store_tree(&mut store, &parent));
        }
        let snapshot = snapshot(*tree_ids.last().expect("root"));

        let mut manifests: BTreeMap<ContentId, Manifest> = BTreeMap::new();
        let mut current = manifest(&snapshot, Vec::new());
        let mut current_id = manifest_id(&current);
        for i in 1..=DEPTH {
            let child_tree = tree_ids[i - 1];
            let parent = manifest_with_children(
                &snapshot,
                Vec::new(),
                vec![ChildManifest {
                    tree: child_tree,
                    manifest: current_id,
                    storage: StorageId::from_bytes([0xE0; 32]),
                    transport: BaoRoot::from_bytes([0xF0; 32]),
                }],
            );
            let parent_id = manifest_id(&parent);
            manifests.insert(current_id, current);
            current = parent;
            current_id = parent_id;
        }
        verify_snapshot_manifest(
            &snapshot,
            &store,
            &current_id,
            &current,
            &manifests,
            &Limits::V0,
        )
        .unwrap();
    }

    #[test]
    fn missing_tree_object_fails_closed() {
        let (store, _, _, root) = flat();
        let missing = Snapshot::new(
            Vec::new(),
            ContentId::from_bytes([0x99; 32]),
            DeviceId::from_bytes([0x01; 32]),
            TransitionId::from_bytes([0x02; 32]),
            1,
            0,
            7,
        )
        .unwrap();
        let root = Manifest::new(
            missing.snapshot_id(),
            root.entries().to_vec(),
            root.children().to_vec(),
        )
        .unwrap();
        let err = verify_snapshot_manifest(
            &missing,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap_err();
        assert!(matches!(err, ClosureError::TreeUnavailable(_)));
    }

    /// Every rejection classifies for the operator summary: unfetched
    /// closures read as incomplete (may heal), bytes that do not hash
    /// back read as identity mismatches (will not heal), and the
    /// remaining structural verdicts read as other damage. Store I/O
    /// is not a rejection — it keeps the store's own classification.
    #[test]
    fn rejection_classes_cover_every_variant() {
        use RejectionClass::{IdentityMismatch, Incomplete, Other};
        let id = ContentId::from_bytes([0x11; 32]);
        let sid = SnapshotId::from_bytes([0x22; 32]);
        let cases: Vec<(ClosureError, Option<RejectionClass>)> = vec![
            (
                ClosureError::RootSnapshotMismatch {
                    expected: sid,
                    found: sid,
                },
                Some(Other),
            ),
            (ClosureError::RootManifestMissing(sid), Some(Incomplete)),
            (
                ClosureError::RootIdentityMismatch {
                    found: id,
                    derived: id,
                },
                Some(IdentityMismatch),
            ),
            (
                ClosureError::ChildIdentityMismatch {
                    expected: id,
                    derived: id,
                },
                Some(IdentityMismatch),
            ),
            (ClosureError::NonCanonicalManifest, Some(Other)),
            (ClosureError::TreeUnavailable(id), Some(Incomplete)),
            (
                ClosureError::TreeIdentityMismatch(id),
                Some(IdentityMismatch),
            ),
            (ClosureError::InvalidTree(id), Some(Other)),
            (
                ClosureError::AmbiguousTreeMapping {
                    tree: id,
                    first: id,
                    second: id,
                },
                Some(Other),
            ),
            (
                ClosureError::MissingChildManifest {
                    tree: id,
                    child: id,
                },
                Some(Other),
            ),
            (
                ClosureError::MissingChildManifestRecord {
                    child: id,
                    manifest: id,
                },
                Some(Incomplete),
            ),
            (
                ClosureError::ChildSnapshotMismatch { manifest: id },
                Some(Other),
            ),
            (
                ClosureError::UnexpectedChildManifest {
                    tree: id,
                    child: id,
                },
                Some(Other),
            ),
            (
                ClosureError::MissingChunkEntry {
                    tree: id,
                    chunk: id,
                },
                Some(Other),
            ),
            (ClosureError::UnrelatedEntry { content: id }, Some(Other)),
            (
                ClosureError::KindMismatch {
                    content: id,
                    expected: ObjectKind::Chunk,
                    found: ObjectKind::Tree,
                },
                Some(Other),
            ),
            (
                ClosureError::TreeEntrySizeMismatch {
                    content: id,
                    expected: 1,
                    found: 2,
                },
                Some(Other),
            ),
            (
                ClosureError::Ingest(IngestError::TooLarge {
                    what: "tree",
                    bytes: 9,
                    max: 8,
                }),
                Some(Other),
            ),
            (
                ClosureError::ObjectStore {
                    failure: StoreFailure::StorageFull,
                    detail: "simulated".to_string(),
                },
                None,
            ),
            (ClosureError::WorkBudgetExhausted, None),
        ];
        assert_eq!(
            cases.len(),
            20,
            "one case per ClosureError variant, mirroring the exhaustive match in rejection_class"
        );
        for (error, expected) in cases {
            assert_eq!(
                error.rejection_class(),
                expected,
                "wrong operator class for {error:?}"
            );
        }
    }

    /// A wide flat closure verifies under `Limits::V0`: thousands of
    /// files, every chunk mapped, the root tree self-mapped. At 4096
    /// files this is a sample, not a ceiling measurement — about 0.7%
    /// of `Limits::V0.max_manifest_entries` and 0.4% of
    /// `max_tree_entries` (sample run 2026-10-01, Apple Silicon:
    /// ~30 ms); the ceiling edge itself is pinned by
    /// `pinned_limits_reject_an_overwide_manifest` instead. The
    /// elapsed time is recorded, not asserted — wall clocks are not
    /// contracts — so the fetch hot path has a number to revisit.
    /// Peak memory was not measured.
    #[test]
    fn wide_flat_closure_verifies_and_reports_cost() {
        const FILES: usize = 4096;
        let mut store = MemoryObjectStore::default();
        let mut tree_entries = Vec::with_capacity(FILES);
        let mut manifest_entries = Vec::with_capacity(FILES + 1);
        for i in 0..FILES {
            // Fixed-width indexes keep every payload the same
            // length, so the declared file size is constant.
            let payload = format!("wide file {i:04}");
            let (id, entry) = chunk(payload.as_bytes());
            tree_entries.push(
                Entry::file(
                    format!("file-{i:05}.txt"),
                    payload.len() as u64,
                    false,
                    vec![id],
                )
                .unwrap(),
            );
            manifest_entries.push(entry);
        }
        let tree = Tree::from_entries(tree_entries).unwrap();
        let tree_bytes = tree.encode();
        let tree_id = store_tree(&mut store, &tree);
        manifest_entries.push(ManifestEntry {
            content_id: tree_id,
            kind: ObjectKind::Tree,
            version: 0,
            storage_id: StorageId::from_bytes([0xC0; 32]),
            encryption_epoch: 1,
            size: tree_bytes.len() as u64,
            transport: BaoRoot::from_bytes([0xD0; 32]),
        });
        let snapshot = snapshot(tree_id);
        let root = manifest(&snapshot, manifest_entries);
        let root_id = manifest_id(&root);

        let start = std::time::Instant::now();
        verify_snapshot_manifest(
            &snapshot,
            &store,
            &root_id,
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &Limits::V0,
        )
        .unwrap();
        eprintln!(
            "wide flat closure: {FILES} files ({} manifest entries) verified in {:?}",
            root.entries().len(),
            start.elapsed()
        );
    }

    /// The count ceilings bind just over the edge: a pinned table
    /// with room for two manifest entries rejects a three-entry
    /// closure, proving the near-limit behavior without
    /// million-entry fixtures.
    #[test]
    fn pinned_limits_reject_an_overwide_manifest() {
        let (store, snapshot, _tree_id, root) = flat();
        let (_, extra) = chunk(b"extra");
        let mut entries = root.entries().to_vec();
        entries.push(extra);
        let root = Manifest::new(snapshot.snapshot_id(), entries, Vec::new()).unwrap();
        assert_eq!(
            root.entries().len(),
            3,
            "the fixture exceeds the pin by one"
        );
        let limits = Limits {
            max_manifest_entries: 2,
            ..Limits::V0
        };
        let err = verify_snapshot_manifest(
            &snapshot,
            &store,
            &manifest_id(&root),
            &root,
            &BTreeMap::<ContentId, Manifest>::new(),
            &limits,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                ClosureError::Ingest(IngestError::TooMany {
                    what: "manifest entries",
                    count: 3,
                    max: 2,
                })
            ),
            "unexpected error: {err:?}"
        );
    }
}
