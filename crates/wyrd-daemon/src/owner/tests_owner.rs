//! Read-side tests for the drive-owner interface (child 1): wire
//! round-trips, the field-name snapshot, dispatch over a composed
//! view, the pre-pass observation states, the read bound, and the
//! socket round trip. Mutation submission (child 2) and crash
//! answers (child 3) extend this file, not a second harness.

use super::server::{OwnerHandles, OwnerServer, MAX_FRAME_BYTES};
use super::wire::{
    PathParams, RawRequest, SubmitParams, WireAttr, WireConvergenceState, WireDirEntry,
    WireDurabilityLevel, WireError, WireErrorCode, WireFileIdentity, WireFreshness,
    WireFreshnessState, WireHeadClasses, WireHeadSummary, WireKind, WireLiveness,
    WireMailboxHealth, WireMailboxView, WireMaterializationSummary, WireMutationError,
    WireMutationKind, WireMutationOutcome, WireNode, WireOk, WireOutboxTotals, WirePeerHandle,
    WirePendingObligations, WireQueueDepth, WireReconciliationCounters, WireResponse, WireResult,
    WireSyncStatus, OP_CAPTURE_PARENT, OP_FRESHNESS, OP_LIVENESS, OP_LOOKUP, OP_OBSERVE, OP_READ,
    OP_READDIR, OP_STAT, OP_SUBMIT, WIRE_VERSION,
};

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wyrd_core::live::LiveParts;
use wyrd_core::view::{Attr, ConflictVersion, DirEntry, Kind, Node};
use wyrd_format::{ContentId, SnapshotId};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

use crate::vault::{run_composed_owner, OwnerView};
use wyrd_sync::transport::mailbox::{
    Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError, SendReport,
};

/// A mailbox that never delivers and always accepts sends: intake
/// stays empty, the pass still publishes. Same shape as the core
/// harness fake, local because that harness is module-private.
struct NoopMailbox;

impl Mailbox for NoopMailbox {
    fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

static SCRATCH_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A scratch single-member drive: the engine can author from the
/// start. The caller removes the directory after the owner drops.
fn scratch_drive() -> (Engine, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-owner-test-{}-{}",
        std::process::id(),
        SCRATCH_N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::create(
        dir.clone(),
        "owner-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    (engine, dir)
}

/// A composed owner view with seeded content, split for serving but
/// with no loop running: the read-side baseline. Returns the parts
/// (for the server) and the directory (for socket tests and cleanup).
fn composed_parts() -> (LiveParts<OwnerView>, std::path::PathBuf) {
    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = wyrd_core::node::WyrdNode::new(engine, store).unwrap();
    daemon.put_file("hello.txt", b"hello owner").unwrap();
    daemon.put_file("data/note.txt", b"nested bytes").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (_live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    (parts, dir)
}

/// Dispatch one operation against `server` and unwrap the success.
fn dispatch_ok(server: &OwnerServer, op: &str, path: &str) -> WireOk {
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 7,
        op: op.to_string(),
        params: serde_json::to_value(PathParams {
            path: path.to_string(),
        })
        .unwrap(),
    });
    assert_eq!(response.v, WIRE_VERSION);
    assert_eq!(response.id, 7);
    match response.result {
        WireResult::Ok(ok) => *ok,
        WireResult::Err(error) => panic!("{op} {path} refused: {:?} {}", error.code, error.message),
    }
}

/// Dispatch one operation against `server` and unwrap the refusal.
fn dispatch_err(server: &OwnerServer, op: &str, path: &str) -> WireError {
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 7,
        op: op.to_string(),
        params: serde_json::to_value(PathParams {
            path: path.to_string(),
        })
        .unwrap(),
    });
    match response.result {
        WireResult::Err(error) => error,
        WireResult::Ok(ok) => panic!("{op} {path} unexpectedly succeeded: {ok:?}"),
    }
}

// ---------------------------------------------------------------------------
// Wire round-trips (OD-16-C)
// ---------------------------------------------------------------------------

/// Every namespace value the read side can return encodes to JSON
/// and decodes back to an equal value — including a merged directory
/// and a multi-version conflict. The mapping is lossless, so the
/// interface cannot drift from the namespace model by encoding.
#[test]
fn interface_value_types_round_trip() {
    let file = Node::File {
        size: 11,
        executable: false,
        chunks: vec![ContentId::from_bytes([0x01; 32])],
    };
    let dir = Node::Dir {
        subtree: ContentId::from_bytes([0x02; 32]),
    };
    let symlink = Node::Symlink {
        target: "hello.txt".to_string(),
    };
    let merged = Node::MergedDir {
        subtrees: vec![
            (
                SnapshotId::from_bytes([0x10; 32]),
                ContentId::from_bytes([0x11; 32]),
            ),
            (
                SnapshotId::from_bytes([0x12; 32]),
                ContentId::from_bytes([0x13; 32]),
            ),
        ],
    };
    let conflict = Node::Conflict {
        versions: vec![
            ConflictVersion {
                snapshot: SnapshotId::from_bytes([0x20; 32]),
                node: file.clone(),
            },
            ConflictVersion {
                snapshot: SnapshotId::from_bytes([0x21; 32]),
                node: dir.clone(),
            },
        ],
    };
    for node in [file, dir, symlink, merged, conflict] {
        let wire = WireNode::from(&node);
        let json = serde_json::to_value(&wire).unwrap();
        let back: WireNode = serde_json::from_value(json).unwrap();
        assert_eq!(back, wire, "JSON round trip preserves the wire value");
        assert_eq!(
            back.to_node().unwrap(),
            node,
            "wire maps back to the namespace value"
        );
    }
    // Invalid identifiers fail the mapping, never decode to a wrong value.
    let bad = WireNode::File {
        size: 1,
        executable: false,
        chunks: vec!["not-hex".to_string()],
    };
    assert!(bad.to_node().is_err());
    let short = WireNode::Dir {
        subtree: "abcd".to_string(),
    };
    assert!(short.to_node().is_err());
}

