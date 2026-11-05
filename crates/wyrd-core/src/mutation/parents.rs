use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use wyrd_format::ContentId;

use super::kind::MutationError;

/// Most distinct parent paths whose session-local create tokens are retained.
/// Once full, new parent observations fail closed until an invalidation
/// removes entries.
pub const DEFAULT_MAX_PARENT_TOKENS: usize = 4096;

/// The base identity a writable handle opened against, and the identity
/// the loop compares against the current head at commit. A commit is
/// accepted only if the path still carries exactly this identity;
/// anything else — a content change, a kind change, or removal — is
/// [`MutationError::Stale`], never a silent merge.
///
/// The tuple is deliberately the full file identity (size, exec, ordered
/// chunk ids) rather than a single content id: `docs/contracts/write-path.md` names
/// kind, size, exec, and content as the comparison, and the format layer
/// does not yet expose a canonical file-node id. A cheaper id is future
/// work; it must not weaken the comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    size: u64,
    executable: bool,
    chunks: Vec<ContentId>,
}

impl FileIdentity {
    /// The identity of a regular file as the view presented it.
    pub fn new(size: u64, executable: bool, chunks: Vec<ContentId>) -> Self {
        FileIdentity {
            size,
            executable,
            chunks,
        }
    }

    /// The declared byte size.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Whether the exec bit is set.
    pub fn executable(&self) -> bool {
        self.executable
    }

    /// The ordered chunk ids.
    pub fn chunks(&self) -> &[ContentId] {
        &self.chunks
    }

    /// Retained chunk-list bytes: what this identity pins in memory
    /// while a writable handle holds it as its commit base. Counted
    /// alongside the handle's read capture by the open-handle byte
    /// budget.
    pub fn capture_bytes(&self) -> usize {
        self.chunks.len() * std::mem::size_of::<ContentId>()
    }
}

/// Opaque, session-local identity of a parent directory observed by a
/// presentation caller.
///
/// A token remains stable while the directory's descendants and siblings
/// change. The live loop rotates it when the directory itself is replaced or
/// removed, or when the eligible head set changes. A queued create carrying
/// an older token is rejected with [`MutationError::StaleParent`]. Tokens
/// are meaningful only within their owning mutation queue and process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParentToken(pub(super) u64);

impl ParentToken {
    /// The raw token value for the owner interface wire: opaque to
    /// the consumer, validated by the queue on use. Forgery buys
    /// nothing — an unknown value fails `validate_parent` closed
    /// and the create reports `StaleParent`.
    pub fn wire_value(&self) -> u64 {
        self.0
    }

    /// Rebuild from a wire value. No validation here; the queue
    /// validates on use, and a stale or forged value refuses as
    /// `StaleParent`, never as a wrong-directory create.
    pub fn from_wire_value(value: u64) -> Self {
        ParentToken(value)
    }
}

// Parent state is part of the mutation precondition, so poisoning fails
// closed here instead of recovering a possibly inconsistent token table.
#[derive(Debug, Default)]
struct ParentState {
    next: u64,
    tokens: HashMap<String, ParentToken>,
    blocked: HashSet<String>,
    block_all: bool,
}

#[derive(Debug)]
pub(super) struct ParentRegistry {
    state: Mutex<ParentState>,
    limit: usize,
    /// Lock-poison observations. Bumped once per failed acquisition,
    /// never reset.
    poison: AtomicU64,
}

impl Default for ParentRegistry {
    fn default() -> Self {
        Self::with_limit(DEFAULT_MAX_PARENT_TOKENS)
    }
}

fn is_blocked(blocked: &HashSet<String>, path: &str) -> bool {
    if blocked.contains(path) {
        return true;
    }
    let mut ancestor = path;
    while let Some((parent, _)) = ancestor.rsplit_once('/') {
        ancestor = parent;
        if blocked.contains(ancestor) {
            return true;
        }
    }
    false
}

impl ParentRegistry {
    pub(super) fn with_limit(limit: usize) -> Self {
        Self {
            state: Mutex::new(ParentState::default()),
            limit,
            poison: AtomicU64::new(0),
        }
    }

