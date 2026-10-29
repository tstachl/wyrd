//! Read-side tests for the drive-owner interface (child 1): wire
//! round-trips, the field-name snapshot, dispatch over a composed
//! view, the pre-pass observation states, the read bound, and the
//! socket round trip. Mutation submission (child 2) and crash
//! answers (child 3) extend this file, not a second harness.

use super::server::{OwnerHandles, OwnerServer, MAX_FRAME_BYTES};
use super::wire::{
    PathParams, RawRequest, WireAttr, WireConvergenceState, WireDirEntry, WireDurabilityLevel,
    WireError, WireErrorCode, WireFreshness, WireFreshnessState, WireHeadClasses, WireHeadSummary,
    WireKind, WireLiveness, WireMailboxHealth, WireMailboxView, WireMaterializationSummary,
    WireNode, WireOk, WireOutboxTotals, WirePeerHandle, WirePendingObligations, WireQueueDepth,
    WireReconciliationCounters, WireResponse, WireResult, WireSyncStatus, OP_FRESHNESS,
    OP_LIVENESS, OP_LOOKUP, OP_OBSERVE, OP_READ, OP_READDIR, OP_STAT, WIRE_VERSION,
};

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wyrd_core::live::LiveParts;
use wyrd_core::view::{Attr, ConflictVersion, DirEntry, Kind, Node};
use wyrd_format::{ContentId, SnapshotId};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

use crate::vault::OwnerView;
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
fn wire_node_round_trips_every_variant() {
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
fn observation_is_repeatable_without_side_effects() {
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