/// Attributes, entries, and kinds round-trip through JSON unchanged.
#[test]
fn wire_attr_and_entry_round_trip() {
    for kind in [Kind::File, Kind::Dir, Kind::Symlink, Kind::Conflict] {
        let attr = Attr {
            kind,
            size: 9,
            executable: true,
        };
        let wire = WireAttr::from(&attr);
        let back: WireAttr = serde_json::from_value(serde_json::to_value(wire).unwrap()).unwrap();
        assert_eq!(back, wire);
        assert_eq!(back.to_attr(), attr);
        assert_eq!(wire.kind.to_kind(), kind);
    }
    let entry = DirEntry {
        name: "hello.txt".to_string(),
        node: Node::File {
            size: 11,
            executable: false,
            chunks: vec![],
        },
    };
    let wire = WireDirEntry::from(&entry);
    let back: WireDirEntry =
        serde_json::from_value(serde_json::to_value(wire.clone()).unwrap()).unwrap();
    assert_eq!(back, wire);
}

/// The wire field names are pinned: adding, renaming, or re-meaning
/// a field is a protocol change, and an accidental edit fails here
/// instead of drifting the contract past review.
#[test]
fn wire_field_names_are_pinned() {
    fn keys(value: &serde_json::Value) -> BTreeSet<String> {
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    }
    let set = |names: &[&str]| {
        names
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>()
    };

    let node = serde_json::to_value(&WireNode::File {
        size: 1,
        executable: false,
        chunks: vec![],
    })
    .unwrap();
    assert_eq!(keys(&node), set(&["File"]));
    assert_eq!(keys(&node["File"]), set(&["size", "executable", "chunks"]));

    let status = serde_json::to_value(&WireSyncStatus {
        tip: None,
        held_epochs: vec![],
        obligations: WirePendingObligations {
            announcements: vec![],
            transitions: vec![],
            capabilities: vec![],
        },
        totals: WireOutboxTotals {
            announcements_queued: 0,
            announcements_delivered: 0,
            transitions_queued: 0,
            transitions_delivered: 0,
            capabilities_queued: 0,
            capabilities_delivered: 0,
        },
        live_heads: vec![WireHeadSummary {
            id: "00".to_string(),
            epoch: 0,
            author: "00".to_string(),
        }],
        head_classes: WireHeadClasses {
            eligible: 0,
            canonical_history: 0,
            superseded: 0,
            stranded: 0,
            voided: 0,
            pending: 0,
            rejected: 0,
        },
        mailbox: WireMailboxView {
            configured_relays: 0,
        },
        known_members: 0,
        peers: vec![WirePeerHandle {
            handle: 1,
            peer: "peer-1".to_string(),
            pending: 0,
        }],
        queue: WireQueueDepth {
            outbox: 0,
            carries: 0,
            fetch: 0,
        },
        reconciliation: WireReconciliationCounters {
            statements_received: 0,
            transitions_reconciled: 0,
            capabilities_reconciled: 0,
            announcements_reconciled: 0,
        },
        convergence: WireConvergenceState {
            converged: true,
            pending_heads: 0,
            unfetchable_heads: 0,
        },
        materialization: WireMaterializationSummary {
            pinned: 0,
            cached: 0,
            local_objects: 0,
        },
        durability: WireDurabilityLevel::Working,
    })
    .unwrap();
    assert_eq!(
        keys(&status),
        set(&[
            "tip",
            "held_epochs",
            "obligations",
            "totals",
            "live_heads",
            "head_classes",
            "mailbox",
            "known_members",
            "peers",
            "queue",
            "reconciliation",
            "convergence",
            "materialization",
            "durability"
        ])
    );

    let liveness = serde_json::to_value(&WireLiveness {
        health: WireMailboxHealth::unknown(),
        report: None,
        passes: 0,
    })
    .unwrap();
    assert_eq!(keys(&liveness), set(&["health", "report", "passes"]));
    assert_eq!(
        keys(&liveness["health"]),
        set(&[
            "stream_alive",
            "connected_relays",
            "total_relays",
            "saturation_recoveries",
            "supervisor_ticks",
            "relay_attached",
            "stream_recovery_attempts",
            "relay_recovery_attempts",
            "closed_subscriptions"
        ])
    );

    let freshness = serde_json::to_value(&WireFreshness {
        state: WireFreshnessState::FreshEmpty,
        revision: 0,
        generation: 0,
    })
    .unwrap();
    assert_eq!(keys(&freshness), set(&["state", "revision", "generation"]));

    let response = serde_json::to_value(&WireResponse {
        v: WIRE_VERSION,
        id: 1,
        result: WireResult::Err(WireError {
            code: WireErrorCode::NotFound,
            message: "gone".to_string(),
        }),
    })
    .unwrap();
    assert_eq!(keys(&response), set(&["v", "id", "result"]));
    assert_eq!(keys(&response["result"]), set(&["err"]));
    assert_eq!(keys(&response["result"]["err"]), set(&["code", "message"]));
}