    /// Lock-poison observations since construction. Any nonzero value
    /// means the token state behind the capture/validate answers is
    /// suspect: creates still fail closed, but a `None` or `false`
    /// beside a nonzero count reads as "table dead", never as "stale
    /// parent".
    pub(super) fn poison_count(&self) -> u64 {
        self.poison.load(Ordering::Relaxed)
    }

    /// Record one poisoned acquisition, loudly: the counter names it
    /// for status consumers, the log names it for the operator. Only
    /// ever called with a lock already known poisoned.
    fn note_poison(&self, op: &'static str) {
        self.poison.fetch_add(1, Ordering::Relaxed);
        tracing::error!(op, "parent registry lock poisoned; token table unreachable");
    }

    pub(super) fn capture(&self, path: &str) -> Result<Option<ParentToken>, MutationError> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                self.note_poison("capture");
                return Err(MutationError::Lock);
            }
        };
        if state.block_all || is_blocked(&state.blocked, path) {
            return Ok(None);
        }
        if let Some(token) = state.tokens.get(path) {
            return Ok(Some(*token));
        }
        if state.tokens.len() >= self.limit {
            return Ok(None);
        }
        // Token-counter overflow is unreachable in practice (u64 per
        // path); if it ever happens the capture refuses like a
        // saturated table, exactly as before the poison binding.
        let Some(next) = state.next.checked_add(1) else {
            return Ok(None);
        };
        let token = ParentToken(next);
        state.next = next;
        state.tokens.insert(path.to_string(), token);
        Ok(Some(token))
    }

    pub(super) fn validate(&self, path: &str, token: ParentToken) -> Result<bool, MutationError> {
        match self.state.lock() {
            Ok(state) => Ok(!state.block_all && state.tokens.get(path) == Some(&token)),
            Err(_) => {
                self.note_poison("validate");
                Err(MutationError::Lock)
            }
        }
    }

    pub(super) fn invalidate_subtree(&self, path: &str) {
        let Ok(mut state) = self.state.lock() else {
            self.note_poison("invalidate_subtree");
            return;
        };
        if path.is_empty() {
            state.tokens.clear();
            state.blocked.clear();
            state.block_all = true;
            return;
        }
        let prefix = format!("{path}/");
        state
            .tokens
            .retain(|candidate, _| candidate != path && !candidate.starts_with(&prefix));
        state.blocked.insert(path.to_string());
    }

    pub(super) fn invalidate_all(&self) {
        let Ok(mut state) = self.state.lock() else {
            self.note_poison("invalidate_all");
            return;
        };
        state.tokens.clear();
        state.blocked.clear();
        state.block_all = true;
    }

    pub(super) fn publish(&self) {
        let Ok(mut state) = self.state.lock() else {
            self.note_poison("publish");
            return;
        };
        state.blocked.clear();
        state.block_all = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poison the parent table lock the way
    /// `poisoned_state_lock_is_recovered` does for the mutation
    /// queue: capture and validate name the poison instead of
    /// reading as blocked or stale, and the silent invalidations
    /// count instead of vanishing.
    #[test]
    fn poisoned_parent_table_names_poison_not_stale() {
        use std::sync::Arc;
        let registry = Arc::new(ParentRegistry::with_limit(8));
        let token = registry.capture("parent").unwrap().unwrap();
        assert!(registry.validate("parent", token).unwrap());
        assert_eq!(registry.poison_count(), 0, "no poison observed yet");
        let poisoner = Arc::clone(&registry);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.state.lock().unwrap();
            panic!("poison the parent table");
        })
        .join();
        // Capture and validate name the poison instead of a stale
        // parent: a dead table never presents as a retired inode.
        assert_eq!(registry.capture("parent"), Err(MutationError::Lock));
        assert_eq!(registry.validate("parent", token), Err(MutationError::Lock));
        // Silent invalidations count instead of vanishing.
        registry.invalidate_subtree("parent");
        registry.invalidate_all();
        registry.publish();
        assert!(registry.poison_count() > 0, "every silent path counts");
    }
}
