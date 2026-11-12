use wyrd_format::{ContentId, EntryIdentity};

use super::{ConflictVersion, Node, ViewError};

/// Merge per-head resolutions: unanimous absence is not-found,
/// unanimous presence with agreement serves, presence with all-dir
/// disagreement merges structurally — the path is a directory in every
/// head, so it serves as one directory and only its children can
/// conflict. Anything else — kind divergence, presence versus deletion,
/// differing leaf identity — is a path conflict. Presence versus
/// deletion disagrees — deletion is a state change, so a path surviving
/// in only some heads never serves quietly. Versions list only the
/// heads where the path resolves. Each resolution carries its head's
/// `(snapshot, timestamp)` so conflict versions label by snapshot and
/// timestamp without peer identity; merged directories drop the
/// timestamps (they are not conflicts, and nested conflicts re-resolve
/// timestamps from heads).
pub(crate) fn merge(
    resolutions: Vec<(wyrd_format::SnapshotId, u64, Option<Node>)>,
) -> Result<Node, ViewError> {
    let mut present = Vec::with_capacity(resolutions.len());
    for (snapshot, timestamp, node) in &resolutions {
        if let Some(node) = node {
            present.push((*snapshot, *timestamp, node.clone()));
        }
    }
    if present.is_empty() {
        return Err(ViewError::NotFound);
    }
    if present.len() == resolutions.len() {
        let first = &present[0].2;
        if present.iter().all(|(_, _, node)| node == first) {
            return Ok(first.clone());
        }
        if let Some(subtrees) = all_dirs(&present) {
            // The union serves as one directory; the identity it
            // reports is the agreed token when every merged head
            // carries the identical `Some` token, and `None`
            // otherwise. Presentational only: the union never
            // resolves, and differing tokens still merge.
            return Ok(Node::MergedDir {
                subtrees,
                identity: agreed_identity(&present),
            });
        }
    }
    Ok(Node::Conflict {
        versions: present
            .into_iter()
            .map(|(snapshot, timestamp, node)| ConflictVersion {
                snapshot,
                timestamp,
                node,
            })
            .collect(),
    })
}

/// The token every merged head agrees on: `Some(T)` when each
/// head carries the identical `Some(T)`, `None` when any head
/// carries `None` or a different token. Only whole-directory
/// unions consult it — `all_dirs` yields `Some` solely for
/// all-`Dir` resolutions, so the non-`Dir` arms below are
/// defensive unreachable, kept so a future caller broadening the
/// inputs fails safe instead of agreeing loudly.
/// Conflict versions keep their own tokens and never agree.
fn agreed_identity(present: &[(wyrd_format::SnapshotId, u64, Node)]) -> Option<EntryIdentity> {
    let mut tokens = present.iter().map(|(_, _, node)| match node {
        Node::File { identity, .. }
        | Node::Dir { identity, .. }
        | Node::Symlink { identity, .. } => *identity,
        Node::MergedDir { identity, .. } => *identity,
        Node::Conflict { .. } => None,
    });
    let first = tokens.next().flatten()?;
    if tokens.all(|token| token == Some(first)) {
        Some(first)
    } else {
        None
    }
}

/// The per-head subtrees when every head resolves the path to a
/// directory, or `None` when any head resolves to a different kind.
fn all_dirs(
    present: &[(wyrd_format::SnapshotId, u64, Node)],
) -> Option<Vec<(wyrd_format::SnapshotId, ContentId)>> {
    present
        .iter()
        .map(|(snapshot, _, node)| match node {
            Node::Dir { subtree, .. } => Some((*snapshot, *subtree)),
            _ => None,
        })
        .collect()
}