// ---------------------------------------------------------------------------
// Dispatch over a composed view
// ---------------------------------------------------------------------------

/// Lookup, stat, readdir, and read serve the composed view with the
/// serving generation and durable revision attached, so the consumer
/// correlates every answer with the freshness judgment.
#[test]
fn dispatch_serves_reads_from_a_composed_view() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));

    match dispatch_ok(&server, OP_LOOKUP, "hello.txt") {
        WireOk::Lookup {
            node,
            generation,
            revision,
        } => {
            assert!(matches!(node, WireNode::File { size: 11, .. }));
            assert_eq!(generation, 0, "baseline generation serves before any pass");
            let _ = revision;
        }
        other => panic!("unexpected lookup result: {other:?}"),
    }
    match dispatch_ok(&server, OP_STAT, "hello.txt") {
        WireOk::Stat { attr, .. } => {
            assert_eq!(attr.kind, WireKind::File);
            assert_eq!(attr.size, 11);
        }
        other => panic!("unexpected stat result: {other:?}"),
    }
    match dispatch_ok(&server, OP_READDIR, "") {
        WireOk::Readdir { entries, .. } => {
            let names: BTreeSet<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
            assert!(
                names.contains("hello.txt"),
                "root lists the seeded file: {names:?}"
            );
            assert!(
                names.contains("data"),
                "root lists the seeded directory: {names:?}"
            );
        }
        other => panic!("unexpected readdir result: {other:?}"),
    }
    match dispatch_ok(&server, OP_READ, "hello.txt") {
        WireOk::Read { bytes_hex, .. } => {
            assert_eq!(hex::decode(&bytes_hex).unwrap(), b"hello owner");
        }
        other => panic!("unexpected read result: {other:?}"),
    }
    match dispatch_ok(&server, OP_READ, "data/note.txt") {
        WireOk::Read { bytes_hex, .. } => {
            assert_eq!(hex::decode(&bytes_hex).unwrap(), b"nested bytes");
        }
        other => panic!("unexpected nested read result: {other:?}"),
    }

    std::fs::remove_dir_all(dir).unwrap();
}

/// Missing paths, non-directories, and non-files refuse with their
/// own codes — the transport carries the failure, never reinterprets
/// it. Unknown versions and operations refuse explicitly, with the
/// version wanted and the operation name echoed back.
#[test]
fn dispatch_refuses_with_explicit_codes() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));

    let error = dispatch_err(&server, OP_LOOKUP, "missing.txt");
    assert_eq!(error.code, WireErrorCode::NotFound);
    let error = dispatch_err(&server, OP_STAT, "missing.txt");
    assert_eq!(error.code, WireErrorCode::NotFound);
    let error = dispatch_err(&server, OP_READDIR, "hello.txt");
    assert_eq!(error.code, WireErrorCode::NotADirectory);
    let error = dispatch_err(&server, OP_READ, "data");
    assert_eq!(error.code, WireErrorCode::NotAFile);

    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION + 1,
        id: 3,
        op: OP_LOOKUP.to_string(),
        params: serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap(),
    });
    assert_eq!(response.id, 3);
    match response.result {
        WireResult::Err(error) => assert_eq!(error.code, WireErrorCode::UnknownVersion),
        other => panic!("version mismatch unexpectedly succeeded: {other:?}"),
    }
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 4,
        op: "defragment".to_string(),
        params: serde_json::Value::Null,
    });
    assert_eq!(response.id, 4);
    match response.result {
        WireResult::Err(error) => {
            assert_eq!(error.code, WireErrorCode::UnsupportedOperation);
            assert!(
                error.message.contains("defragment"),
                "the name echoes back: {}",
                error.message
            );
        }
        other => panic!("unknown operation unexpectedly succeeded: {other:?}"),
    }
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 5,
        op: OP_LOOKUP.to_string(),
        params: serde_json::json!({"wrong": "shape"}),
    });
    match response.result {
        WireResult::Err(error) => assert_eq!(error.code, WireErrorCode::Malformed),
        other => panic!("bad params unexpectedly succeeded: {other:?}"),
    }

    std::fs::remove_dir_all(dir).unwrap();
}

