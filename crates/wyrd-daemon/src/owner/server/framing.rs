//! Wire framing and error mapping: bounded frame assembly, the
//! write path, and the refusal codes shared by dispatch and the
//! operations. No protocol bytes or codes change here.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use wyrd_core::read::ReadError;
use wyrd_core::view::{Node, ViewError};
use wyrd_core::want::WantError;
use wyrd_format::EntryIdentity;

use crate::owner::mapping::node_token;
use crate::owner::wire::{
    PathParams, WireError, WireErrorCode, WireMutationKind, WireResponse, WireResult, WIRE_VERSION,
};
use wyrd_owner_client::transport::MAX_FRAME_BYTES;
/// File content one submit carries, in decoded bytes: the commit
/// and append bodies, summed across fold members (recursively — a
/// nested fold is refused later as malformed, but its bytes still
/// count here). Measured on the hex length so an over-bound submit
/// refuses before any decode allocates. Saturating: the sum names a
/// magnitude for the refusal message, never an exact ledger.
pub(super) fn submit_content_bytes(kind: &WireMutationKind) -> u64 {
    match kind {
        WireMutationKind::CommitFile { content_hex, .. }
        | WireMutationKind::AppendFile { content_hex, .. } => (content_hex.len() / 2) as u64,
        WireMutationKind::Fold { members } => members
            .iter()
            .map(|member| submit_content_bytes(&member.kind))
            .fold(0u64, |total, carried| total.saturating_add(carried)),
        _ => 0,
    }
}

/// Build a refusal response to a parsed request.
pub(super) fn refuse(id: u64, code: WireErrorCode, message: String) -> WireResponse {
    WireResponse {
        v: WIRE_VERSION,
        id,
        result: WireResult::Err(WireError { code, message }),
    }
}

/// The bound-read refusal for a resolved node, decided before any
/// byte is touched: a conflicted path refuses as conflicted before
/// any token comparison (four conditions stay distinct, never
/// collapsed into one), then a presented token must equal the
/// node's own — `None` when the entry is identity-less — or the
/// read refuses as `StaleEntry`. Absent with no expectation serves;
/// the caller distinguishes "no token asked" from "token asked and
/// matched" nowhere, because both proceed identically.
pub(in crate::owner) fn bound_read_refusal(
    node: &Node,
    expected: Option<&EntryIdentity>,
    path: &str,
) -> Option<(WireErrorCode, String)> {
    if matches!(node, Node::Conflict { .. }) {
        return Some((
            WireErrorCode::Conflicted,
            format!("{path} is conflicted across heads"),
        ));
    }
    match (expected, node_token(node)) {
        (Some(wanted), found) if found.as_ref() != Some(wanted) => Some((
            WireErrorCode::StaleEntry,
            format!(
                "stale entry identity for {path:?}: the path no longer resolves to the verified entry"
            ),
        )),
        _ => None,
    }
}

/// The entry token binds reads only: a caller that passes one to
/// any path-addressed operation besides `read` believes the answer
/// is bound, and it is not — refuse loudly instead of serving an
/// unbound answer under a bound request.
pub(super) fn reject_bound_non_read(
    op: &str,
    params: &PathParams,
) -> Result<(), (WireErrorCode, String)> {
    if params.identity.is_some() {
        return Err((
            WireErrorCode::Malformed,
            format!("bad {op} params: identity binds reads only"),
        ));
    }
    Ok(())
}

/// Map a view failure to a wire refusal. Every variant names its
/// own code — the transport never reinterprets a failure, it carries
/// it.
pub(super) fn map_view_error(error: ViewError) -> (WireErrorCode, String) {
    let message = error.to_string();
    let code = match &error {
        ViewError::NotFound => WireErrorCode::NotFound,
        ViewError::InvalidPath => WireErrorCode::Malformed,
        ViewError::NotADirectory => WireErrorCode::NotADirectory,
        ViewError::NotAFile => WireErrorCode::NotAFile,
        ViewError::Conflict => WireErrorCode::Conflicted,
        ViewError::NotMaterialized { .. } => WireErrorCode::NotMaterialized,
        ViewError::Unavailable { .. } => WireErrorCode::Unavailable,
        ViewError::Corrupt => WireErrorCode::Corrupt,
        ViewError::RejectedRepresentation { .. } => WireErrorCode::RejectedRepresentation,
        ViewError::LostRepresentation { .. } => WireErrorCode::LostRepresentation,
        ViewError::Store(..) => WireErrorCode::Store,
    };
    (code, message)
}

/// Map a demand-driven read failure to a wire refusal. Timeouts and
/// saturated demand are transient — the message says retry — while
/// terminal verdicts carry the view's own code.
pub(super) fn map_read_error(error: ReadError) -> (WireErrorCode, String) {
    match error {
        ReadError::View(error) => map_view_error(error),
        ReadError::Timeout { content } => (
            WireErrorCode::Timeout,
            format!("fetch demand expired for {content}, retry"),
        ),
        ReadError::Terminal(error) => map_view_error(error),
        ReadError::Demand(WantError::Saturated) => (
            WireErrorCode::Saturated,
            "demand registry saturated, retry".to_string(),
        ),
        ReadError::Demand(WantError::TimedOut) => (
            WireErrorCode::Timeout,
            "demand wait expired, retry".to_string(),
        ),
        ReadError::Demand(WantError::Lock) => (
            WireErrorCode::Poisoned,
            "demand registry lock poisoned".to_string(),
        ),
    }
}

/// Read one length-delimited frame: u32 big-endian length, then
/// that many JSON bytes. Returns `Ok(None)` on an idle read deadline
/// at a frame boundary — the caller re-checks its stop flag and
/// polls again. Anything short of a complete frame ends the
/// connection with `Err`: a peer that goes quiet mid-frame (a short
/// length prefix or a short body at the deadline), a vanished peer,
/// an overlong length, or any transport error. The connection closes
/// without an answer and without dispatching the partial bytes — the
/// client treats the break as unconfirmed, and the next connection
/// starts at a fresh frame boundary, never mid-stream.
pub(super) fn read_frame(stream: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut prefix_read = 0;
    while prefix_read < len.len() {
        match stream.read(&mut len[prefix_read..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "owner interface peer closed mid-frame",
                ));
            }
            Ok(n) => prefix_read += n,
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                if prefix_read == 0 {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "owner interface frame stalled mid-length; closing",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds interface bound",
        ));
    }
    let mut frame = vec![0u8; len];
    let mut body_read = 0;
    while body_read < len {
        match stream.read(&mut frame[body_read..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "owner interface peer closed mid-frame",
                ));
            }
            Ok(n) => body_read += n,
            // The length prefix is already consumed: a stall here is
            // a torn frame however many body bytes arrived, including
            // none — idleness only polls at a frame boundary, never
            // mid-frame.
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "owner interface frame stalled mid-body; closing",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Some(frame))
}

/// Write one length-delimited frame.
/// Write one length-delimited frame. A response past the frame
/// ceiling never goes out: the connection closes instead, and the
/// client treats the break as unconfirmed. The read bound keeps
/// legitimate responses under the ceiling; this check is the
/// backstop that makes the bound load-bearing rather than
/// aspirational.
pub(super) fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "response exceeds frame bound",
        ));
    }
    let len = u32::try_from(bytes.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "response exceeds frame bound",
        )
    })?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}
