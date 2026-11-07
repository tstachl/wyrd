//! Conflict surface: the person-visible divergence vocabulary (OD-23-A/B/C/D,
//! all decided as recommended).
//!
//! Three responsibilities, kept distinct in type and copy:
//!
//! 1. Indicate: a standing drive-level verdict from the eligible heads —
//!    more than one eligible head means the drive has diverged. Never a
//!    per-path claim, never a zero-heads false alarm (a fresh drive has
//!    no live head; that is not a conflict).
//! 2. Inspect: one bounded, read-only root-path comparison built from
//!    the merge plan the resolver UI already consumes (`MergePlan`),
//!    filtered to `!agreed()` before rendering. The scope sentence
//!    ("root paths only") rides every rendering, and a budget
//!    exhaustion yields an explicitly incomplete result — never a
//!    partial set presented as the complete set.
//! 3. Explain limits: membership freezes named with their remedy, and
//!    unreadable branch contents rendered as unavailable — never an
//!    empty document, never a zero-byte file, never evidence of
//!    absence.
//!
//! What the surface never does (DG-3's negative invariant, quoted in
//! the issue and pinned by test): author a resolution, hide one, or
//! reduce what is addressable. Every function here is read-only over
//! references; none takes an engine, a store, or anything mutable, so
//! the authorship test can only fail if someone adds an authoring
//! affordance later — which is the point.
//!
//! Branch labels (OD-23-A option A): author device, epoch, short
//! snapshot id. No `@N` anywhere in this module's output — not as a
//! branch name, not as an inspection label, not as a plan column. The
//! version-qualified lookup stays what it is today, something a person
//! may type, never a promise the UI makes about numbering. Neither
//! this module nor its tests implement version ordering: the grammar
//! is `wyrd-fuse`'s (`grammar.rs`), and a second copy of the numbering
//! would be a second answer to `docs/object-model.md:382`.
//!
//! When a host presenter exists to own person-visible copy, the
//! rendering below migrates there and this module keeps the state
//! vocabulary and predicates; until then the copy lives next to the
//! mapping its tests pin (same arrangement as `relay_posture`,
//! `status_surface`, and `storage_surface`).

use std::collections::BTreeMap;

use wyrd_format::{DeviceId, SnapshotId};
use wyrd_sync::runtime::{MergePath, MergePlan};

use super::status::HeadSummary;
use wyrd_namespace::view::{Node, ViewError};

/// One diverged branch, labelled by facts the engine already holds
/// (OD-23-A): author device, epoch, short snapshot id. The short id is
/// the first 8 hex characters — enough to tell branches apart on
/// screen, never a lookup key, never a version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchHead {
    pub id: SnapshotId,
    pub epoch: u64,
    pub author: DeviceId,
}

impl From<&HeadSummary> for BranchHead {
    fn from(head: &HeadSummary) -> Self {
        BranchHead {
            id: head.id,
            epoch: head.epoch,
            author: head.author,
        }
    }
}

impl BranchHead {
    /// First 8 hex characters of the snapshot id. A display
    /// disambiguator, not an address: nothing here resolves it.
    pub fn short_id(&self) -> String {
        let hex = self.id.to_string();
        hex.chars().take(8).collect()
    }

    /// The only branch label this surface renders. Names facts, never
    /// a number: no `@N`, no "version 1/2", no "head 1/2".
    pub fn label(&self) -> String {
        format!(
            "epoch {} author {} snapshot {}",
            self.epoch,
            self.author,
            self.short_id()
        )
    }
}

/// The standing drive-level verdict (OD-23-B option A): sourced from
/// the eligible heads, exactly as the decision prescribes. More than
/// one eligible head means the drive has diverged; zero or one means
/// it has not — including a fresh drive with zero heads, which is
/// none of the three divergences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DivergenceState {
    Converged,
    Diverged { branches: Vec<BranchHead> },
}

impl DivergenceState {
    /// Build the verdict from the eligible heads. The caller supplies
    /// the heads (from `snapshot_heads` eligibility or `live_heads`
    /// summaries); the type supplies the verdict, so no caller can
    /// re-mean a two-head drive as converged.
    pub fn from_branches(branches: &[BranchHead]) -> DivergenceState {
        if branches.len() >= 2 {
            DivergenceState::Diverged {
                branches: branches.to_vec(),
            }
        } else {
            DivergenceState::Converged
        }
    }

    pub fn is_diverged(&self) -> bool {
        matches!(self, DivergenceState::Diverged { .. })
    }
}