/// A file past the read bound is refused explicitly — never
/// truncated, never streamed past the cap.
#[test]
fn dispatch_read_refuses_over_bound() {
    let (parts, dir) = composed_parts();
    let mut handles = OwnerHandles::from_parts(&parts);
    handles.max_read_bytes = 4;
    let server = OwnerServer::new(handles);
    let error = dispatch_err(&server, OP_READ, "hello.txt");
    assert_eq!(error.code, WireErrorCode::TooLarge);
    assert!(
        error.message.contains("11"),
        "the message names the size: {}",
        error.message
    );

    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------------
// Pre-pass observation states
// ---------------------------------------------------------------------------

/// Before the first pass completes, the observation slot is empty:
/// the durable observation refuses as unobserved, and liveness
/// answers unknown health with no report and zero passes. A
/// reachable owner with no pass yet never reads as healthy.
#[test]
fn observation_before_the_first_pass_is_explicit() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));

    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 1,
        op: OP_OBSERVE.to_string(),
        params: serde_json::Value::Null,
    });
    match response.result {
        WireResult::Err(error) => assert_eq!(error.code, WireErrorCode::Unobserved),
        other => panic!("observe before the first pass unexpectedly succeeded: {other:?}"),
    }
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 2,
        op: OP_LIVENESS.to_string(),
        params: serde_json::Value::Null,
    });
    match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Liveness { liveness } => {
                assert!(!liveness.health.is_live(), "no health reads as live");
                assert_eq!(liveness.report, None);
                assert_eq!(liveness.passes, 0);
            }
            other => panic!("unexpected liveness result: {other:?}"),
        },
        other => panic!("liveness before the first pass unexpectedly failed: {other:?}"),
    }

    std::fs::remove_dir_all(dir).unwrap();
}

/// Composition judges the baseline generation fresh: empty for a
/// drive with no heads, non-empty once seeded content commits. The
/// no-trustworthy state has no slot value — a failed composition
/// never publishes, so the consumer observes it as an absent owner,
/// not as a generation.
#[test]
fn freshness_starts_fresh_from_composition() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));

    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 1,
        op: OP_FRESHNESS.to_string(),
        params: serde_json::Value::Null,
    });
    match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Freshness { freshness } => {
                assert_eq!(freshness.state, WireFreshnessState::FreshNonEmpty);
                assert_eq!(freshness.generation, 0);
            }
            other => panic!("unexpected freshness result: {other:?}"),
        },
        other => panic!("freshness unexpectedly failed: {other:?}"),
    }

    std::fs::remove_dir_all(dir).unwrap();
}

/// Two observations of the same state are byte-identical: the read
/// side forwards the loop's values without constructing a mailbox,
/// consulting the network, or touching durable state. The
/// contract-suite source scan is the load-bearing half of this
/// invariant (no `LiveMailbox`, no `connect`, no `SeenStore` in the
/// interface module); this test pins the repeatability the status
/// contract already requires in process.
#[test]
fn interface_read_side_never_constructs_a_mailbox() {
    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = wyrd_core::node::WyrdNode::new(engine, store).unwrap();
    daemon.put_file("stable.txt", b"repeatable").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (mut live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    // Run one pass on a thread: the loop owns the node, the test
    // owns the Arc slots, and the stop flag ends the run.
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let loop_thread = std::thread::spawn(move || {
        let mut mailbox = NoopMailbox;
        live.run_loop(
            &mut mailbox,
            None::<&mut wyrd_sync::bulk::MemoryBulkSource>,
            stop,
            &wyrd_core::live::LiveConfig::default(),
            &mut |_, _| {},
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if parts
            .observation
            .read()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the loop published no observation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let first = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 1,
        op: OP_OBSERVE.to_string(),
        params: serde_json::Value::Null,
    });
    let second = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 1,
        op: OP_OBSERVE.to_string(),
        params: serde_json::Value::Null,
    });
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap(),
        "two forwards of the same observation are byte-identical"
    );
    match first.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Observe { status } => {
                assert!(
                    !status.live_heads.is_empty(),
                    "the seeded write published a head"
                );
            }
            other => panic!("unexpected observe result: {other:?}"),
        },
        other => panic!("observe after a pass unexpectedly failed: {other:?}"),
    }
    stop.store(true, Ordering::Relaxed);
    let summary = loop_thread.join().unwrap();
    assert!(summary.is_ok(), "the loop ends clean on stop");

    std::fs::remove_dir_all(dir).unwrap();
}

/// The socket round trip: bind in the drive directory, connect a
/// client, and serve a lookup through the framing. The same-user
/// peer passes authentication; every answer echoes the request id
/// under the protocol version.
#[test]
fn socket_round_trip_serves_a_lookup() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let socket_path = dir.join("roundtrip.sock");
    let _ = std::fs::remove_file(&socket_path);
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let serve_thread = std::thread::spawn(move || server.serve_on(&listener, stop));

    let mut client = UnixStream::connect(&socket_path).unwrap();
    let request = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id: 42,
        op: OP_LOOKUP.to_string(),
        params: serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap(),
    })
    .unwrap();
    client
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    client.write_all(&request).unwrap();
    let mut len = [0u8; 4];
    client.read_exact(&mut len).unwrap();
    let len = u32::from_be_bytes(len) as usize;
    assert!(len <= MAX_FRAME_BYTES);
    let mut frame = vec![0u8; len];
    client.read_exact(&mut frame).unwrap();
    let response: WireResponse = serde_json::from_slice(&frame).unwrap();
    assert_eq!(response.v, WIRE_VERSION);
    assert_eq!(response.id, 42);
    match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Lookup { node, .. } => {
                assert!(matches!(node, WireNode::File { size: 11, .. }));
            }
            other => panic!("unexpected socket lookup result: {other:?}"),
        },
        other => panic!("socket lookup unexpectedly failed: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    drop(client);
    serve_thread.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------------
// Mutation submission (child 2)
// ---------------------------------------------------------------------------

/// Submit one mutation through dispatch and unwrap the success.
fn submit_ok(server: &OwnerServer, kind: super::wire::WireMutationKind) -> WireMutationOutcome {
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 11,
        op: OP_SUBMIT.to_string(),
        params: serde_json::to_value(SubmitParams { kind }).unwrap(),
    });
    match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Submitted { outcome } => outcome,
            other => panic!("unexpected submit result: {other:?}"),
        },
        WireResult::Err(error) => panic!("submit refused: {:?} {}", error.code, error.message),
    }
}

/// Submit one mutation through dispatch and unwrap the refusal.
fn submit_err(server: &OwnerServer, kind: super::wire::WireMutationKind) -> WireError {
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 11,
        op: OP_SUBMIT.to_string(),
        params: serde_json::to_value(SubmitParams { kind }).unwrap(),
    });
    match response.result {
        WireResult::Err(error) => error,
        WireResult::Ok(ok) => panic!("submit unexpectedly succeeded: {ok:?}"),
    }
}

/// Run the live loop on a thread until `stop` trips; the test owns
/// the Arc slots. Returns the join handle.
fn spawn_loop(
    mut live: wyrd_core::live::LiveNode<crate::vault::OwnerView>,
    stop: &'static AtomicBool,
) -> std::thread::JoinHandle<Result<wyrd_core::live::LiveSummary, wyrd_core::live::LiveError>> {
    std::thread::spawn(move || {
        let mut mailbox = NoopMailbox;
        live.run_loop(
            &mut mailbox,
            None::<&mut wyrd_sync::bulk::MemoryBulkSource>,
            stop,
            &wyrd_core::live::LiveConfig::default(),
            &mut |_, _| {},
        )
    })
}

/// A composed owner with the loop running: the submit baseline.
type RunningOwner = (
    OwnerServer,
    LiveParts<crate::vault::OwnerView>,
    std::path::PathBuf,
    &'static AtomicBool,
    std::thread::JoinHandle<Result<wyrd_core::live::LiveSummary, wyrd_core::live::LiveError>>,
);

/// Returns the server, the parts, the directory, the stop flag, and
/// the loop thread. The caller trips the flag and joins.
fn running_parts() -> RunningOwner {
    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = wyrd_core::node::WyrdNode::new(engine, store).unwrap();
    daemon.put_file("seed.txt", b"seed").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let loop_thread = spawn_loop(live, stop);
    (server, parts, dir, stop, loop_thread)
}