/// Render the standing indicator: diverged names every branch by its
/// facts; converged (including fresh) says so in one line.
pub fn render_indicator(state: &DivergenceState) -> String {
    match state {
        DivergenceState::Converged => {
            "divergence: none observed (zero or one eligible head)\n".to_owned()
        }
        DivergenceState::Diverged { branches } => {
            let mut out = format!("divergence: diverged ({} eligible heads)\n", branches.len());
            for branch in branches {
                out.push_str(&format!("  branch: {}\n", branch.label()));
            }
            out
        }
    }
}

/// The membership-freeze half (OD-23-C option A): a frozen epoch is
/// indicated and its remedy named; nothing is offered and nothing is
/// authored. The remedy lives in member management, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeState {
    Clear,
    Frozen { epoch: u64 },
}

impl FreezeState {
    /// Build from the frozen epoch, when the drive carries one.
    /// `None` (no freeze observed) is `Clear` — an absent observation
    /// never renders as a frozen epoch zero.
    pub fn from_frozen_epoch(epoch: Option<u64>) -> FreezeState {
        match epoch {
            Some(epoch) => FreezeState::Frozen { epoch },
            None => FreezeState::Clear,
        }
    }
}

/// Render the freeze half: the epoch and the remedy, in the same words
/// the CLI's frozen hint uses (`member resolve`), so the frozen drive
/// points one place from both surfaces.
pub fn render_freeze(state: &FreezeState) -> String {
    match state {
        FreezeState::Clear => "membership: not frozen\n".to_owned(),
        FreezeState::Frozen { epoch } => format!(
            "membership: frozen at epoch {epoch} (a membership conflict holds progress; member resolve is the remedy, offered in member management, never here)\n"
        ),
    }
}

/// What kind of divergence one namespace node carries. The standing
/// indicator stays drive-level; this names the per-path kind for the
/// inspection rows without walking anything — the caller supplies the
/// node the view already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeDivergence {
    /// The heads disagree at this path. Read through a
    /// version-qualified lookup, never direct.
    PathConflict,
    /// Every head resolves this path to a directory but the contents
    /// differ: a DAG conflict that is not a path conflict. Serves as
    /// one directory (union of children), so no marker in a listing.
    DirectoryContent,
    /// An ordinary node: agrees everywhere, or absent. Not a
    /// divergence of any kind.
    None,
}

/// Classify one resolved node. Total over `Node`: every current and
/// future variant maps somewhere, so a new node kind cannot silently
/// become (or stop being) a divergence.
pub fn classify_node(node: &Node) -> NodeDivergence {
    match node {
        Node::Conflict { .. } => NodeDivergence::PathConflict,
        Node::MergedDir { .. } => NodeDivergence::DirectoryContent,
        Node::File { .. } | Node::Dir { .. } | Node::Symlink { .. } => NodeDivergence::None,
    }
}

/// One root path where the selected heads disagree: the path plus, per
/// head in plan order, whether that head holds an entry there. An
/// absence is part of the divergence (present-vs-absent), never a
/// separate version and never unavailable — compare
/// [`describe_unavailable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDifference {
    pub path: String,
    pub cells: Vec<BranchCell>,
}

/// One head's cell in a difference row: which head (by facts where the
/// caller supplied them, else by short id alone) and whether it holds
/// the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCell {
    pub head: String,
    pub present: bool,
}

/// The bounded inspection outcome (OD-23-B plus the decider's extra
/// acceptance condition): either the complete differing set, or an
/// explicitly incomplete result. A partial set is never presented as
/// complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectionOutcome {
    /// Every differing root path, in plan (ascending) order.
    Complete { differences: Vec<PathDifference> },
    /// Budget exhaustion (or a partial read): the rows shown so far
    /// plus the totals that prove the set is not complete. Renders
    /// with the incomplete marker, never as the whole set.
    Incomplete {
        shown: Vec<PathDifference>,
        total_differing: usize,
        budget: usize,
    },
}

impl InspectionOutcome {
    pub fn is_complete(&self) -> bool {
        matches!(self, InspectionOutcome::Complete { .. })
    }
}

/// Run the bounded "what differs" comparison over a read-only merge
/// plan: filter to `!agreed()` root paths, cap at `budget` rows. Pure
/// over the plan the caller already built — this function plans
/// nothing, commits nothing, fetches nothing.
///
/// `branches` carries the branch facts for column labels, matched by
/// snapshot id; heads without facts label by short id alone (still no
/// `@N`). `budget` bounds the rows rendered: when the differing set
/// exceeds it, the outcome is `Incomplete` with the first `budget`
/// rows shown. A `budget` of zero with differences present is
/// incomplete, never empty-as-complete.
pub fn inspect_differences(
    plan: &MergePlan,
    branches: &[BranchHead],
    budget: usize,
) -> InspectionOutcome {
    let facts: BTreeMap<SnapshotId, &BranchHead> =
        branches.iter().map(|branch| (branch.id, branch)).collect();
    let label = |id: &SnapshotId| -> String {
        match facts.get(id) {
            Some(branch) => branch.label(),
            None => {
                let hex = id.to_string();
                let short: String = hex.chars().take(8).collect();
                format!("snapshot {short}")
            }
        }
    };
    let differing: Vec<PathDifference> = plan
        .paths
        .iter()
        .filter(|path| !path.agreed())
        .map(|path| difference_row(path, &plan.heads, &label))
        .collect();
    if differing.len() <= budget {
        InspectionOutcome::Complete {
            differences: differing,
        }
    } else {
        let shown = differing.into_iter().take(budget).collect::<Vec<_>>();
        InspectionOutcome::Incomplete {
            total_differing: plan.paths.iter().filter(|path| !path.agreed()).count(),
            budget,
            shown,
        }
    }
}