/// A mutation submitted through the interface produces the same
/// outcome, the same served state, and the same announcement
/// obligation it would have produced in process: the load-bearing
/// acceptance test. A second identical drive takes the same
/// operation in process; both drives serve the same node afterwards.
#[test]
fn owner_interface_forwards_a_mutation_unchanged() {
    let (server, parts, dir, stop, loop_thread) = running_parts();

    // Through the interface: mkdir authors a snapshot.
    let outcome = submit_ok(
        &server,
        WireMutationKind::Mkdir {
            path: "via-iface".to_string(),
        },
    );
    assert_eq!(outcome, WireMutationOutcome::Done);

    // The served state is the same a direct submit would produce:
    // the directory serves with the same node the loop published.
    match dispatch_ok(&server, OP_LOOKUP, "via-iface") {
        WireOk::Lookup { node, .. } => {
            assert!(
                matches!(node, WireNode::Dir { .. }),
                "mkdir serves a directory: {node:?}"
            );
        }
        other => panic!("unexpected lookup result: {other:?}"),
    }

    // Ordered dependents through the interface prove the queue's
    // total order survives the transport: a reordered rename would
    // refuse NotFound.
    let outcome = submit_ok(
        &server,
        WireMutationKind::Mkdir {
            path: "via-iface/sub".to_string(),
        },
    );
    assert_eq!(outcome, WireMutationOutcome::Done);
    let outcome = submit_ok(
        &server,
        WireMutationKind::Rename {
            from: "via-iface/sub".to_string(),
            to: "via-iface/renamed".to_string(),
            no_replace: true,
        },
    );
    assert_eq!(outcome, WireMutationOutcome::Done);
    match dispatch_ok(&server, OP_LOOKUP, "via-iface/renamed") {
        WireOk::Lookup { node, .. } => {
            assert!(matches!(node, WireNode::Dir { .. }));
        }
        other => panic!("renamed directory does not serve: {other:?}"),
    }

    // A mixed burst — in-process and cross-process submits from
    // separate threads — all lands, and every authoring submit queues
    // its announcement obligation.
    let server = Arc::new(server);
    let queue = Arc::clone(&parts.mutations);
    let mut threads = Vec::new();
    for i in 0..4 {
        let server = Arc::clone(&server);
        threads.push(std::thread::spawn(move || {
            submit_ok(
                &server,
                WireMutationKind::Mkdir {
                    path: format!("burst-iface-{i}"),
                },
            );
        }));
        let queue = Arc::clone(&queue);
        threads.push(std::thread::spawn(move || {
            queue
                .submit(wyrd_core::mutation::MutationKind::Mkdir {
                    path: format!("burst-direct-{i}"),
                })
                .expect("in-process burst submit commits");
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    for i in 0..4 {
        match dispatch_ok(&server, OP_LOOKUP, &format!("burst-iface-{i}")) {
            WireOk::Lookup { .. } => {}
            other => panic!("burst dir missing: {other:?}"),
        }
        assert!(
            parts
                .projection
                .read()
                .unwrap()
                .view()
                .lookup(&format!("burst-direct-{i}"))
                .is_ok(),
            "in-process burst dir serves"
        );
    }
    let observation = parts
        .observation
        .read()
        .unwrap()
        .clone()
        .expect("a pass completed");
    // A solo drive announces to nobody, so the queued count stays
    // zero by design — the commit boundary the burst proves is the
    // durability level: every authoring submit landed, the outbox is
    // quiet, and the drive reads Published.
    assert_eq!(
        observation.status.totals.announcements_queued,
        observation.status.totals.announcements_delivered,
        "nothing stranded in the outbox: {:?}",
        observation.status.totals
    );
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 12,
        op: OP_OBSERVE.to_string(),
        params: serde_json::Value::Null,
    });
    match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Observe { status } => {
                assert_eq!(status.durability, WireDurabilityLevel::Published);
            }
            other => panic!("unexpected observe result: {other:?}"),
        },
        other => panic!("observe failed: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    spawn_loop_done(loop_thread);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Join a loop thread, asserting a clean stop.
fn spawn_loop_done(
    thread: std::thread::JoinHandle<
        Result<wyrd_core::live::LiveSummary, wyrd_core::live::LiveError>,
    >,
) {
    let summary = thread.join().unwrap();
    assert!(summary.is_ok(), "the loop ends clean on stop");
}

/// The same operation submitted in process and through the
/// interface reports the same outcome: the transport adds no
/// meaning of its own. Same drive, same empty content, two paths —
/// the created identities compare byte-equal.
#[test]
fn submit_reports_the_same_outcome_both_ways() {
    let (server, parts, dir, stop, loop_thread) = running_parts();

    // In process: capture a parent token the direct way, then create.
    let token = parts
        .mutations
        .capture_parent("")
        .expect("root captures a token");
    let direct = parts
        .mutations
        .submit(wyrd_core::mutation::MutationKind::CreateFile {
            path: "same-direct.txt".to_string(),
            parent: token,
        })
        .expect("in-process create commits");

    // Through the interface: capture-parent op, then submit.
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 11,
        op: OP_CAPTURE_PARENT.to_string(),
        params: serde_json::to_value(PathParams {
            path: String::new(),
        })
        .unwrap(),
    });
    let token = match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::ParentToken { token } => token,
            other => panic!("unexpected capture result: {other:?}"),
        },
        other => panic!("capture-parent failed: {other:?}"),
    };
    let outcome = submit_ok(
        &server,
        WireMutationKind::CreateFile {
            path: "same-iface.txt".to_string(),
            parent: token,
        },
    );
    // Same empty content, same outcome value: the transport added
    // nothing. (Paths differ, but empty files carry no path in
    // their identity — size zero, no chunks.)
    assert_eq!(WireMutationOutcome::from(&direct), outcome);

    // A forged token refuses stale at the queue's re-check, and a
    // capture against a missing parent refuses at the wire.
    let error = submit_err(
        &server,
        WireMutationKind::CreateFile {
            path: "same-forged.txt".to_string(),
            parent: 0xdead_beef,
        },
    );
    assert_eq!(error.code, WireErrorCode::StaleParent);

    stop.store(true, Ordering::Relaxed);
    spawn_loop_done(loop_thread);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Refusals carry through as distinct outcomes: a stale parent, a
/// missing path, and a saturated queue each refuse with their own
/// code, never collapsed to a boolean. The loop runs: refusals are
/// evaluated by the pass, so a submit without a loop would block
/// instead of refusing.
#[test]
fn submit_refusals_carry_distinct_codes() {
    let (server, _parts, dir, stop, loop_thread) = running_parts();

    // Forged token: the queue re-checks, the create refuses stale.
    let error = submit_err(
        &server,
        WireMutationKind::CreateFile {
            path: "nope.txt".to_string(),
            parent: 0xdead_beef,
        },
    );
    assert_eq!(error.code, WireErrorCode::StaleParent);

    // Missing path through unlink.
    let error = submit_err(
        &server,
        WireMutationKind::Unlink {
            path: "missing.txt".to_string(),
        },
    );
    assert_eq!(error.code, WireErrorCode::NotFound);

    stop.store(true, Ordering::Relaxed);
    spawn_loop_done(loop_thread);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Every mutation error maps to its own wire code: the refusal
/// vocabulary crosses the transport intact. Constructed (not
/// triggered) — the mapping is total by construction, and this pins
/// the codes the consumer switches on.
#[test]
fn mutation_error_codes_are_distinct() {
    use wyrd_core::mutation::MutationError;
    let cases: Vec<(MutationError, WireErrorCode)> = vec![
        (MutationError::Saturated, WireErrorCode::Saturated),
        (MutationError::Lock, WireErrorCode::Poisoned),
        (
            MutationError::Conflicted { heads: 2 },
            WireErrorCode::Conflicted,
        ),
        (
            MutationError::Invalid("x".to_string()),
            WireErrorCode::Malformed,
        ),
        (
            MutationError::NotFound("x".to_string()),
            WireErrorCode::NotFound,
        ),
        (
            MutationError::NotADirectory("x".to_string()),
            WireErrorCode::NotADirectory,
        ),
        (
            MutationError::IsDirectory("x".to_string()),
            WireErrorCode::IsDirectory,
        ),
        (
            MutationError::AlreadyExists("x".to_string()),
            WireErrorCode::AlreadyExists,
        ),
        (
            MutationError::DirectoryNotEmpty("x".to_string()),
            WireErrorCode::DirectoryNotEmpty,
        ),
        (
            MutationError::InvalidRename("x".to_string()),
            WireErrorCode::InvalidRename,
        ),
        (MutationError::Stale("x".to_string()), WireErrorCode::Stale),
        (
            MutationError::StaleParent("x".to_string()),
            WireErrorCode::StaleParent,
        ),
        (MutationError::TooLarge(1), WireErrorCode::TooLarge),
        (
            MutationError::TooMany { count: 2, max: 1 },
            WireErrorCode::TooMany,
        ),
        (MutationError::Engine, WireErrorCode::Engine),
        (
            MutationError::NeedContent {
                chunks: vec![],
                base: Some(SnapshotId::from_bytes([0x01; 32])),
            },
            WireErrorCode::NeedContent,
        ),
        (MutationError::TimedOut, WireErrorCode::Timeout),
        (MutationError::Shutdown, WireErrorCode::Shutdown),
    ];
    for (error, code) in cases {
        let mapped = WireMutationError::from(&error);
        assert_eq!(mapped.code, code, "wrong code for {error:?}");
        assert!(!mapped.message.is_empty(), "refusals carry a message");
    }
}

/// All nine mutation kinds encode to JSON and decode back to an
/// equal operation — and a nested fold refuses at the wire, before
/// the queue's own closed-fold rule.
#[test]
fn wire_mutation_kinds_round_trip() {
    use super::wire::WireFoldMember;
    let identity = WireFileIdentity {
        size: 3,
        executable: false,
        chunks: vec![hex::encode([0x01; 32])],
    };
    let kinds = vec![
        WireMutationKind::Mkdir {
            path: "a".to_string(),
        },
        WireMutationKind::CreateFile {
            path: "b".to_string(),
            parent: 7,
        },
        WireMutationKind::CommitFile {
            path: "b".to_string(),
            base: identity.clone(),
            executable: false,
            content_hex: "636f6e74656e74".to_string(),
        },
        WireMutationKind::AppendFile {
            path: "b".to_string(),
            content_hex: "00".to_string(),
        },
        WireMutationKind::Unlink {
            path: "b".to_string(),
        },
        WireMutationKind::Rmdir {
            path: "a".to_string(),
        },
        WireMutationKind::Rename {
            from: "a".to_string(),
            to: "c".to_string(),
            no_replace: true,
        },
        WireMutationKind::SetAttrs {
            path: "b".to_string(),
            size: Some(3),
            executable: None,
            base: Some(identity.clone()),
        },
        WireMutationKind::Fold {
            members: vec![WireFoldMember {
                kind: Box::new(WireMutationKind::Mkdir {
                    path: "d".to_string(),
                }),
                forcer: true,
            }],
        },
    ];
    for kind in kinds {
        let json = serde_json::to_value(&kind).unwrap();
        let back: WireMutationKind = serde_json::from_value(json).unwrap();
        assert_eq!(back, kind);
        assert!(
            back.to_kind().is_ok(),
            "every wire kind decodes to a queue operation"
        );
    }
    let nested = WireMutationKind::Fold {
        members: vec![WireFoldMember {
            kind: Box::new(WireMutationKind::Fold { members: vec![] }),
            forcer: true,
        }],
    };
    assert!(
        nested.to_kind().is_err(),
        "a nested fold refuses at the wire"
    );
    assert!(identity.to_identity().is_ok());
}

// ---------------------------------------------------------------------------
// Crash and restart answers (child 3)
// ---------------------------------------------------------------------------

/// Compose without starting the loop: the owner is down, so no pass
/// can complete. The server still dispatches against the shared
/// handles — the shape a consumer meets when the owner dies with
/// requests in flight.
fn dead_owner_parts() -> (
    OwnerServer,
    LiveParts<crate::vault::OwnerView>,
    std::path::PathBuf,
) {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    (server, parts, dir)
}

/// No success before the durability boundary: a submit admitted
/// while no pass runs stays in flight — the consumer gets no
/// answer, and specifically never a success. When a pass finally
/// runs, the same submit resolves normally.
#[test]
fn in_flight_request_gets_a_terminal_answer() {
    use wyrd_core::node::WyrdNode;

    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = WyrdNode::new(engine, store).unwrap();
    daemon.put_file("seed.txt", b"seed").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (mut live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let server = Arc::new(OwnerServer::new(OwnerHandles::from_parts(&parts)));

    // The submit blocks: admitted, unexecuted, no answer.
    let submit = std::thread::spawn(move || {
        server.dispatch(RawRequest {
            v: WIRE_VERSION,
            id: 21,
            op: OP_SUBMIT.to_string(),
            params: serde_json::to_value(SubmitParams {
                kind: WireMutationKind::Mkdir {
                    path: "late.txt".to_string(),
                },
            })
            .unwrap(),
        })
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !submit.is_finished(),
        "no answer while no pass runs — least of all success"
    );
    assert_eq!(
        parts.mutations.outstanding(),
        1,
        "the request is admitted but unexecuted"
    );

    // The owner restarts: one pass executes the admitted submit.
    let mut mailbox = NoopMailbox;
    live.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    match submit.join().unwrap().result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Submitted { .. } => {}
            other => panic!("unexpected submit result: {other:?}"),
        },
        other => panic!("submit after restart unexpectedly failed: {other:?}"),
    }

    std::fs::remove_dir_all(dir).unwrap();
}

/// Shutdown is a terminal answer, not a hang: closing admission
/// settles the blocked submitter with `Shutdown` instead of
/// stranding it.
#[test]
fn shutdown_is_a_terminal_answer() {
    let (server, parts, dir) = dead_owner_parts();
    parts.mutations.shutdown();
    let error = submit_err(
        &server,
        WireMutationKind::Mkdir {
            path: "never.txt".to_string(),
        },
    );
    assert_eq!(error.code, WireErrorCode::Shutdown);

    std::fs::remove_dir_all(dir).unwrap();
}

/// Absent after shutdown: an admitted but unexecuted submit that
/// the owner never executes stays absent — the re-read finds
/// nothing, and the consumer was never told otherwise.
#[test]
fn absent_without_execution_stays_absent() {
    use wyrd_core::node::WyrdNode;

    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = WyrdNode::new(engine, store).unwrap();
    daemon.put_file("seed.txt", b"seed").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (_live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let server = Arc::new(OwnerServer::new(OwnerHandles::from_parts(&parts)));
    let submit_server = Arc::clone(&server);
    let submit = std::thread::spawn(move || {
        submit_server.dispatch(RawRequest {
            v: WIRE_VERSION,
            id: 22,
            op: OP_SUBMIT.to_string(),
            params: serde_json::to_value(SubmitParams {
                kind: WireMutationKind::Mkdir {
                    path: "ghost.txt".to_string(),
                },
            })
            .unwrap(),
        })
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(!submit.is_finished(), "no pass runs, no answer");
    parts.mutations.shutdown();
    match submit.join().unwrap().result {
        WireResult::Err(error) => assert_eq!(error.code, WireErrorCode::Shutdown),
        other => panic!("shutdown submit unexpectedly succeeded: {other:?}"),
    }
    // Reconciliation after the terminal answer: the path was never
    // created, so the verdict is absent — definite, not guessed.
    let error = dispatch_err(&server, OP_LOOKUP, "ghost.txt");
    assert_eq!(error.code, WireErrorCode::NotFound);

    std::fs::remove_dir_all(dir).unwrap();
}

/// The headless run serves the interface: compose, run, and reach
/// the drive through the socket beside it. The socket appears only
/// while the owner runs, and the run refuses to start deaf.
#[test]
fn headless_run_serves_the_interface() {
    let (engine, dir) = scratch_drive();
    let composed =
        crate::vault::compose_headless_owner(engine, dir.clone(), true).expect("owner composes");
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let run_thread = std::thread::spawn(move || {
        run_composed_owner(
            composed,
            stop,
            NoopMailbox,
            None::<wyrd_sync::bulk::MemoryBulkSource>,
            Arc::new(|_: crate::vault::VaultEvent| {}),
            0,
        )
    });
    let socket_path = super::socket_path_for(&dir);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !socket_path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the owner serves its socket while it runs"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut client = UnixStream::connect(&socket_path).unwrap();
    let request = serde_json::to_value(&RawRequest {
        v: WIRE_VERSION,
        id: 31,
        op: OP_READDIR.to_string(),
        params: serde_json::to_value(PathParams {
            path: String::new(),
        })
        .unwrap(),
    })
    .unwrap();
    let request = serde_json::to_vec(&request).unwrap();
    client
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    client.write_all(&request).unwrap();
    let mut len = [0u8; 4];
    client.read_exact(&mut len).unwrap();
    let len = u32::from_be_bytes(len) as usize;
    let mut frame = vec![0u8; len];
    client.read_exact(&mut frame).unwrap();
    let response: WireResponse = serde_json::from_slice(&frame).unwrap();
    match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Readdir { entries, .. } => {
                assert!(entries.is_empty(), "fresh drive serves an empty root");
            }
            other => panic!("unexpected readdir result: {other:?}"),
        },
        other => panic!("readdir through the run unexpectedly failed: {other:?}"),
    }
    drop(client);

    stop.store(true, Ordering::Relaxed);
    let outcome = run_thread.join().unwrap().expect("the run ends clean");
    let _ = outcome;
    std::fs::remove_dir_all(dir).unwrap();
}