fn difference_row(
    path: &MergePath,
    heads: &[SnapshotId],
    label: &dyn Fn(&SnapshotId) -> String,
) -> PathDifference {
    PathDifference {
        path: path.path.clone(),
        cells: heads
            .iter()
            .map(|head| BranchCell {
                head: label(head),
                present: path.versions.get(head).is_some_and(Option::is_some),
            })
            .collect(),
    }
}

/// Render the inspection half: scope first ("root paths only"), then
/// the rows, then — for an incomplete outcome — the incomplete marker
/// with totals. An incomplete rendering never claims completeness.
pub fn render_inspection(outcome: &InspectionOutcome) -> String {
    let mut out = String::new();
    out.push_str("what differs: root paths only (this comparison lists root paths, not a recursive inventory of all differences)\n");
    match outcome {
        InspectionOutcome::Complete { differences } => {
            if differences.is_empty() {
                out.push_str("  no differing root paths\n");
            } else {
                for row in differences {
                    out.push_str(&format!("  {}: {}\n", row.path, render_cells(&row.cells)));
                }
            }
        }
        InspectionOutcome::Incomplete {
            shown,
            total_differing,
            budget,
        } => {
            for row in shown {
                out.push_str(&format!("  {}: {}\n", row.path, render_cells(&row.cells)));
            }
            out.push_str(&format!(
                "  incomplete: showing {} of {total_differing} differing root paths (inspection budget {budget}; this is not the complete set)\n",
                shown.len(),
            ));
        }
    }
    out
}

fn render_cells(cells: &[BranchCell]) -> String {
    cells
        .iter()
        .map(|cell| {
            format!(
                "[{}: {}]",
                cell.head,
                if cell.present { "present" } else { "absent" }
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why a branch's contents cannot be shown (OD-23-D option A): the
/// branch metadata stays, the contents render unavailable. Returns
/// `Some` copy for availability failures, `None` for structural
/// outcomes the caller already handles (absent paths, conflicted
/// paths, bad paths) — absence is divergence data, not unavailability.
///
/// Never an empty document, never a zero-byte file, never evidence the
/// branch holds no content: every `Some` copy says "unavailable".
pub fn describe_unavailable(error: &ViewError) -> Option<String> {
    match error {
        ViewError::NotMaterialized { .. } => Some(
            "contents unavailable (remote-only: no peer reachable holds these bytes right now; the branch metadata above still stands)\n"
                .to_owned(),
        ),
        ViewError::Unavailable { .. } => Some(
            "contents unavailable (no peer reachable and nothing cached; the branch metadata above still stands)\n"
                .to_owned(),
        ),
        ViewError::LostRepresentation { .. }
        | ViewError::RejectedRepresentation { .. }
        | ViewError::Corrupt
        | ViewError::Store(_, _) => Some(
            "contents unavailable (the stored representation cannot be read right now; the branch metadata above still stands)\n"
                .to_owned(),
        ),
        ViewError::NotFound
        | ViewError::InvalidPath
        | ViewError::NotADirectory
        | ViewError::NotAFile
        | ViewError::Conflict => None,
    }
}

/// Render the whole screen: indicator first, freeze beside it,
/// inspection below. The composition a host presenter takes over; the
/// vocabulary stays here.
pub fn render_conflict_screen(
    divergence: &DivergenceState,
    freeze: &FreezeState,
    inspection: Option<&InspectionOutcome>,
) -> String {
    let mut out = String::new();
    out.push_str(&render_indicator(divergence));
    out.push_str(&render_freeze(freeze));
    match inspection {
        Some(outcome) => out.push_str(&render_inspection(outcome)),
        None => out.push_str(
            "what differs: not requested in this view — the on-demand comparison refreshes against the live heads with its own inspection budget\n",
        ),
    }
    out
}

#[cfg(test)]
#[path = "tests_conflict_surface.rs"]
mod tests_conflict_surface;
